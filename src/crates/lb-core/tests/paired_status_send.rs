//! Regression: paired+online must report connected/可收发; send while peer serve-loops.
use lb_core::DeviceSession;
use std::time::Duration;

#[tokio::test]
async fn paired_online_status_and_send() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let root = std::env::temp_dir().join(format!("lb-core-status-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);

    let mut host = DeviceSession::open(root.join("host"), "lanbridge-win", "PC").unwrap();
    let mut guest = DeviceSession::open(root.join("guest"), "lanbridge-ohos", "Phone").unwrap();

    let host_port = host.go_online(37_721).await.expect("host online");
    let guest_port = guest.go_online(37_722).await.expect("guest online");

    let code = host.arm_pair_wait().unwrap();
    assert_eq!(host.status().connection, "pairing");

    guest
        .pair("127.0.0.1", host_port, &code)
        .await
        .expect("guest pair");
    for _ in 0..100 {
        if host.poll_pair_wait().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    host.set_peer("127.0.0.1", guest_port).unwrap();

    let sh = host.status();
    let sg = guest.status();
    assert_eq!(sh.connection, "connected");
    assert_eq!(sg.connection, "connected");
    assert!(sh.banner.contains("已连接"), "{}", sh.banner);
    assert!(!sh.banner.contains("正在寻找"), "{}", sh.banner);
    assert!(!sh.banner.contains("对端离线"), "{}", sh.banner);

    let dest = root.join("dst");
    std::fs::create_dir_all(&dest).unwrap();
    let src = root.join("src.bin");
    std::fs::write(&src, b"hello-verify").unwrap();

    let serve = tokio::spawn(async move {
        for _ in 0..30 {
            match guest.serve_once(dest.clone()).await {
                Ok(ev) => return Ok(ev),
                Err(e) if e.to_string().contains("timeout") => continue,
                Err(e) => return Err(e),
            }
        }
        Err(lb_core::Error::Session(lb_session::Error::Protocol(
            "serve gave up",
        )))
    });

    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(host.status().connection, "connected");
    let n = host.send_file(&src, "src.bin").await.expect("send");
    assert_eq!(n, 12);
    let _ev = serve.await.unwrap().expect("recv");
    let _ = std::fs::remove_dir_all(&root);
}
