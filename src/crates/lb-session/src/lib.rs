//! Pairing state machine and TLS 1.3 mTLS control sessions.

#![forbid(unsafe_code)]

mod error;
mod store;
mod stream;
mod tls;
mod trust;

pub use error::{Error, Result};
pub use store::TrustStore;
pub use stream::ControlSession;
pub use tls::{accept_tls, connect_tls, TlsMode};
pub use trust::TrustedPeer;

use lb_crypto::{pairing_tags_equal, pairing_verify_tag, DeviceIdentity, PairingCode, PairingRole};
use lb_protocol::{
    Features, Frame, Hello, MsgType, PairAccept, PairConfirm, PairReject, PairRejectReason,
    PairVerify, Ping, Pong, PROTOCOL_VERSION,
};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

/// Max wait for the peer to complete one pairing control step.
const PAIR_STEP: Duration = Duration::from_secs(30);
/// Max wait for an inbound TCP during an open pairing window.
const PAIR_ACCEPT: Duration = Duration::from_secs(120);
/// Base delay after a failed pairing attempt (grows linearly, then capped).
///
/// The 6-digit code is only 20 bits: without throttling a LAN attacker can spray attempts at
/// handshake speed (~hundreds/s) and cover a meaningful part of the code space inside one
/// window. Charging every failure this delay keeps a 120 s window's attempt budget in the low
/// tens instead of tens of thousands.
const PAIR_FAIL_BACKOFF_BASE: Duration = Duration::from_millis(250);
/// Upper bound on the throttle delay; the pairing window itself is 120 s.
const PAIR_FAIL_BACKOFF_MAX: Duration = Duration::from_secs(5);

/// Outcome of a host-side pairing window.
///
/// A host that is waiting for a 6-digit pairing code must still recognise a peer it already
/// trusts: otherwise a reconnecting phone is rejected for as long as the window is open, which
/// shows up as "PC 连接中 / 手机已连接" on the two shells (and a reconnect storm on the wire).
// The paired variant carries the whole ControlSession; pairing happens once, so the size
// difference is irrelevant next to the clarity of a single outcome enum.
#[allow(clippy::large_enum_variant)]
pub enum HostPairOutcome {
    /// A new peer completed the 6-digit pairing handshake.
    Paired(TrustedPeer, ControlSession, SocketAddr),
    /// A pinned peer connected with a normal session while we were waiting: trust is already
    /// established by the certificate fingerprint, so no code is needed.
    Reconnect(TrustedPeer, SocketAddr),
}

