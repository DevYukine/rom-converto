//! Baked-in PS3 disc-key database.
//!
//! PS3 is end-of-life, so the per-disc AES data keys for the redump disc
//! set are embedded in the binary and looked up by the disc's `TITLE_ID`
//! (read from the plaintext `PARAM.SFO`, no key required). This makes
//! `--key`/`.dkey` optional: a disc in the set decrypts with no key file.
//!
//! The table is keyed by the on-disc `TITLE_ID` form (`XXXXNNNNN`, no
//! dash). A game released under several regional serials registers its
//! key under each. Title IDs whose redump entries disagree on the key
//! are omitted, so a lookup never returns a wrong key.

use std::collections::HashMap;
use std::sync::LazyLock;

use crate::ps3::key::Ps3Key;

/// `TITLE_ID<TAB>32-hex-key` per line, sorted by `TITLE_ID`.
const DB: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../resources/ps3_disc_keys.tsv"
));

static KEYS: LazyLock<HashMap<&'static str, [u8; 16]>> = LazyLock::new(|| {
    DB.lines()
        .filter_map(|line| {
            let (id, hex) = line.split_once('\t')?;
            let mut key = [0u8; 16];
            hex::decode_to_slice(hex, &mut key).ok()?;
            Some((id, key))
        })
        .collect()
});

/// Look up the embedded data key for a disc `TITLE_ID` (e.g. `BLUS30490`).
pub fn embedded_key(title_id: &str) -> Option<Ps3Key> {
    KEYS.get(title_id).copied().map(Ps3Key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_title_id_resolves() {
        // 3D Dot Game Heroes (USA), redump serial BLUS-30490.
        assert_eq!(
            embedded_key("BLUS30490"),
            Some(Ps3Key(hex_literal::hex!(
                "E152AE2CEF05915DFD5FB307A0C062E3"
            )))
        );
    }

    #[test]
    fn multi_region_serial_shares_one_key() {
        // Rainbow Six Vegas 2 ships under four serials, one disc key.
        let k = embedded_key("BLUS30125").expect("present");
        assert_eq!(embedded_key("BLAS50045"), Some(k));
        assert_eq!(embedded_key("BLKS20067"), Some(k));
    }

    #[test]
    fn unknown_title_id_is_none() {
        assert_eq!(embedded_key("ZZZZ00000"), None);
    }

    #[test]
    fn table_parses_and_is_nonempty() {
        assert!(KEYS.len() > 4000);
    }
}
