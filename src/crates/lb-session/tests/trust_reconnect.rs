//! Pair, persist trust + identity, reconnect with pinned fingerprints.

use lb_crypto::{generate_pairing_code, DeviceIdentity};
use lb_session::{
    accept_trusted, answer_ping, connect_trusted, pair_as_guest, pair_as_host, ping_pong,
    TrustStore,
};
use std::sync::Arc;
use tokio::net::TcpListener;

#[tokio::test]
async fn persist_then_reconnect() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let root = std::env::temp_dir().join(format!("lb-trust-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let host_dir = root.join("host-id");
    let guest_dir = root.join("guest-id");
    let host_trust_path = root.join("host-peers.json");
    let guest_trust_path = root.join("guest-peers.json");

    let host_id = DeviceIdentity::generate("host", "H").unwrap();
    let guest_id = DeviceIdentity::generate("guest", "G").unwrap();
    host_id.save_to_dir(&host_dir).unwrap();
    guest_id.save_to_dir(&guest_dir).unwrap();

    let code = generate_pairing_code();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let host_arc = Arc::new(host_id);
    let code_h = code.clone();
    let host_task = tokio::spawn(async move {
        let (peer, mut session, _) = pair_as_host(host_arc, &listener, &code_h).await.unwrap();
        answer_ping(&mut session).await.unwrap();
        drop(session);
        peer
    });

    let guest_arc = Arc::new(guest_id);
    let (host_peer, mut guest_session) = pair_as_guest(guest_arc, addr, &code).await.unwrap();
    let echoed = ping_pong(&mut guest_session, 7).await.unwrap();
    assert_eq!(echoed, 7);
    drop(guest_session);
    let guest_peer = host_task.await.unwrap();

    let mut host_store = TrustStore::new();
    host_store.insert(guest_peer.clone());
    host_store.save(&host_trust_path).unwrap();
    let mut guest_store = TrustStore::new();
    guest_store.insert(host_peer.clone());
    guest_store.save(&guest_trust_path).unwrap();

    let host_id = Arc::new(DeviceIdentity::load_from_dir(&host_dir).unwrap());
    let guest_id = Arc::new(DeviceIdentity::load_from_dir(&guest_dir).unwrap());
    let host_store = TrustStore::load(&host_trust_path).unwrap();
    let guest_store = TrustStore::load(&guest_trust_path).unwrap();
    let guest_peer = host_store.get("guest").unwrap().clone();
    let host_peer = guest_store.get("host").unwrap().clone();
    assert_eq!(guest_peer.fingerprint, guest_id.fingerprint);
    assert_eq!(host_peer.fingerprint, host_id.fingerprint);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let expect = guest_peer.clone();
    let host_task = tokio::spawn(async move {
        let mut session = accept_trusted(host_id, &listener, &expect).await.unwrap();
        answer_ping(&mut session).await.unwrap();
    });

    let mut session = connect_trusted(guest_id, addr, &host_peer).await.unwrap();
    let n = ping_pong(&mut session, 99).await.unwrap();
    assert_eq!(n, 99);
    host_task.await.unwrap();

    let _ = std::fs::remove_dir_all(&root);
}
