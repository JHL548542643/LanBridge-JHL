//! Acceptance: Bye propagates an unpair, heartbeat times out, restart keeps trust.

use lb_core::{DeviceSession, DisconnectReason, InboundEvent};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn install_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn temp_root(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("lb-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

async fn pair_host_guest(
    root: &std::path::Path,
    host_port: u16,
    guest_port: u16,
) -> (DeviceSession, DeviceSession) {
    let mut host = DeviceSession::open(root.join("host"), "lanbridge-win", "PC").unwrap();
    let mut guest = DeviceSession::open(root.join("guest"), "lanbridge-ohos", "Phone").unwrap();
    host.go_online(host_port).await.expect("host online");
    guest.go_online(guest_port).await.expect("guest online");
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
    host.set_peer("127.0.0.1", guest_port).unwrap();
    (host, guest)
}

/// §6 test 1: A unparis → B learns it in one round trip (well inside the 3 s budget).
#[tokio::test]
async fn unpair_propagates_to_peer() {
    install_crypto();
    let root = temp_root("core-unpair");
    let (mut host, mut guest) = pair_host_guest(&root, 37_741, 37_742).await;
    assert_eq!(host.status().connection, "connected");

    let dest = root.join("dst");
    std::fs::create_dir_all(&dest).unwrap();
    // Guest accepts the Bye session while it is serving.
    let serve = tokio::spawn(async move {
        for _ in 0..60 {
            match guest.serve_once(dest.clone()).await {
                Ok(InboundEvent::PeerLeft { reason }) => return (reason, guest),
                Ok(_) => continue,
                Err(e) if e.to_string().contains("timeout") => continue,
                Err(e) => panic!("serve failed: {e}"),
            }
        }
        panic!("peer never learned about the unpair");
    });

    tokio::time::sleep(Duration::from_millis(200)).await;
    host.unpair().await.expect("unpair");

    let (reason, guest) = tokio::time::timeout(Duration::from_secs(3), serve)
        .await
        .expect("Bye must land within 3 s")
        .expect("serve task");
    assert_eq!(reason, "unpaired");

    let st = guest.status();
    assert!(!st.paired, "receiving Bye must drop trust too");
    assert_eq!(st.disconnect_reason, Some(DisconnectReason::Unpaired));
    assert!(st.banner.contains("对端已取消配对"), "banner={}", st.banner);
    assert!(!host.status().paired, "sender removes its own trust");

    let _ = std::fs::remove_dir_all(&root);
}

/// Regression: a restart (no Bye, peer not answering) must never wipe pinned trust.
#[tokio::test]
async fn trust_survives_restart() {
    install_crypto();
    let root = temp_root("core-restart");
    let (host, guest) = pair_host_guest(&root, 37_771, 37_772).await;
    drop(host);
    drop(guest);

    // Reopen on the same data dir; the peer is gone (nothing serves, nothing answers).
    let mut host = DeviceSession::open(root.join("host"), "lanbridge-win", "PC").unwrap();
    host.go_online(37_771).await.expect("host online");
    for _ in 0..30 {
        host.poll_heartbeat();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let st = host.status();
    assert!(
        st.paired,
        "restart must keep pinned trust: banner={}",
        st.banner
    );
    let trust = std::fs::read_to_string(root.join("host").join("trust.json")).unwrap();
    assert!(
        trust.contains("lanbridge-ohos"),
        "trust file was wiped on restart: {trust}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// §6 test 2: no Bye, no traffic — the heartbeat window alone must flip the link to offline.
#[tokio::test]
async fn heartbeat_times_out_half_open() {
    install_crypto();
    let root = temp_root("core-hb");
    let (mut host, guest) = pair_host_guest(&root, 37_751, 37_752).await;

    // Real beats only flow when the peer serves; count them so this is not just the pair touch.
    let beats = Arc::new(AtomicUsize::new(0));
    let counter = beats.clone();
    let dest = root.join("dst");
    std::fs::create_dir_all(&dest).unwrap();
    let serve = tokio::spawn(async move {
        let mut guest = guest;
        loop {
            match guest.serve_once(dest.clone()).await {
                Ok(InboundEvent::Heartbeat { .. }) => {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                Ok(_) => {}
                Err(e) if e.to_string().contains("timeout") => {}
                Err(_) => {}
            }
        }
    });

    // 3 s cadence → one beat must land well inside 6 s.
    let mut beat_seen = false;
    for _ in 0..60 {
        host.poll_heartbeat();
        if beats.load(Ordering::Relaxed) > 0 {
            beat_seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(beat_seen, "idle pair must exchange real Ping/Pong beats");
    assert_eq!(host.status().connection, "connected");

    // Peer process dies: listener gone, no Bye, no beacons.
    serve.abort();

    // Simulate the live window elapsing instead of sleeping 12 s.
    host.poll_heartbeat();
    host.debug_age_peer_evidence(60_000);
    let st = host.status();
    assert!(st.paired, "a timeout keeps trust for the auto-retry path");
    assert_eq!(st.connection, "disconnected");
    assert_eq!(st.disconnect_reason, Some(DisconnectReason::Timeout));
    assert!(st.banner.contains("对端离线"), "banner={}", st.banner);

    let _ = std::fs::remove_dir_all(&root);
}

/// Real-clock variant: measures the actual detection latency against the ≤15 s acceptance bound.
/// Ignored by default because it waits the whole liveness window.
#[tokio::test]
#[ignore = "waits the real liveness window"]
async fn heartbeat_timeout_real_clock() {
    install_crypto();
    let root = temp_root("core-hb-real");
    let (mut host, guest) = pair_host_guest(&root, 37_761, 37_762).await;

    let beats = Arc::new(AtomicUsize::new(0));
    let counter = beats.clone();
    let dest = root.join("dst");
    std::fs::create_dir_all(&dest).unwrap();
    let serve = tokio::spawn(async move {
        let mut guest = guest;
        loop {
            match guest.serve_once(dest.clone()).await {
                Ok(InboundEvent::Heartbeat { .. }) => {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }
    });

    // Let several real beats land first.
    for _ in 0..80 {
        host.poll_heartbeat();
        if beats.load(Ordering::Relaxed) >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        beats.load(Ordering::Relaxed) >= 2,
        "heartbeat must keep beating on an idle pair"
    );
    host.poll_heartbeat();
    assert_eq!(host.status().connection, "connected");

    // Peer process dies: drop the serving device (closes its listener), no Bye, no beacons.
    serve.abort();

    let started = std::time::Instant::now();
    while host.status().connection != "disconnected" {
        host.poll_heartbeat();
        if started.elapsed() > Duration::from_secs(20) {
            panic!("peer still looks connected after 20 s");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed <= Duration::from_secs(15),
        "offline detected after {elapsed:?} (> 15 s acceptance bound)"
    );
    assert_eq!(
        host.status().disconnect_reason,
        Some(DisconnectReason::Timeout)
    );
    println!("half-open detected after {elapsed:?}");

    let _ = std::fs::remove_dir_all(&root);
}
