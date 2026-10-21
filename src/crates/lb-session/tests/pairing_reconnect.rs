//! The host pairing window must recognise an already-pinned peer.
//!
//! Regression: while waiting for a 6-digit code the host dropped every normal session, so a
//! trusted phone could not reconnect ("PC 杩炴帴涓?/ 鎵嬫満宸茶繛鎺?).

use lb_crypto::{generate_pairing_code, DeviceIdentity};
use lb_session::{await_host_pair, connect_trusted, HostPairOutcome, TrustedPeer};
use std::sync::Arc;
use tokio::net::TcpListener;

#[tokio::test]
async fn pinned_peer_reconnects_during_pairing_window() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let host_id = Arc::new(DeviceIdentity::generate("lanbridge-win", "PC").unwrap());
    let guest_id = Arc::new(DeviceIdentity::generate("lanbridge-ohos", "Phone").unwrap());
    let host_peer = TrustedPeer {
        device_id: host_id.device_id.clone(),
        fingerprint: host_id.fingerprint,
    };
    let guest_peer = TrustedPeer {
        device_id: guest_id.device_id.clone(),
        fingerprint: guest_id.fingerprint,
    };

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let code = generate_pairing_code();

    let host = {
        let host_id = host_id.clone();
        let code = code.clone();
        let trusted = vec![guest_peer.clone()];
        tokio::spawn(async move { await_host_pair(host_id, &listener, &code, &trusted).await })
    };

    tokio::time::sleep(std::time::Duration::from_millis(80)).await;

    let guest = {
        let guest_id = guest_id.clone();
        let host_peer = host_peer.clone();
        tokio::spawn(async move { connect_trusted(guest_id, addr, &host_peer).await })
    };

    let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), host)
        .await
        .expect("host did not return")
        .expect("host task panicked")
        .expect("host returned error");
    match outcome {
        HostPairOutcome::Reconnect(peer, _addr) => assert_eq!(peer, guest_peer),
        HostPairOutcome::Paired(_, _, _) => panic!("must recognise the pinned peer, not re-pair"),
    }

    // The guest handshake must also complete (HelloAck), not hang until its timeout.
    let guest_result = tokio::time::timeout(std::time::Duration::from_secs(5), guest)
        .await
        .expect("guest hung")
        .expect("guest task panicked");
    assert!(
        guest_result.is_ok(),
        "guest handshake failed: {:?}",
        guest_result.err()
    );
}
