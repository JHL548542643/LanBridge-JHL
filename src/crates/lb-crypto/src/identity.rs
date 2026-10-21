use crate::error::{Error, Result};
use rcgen::{CertificateParams, KeyPair};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

pub type Fingerprint = [u8; 32];

/// SHA-256 over certificate DER bytes (pinning material).
pub fn fingerprint_sha256(cert_der: &[u8]) -> Fingerprint {
    let digest = Sha256::digest(cert_der);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

#[derive(Clone)]
pub struct DeviceIdentity {
    pub device_id: String,
    pub alias: String,
    cert_der: Vec<u8>,
    key_pkcs8: Vec<u8>,
    pub fingerprint: Fingerprint,
}

impl DeviceIdentity {
    /// Generate a self-signed identity for LAN mTLS (personal use).
    pub fn generate(device_id: impl Into<String>, alias: impl Into<String>) -> Result<Self> {
        let device_id = device_id.into();
        let alias = alias.into();
        let mut params = CertificateParams::new(vec!["lanbridge.device".into()])
            .map_err(|e| Error::Cert(e.to_string()))?;
        params.is_ca = rcgen::IsCa::NoCa;
        let key_pair = KeyPair::generate().map_err(|e| Error::Cert(e.to_string()))?;
        let cert = params
            .self_signed(&key_pair)
            .map_err(|e| Error::Cert(e.to_string()))?;
        let cert_der = cert.der().to_vec();
        let key_pkcs8 = key_pair.serialize_der();
        let fingerprint = fingerprint_sha256(&cert_der);
        Ok(Self {
            device_id,
            alias,
            cert_der,
            key_pkcs8,
            fingerprint,
        })
    }

    pub fn certificate_der(&self) -> CertificateDer<'static> {
        CertificateDer::from(self.cert_der.clone())
    }

    pub fn private_key_der(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key_pkcs8.clone()))
    }

    pub fn fingerprint_hex(&self) -> String {
        hex::encode(self.fingerprint)
    }

    /// Persist identity under `dir` (`meta.json`, `cert.der`, `key.pkcs8`).
    /// Caller chooses the directory; do not commit it.
    pub fn save_to_dir(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        let meta = IdentityMeta {
            device_id: self.device_id.clone(),
            alias: self.alias.clone(),
        };
        let meta_bytes =
            serde_json::to_vec_pretty(&meta).map_err(|e| Error::Store(e.to_string()))?;
        write_file(&dir.join("meta.json"), &meta_bytes)?;
        write_file(&dir.join("cert.der"), &self.cert_der)?;
        write_file(&dir.join("key.pkcs8"), &self.key_pkcs8)?;
        Ok(())
    }

    /// Load identity and recompute the certificate fingerprint (not taken from disk metadata).
    pub fn load_from_dir(dir: &Path) -> Result<Self> {
        let meta_bytes = std::fs::read(dir.join("meta.json"))?;
        let meta: IdentityMeta =
            serde_json::from_slice(&meta_bytes).map_err(|e| Error::Store(e.to_string()))?;
        let cert_der = std::fs::read(dir.join("cert.der"))?;
        let key_pkcs8 = std::fs::read(dir.join("key.pkcs8"))?;
        if cert_der.is_empty() || key_pkcs8.is_empty() {
            return Err(Error::Store("empty cert or key".into()));
        }
        let fingerprint = fingerprint_sha256(&cert_der);
        Ok(Self {
            device_id: meta.device_id,
            alias: meta.alias,
            cert_der,
            key_pkcs8,
            fingerprint,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct IdentityMeta {
    device_id: String,
    alias: String,
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_has_stable_fingerprint_len() {
        let id = DeviceIdentity::generate("dev-a", "A").unwrap();
        assert_eq!(id.fingerprint.len(), 32);
        assert!(!id.fingerprint_hex().is_empty());
    }

    #[test]
    fn save_load_roundtrip() {
        let id = DeviceIdentity::generate("dev-a", "A").unwrap();
        let dir = std::env::temp_dir().join(format!("lb-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        id.save_to_dir(&dir).unwrap();
        let loaded = DeviceIdentity::load_from_dir(&dir).unwrap();
        assert_eq!(loaded.device_id, id.device_id);
        assert_eq!(loaded.alias, id.alias);
        assert_eq!(loaded.fingerprint, id.fingerprint);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
