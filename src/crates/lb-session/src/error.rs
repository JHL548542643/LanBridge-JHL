use lb_protocol::PairRejectReason;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("tls: {0}")]
    Tls(String),
    #[error("protocol: {0}")]
    Protocol(&'static str),
    #[error("wire: {0}")]
    Wire(#[from] lb_protocol::Error),
    #[error("crypto: {0}")]
    Crypto(#[from] lb_crypto::Error),
    #[error("bad pairing code")]
    BadPairingCode,
    #[error("pairing rejected: {0:?}")]
    Rejected(PairRejectReason),
    /// Peer sent `Bye`: it is leaving on purpose (unpair / shutdown).
    #[error("peer left")]
    PeerLeft,
    /// Peer closed the stream without `Bye` (crash, probe teardown, network drop).
    #[error("peer closed the connection")]
    PeerClosed,
    #[error("peer certificate fingerprint mismatch")]
    FingerprintMismatch,
    #[error("store: {0}")]
    Store(String),
}
