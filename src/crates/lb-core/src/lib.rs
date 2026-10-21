//! LanBridge core facade — apps and FFI call into this crate.

#![forbid(unsafe_code)]

pub use lb_clipboard::{push_clipboard, recv_clipboard, ClipboardItem};
pub use lb_crypto::{
    generate_pairing_code, pairing_tags_equal, pairing_verify_tag, DeviceIdentity, PairingCode,
    PairingRole,
};
pub use lb_discovery::{
    Discoverer, ManualEndpoint, MdnsDiscoverer, PeerInfo, UdpBeaconConfig, UdpDiscoverer,
    DEFAULT_DISCOVERY_PORT,
};
pub use lb_protocol::{Features, MsgType, PROTOCOL_VERSION};
pub use lb_session::{
    accept_trusted, answer_ping, await_host_pair, connect_trusted, pair_as_guest, pair_as_host,
    ping_pong, send_bye, ControlSession, HostPairOutcome, TrustStore, TrustedPeer,
};
pub use lb_transfer::{
    establish_receiver, establish_sender, recv_dir, recv_file, send_dir, send_file, shutdown_link,
    ByteProgress, Link, RecvOptions, SendOptions, TransferPlan, TransferSummary,
    DEFAULT_PROBE_BYTES,
};

pub mod app_session;
pub use app_session::{
    ClipboardOutWork, DeviceSession, DisconnectReason, GuestService, HostService, InboundEvent,
    InboundFileWork, InboundRead, NearbyPeer, OutboundFileWork, ProbeWork, SendFileResult,
    ServeListenParts, SessionState, SessionStatus, DEFAULT_CONTROL_PORT,
};

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Crypto(#[from] lb_crypto::Error),
    #[error(transparent)]
    Discovery(#[from] lb_discovery::Error),
    #[error(transparent)]
    Session(#[from] lb_session::Error),
    #[error(transparent)]
    Protocol(#[from] lb_protocol::Error),
    #[error(transparent)]
    Transfer(#[from] lb_transfer::Error),
    #[error(transparent)]
    Clipboard(#[from] lb_clipboard::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
