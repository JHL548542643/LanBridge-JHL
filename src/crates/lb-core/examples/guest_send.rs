//! Headless guest: pair to host and send one file.

use lb_core::{
    establish_sender, pair_as_guest, send_file, DeviceIdentity, PairingCode, SendOptions,
    DEFAULT_PROBE_BYTES,
};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut args = std::env::args().skip(1);
    let host = args
        .next()
        .ok_or("usage: guest_send <host> <port> <code> <file> [remote]")?;
    let port: u16 = args.next().ok_or("port")?.parse()?;
    let code = PairingCode::parse(&args.next().ok_or("code")?)?;
    let file = PathBuf::from(args.next().ok_or("file")?);
    let remote = args.next().unwrap_or_else(|| {
        file.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("file.bin")
            .to_string()
    });
    if !file.is_file() {
        return Err(format!("not a file: {}", file.display()).into());
    }
    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    let identity = Arc::new(DeviceIdentity::generate(
        format!(
            "win-guest-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis()
        ),
        "PC-guest",
    )?);
    println!("connecting {addr} …");
    let (peer, session) = pair_as_guest(identity.clone(), addr, &code).await?;
    println!("paired host={}", peer.device_id);
    let mut link = establish_sender(session, addr, identity, &peer, DEFAULT_PROBE_BYTES).await?;
    let summary = send_file(
        &mut link,
        &file,
        &SendOptions {
            remote_path: remote.clone(),
            progress: None,
            live_name: None,
        },
    )
    .await?;
    println!(
        "LANBRIDGE_GUEST_DONE bytes={} remote={remote}",
        summary.bytes_transferred
    );
    Ok(())
}
