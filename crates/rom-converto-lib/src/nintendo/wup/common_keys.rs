//! Wii U common keys.
//!
//! Per `feedback_hardcoded_keys.md`, EOL consoles (including the
//! Wii U) can ship embedded keys: the retail common key below is
//! baked into every Wii U console and is used by every title, so
//! carrying it in-tree lets us decrypt NUS content without asking
//! the user to supply a key file.
//!
//! Sourced verbatim from the upstream implementation (`ncrypto.cpp`).
//! The key is only compiled in with the `embedded-keys` feature.

use crate::util::embedded_keys::{EmbeddedKey, embedded_key};

/// Retail Wii U common key. Used as the AES-128-CBC key when
/// decrypting a ticket's encrypted title key.
pub const WII_U_COMMON_KEY: EmbeddedKey<[u8; 16]> = embedded_key!([
    0xD7, 0xB0, 0x04, 0x02, 0x65, 0x9B, 0xA2, 0xAB, 0xD2, 0xCB, 0x0D, 0xB2, 0x7F, 0xA2, 0xB6, 0x56,
]);
