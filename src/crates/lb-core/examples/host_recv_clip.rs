//! Host: wait for guest, receive clipboard text or PNG, write OS clipboard.
//!
//! Usage: cargo run -p lb-core --example host_recv_clip --release
//! Prints: LISTEN port=... code=... then CLIP_RECV / OS_CLIPBOARD_WRITTEN
//! (Does not print clipboard body — ADR-001.)

use lb_core::{generate_pairing_code, pair_as_host, recv_clipboard, ClipboardItem, DeviceIdentity};
use std::sync::Arc;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let identity = Arc::new(DeviceIdentity::generate(
        format!(
            "clip-host-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis()
        ),
        "clip-host",
    )?);
    let code = generate_pairing_code();
    let listener = TcpListener::bind("0.0.0.0:0").await?;
    let port = listener.local_addr()?.port();
    println!("LISTEN port={port} code={}", code.as_str());
    let (_peer, mut session, _) = pair_as_host(identity, &listener, &code).await?;
    let (_seq, item) = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        recv_clipboard(&mut session),
    )
    .await
    .map_err(|_| "recv clipboard timeout")??;
    match item {
        ClipboardItem::Text(t) => {
            println!("CLIP_RECV kind=text bytes={}", t.len());
            let mut cb = arboard::Clipboard::new()?;
            cb.set_text(t)?;
            println!("OS_CLIPBOARD_WRITTEN");
        }
        ClipboardItem::ImagePng(b) => {
            println!("CLIP_RECV kind=png bytes={}", b.len());
            let img = image::load_from_memory(&b)?;
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            let mut cb = arboard::Clipboard::new()?;
            cb.set_image(arboard::ImageData {
                width: w as usize,
                height: h as usize,
                bytes: std::borrow::Cow::Owned(rgba.into_raw()),
            })?;
            println!("OS_CLIPBOARD_WRITTEN png={w}x{h}");
        }
    }
    Ok(())
}
