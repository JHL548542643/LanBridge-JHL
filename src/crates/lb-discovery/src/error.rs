use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("discovery not implemented: {0}")]
    NotImplemented(&'static str),
    #[error("invalid endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("beacon encode/decode: {0}")]
    Beacon(String),
}
