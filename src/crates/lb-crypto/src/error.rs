use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("certificate generation failed: {0}")]
    Cert(String),
    #[error("invalid pairing code")]
    BadPairingCode,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("identity store: {0}")]
    Store(String),
}
