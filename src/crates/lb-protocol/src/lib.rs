//! LanBridge wire protocol v0.

#![forbid(unsafe_code)]

mod error;
mod features;
mod frame;
mod messages;
mod types;

pub use error::{Error, Result};
pub use features::Features;
pub use frame::{decode_frame, encode_frame, Frame, HEADER_LEN, MAGIC, MAX_PAYLOAD_LEN};
pub use messages::{
    Bye, ChannelHello, ClipAck, ClipKind, ClipOffer, ClipReject, ClipRejectReason, DirComplete,
    DirOffer, FileAccept, FileComplete, FileOffer, FileReject, FileRejectReason, Hello,
    OpenChannels, PairAccept, PairConfirm, PairReject, PairRejectReason, PairVerify, Ping, Pong,
    ProbeRequest, ProbeResult, SliceHeader,
};
pub use types::{MsgType, PROTOCOL_VERSION};

/// Decode a complete frame from `src`, returning bytes consumed.
pub fn try_decode(src: &[u8]) -> Result<Option<(Frame, usize)>> {
    frame::try_decode(src)
}
