use crate::error::{Error, Result};
use crate::identity::Fingerprint;
use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Which side is proving knowledge of the pairing code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingRole {
    /// Guest → Host (must be sent first; Host never emits this tag).
    Guest,
    /// Host → Guest (only after Guest tag verifies).
    Host,
}

impl PairingRole {
    fn domain(self) -> &'static [u8] {
        match self {
            Self::Guest => b"lb-pair-guest-v0",
            Self::Host => b"lb-pair-host-v0",
        }
    }
}

/// Six-digit pairing code shown during first trust.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PairingCode(String);

impl PairingCode {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Normalize user input to exactly six ASCII digits.
    pub fn parse(raw: &str) -> Result<Self> {
        let normalized: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
        if normalized.len() != 6 {
            return Err(Error::BadPairingCode);
        }
        Ok(PairingCode(normalized))
    }
}

impl std::fmt::Display for PairingCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub fn generate_pairing_code() -> PairingCode {
    let n: u32 = rand::thread_rng().gen_range(0..1_000_000);
    PairingCode(format!("{n:06}"))
}

pub fn verify_pairing_code(expected: &PairingCode, provided: &str) -> Result<()> {
    let normalized: String = provided.chars().filter(|c| c.is_ascii_digit()).collect();
    if normalized == expected.0 {
        Ok(())
    } else {
        Err(Error::BadPairingCode)
    }
}

/// Binding tag: HMAC-SHA256(code, domain || min(fp_a,fp_b) || max(fp_a,fp_b)).
/// Code never goes on the wire. Guest and Host domains differ so Host must not
/// emit the Guest tag (prevents offline brute-force of the 6-digit code).
pub fn pairing_verify_tag(
    code: &PairingCode,
    fp_a: &Fingerprint,
    fp_b: &Fingerprint,
    role: PairingRole,
) -> Fingerprint {
    let (lo, hi) = if fp_a <= fp_b {
        (fp_a, fp_b)
    } else {
        (fp_b, fp_a)
    };
    let mut mac =
        HmacSha256::new_from_slice(code.as_str().as_bytes()).expect("HMAC accepts any key length");
    mac.update(role.domain());
    mac.update(lo);
    mac.update(hi);
    let result = mac.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

pub fn pairing_tags_equal(a: &Fingerprint, b: &Fingerprint) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_code() {
        let code = generate_pairing_code();
        assert_eq!(code.as_str().len(), 6);
        verify_pairing_code(&code, code.as_str()).unwrap();
        assert!(verify_pairing_code(&code, "000000").is_err() || code.as_str() == "000000");
    }

    #[test]
    fn verify_tag_symmetric_domain_separated() {
        let code = PairingCode("123456".into());
        let a = [1u8; 32];
        let b = [2u8; 32];
        let g1 = pairing_verify_tag(&code, &a, &b, PairingRole::Guest);
        let g2 = pairing_verify_tag(&code, &b, &a, PairingRole::Guest);
        assert!(pairing_tags_equal(&g1, &g2));
        let h = pairing_verify_tag(&code, &a, &b, PairingRole::Host);
        assert!(!pairing_tags_equal(&g1, &h));
        let other = PairingCode("654321".into());
        let g3 = pairing_verify_tag(&other, &a, &b, PairingRole::Guest);
        assert!(!pairing_tags_equal(&g1, &g3));
        let mitm = [9u8; 32];
        let g4 = pairing_verify_tag(&code, &a, &mitm, PairingRole::Guest);
        assert!(!pairing_tags_equal(&g1, &g4));
    }

    #[test]
    fn vector_pair_verify_v0() {
        #[derive(serde::Deserialize)]
        struct V {
            code: String,
            fp_lo_hex: String,
            fp_hi_hex: String,
            guest_tag_hex: String,
            host_tag_hex: String,
        }
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../protocol/vectors/pair_verify_v0.json");
        let v: V = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let mut lo = [0u8; 32];
        let mut hi = [0u8; 32];
        lo.copy_from_slice(&hex::decode(&v.fp_lo_hex).unwrap());
        hi.copy_from_slice(&hex::decode(&v.fp_hi_hex).unwrap());
        let code = PairingCode(v.code);
        let guest = pairing_verify_tag(&code, &lo, &hi, PairingRole::Guest);
        let host = pairing_verify_tag(&code, &lo, &hi, PairingRole::Host);
        assert_eq!(hex::encode(guest), v.guest_tag_hex);
        assert_eq!(hex::encode(host), v.host_tag_hex);
    }
}
