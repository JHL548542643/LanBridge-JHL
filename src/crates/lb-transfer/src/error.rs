use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("session: {0}")]
    Session(#[from] lb_session::Error),
    #[error("protocol: {0}")]
    Protocol(#[from] lb_protocol::Error),
    #[error("bad path: {0}")]
    BadPath(&'static str),
    #[error("transfer rejected: {0:?}")]
    Rejected(lb_protocol::FileRejectReason),
    #[error("transfer failed: {0}")]
    Failed(&'static str),
    #[error("incomplete transfer: {done}/{expected} bytes")]
    Incomplete { done: u64, expected: u64 },
}
