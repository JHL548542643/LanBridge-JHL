use crate::error::{Error, Result};
use crate::traits::PeerInfo;
use std::time::Duration;

/// Placeholder for future mDNS (Bonjour) discovery.
#[derive(Debug, Default, Clone)]
pub struct MdnsDiscoverer;

impl MdnsDiscoverer {
    pub fn new() -> Self {
        Self
    }

    pub async fn advertise(&self, _self_info: &PeerInfo) -> Result<()> {
        Err(Error::NotImplemented(
            "mDNS advertise reserved for later M1+/M2; use UDP or manual IP",
        ))
    }

    pub async fn discover(&self, _timeout: Duration) -> Result<Vec<PeerInfo>> {
        // Soft-empty so callers can OR backends without hard-fail.
        Ok(Vec::new())
    }
}