/// Host side: listen within [`PAIR_ACCEPT`], retrying after bad/spoofed peers.
///
/// `trusted` is the current trust store: a pinned peer that reconnects is reported as
/// [`HostPairOutcome::Reconnect`] instead of being dropped.
pub async fn await_host_pair(
    identity: Arc<DeviceIdentity>,
    listener: &TcpListener,
    code: &PairingCode,
    trusted: &[TrustedPeer],
) -> Result<HostPairOutcome> {
    let deadline = tokio::time::Instant::now() + PAIR_ACCEPT;
    let mut failures: u32 = 0;
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(Error::Protocol("pairing accept timeout"));
        }
        let remaining = deadline - now;
        let (tcp, peer_addr) = match timeout(remaining, listener.accept()).await {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return Err(Error::Io(e)),
            Err(_) => return Err(Error::Protocol("pairing accept timeout")),
        };
        let mut tls = match accept_tls(tcp, identity.clone(), TlsMode::Pairing).await {
            Ok(s) => s,
            // TLS handshake failures: keep window open for the real guest, but slow the peer down.
            Err(_) => {
                throttle_after_failure(&mut failures).await;
                continue;
            }
        };
        let first = match recv_frame_timed(&mut tls).await {
            Ok(f) => f,
            // Client hung up mid-handshake (probe/timeout): keep waiting.
            Err(_) => {
                throttle_after_failure(&mut failures).await;
                continue;
            }
        };

        if first.msg_type == MsgType::Hello.as_u16() {
            let hello = Hello::decode(&first.payload)?;
            let known = trusted
                .iter()
                .find(|p| p.device_id == hello.device_id && p.fingerprint == hello.fingerprint)
                .cloned();
            if let Some(peer) = known {
                // Answer the hello so the caller's handshake completes instead of timing out.
                let _ = send_hello_ack(&mut tls, identity.as_ref()).await;
                return Ok(HostPairOutcome::Reconnect(peer, peer_addr));
            }
            throttle_after_failure(&mut failures).await;
            continue;
        }

        match complete_host_pairing(&mut tls, identity.as_ref(), code, first).await {
            Ok(peer) => return Ok(HostPairOutcome::Paired(peer, tls, peer_addr)),
            Err(Error::BadPairingCode)
            | Err(Error::FingerprintMismatch)
            | Err(Error::Protocol(_))
            | Err(Error::PeerLeft)
            | Err(Error::PeerClosed)
            | Err(Error::Rejected(_))
            | Err(Error::Wire(_)) => {
                throttle_after_failure(&mut failures).await;
                continue;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Linear backoff (capped) after a failed pairing attempt.
async fn throttle_after_failure(failures: &mut u32) {
    *failures = failures.saturating_add(1);
    let capped = (*failures).min(20);
    let delay = PAIR_FAIL_BACKOFF_BASE
        .saturating_mul(capped)
        .min(PAIR_FAIL_BACKOFF_MAX);
    tracing::debug!(attempt = *failures, "pairing attempt rejected; backing off");
    tokio::time::sleep(delay).await;
}

/// Host side: listen within [`PAIR_ACCEPT`], retrying after bad/spoofed peers.
pub async fn pair_as_host(
    identity: Arc<DeviceIdentity>,
    listener: &TcpListener,
    code: &PairingCode,
) -> Result<(TrustedPeer, ControlSession, SocketAddr)> {
    match await_host_pair(identity, listener, code, &[]).await? {
        HostPairOutcome::Paired(peer, session, addr) => Ok((peer, session, addr)),
        HostPairOutcome::Reconnect(_, _) => Err(Error::Protocol("unexpected trusted reconnect")),
    }
}

/// Guest side: connect, bind with locally entered code (never sent on wire).
pub async fn pair_as_guest(
    identity: Arc<DeviceIdentity>,
    addr: SocketAddr,
    code: &PairingCode,
) -> Result<(TrustedPeer, ControlSession)> {
    let tcp = TcpStream::connect(addr).await?;
    let mut tls = connect_tls(tcp, identity.clone(), TlsMode::Pairing, None).await?;
    let peer = complete_guest_pairing(&mut tls, identity.as_ref(), code).await?;
    Ok((peer, tls))
}

/// Reconnect to an already-trusted peer (fingerprint pinned).
pub async fn connect_trusted(
    identity: Arc<DeviceIdentity>,
    addr: SocketAddr,
    peer: &TrustedPeer,
) -> Result<ControlSession> {
    let tcp = TcpStream::connect(addr).await?;
    let mut tls = connect_tls(
        tcp,
        identity.clone(),
        TlsMode::Trusted {
            expect_fingerprint: peer.fingerprint,
        },
        Some(peer),
    )
    .await?;
    send_hello(&mut tls, identity.as_ref()).await?;
    let remote = recv_hello_ack(&mut tls).await?;
    enforce_hello_features(&remote)?;
    Ok(tls)
}

/// Accept a reconnect from an already-trusted peer (fingerprint pinned).
pub async fn accept_trusted(
    identity: Arc<DeviceIdentity>,
    listener: &TcpListener,
    peer: &TrustedPeer,
) -> Result<ControlSession> {
    let (tcp, _peer_addr) = listener.accept().await?;
    let mut tls = accept_tls(
        tcp,
        identity.clone(),
        TlsMode::Trusted {
            expect_fingerprint: peer.fingerprint,
        },
    )
    .await?;
    let hello = recv_hello(&mut tls).await?;
    if hello.device_id != peer.device_id || hello.fingerprint != peer.fingerprint {
        return Err(Error::FingerprintMismatch);
    }
    enforce_hello_features(&hello)?;
    send_hello_ack(&mut tls, identity.as_ref()).await?;
    Ok(tls)
}

async fn reject(session: &mut ControlSession, reason: PairRejectReason) -> Result<()> {
    session
        .send_msg(MsgType::PairReject, PairReject { reason }.encode())
        .await
}

async fn complete_host_pairing(
    session: &mut ControlSession,
    identity: &DeviceIdentity,
    code: &PairingCode,
    frame: Frame,
) -> Result<TrustedPeer> {
    let peer_tls_fp = session.peer_fingerprint()?;

    if frame.msg_type != MsgType::PairAccept.as_u16() {
        reject(session, PairRejectReason::Internal).await?;
        return Err(Error::Protocol("expected PairAccept"));
    }
    let accept = PairAccept::decode(&frame.payload)?;
    if accept.fingerprint != peer_tls_fp {
        reject(session, PairRejectReason::FingerprintMismatch).await?;
        return Err(Error::FingerprintMismatch);
    }

    let confirm = PairConfirm {
        device_id: identity.device_id.clone(),
        fingerprint: identity.fingerprint,
    };
    session
        .send_msg(MsgType::PairConfirm, confirm.encode()?)
        .await?;

    // Guest proves first — Host never emits the Guest-domain tag.
    let vframe = recv_frame_timed(session).await?;
    if vframe.msg_type != MsgType::PairVerify.as_u16() {
        return Err(Error::Protocol("expected PairVerify"));
    }
    let verify = PairVerify::decode(&vframe.payload)?;
    let expected_guest = pairing_verify_tag(
        code,
        &identity.fingerprint,
        &peer_tls_fp,
        PairingRole::Guest,
    );
    if !pairing_tags_equal(&verify.tag, &expected_guest) {
        reject(session, PairRejectReason::BadCode).await?;
        return Err(Error::BadPairingCode);
    }

    let host_tag = pairing_verify_tag(code, &identity.fingerprint, &peer_tls_fp, PairingRole::Host);
    session
        .send_msg(MsgType::PairVerify, PairVerify { tag: host_tag }.encode())
        .await?;

    let peer = TrustedPeer {
        device_id: accept.device_id,
        fingerprint: peer_tls_fp,
    };

    send_hello(session, identity).await?;
    let remote = recv_hello_ack_timed(session).await?;
    if remote.fingerprint != peer.fingerprint {
        return Err(Error::FingerprintMismatch);
    }
    enforce_hello_features(&remote)?;
    Ok(peer)
}

async fn complete_guest_pairing(
    session: &mut ControlSession,
    identity: &DeviceIdentity,
    code: &PairingCode,
) -> Result<TrustedPeer> {
    let peer_tls_fp = session.peer_fingerprint()?;

    let accept = PairAccept {
        device_id: identity.device_id.clone(),
        fingerprint: identity.fingerprint,
    };
    session
        .send_msg(MsgType::PairAccept, accept.encode()?)
        .await?;

    let frame = recv_frame_timed(session).await?;
    match MsgType::from_u16(frame.msg_type) {
        Some(MsgType::PairConfirm) => {}
        Some(MsgType::PairReject) => {
            let rej = PairReject::decode(&frame.payload)?;
            return Err(Error::Rejected(rej.reason));
        }
        _ => return Err(Error::Protocol("expected PairConfirm")),
    }
    let confirm = PairConfirm::decode(&frame.payload)?;
    if confirm.fingerprint != peer_tls_fp {
        return Err(Error::FingerprintMismatch);
    }

    let guest_tag = pairing_verify_tag(
        code,
        &identity.fingerprint,
        &peer_tls_fp,
        PairingRole::Guest,
    );
    session
        .send_msg(MsgType::PairVerify, PairVerify { tag: guest_tag }.encode())
        .await?;

    let vframe = recv_frame_timed(session).await?;
    match MsgType::from_u16(vframe.msg_type) {
        Some(MsgType::PairVerify) => {}
        Some(MsgType::PairReject) => {
            let rej = PairReject::decode(&vframe.payload)?;
            return Err(Error::Rejected(rej.reason));
        }
        _ => return Err(Error::Protocol("expected PairVerify")),
    }
    let verify = PairVerify::decode(&vframe.payload)?;
    let expected_host =
        pairing_verify_tag(code, &identity.fingerprint, &peer_tls_fp, PairingRole::Host);
    if !pairing_tags_equal(&verify.tag, &expected_host) {
        return Err(Error::BadPairingCode);
    }

    let peer = TrustedPeer {
        device_id: confirm.device_id,
        fingerprint: peer_tls_fp,
    };

    let remote_hello = recv_hello_timed(session).await?;
    if remote_hello.fingerprint != peer.fingerprint {
        return Err(Error::FingerprintMismatch);
    }
    enforce_hello_features(&remote_hello)?;
    send_hello_ack(session, identity).await?;
    Ok(peer)
}

async fn recv_frame_timed(session: &mut ControlSession) -> Result<Frame> {
    timeout(PAIR_STEP, session.recv_frame())
        .await
        .map_err(|_| Error::Protocol("pairing step timeout"))?
}

async fn recv_hello_timed(session: &mut ControlSession) -> Result<Hello> {
    let frame = recv_frame_timed(session).await?;
    if frame.msg_type != MsgType::Hello.as_u16() {
        return Err(Error::Protocol("expected Hello"));
    }
    Ok(Hello::decode(&frame.payload)?)
}

async fn recv_hello_ack_timed(session: &mut ControlSession) -> Result<Hello> {
    let frame = recv_frame_timed(session).await?;
    if frame.msg_type != MsgType::HelloAck.as_u16() {
        return Err(Error::Protocol("expected HelloAck"));
    }
    Ok(Hello::decode(&frame.payload)?)
}

async fn send_hello(session: &mut ControlSession, identity: &DeviceIdentity) -> Result<()> {
    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        features: Features::M3_CLIPBOARD,
        device_id: identity.device_id.clone(),
        fingerprint: identity.fingerprint,
        alias: identity.alias.clone(),
    };
    session.send_msg(MsgType::Hello, hello.encode()?).await
}

async fn send_hello_ack(session: &mut ControlSession, identity: &DeviceIdentity) -> Result<()> {
    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        features: Features::M3_CLIPBOARD,
        device_id: identity.device_id.clone(),
        fingerprint: identity.fingerprint,
        alias: identity.alias.clone(),
    };
    session.send_msg(MsgType::HelloAck, hello.encode()?).await
}

