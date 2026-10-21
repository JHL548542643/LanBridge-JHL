//! Loopback probe vs transfer. Writes `data/results/m2-loopback-bench.md`.
//! This is not a phone-LAN acceptance run; it checks transfer stays within
//! 85% of the same-process probe on 127.0.0.1.

use lb_crypto::{generate_pairing_code, DeviceIdentity};
use lb_session::pair_as_guest;
use lb_transfer::{
    establish_receiver, establish_sender, recv_file, send_file, RecvOptions, SendOptions,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;

/// `YYYY-MM-DDTHH:MM:SSZ` without pulling a date crate (Hinnant's civil_from_days).
fn utc_stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_string())
}

#[tokio::test]
async fn loopback_probe_vs_transfer() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let probe_bytes: u64 = 16 * 1024 * 1024;
    let file_bytes: usize = 128 * 1024 * 1024;

    let root = std::env::temp_dir().join(format!("lb-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let src = root.join("blob.bin");
    let payload = vec![0x5Au8; file_bytes];
    std::fs::write(&src, &payload).unwrap();
    let dst = root.join("out");

    let host_id = Arc::new(DeviceIdentity::generate("host", "H").unwrap());
    let guest_id = Arc::new(DeviceIdentity::generate("guest", "G").unwrap());
    let code = generate_pairing_code();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let code_h = code.clone();
    let dst_root = dst.clone();
    let host = tokio::spawn(async move {
        let (guest_peer, session, _) =
            lb_session::pair_as_host(host_id.clone(), &listener, &code_h)
                .await
                .unwrap();
        let mut link = establish_receiver(session, &listener, host_id, &guest_peer)
            .await
            .unwrap();
        let plan = *link.plan();
        let (summary, dest) = recv_file(
            &mut link,
            &RecvOptions {
                dest_root: dst_root,
                overwrite: false,
                progress: None,
                live_name: None,
            },
        )
        .await
        .unwrap();
        (plan, summary, dest)
    });

    let (host_peer, session) = pair_as_guest(guest_id.clone(), addr, &code).await.unwrap();
    let mut link = establish_sender(session, addr, guest_id, &host_peer, probe_bytes)
        .await
        .unwrap();
    let plan = *link.plan();
    let started = Instant::now();
    let sent = send_file(
        &mut link,
        &src,
        &SendOptions {
            remote_path: "blob.bin".into(),
            progress: None,
            live_name: None,
        },
    )
    .await
    .unwrap();
    let _ = lb_transfer::shutdown_link(&mut link).await;
    let elapsed_us = started.elapsed().as_micros().max(1) as u64;
    let transfer_bps = sent.bytes_transferred.saturating_mul(1_000_000) / elapsed_us;
    let (recv_plan, summary, dest) = host.await.unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), payload);
    assert_eq!(summary.bytes_transferred, file_bytes as u64);

    let probe_bps = plan.probe_bps.max(1);
    let ratio_millis = transfer_bps.saturating_mul(1000) / probe_bps;
    // Metadata first: a bare ratio is meaningless without the machine and the build type, and a
    // stale report is worse than no report.
    let report = format!(
        "# M2 loopback bench\n\n\
         - scope: 127.0.0.1 TLS, **trend only** — LAN dual-device ≥85% is the acceptance bar\n\
         - measured_at_utc: {}\n\
         - host: {}\n\
         - cpu: {}\n\
         - build: {}\n\
         - ratio_semantics: transfer_bps / probe_bps. The probe is CPU-bound (16 MiB in RAM), \
           the transfer is disk-bound (128 MiB to temp) — a faster CPU alone lowers the ratio\n\
         - probe_bytes: {probe_bytes}\n\
         - probe_elapsed_us: {}\n\
         - probe_bps: {probe_bps}\n\
         - streams: {}\n\
         - chunk_size: {}\n\
         - transfer_bytes: {}\n\
         - transfer_elapsed_us: {elapsed_us}\n\
         - transfer_bps: {transfer_bps}\n\
         - transfer_per_mille_of_probe: {ratio_millis}\n\
         - receiver_streams: {}\n",
        utc_stamp(),
        env_or("COMPUTERNAME", "unknown-host"),
        env_or("PROCESSOR_IDENTIFIER", "unknown-cpu"),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        plan.probe_elapsed_us,
        plan.streams,
        plan.chunk_size,
        sent.bytes_transferred,
        recv_plan.streams,
    );
    // The report is a committed artifact, so only overwrite it on an explicit measurement run —
    // otherwise any `cargo test` dirties the tracked file (and a busy machine would publish a
    // misleading number).
    print!("{report}");
    if std::env::var("LB_BENCH_REPORT").as_deref() == Ok("1") {
        let out = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../data/results/m2-loopback-bench.md");
        std::fs::write(&out, &report).expect("write bench report");
        println!("bench report written to {}", out.display());
    }
    // Debug builds are noisy on a busy desktop; release is the local gate.
    // Phone LAN 85% remains a dual-device acceptance item.
    if cfg!(debug_assertions) {
        assert!(
            ratio_millis >= 500,
            "debug smoke: transfer {transfer_bps} B/s vs probe {probe_bps} ({ratio_millis}/1000)"
        );
    } else {
        assert!(
            ratio_millis >= 850,
            "transfer {transfer_bps} B/s is below 85% of probe {probe_bps} B/s ({ratio_millis}/1000)"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}
