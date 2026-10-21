//! Loopback smoke: pair over 127.0.0.1 then Ping/Pong.

use lb_core::{
    answer_ping, generate_pairing_code, pair_as_guest, pair_as_host, ping_pong, DeviceIdentity,
};
use std::sync::Arc;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let host_id = Arc::new(DeviceIdentity::generate("host-pc", "PC")?);
    let guest_id = Arc::new(DeviceIdentity::generate("guest-phone", "Phone")?);
    let code = generate_pairing_code();

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    println!("listening on {addr}");
    println!(
        "pairing code ready (not printed; length={})",
        code.as_str().len()
    );

    let code_host = code.clone();
    let host = tokio::spawn(async move {
        let (peer, mut session, _) = pair_as_host(host_id, &listener, &code_host).await?;
        println!("host trusted guest {}", peer.device_id);
        answer_ping(&mut session).await?;
        Ok::<_, lb_session::Error>(())
    });

    let (peer, mut session) = pair_as_guest(guest_id, addr, &code).await?;
    println!("guest trusted host {}", peer.device_id);
    let echoed = ping_pong(&mut session, 0xA11CE).await?;
    assert_eq!(echoed, 0xA11CE);
    println!("ping/pong ok nonce={echoed:#x}");

    host.await??;
    println!("loopback_pair: PASS");
    Ok(())
}
