//! Device identity and pairing materials.

#![forbid(unsafe_code)]

mod error;
mod identity;
mod pairing;

pub use error::{Error, Result};
pub use identity::{fingerprint_sha256, DeviceIdentity, Fingerprint};
pub use pairing::{
    generate_pairing_code, pairing_tags_equal, pairing_verify_tag, verify_pairing_code,
    PairingCode, PairingRole,
};
