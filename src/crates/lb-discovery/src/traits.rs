use crate::error::Result;
use crate::manual::ManualEndpoint;
use crate::mdns::MdnsDiscoverer;
use crate::udp::UdpDiscoverer;
use std::net::SocketAddr;
use std::time::Duration;

/// Advertised / discovered peer summary (no secrets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    pub device_id: String,
    pub alias: String,
    pub addr: SocketAddr,
}

/// Discovery backends for M1. mDNS is reserved (returns empty / not-implemented advertise).
pub enum Discoverer {
    Udp(UdpDiscoverer),
    Manual(ManualEndpoint),
    Mdns(MdnsDiscoverer),
}

impl Discoverer {
    pub async fn advertise(&self, self_info: &PeerInfo) -> Result<()> {
        match self {
            Self::Udp(d) => d.advertise(self_info).await,
            Self::Manual(d) => d.advertise(self_info).await,
            Self::Mdns(d) => d.advertise(self_info).await,
        }
    }

    pub async fn discover(&self, timeout: Duration) -> Result<Vec<PeerInfo>> {
        match self {
            Self::Udp(d) => d.discover(timeout).await,
            Self::Manual(d) => d.discover(timeout).await,
            Self::Mdns(d) => d.discover(timeout).await,
        }
    }
}
