//! A Bye is only honoured inside an established (fingerprint-pinned) session.

use lb_crypto::{generate_pairing_code, DeviceIdentity};
use lb_session::{
    accept_tls, accept_trusted, connect_tls, connect_trusted, pair_as_guest, pair_as_host,
    send_bye, TlsMode,
};
use std::sync::Arc;
use tokio::net::TcpListener;

/// §6 test 3 (security): an unpaired peer cannot clear trust by sending Bye.
#[tokio::test]
async fn bye_from_unknown_peer_ignored() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let host_id = Arc::new(DeviceIdentity::generate("host", "H").unwrap());
    let guest_id = Arc::new(DeviceIdentity::generate("guest", "G").unwrap());
    let attacker_id = Arc::new(DeviceIdentity::generate("attacker", "X").unwrap());

    // Real pairing first, so the host holds a pinned peer worth protecting.
    let listener = Arc::new(TcpListener::bind("127.0.0.1:0").await.unwrap());
    let addr = listener.local_addr().unwrap();
    let code = generate_pairing_code();
    let host_side = host_id.clone();
    let host_code = code.clone();
    let listen = listener.clone();
    let host_task = tokio::spawn(async move { pair_as_host(host_side, &listen, &host_code).await });

    let (host_peer, guest_session) = pair_as_guest(guest_id.clone(), addr, &code).await.unwrap();
    let (guest_peer, _session, _addr) = host_task.await.unwrap().unwrap();
    drop(guest_session);
    assert_eq!(host_peer.device_id, "host");
    assert_eq!(guest_peer.device_id, "guest");

    // The attacker completes TLS with its own certificate, then says Bye.
    let attacker = tokio::spawn(async move {
        if let Ok(mut tls) = connect_tls(
            tokio::net::TcpStream::connect(addr).await.unwrap(),
            attacker_id,
            TlsMode::Pairing,
            None,
        )
        .await
        {
            let _ = send_bye(&mut tls).await;
            let _ = tls.shutdown().await;
        }
    });

    // A trusted accept pins the *paired* fingerprint, so the stranger's handshake is refused
    // before any frame (Bye included) reaches the session layer.
    let rejected = accept_tls(
        listener.accept().await.unwrap().0,
        host_id.clone(),
        TlsMode::Trusted {
            expect_fingerprint: guest_peer.fingerprint,
        },
    )
    .await;
    assert!(rejected.is_err(), "stranger must not be accepted");
    let _ = attacker.await;

    // Trust is intact: the genuine peer still gets a pinned session afterwards.
    let accept_side = host_id.clone();
    let pinned = guest_peer.clone();
    let listen = listener.clone();
    let accept = tokio::spawn(async move { accept_trusted(accept_side, &listen, &pinned).await });
    let mut session = connect_trusted(guest_id, addr, &host_peer)
        .await
        .expect("paired peer must still connect");
    let accepted = accept.await.unwrap();
    assert!(
        accepted.is_ok(),
        "paired peer must still be trusted after the attacker's Bye"
    );
    let _ = session.shutdown().await;
}
