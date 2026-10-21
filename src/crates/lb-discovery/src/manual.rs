use crate::error::{Error, Result};
use crate::traits::PeerInfo;
use std::net::SocketAddr;
use std::time::Duration;

/// Explicit peer address (QR / advanced manual IP path).
#[derive(Debug, Clone)]
pub struct ManualEndpoint {
    peer: PeerInfo,
}

impl ManualEndpoint {
    pub fn new(device_id: impl Into<String>, alias: impl Into<String>, addr: SocketAddr) -> Self {
        Self {
            peer: PeerInfo {
                device_id: device_id.into(),
                alias: alias.into(),
                addr,
            },
        }
    }

    pub fn from_addr(addr: SocketAddr) -> Self {
        Self::new("manual", "manual", addr)
    }

    pub async fn advertise(&self, _self_info: &PeerInfo) -> Result<()> {
        Ok(())
    }

    pub async fn discover(&self, _timeout: Duration) -> Result<Vec<PeerInfo>> {
        Ok(vec![self.peer.clone()])
    }

    pub fn parse_host_port(host: &str, port: u16) -> Result<SocketAddr> {
        let addr: SocketAddr = format!("{host}:{port}")
            .parse()
            .map_err(|e| Error::InvalidEndpoint(format!("{e}")))?;
        Ok(addr)
    }
}
