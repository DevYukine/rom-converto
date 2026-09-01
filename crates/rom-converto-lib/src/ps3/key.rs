//! PS3 disc data key loading.
//!
//! A `.dkey` file holds the 128 bit data key as 32 ASCII hex characters,
//! used verbatim (no transform) for sector decryption.

use std::fs;
use std::path::{Path, PathBuf};

use crate::ps3::error::{Ps3Error, Ps3Result};

/// The 16-byte AES-128 data key used to decrypt encrypted sectors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ps3Key(pub [u8; 16]);

impl Ps3Key {
    /// The raw 16-byte AES-128 key.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Parse a `.dkey` file: 32 ASCII hex characters (ASCII whitespace
    /// tolerated, an optional leading `0x` stripped) decoded verbatim.
    pub fn from_dkey_contents(contents: &[u8]) -> Ps3Result<Self> {
        let stripped: String = contents
            .iter()
            .filter(|b| !b.is_ascii_whitespace())
            .map(|b| *b as char)
            .collect();
        let text = stripped
            .strip_prefix("0x")
            .or_else(|| stripped.strip_prefix("0X"))
            .unwrap_or(&stripped);
        if text.len() != 32 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Ps3Error::KeyMalformed(format!(
                "expected 32 hex chars, got {}; supply a .dkey file holding the final disc key as hex (raw 16-byte d1 .key files and IRD files are not supported)",
                text.len()
            )));
        }
        let raw = hex::decode(text).map_err(|e| Ps3Error::KeyMalformed(e.to_string()))?;
        let mut out = [0u8; 16];
        out.copy_from_slice(&raw);
        Ok(Self(out))
    }
}

/// Resolve the data key for `disc_path`.
///
/// Precedence: an explicit `override_path` (`--key`) always wins; else
/// the embedded disc-key database keyed by the disc's `TITLE_ID`; else a
/// sibling `<key_basis>.dkey`. `key_basis` is the naming basis for the
/// sibling lookup (the original input), which can differ from
/// `disc_path` when the disc was extracted from an archive.
///
/// Returns [`Ps3Error::KeyMissing`] naming the override path when it was
/// given but doesn't exist, or `key_basis` when no key is found anywhere.
pub fn resolve_ps3_key(
    disc_path: &Path,
    key_basis: &Path,
    override_path: Option<&Path>,
) -> Ps3Result<Ps3Key> {
    if let Some(p) = override_path {
        return if p.is_file() {
            load_key_file(p)
        } else {
            Err(Ps3Error::KeyMissing(p.to_path_buf()))
        };
    }
    if let Some(key) = embedded_key_for_disc(disc_path) {
        return Ok(key);
    }
    if let Some(sibling) = resolve_sibling_key_path(key_basis) {
        return load_key_file(&sibling);
    }
    Err(Ps3Error::KeyMissing(key_basis.to_path_buf()))
}

/// Read the disc's `TITLE_ID` from the plaintext metadata and look it up
/// in the embedded database. Any read/parse failure yields `None` so
/// resolution falls through to a `.dkey` file.
fn embedded_key_for_disc(disc_path: &Path) -> Option<Ps3Key> {
    let title_id = crate::ps3::read_ps3_info(disc_path).ok()?.title_id?;
    crate::ps3::embedded_keys::embedded_key(&title_id)
}

fn load_key_file(path: &Path) -> Ps3Result<Ps3Key> {
    fs::read(path)
        .map_err(|e| {
            Ps3Error::Io(std::io::Error::new(
                e.kind(),
                format!("{}: {e}", path.display()),
            ))
        })
        .and_then(|contents| Ps3Key::from_dkey_contents(&contents))
}

fn resolve_sibling_key_path(input: &Path) -> Option<PathBuf> {
    let mut sibling = input.to_path_buf();
    sibling.set_extension("dkey");
    sibling.is_file().then_some(sibling)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX_KEY: &str = "000102030405060708090A0B0C0D0E0F";
    const RAW_KEY: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

    #[test]
    fn missing_override_path_reports_key_missing_for_the_override() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.iso");
        std::fs::write(&input, b"").unwrap();
        let override_path = dir.path().join("nope.dkey");

        let err = resolve_ps3_key(&input, &input, Some(&override_path)).unwrap_err();
        assert!(matches!(err, Ps3Error::KeyMissing(p) if p == override_path));
    }

    #[test]
    fn override_file_wins() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.iso");
        std::fs::write(&input, b"").unwrap();
        let override_path = dir.path().join("k.dkey");
        std::fs::write(&override_path, HEX_KEY).unwrap();

        let key = resolve_ps3_key(&input, &input, Some(&override_path)).unwrap();
        assert_eq!(key, Ps3Key(RAW_KEY));
    }

    #[test]
    fn falls_back_to_sibling_dkey() {
        // A non-PS3 input can't resolve an embedded key, so a sibling
        // .dkey is used.
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.iso");
        std::fs::write(&input, b"").unwrap();
        std::fs::write(dir.path().join("game.dkey"), HEX_KEY).unwrap();

        let key = resolve_ps3_key(&input, &input, None).unwrap();
        assert_eq!(key, Ps3Key(RAW_KEY));
    }

    #[test]
    fn no_key_anywhere_reports_key_missing_for_the_basis() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.iso");
        std::fs::write(&input, b"").unwrap();

        let err = resolve_ps3_key(&input, &input, None).unwrap_err();
        assert!(matches!(err, Ps3Error::KeyMissing(p) if p == input));
    }
}
