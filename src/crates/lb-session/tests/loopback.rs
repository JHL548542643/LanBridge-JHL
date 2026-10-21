use lb_crypto::{generate_pairing_code, DeviceIdentity};
use lb_session::{answer_ping, pair_as_guest, pair_as_host, ping_pong};
use std::sync::Arc;
use tokio::net::TcpListener;

#[tokio::test]
async fn loopback_pair_and_ping() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let host_id = Arc::new(DeviceIdentity::generate("host", "H").unwrap());
    let guest_id = Arc::new(DeviceIdentity::generate("guest", "G").unwrap());
    let code = generate_pairing_code();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let code_h = code.clone();
    let host = tokio::spawn(async move {
        let (_peer, mut session, _) = pair_as_host(host_id, &listener, &code_h).await.unwrap();
        answer_ping(&mut session).await.unwrap();
    });

    let (peer, mut session) = pair_as_guest(guest_id, addr, &code).await.unwrap();
    assert_eq!(peer.device_id, "host");
    let n = ping_pong(&mut session, 42).await.unwrap();
    assert_eq!(n, 42);
    host.await.unwrap();
}