/// Require overlapping FILE capability (and matching protocol era via Hello bits).
fn enforce_hello_features(remote: &Hello) -> Result<Features> {
    if remote.protocol_version != PROTOCOL_VERSION {
        return Err(Error::Rejected(PairRejectReason::VersionMismatch));
    }
    let common = Features::M3_CLIPBOARD.intersect(remote.features);
    if !common.contains(Features::FILE) {
        return Err(Error::Rejected(PairRejectReason::FeatureMismatch));
    }
    Ok(common)
}

async fn recv_hello(session: &mut ControlSession) -> Result<Hello> {
    let frame = session.recv_frame().await?;
    if frame.msg_type != MsgType::Hello.as_u16() {
        return Err(Error::Protocol("expected Hello"));
    }
    Ok(Hello::decode(&frame.payload)?)
}

async fn recv_hello_ack(session: &mut ControlSession) -> Result<Hello> {
    let frame = session.recv_frame().await?;
    if frame.msg_type != MsgType::HelloAck.as_u16() {
        return Err(Error::Protocol("expected HelloAck"));
    }
    Ok(Hello::decode(&frame.payload)?)
}

/// Tell the peer we are leaving (unpair / shutdown) **before** closing the stream.
///
/// Order matters: `Bye` + flush, then `close_notify`. Closing first leaves the peer with a bare
/// EOF, indistinguishable from a crash — the whole point of this message.
pub async fn send_bye(session: &mut ControlSession) -> Result<()> {
    session.send_bye().await?;
    session.shutdown().await
}

