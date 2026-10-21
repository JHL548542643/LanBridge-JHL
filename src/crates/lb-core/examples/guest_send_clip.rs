//! Guest: connect and push clipboard text.
//!
//! Usage:
//!   $env:LB_HOST="192.168.x.x"; $env:LB_PORT="12345"; $env:LB_CODE="123456"
//!   cargo run -p lb-core --example guest_send_clip --release -- "hello"

use lb_core::{pair_as_guest, push_clipboard, ClipboardItem, DeviceIdentity, PairingCode};
use std::env;
use std::net::SocketAddr;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let host = env::var("LB_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: u16 = env::var("LB_PORT")?.parse()?;
    let code = PairingCode::parse(&env::var("LB_CODE")?)?;
    let text = env::args()
        .nth(1)
        .unwrap_or_else(|| "lanbridge-clip-e2e".into());
    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    let identity = Arc::new(DeviceIdentity::generate(
        format!(
            "clip-guest-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis()
        ),
        "clip-guest",
    )?);
    let (_peer, mut session) = pair_as_guest(identity, addr, &code).await?;
    push_clipboard(&mut session, &ClipboardItem::Text(text.clone())).await?;
    println!("CLIP_SENT bytes={}", text.len());
    Ok(())
}
