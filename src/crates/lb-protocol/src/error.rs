use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("buffer too short")]
    Truncated,
    #[error("bad magic")]
    BadMagic,
    #[error("unsupported protocol version: {0}")]
    BadVersion(u16),
    #[error("payload too large: {0}")]
    PayloadTooLarge(u32),
    #[error("unknown message type: 0x{0:04X} (skippable)")]
    UnknownMsgType(u16),
    #[error("invalid payload: {0}")]
    InvalidPayload(&'static str),
    #[error("utf8 error")]
    Utf8,
}
