//! Loopback file transfer via lb-core (CI / agent smoke; no UI).

use lb_core::{
    establish_receiver, establish_sender, generate_pairing_code, pair_as_guest, pair_as_host,
    recv_file, send_file, DeviceIdentity, RecvOptions, SendOptions,
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../.tmp/core-loopback");
    let src_dir = root.join("src");
    let dst_dir = root.join("dst");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&src_dir)?;
    std::fs::create_dir_all(&dst_dir)?;
    let src_file = src_dir.join("smoke.bin");
    let payload: Vec<u8> = (0..12_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&src_file, &payload)?;

    let host_id = Arc::new(DeviceIdentity::generate("core-host", "H")?);
    let guest_id = Arc::new(DeviceIdentity::generate("core-guest", "G")?);
    let code = generate_pairing_code();
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let code_h = code.clone();
    let dest = dst_dir.clone();

    let host = tokio::spawn(async move {
        let (peer, session, _) = pair_as_host(host_id.clone(), &listener, &code_h).await?;
        let mut link = establish_receiver(session, &listener, host_id, &peer).await?;
        recv_file(
            &mut link,
            &RecvOptions {
                dest_root: dest,
                overwrite: false,
                progress: None,
                live_name: None,
            },
        )
        .await
    });

    let (peer, session) = pair_as_guest(guest_id.clone(), addr, &code).await?;
    let mut link = establish_sender(session, addr, guest_id, &peer, 256 * 1024).await?;
    let sent = send_file(
        &mut link,
        &src_file,
        &SendOptions {
            remote_path: "smoke.bin".into(),
            progress: None,
            live_name: None,
        },
    )
    .await?;
    let (recv, path) = host.await??;
    assert_eq!(sent.bytes_transferred, recv.bytes_transferred);
    assert_eq!(recv.bytes_transferred, payload.len() as u64);
    println!(
        "loopback_file: PASS bytes={} path={}",
        recv.bytes_transferred,
        path.display()
    );
    Ok(())
}
