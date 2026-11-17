//! Baked-in Nintendo DS KEY1 Blowfish table.
//!
//! The DS is end-of-life, so the cartridge KEY1 key buffer is embedded in
//! the binary the same way the PS3 and Wii U disc-key databases are (see
//! [`crate::sony::ps3::embedded_keys`] and
//! [`crate::nintendo::wup::disc::embedded_keys`]). It is not per-title key
//! material: every retail cartridge derives its secure-area key from this
//! one table plus the header id code, so no user-supplied key file exists
//! to ask for.
//!
//! The bytes are the `encr_data` array from upstream `source/encryption.cpp`,
//! byte-identical to the table other tools validate against an embedded
//! SHA-512 digest.
//!
//! DSi-enhanced and DSi-exclusive cartridges use a different KEY1 table
//! plus modcrypt for the DSi-only regions; neither is handled here.
//!
//! The table is only compiled in with the `embedded-keys` feature.

use crate::util::embedded_keys::{EmbeddedKey, embedded_key};

/// Bytes in the KEY1 key buffer: 18 P-array words plus four 256-word S-boxes.
pub const BLOWFISH_TABLE_SIZE: usize = 0x1048;

const TABLE: EmbeddedKey<&[u8; BLOWFISH_TABLE_SIZE]> = embedded_key!(include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../resources/nds_blowfish.bin"
)));

/// Returns the embedded KEY1 Blowfish table in its on-disk byte order.
///
/// # Errors
///
/// Returns [`crate::util::embedded_keys::NoEmbeddedKeys`] in a build
/// without the `embedded-keys` feature.
pub fn blowfish_table() -> EmbeddedKey<&'static [u8; BLOWFISH_TABLE_SIZE]> {
    TABLE
}
