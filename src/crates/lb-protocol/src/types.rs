//! Message type registry.

/// Wire protocol version carried in every frame header.
pub const PROTOCOL_VERSION: u16 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum MsgType {
    Hello = 0x0001,
    HelloAck = 0x0002,
    PairOffer = 0x0010,
    PairAccept = 0x0011,
    PairConfirm = 0x0012,
    PairReject = 0x0013,
    PairVerify = 0x0014,
    Ping = 0x0020,
    Pong = 0x0021,
    /// Graceful goodbye: the sender is leaving (unpaired / shutting down). Receivers must drop
    /// the peer's trust and show "peer left" instead of a stale "connected".
    Bye = 0x0030,
    FileOffer = 0x0100,
    FileAccept = 0x0101,
    FileReject = 0x0102,
    FileComplete = 0x0103,
    DirOffer = 0x0110,
    DirComplete = 0x0111,
    ProbeRequest = 0x0120,
    ProbeResult = 0x0121,
    OpenChannels = 0x0140,
    ChannelsReady = 0x0141,
    ChannelHello = 0x0142,
    SliceHeader = 0x0143,
    ClipOffer = 0x0200,
    ClipAck = 0x0201,
    ClipReject = 0x0202,
}

impl MsgType {
    pub fn from_u16(v: u16) -> Option<Self> {
        Some(match v {
            0x0001 => Self::Hello,
            0x0002 => Self::HelloAck,
            0x0010 => Self::PairOffer,
            0x0011 => Self::PairAccept,
            0x0012 => Self::PairConfirm,
            0x0013 => Self::PairReject,
            0x0014 => Self::PairVerify,
            0x0020 => Self::Ping,
            0x0021 => Self::Pong,
            0x0030 => Self::Bye,
            0x0100 => Self::FileOffer,
            0x0101 => Self::FileAccept,
            0x0102 => Self::FileReject,
            0x0103 => Self::FileComplete,
            0x0110 => Self::DirOffer,
            0x0111 => Self::DirComplete,
            0x0120 => Self::ProbeRequest,
            0x0121 => Self::ProbeResult,
            0x0140 => Self::OpenChannels,
            0x0141 => Self::ChannelsReady,
            0x0142 => Self::ChannelHello,
            0x0143 => Self::SliceHeader,
            0x0200 => Self::ClipOffer,
            0x0201 => Self::ClipAck,
            0x0202 => Self::ClipReject,
            _ => return None,
        })
    }

    pub fn as_u16(self) -> u16 {
        self as u16
    }

    /// Unknown types in reserved ranges must be skipped by receivers.
    pub fn is_skippable_unknown(raw: u16) -> bool {
        Self::from_u16(raw).is_none()
    }
}
