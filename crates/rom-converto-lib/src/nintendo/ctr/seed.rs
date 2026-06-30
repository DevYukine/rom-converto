//! Offline NCCH seed-crypto resolution.
//!
//! The decrypt path ([`crate::nintendo::ctr::decrypt`]) can fetch a title's
//! seed from Nintendo's CDN. The `info` path stays offline: it only resolves
//! seeds from a local `seeddb.bin` in the working directory and reports
//! whether the seed verifies against the NCCH `seedcheck`.

use crate::nintendo::ctr::models::ncch_header::NcchHeader;
use crate::nintendo::ctr::models::seeddb::SeedDatabase;
use binrw::BinRead;
use byteorder::{BigEndian, ByteOrder};
use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;

/// Outcome of resolving an NCCH's seed from a local `seeddb.bin`.
pub struct SeedResolution {
    /// A matching seed was present locally and verified against `seedcheck`.
    pub found: bool,
    /// Seed KeyY derived from the base KeyY and the seed, when verified.
    pub derived_key_y: Option<u128>,
}

fn load_local_seeds() -> HashMap<String, [u8; 16]> {
    let db_path = Path::new("seeddb.bin");
    match std::fs::read(db_path) {
        Ok(data) => match SeedDatabase::read(&mut Cursor::new(data)) {
            Ok(db) => db.seeds.into_iter().map(|s| (s.key, s.value)).collect(),
            Err(_) => HashMap::new(),
        },
        Err(_) => HashMap::new(),
    }
}

/// Look up the NCCH's title seed in a local `seeddb.bin`, verify it against
/// the header's `seedcheck`, and derive the seed KeyY. Never touches the
/// network. Returns `found = false` when no local seed matches or the seed
/// fails verification.
pub fn resolve_seed_offline(header: &NcchHeader) -> SeedResolution {
    // `seeddb.bin` keys titles by big-endian title id hex; the header stores
    // the title id little-endian.
    let mut tid_be = header.titleid;
    tid_be.reverse();
    let title_id_hex = hex::encode(tid_be);

    let seeds = load_local_seeds();
    let Some(seed) = seeds.get(&title_id_hex).copied() else {
        return SeedResolution {
            found: false,
            derived_key_y: None,
        };
    };

    let seed_check = BigEndian::read_u32(&header.seedcheck);
    if seedcheck_value(&seed, &header.titleid) != seed_check {
        return SeedResolution {
            found: false,
            derived_key_y: None,
        };
    }

    let base_key_y = BigEndian::read_u128(&header.signature[0..16]);
    SeedResolution {
        found: true,
        derived_key_y: Some(derive_seed_key_y(base_key_y, &seed)),
    }
}

/// First 4 bytes (big-endian) of `sha256(seed || title_id_le)`, the value an
/// NCCH stores in its `seedcheck` field.
fn seedcheck_value(seed: &[u8; 16], title_id_le: &[u8; 8]) -> u32 {
    let mut buf = Vec::with_capacity(seed.len() + title_id_le.len());
    buf.extend_from_slice(seed);
    buf.extend_from_slice(title_id_le);
    let sha = sha256::digest(buf);
    hex::decode(&sha[..8])
        .map(|b| BigEndian::read_u32(&b))
        .unwrap_or(0)
}

/// Seed KeyY = first 16 bytes of `sha256(base_key_y || seed)`.
fn derive_seed_key_y(base_key_y: u128, seed: &[u8; 16]) -> u128 {
    let mut buf = Vec::with_capacity(16 + seed.len());
    buf.extend_from_slice(&base_key_y.to_be_bytes());
    buf.extend_from_slice(seed);
    let keystr = sha256::digest(buf);
    hex::decode(&keystr[..32])
        .map(|b| BigEndian::read_u128(&b))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seedcheck_matches_self_generated_value() {
        let seed = [0x11u8; 16];
        let title_id_le = [0x00, 0x00, 0x10, 0x10, 0x0E, 0x00, 0x05, 0x00];
        let check = seedcheck_value(&seed, &title_id_le);
        // Recomputing with the same inputs is stable; a different seed differs.
        assert_eq!(check, seedcheck_value(&seed, &title_id_le));
        assert_ne!(check, seedcheck_value(&[0x22u8; 16], &title_id_le));
    }

    #[test]
    fn derive_seed_key_y_is_deterministic_and_mixes_inputs() {
        let seed = [0xABu8; 16];
        let base = 0x0123_4567_89AB_CDEF_0123_4567_89AB_CDEFu128;
        let derived = derive_seed_key_y(base, &seed);
        assert_eq!(derived, derive_seed_key_y(base, &seed));
        assert_ne!(derived, base, "derived KeyY must not equal the base KeyY");
        assert_ne!(derived, derive_seed_key_y(base, &[0xCDu8; 16]));
    }
}
