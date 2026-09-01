//! Baked-in Wii U disc-key database.
//!
//! The Wii U is end-of-life, so the per-disc AES master keys for the
//! redump disc set are embedded in the binary. There is no plaintext
//! title id to key on (the Wii U title id is only readable after the
//! disc key decrypts the partition table), so a candidate key is
//! chosen by the input file name and then verified against the TOC
//! sentinel, falling back to a trial-decrypt probe over the whole set.
//! This makes `--key`/`game.key` optional for a disc in the set.

use std::collections::HashMap;
use std::sync::LazyLock;

use crate::nintendo::wup::disc::disc_key::DiscKey;

/// `NAME<TAB>32-hex-key` per line, sorted by `NAME`. `NAME` is the
/// redump game name, which is also the disc file's stem.
const DB: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../resources/wup_disc_keys.tsv"
));

/// Name (lowercased) to raw 16-byte key.
static KEYS_BY_NAME: LazyLock<HashMap<String, [u8; 16]>> = LazyLock::new(|| {
    DB.lines()
        .filter_map(|line| {
            let (name, hex) = line.split_once('\t')?;
            let mut key = [0u8; 16];
            hex::decode_to_slice(hex, &mut key).ok()?;
            Some((name.to_ascii_lowercase(), key))
        })
        .collect()
});

/// All embedded keys, for trial-decrypt probing.
static ALL_KEYS: LazyLock<Vec<[u8; 16]>> = LazyLock::new(|| {
    DB.lines()
        .filter_map(|line| {
            let (_, hex) = line.split_once('\t')?;
            let mut key = [0u8; 16];
            hex::decode_to_slice(hex, &mut key).ok()?;
            Some(key)
        })
        .collect()
});

/// Look up the embedded disc key by disc-file stem (case-insensitive).
pub fn embedded_key_by_name(stem: &str) -> Option<DiscKey> {
    KEYS_BY_NAME
        .get(&stem.to_ascii_lowercase())
        .copied()
        .map(DiscKey)
}

/// All embedded keys, for probing.
pub fn embedded_keys() -> &'static [[u8; 16]] {
    &ALL_KEYS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_parses_to_full_length() {
        assert_eq!(KEYS_BY_NAME.len(), 501);
        assert_eq!(ALL_KEYS.len(), 501);
    }

    #[test]
    fn known_name_resolves() {
        assert_eq!(
            embedded_key_by_name("007 Legends (USA) (En,Fr)"),
            Some(DiscKey(hex_literal::hex!(
                "C47AB82D4AA06B2E87F717370A6961BE"
            )))
        );
    }

    #[test]
    fn lookup_is_case_insensitive() {
        assert_eq!(
            embedded_key_by_name("007 legends (usa) (en,fr)"),
            embedded_key_by_name("007 Legends (USA) (En,Fr)")
        );
    }

    #[test]
    fn unknown_name_is_none() {
        assert_eq!(embedded_key_by_name("not a real game"), None);
    }
}
