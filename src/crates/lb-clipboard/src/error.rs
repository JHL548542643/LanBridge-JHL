use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("session: {0}")]
    Session(#[from] lb_session::Error),
    #[error("protocol: {0}")]
    Protocol(#[from] lb_protocol::Error),
    #[error("rejected: {0:?}")]
    Rejected(lb_protocol::ClipRejectReason),
    #[error("clipboard: {0}")]
    Failed(&'static str),
}
