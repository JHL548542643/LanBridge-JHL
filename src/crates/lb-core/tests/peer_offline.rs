//! Paired trust stays; live connection must drop when peer evidence expires.
use lb_core::DeviceSession;
use std::time::Duration;

#[tokio::test]
async fn peer_offline_when_alive_expires() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let root = std::env::temp_dir().join(format!("lb-core-offline-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);

    let mut host = DeviceSession::open(root.join("host"), "lanbridge-win", "PC").unwrap();
    let mut guest = DeviceSession::open(root.join("guest"), "lanbridge-ohos", "Phone").unwrap();

    let host_port = host.go_online(37_731).await.expect("host online");
    let _guest_port = guest.go_online(37_732).await.expect("guest online");

    let code = host.arm_pair_wait().unwrap();
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

    assert_eq!(guest.status().connection, "connected");
    assert!(guest.status().paired);

    // Stop peer process (drop session) — no more beacons / transfers.
    drop(host);

    // Expire live evidence without waiting the full product window.
    guest.debug_expire_peer_live();

    let st = guest.status();
    assert!(st.paired, "trust must remain");
    assert_eq!(
        st.connection, "disconnected",
        "dead peer must not stay connected: banner={}",
        st.banner
    );
    assert!(
        st.banner.contains("对端离线") || st.banner.contains("心跳"),
        "banner={}",
        st.banner
    );

    let _ = std::fs::remove_dir_all(&root);
}