/// Exchange Ping/Pong on an established session (smoke helper).
pub async fn ping_pong(session: &mut ControlSession, nonce: u64) -> Result<u64> {
    session
        .send_msg(MsgType::Ping, Ping { nonce }.encode())
        .await?;
    let frame = session.recv_frame().await?;
    if frame.msg_type != MsgType::Pong.as_u16() {
        return Err(Error::Protocol("expected Pong"));
    }
    let pong = Pong::decode(&frame.payload)?;
    Ok(pong.nonce)
}

pub async fn answer_ping(session: &mut ControlSession) -> Result<()> {
    let frame = session.recv_frame().await?;
    if frame.msg_type != MsgType::Ping.as_u16() {
        return Err(Error::Protocol("expected Ping"));
    }
    let ping = Ping::decode(&frame.payload)?;
    session
        .send_msg(MsgType::Pong, Pong { nonce: ping.nonce }.encode())
        .await
}

#[cfg(test)]
mod feature_gate_tests {
    use super::*;
    use lb_protocol::PROTOCOL_VERSION;

    fn hello_with(features: Features, version: u16) -> Hello {
        Hello {
            protocol_version: version,
            features,
            device_id: "peer".into(),
            fingerprint: [0u8; 32],
            alias: "t".into(),
        }
    }

    #[test]
    fn enforce_requires_file_overlap() {
        let ok = enforce_hello_features(&hello_with(Features::M3_CLIPBOARD, PROTOCOL_VERSION));
        assert!(ok.is_ok());
        assert!(ok.unwrap().contains(Features::FILE));

        let no_file = Features(Features::CLIPBOARD);
        match enforce_hello_features(&hello_with(no_file, PROTOCOL_VERSION)) {
            Err(Error::Rejected(PairRejectReason::FeatureMismatch)) => {}
            other => panic!("expected FeatureMismatch, got {other:?}"),
        }

        match enforce_hello_features(&hello_with(Features::M3_CLIPBOARD, PROTOCOL_VERSION + 1)) {
            Err(Error::Rejected(PairRejectReason::VersionMismatch)) => {}
            other => panic!("expected VersionMismatch, got {other:?}"),
        }
    }
}
