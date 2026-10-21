//! Local verify: paired+online status must be `connected` / 可收发; send must work.
use lb_core::DeviceSession;
use std::time::Duration;

#[tokio::main]
async fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let root = std::env::temp_dir().join(format!("lb-status-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);

    let mut host = DeviceSession::open(root.join("host"), "lanbridge-win", "PC").unwrap();
    let mut guest = DeviceSession::open(root.join("guest"), "lanbridge-ohos", "Phone").unwrap();

    // Distinct ports: both cannot bind 37532 on one machine.
    let host_port = host.go_online(37_711).await.expect("host online");
    let guest_port = guest.go_online(37_712).await.expect("guest online");

    let code = host.arm_pair_wait().expect("arm pair");
    let st = host.status();
    assert_eq!(st.connection, "pairing", "got {}", st.connection);

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
    // Host saved guest TCP IP with default control port; point at real guest listen port.
    host.set_peer("127.0.0.1", guest_port).expect("set peer");

    let sh = host.status();
    let sg = guest.status();
    println!(
        "HOST banner={} connection={} paired={} nearby={}",
        sh.banner,
        sh.connection,
        sh.paired,
        sh.nearby.len()
    );
    println!(
        "GUEST banner={} connection={} paired={} nearby={}",
        sg.banner,
        sg.connection,
        sg.paired,
        sg.nearby.len()
    );

    assert_eq!(sh.connection, "connected");
    assert_eq!(sg.connection, "connected");
    assert!(
        sh.banner.contains("已连接"),
        "bad host banner: {}",
        sh.banner
    );
    assert!(
        sg.banner.contains("已连接"),
        "bad guest banner: {}",
        sg.banner
    );
    assert!(
        !sh.banner.contains("正在寻找"),
        "host still searching: {}",
        sh.banner
    );
    assert!(
        !sh.banner.contains("对端离线"),
        "host offline wrongly: {}",
        sh.banner
    );

    let dest = root.join("dst");
    std::fs::create_dir_all(&dest).unwrap();
    let src = root.join("src.bin");
    std::fs::write(&src, b"hello-verify").unwrap();

    let serve = tokio::spawn(async move {
        for _ in 0..30 {
            match guest.serve_once(dest.clone()).await {
                Ok(ev) => return Ok(ev),
                Err(e) => {
                    let s = e.to_string();
                    if s.contains("timeout") || s.contains("timed out") {
                        continue;
                    }
                    return Err(e);
                }
            }
        }
        Err(lb_core::Error::Session(lb_session::Error::Protocol(
            "serve gave up",
        )))
    });

    tokio::time::sleep(Duration::from_millis(150)).await;
    // Status must remain usable (no take-session hole).
    assert_eq!(host.status().connection, "connected");
    let n = host
        .send_file(&src, "src.bin")
        .await
        .expect("send_file while peer serving");
    println!("sent_bytes={n}");
    let ev = serve.await.unwrap().expect("recv event");
    println!("recv={ev:?}");
    assert_eq!(n, 12);

    let _ = std::fs::remove_dir_all(&root);
    println!("VERIFY_OK");
}
