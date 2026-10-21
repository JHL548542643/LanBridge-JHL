use crate::error::{Error, Result};
use crate::trust::TrustedPeer;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// On-disk list of paired peers (device id + certificate fingerprint only).
#[derive(Debug, Clone, Default)]
pub struct TrustStore {
    peers: Vec<TrustedPeer>,
}

impl TrustStore {
    pub fn new() -> Self {
        Self { peers: Vec::new() }
    }

    pub fn insert(&mut self, peer: TrustedPeer) {
        if let Some(existing) = self
            .peers
            .iter_mut()
            .find(|p| p.device_id == peer.device_id)
        {
            *existing = peer;
        } else {
            self.peers.push(peer);
        }
    }

    pub fn get(&self, device_id: &str) -> Option<&TrustedPeer> {
        self.peers.iter().find(|p| p.device_id == device_id)
    }

    pub fn peers(&self) -> &[TrustedPeer] {
        &self.peers
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let file = TrustFile {
            v: 0,
            peers: self
                .peers
                .iter()
                .map(|p| TrustedPeerFile {
                    device_id: p.device_id.clone(),
                    fingerprint: hex::encode(p.fingerprint),
                })
                .collect(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|e| Error::Store(e.to_string()))?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, bytes)?;
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        let file: TrustFile =
            serde_json::from_slice(&bytes).map_err(|e| Error::Store(e.to_string()))?;
        if file.v != 0 {
            return Err(Error::Store(format!(
                "unsupported trust file version {}",
                file.v
            )));
        }
        let mut peers = Vec::with_capacity(file.peers.len());
        for p in file.peers {
            let raw = hex::decode(&p.fingerprint).map_err(|e| Error::Store(e.to_string()))?;
            if raw.len() != 32 {
                return Err(Error::Store("fingerprint length".into()));
            }
            let mut fingerprint = [0u8; 32];
            fingerprint.copy_from_slice(&raw);
            peers.push(TrustedPeer {
                device_id: p.device_id,
                fingerprint,
            });
        }
        Ok(Self { peers })
    }
}

#[derive(Serialize, Deserialize)]
struct TrustFile {
    v: u16,
    peers: Vec<TrustedPeerFile>,
}

#[derive(Serialize, Deserialize)]
struct TrustedPeerFile {
    device_id: String,
    fingerprint: String,
}
