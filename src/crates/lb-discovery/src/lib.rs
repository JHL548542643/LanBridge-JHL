//! LAN discovery: UDP beacon + manual endpoint; mDNS reserved.

#![forbid(unsafe_code)]

mod error;
mod manual;
mod mdns;
mod traits;
mod udp;

pub use error::{Error, Result};
pub use manual::ManualEndpoint;
pub use mdns::MdnsDiscoverer;
pub use traits::{Discoverer, PeerInfo};
pub use udp::{UdpBeaconConfig, UdpDiscoverer, DEFAULT_DISCOVERY_PORT};
