//! Hardcoded Wii common keys.
//!
//! These are embedded directly in the source because both the Wii and
//! GameCube are end-of-life consoles. Per the rom-converto key policy, only
//! current-life consoles (Switch) require user-supplied key files.
//!
//! Public source: <https://hackmii.com/2008/04/keys-keys-keys/>

use hex_literal::hex;

/// Standard retail Wii common key. Used to decrypt the partition title key
/// via AES-CBC with the first 8 bytes of the title id (zero-padded to 16)
/// as the IV.
pub const WII_COMMON_KEY: [u8; 16] = hex!("ebe42a225e8593e448d9c5457381aaf7");

/// Korean retail Wii common key. Used for Korean game discs when the
/// ticket's `common_key_index` byte is 1.
pub const WII_KOREAN_COMMON_KEY: [u8; 16] = hex!("63b82bb4f4614e2e13f2fefbba4c9b7e");

/// vWii common key. Selected when `common_key_index` is 2 on Wii U vWii discs.
pub const VWII_COMMON_KEY: [u8; 16] = hex!("30bfc76e7c19afbb23163330ced7c28d");

/// Resolve a `ticket.common_key_index` byte to its AES key. Returns `None`
/// if the index does not correspond to a known key.
pub fn common_key(index: u8) -> Option<&'static [u8; 16]> {
    match index {
        0 => Some(&WII_COMMON_KEY),
        1 => Some(&WII_KOREAN_COMMON_KEY),
        2 => Some(&VWII_COMMON_KEY),
        _ => None,
    }
}
