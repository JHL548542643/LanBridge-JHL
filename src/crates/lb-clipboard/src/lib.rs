//! Clipboard sync over the control TLS session (text + PNG).
//!
//! OS pasteboard (Win CF_UNICODETEXT / HarmonyOS MIMETYPE_TEXT_PLAIN + PixelMap)
//! is applied by app shells; this crate only moves bytes on the wire.
//! Never log clipboard body.

#![forbid(unsafe_code)]

mod error;
mod sync;

pub use error::{Error, Result};
pub use sync::{push_clipboard, recv_clipboard, ClipboardItem};
