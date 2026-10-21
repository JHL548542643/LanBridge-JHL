//! Headless host: listen, print port+code, recv one file (dual-device helper).

use lb_core::{
    establish_receiver, generate_pairing_code, pair_as_host, recv_file, DeviceIdentity, RecvOptions,
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let dest = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../.tmp/host-recv")
        });
    std::fs::create_dir_all(&dest)?;

    let identity = Arc::new(DeviceIdentity::generate(
        format!(
            "win-host-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis()
        ),
        "PC-host",
    )?);
    let code = generate_pairing_code();
    let listener = TcpListener::bind("0.0.0.0:0").await?;
    let port = listener.local_addr()?.port();

    println!("LANBRIDGE_HOST_READY port={port} code={}", code.as_str());
    println!("dest={}", dest.display());
    println!("waiting for guest…");

    let (peer, session, _) = pair_as_host(identity.clone(), &listener, &code).await?;
    println!("paired guest={}", peer.device_id);
    let mut link = establish_receiver(session, &listener, identity, &peer).await?;
    let (summary, path) = recv_file(
        &mut link,
        &RecvOptions {
            dest_root: dest,
            overwrite: false,
            progress: None,
            live_name: None,
        },
    )
    .await?;
    println!(
        "LANBRIDGE_HOST_DONE bytes={} path={}",
        summary.bytes_transferred,
        path.display()
    );
    Ok(())
}
