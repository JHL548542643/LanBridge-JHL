use lb_crypto::Fingerprint;

/// Peer accepted after successful pairing (pinned).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedPeer {
    pub device_id: String,
    pub fingerprint: Fingerprint,
}
