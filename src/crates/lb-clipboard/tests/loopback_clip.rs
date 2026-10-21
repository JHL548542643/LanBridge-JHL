use lb_clipboard::{push_clipboard, recv_clipboard, ClipboardItem};
use lb_crypto::{generate_pairing_code, DeviceIdentity};
use lb_session::{pair_as_guest, pair_as_host};
use std::sync::Arc;
use std::time::Instant;
use tokio::net::TcpListener;

fn tiny_png() -> Vec<u8> {
    // 1x1 PNG
    vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x05, 0xFE, 0x02, 0xFE, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ]
}

#[tokio::test]
async fn loopback_text_and_image() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let host_id = Arc::new(DeviceIdentity::generate("host", "H").unwrap());
    let guest_id = Arc::new(DeviceIdentity::generate("guest", "G").unwrap());
    let code = generate_pairing_code();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let code_h = code.clone();

    let host = tokio::spawn(async move {
        let (_peer, mut session, _) = pair_as_host(host_id, &listener, &code_h).await.unwrap();
        let (seq, item) = recv_clipboard(&mut session).await.unwrap();
        assert!(matches!(item, ClipboardItem::Text(ref t) if t == "hello-lanbridge"));
        let png = tiny_png();
        push_clipboard(&mut session, &ClipboardItem::ImagePng(png.clone().into()))
            .await
            .unwrap();
        (seq, png)
    });

    let (_peer, mut session) = pair_as_guest(guest_id, addr, &code).await.unwrap();
    let started = Instant::now();
    push_clipboard(&mut session, &ClipboardItem::Text("hello-lanbridge".into()))
        .await
        .unwrap();
    let text_ms = started.elapsed().as_millis();
    assert!(
        text_ms < 150,
        "text clipboard latency {text_ms}ms exceeds 150ms (loopback)"
    );

    let (_seq, item) = recv_clipboard(&mut session).await.unwrap();
    let (_host_seq, png) = host.await.unwrap();
    match item {
        ClipboardItem::ImagePng(b) => assert_eq!(b.as_ref(), png.as_slice()),
        _ => panic!("expected png"),
    }
}
