//! LAN guest bench: pair to phone host, probe + send file, write results markdown.
//!
//! Usage:
//!   guest_lan_bench <host> <port> <code> <file> [probe_bytes] [report_path]

use lb_core::{
    establish_sender, pair_as_guest, send_file, DeviceIdentity, PairingCode, SendOptions,
};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut args = std::env::args().skip(1);
    let host = args
        .next()
        .ok_or("usage: guest_lan_bench <host> <port> <code> <file> [probe_bytes] [report]")?;
    let port: u16 = args.next().ok_or("port")?.parse()?;
    let code = PairingCode::parse(&args.next().ok_or("code")?)?;
    let file = PathBuf::from(args.next().ok_or("file")?);
    let probe_bytes: u64 = args
        .next()
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(8 * 1024 * 1024);
    let report = args.next().map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../data/results/m4-lan-phone-bench.md")
    });

    if !file.is_file() {
        return Err(format!("not a file: {}", file.display()).into());
    }
    let file_len = std::fs::metadata(&file)?.len();
    let remote = file
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("blob.bin")
        .to_string();
    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    let identity = Arc::new(DeviceIdentity::generate(
        format!(
            "win-bench-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis()
        ),
        "PC-bench",
    )?);

    println!("pairing {addr} …");
    let (peer, session) = pair_as_guest(identity.clone(), addr, &code).await?;
    println!("paired host={}", peer.device_id);
    // Phone shell serve_once may arm a beat after pair_wait completes.
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    let mut link = match establish_sender(session, addr, identity.clone(), &peer, probe_bytes).await
    {
        Ok(l) => l,
        Err(e1) => {
            eprintln!("establish_sender retry after {e1}");
            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
            let session2 = lb_core::connect_trusted(identity.clone(), addr, &peer).await?;
            establish_sender(session2, addr, identity, &peer, probe_bytes).await?
        }
    };
    let plan = *link.plan();
    println!(
        "probe_bps={} streams={} chunk={}",
        plan.probe_bps, plan.streams, plan.chunk_size
    );

    let started = Instant::now();
    let sent = send_file(
        &mut link,
        &file,
        &SendOptions {
            remote_path: remote.clone(),
            progress: None,
            live_name: None,
        },
    )
    .await?;
    let elapsed_us = started.elapsed().as_micros().max(1) as u64;
    let transfer_bps = sent.bytes_transferred.saturating_mul(1_000_000) / elapsed_us;
    let probe_bps = plan.probe_bps.max(1);
    let ratio_millis = transfer_bps.saturating_mul(1000) / probe_bps;
    let probe_mbps = (probe_bps as f64) * 8.0 / 1_000_000.0;
    let transfer_mbps = (transfer_bps as f64) * 8.0 / 1_000_000.0;

    let report_body = format!(
        "# M4 LAN phone bench\n\n\
         - scope: PC guest → phone host over WLAN (TLS)\n\
         - peer_host: phone-lan:redacted\n\
         - peer_port: {port}\n\
         - peer_device_id: ohos-redacted\n\
         - probe_bytes: {probe_bytes}\n\
         - probe_elapsed_us: {}\n\
         - probe_bps: {probe_bps}\n\
         - probe_mbps: {probe_mbps:.2}\n\
         - streams: {}\n\
         - chunk_size: {}\n\
         - transfer_bytes: {}\n\
         - file_bytes: {file_len}\n\
         - transfer_elapsed_us: {elapsed_us}\n\
         - transfer_bps: {transfer_bps}\n\
         - transfer_mbps: {transfer_mbps:.2}\n\
         - transfer_per_mille_of_probe: {ratio_millis}\n\
         - pass_85pct: {}\n",
        plan.probe_elapsed_us,
        plan.streams,
        plan.chunk_size,
        sent.bytes_transferred,
        if ratio_millis >= 850 { "YES" } else { "NO" },
    );
    let _ = peer; // keep pairing side-effect; identity redacted in report
    if let Some(parent) = report.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&report, &report_body)?;
    print!("{report_body}");
    println!("wrote {}", report.display());
    println!("LANBRIDGE_BENCH_DONE ratio_millis={ratio_millis}");
    Ok(())
}
