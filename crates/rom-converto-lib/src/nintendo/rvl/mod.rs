//! Wii (codename RVL) console support.
//!
//! Houses the Wii-specific pieces that the shared RVZ pipeline in
//! [`crate::nintendo::rvz`] reaches for: disc detection, partition table
//! walking, AES-CBC sector encryption helpers, and the embedded Wii common
//! keys.

pub mod common_keys;
pub mod constants;
pub mod disc;
pub mod partition;

#[cfg(test)]
pub mod test_fixtures;

pub use disc::{
    WiiPartitionEntry, decrypt_sector, decrypt_title_key, encrypt_sector, encrypt_title_key,
    hash_h0, is_wii, read_partition_table,
};
