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

/// Resolve and load the data key for `input`.
///
/// Precedence: explicit `override_path` when supplied, else the sibling
/// `<input_stem>.dkey`. Returns [`Ps3Error::KeyMissing`] naming the
/// override path if it was given but doesn't exist, or `input` if
/// neither exists.
pub fn load_ps3_key(input: &Path, override_path: Option<&Path>) -> Ps3Result<Ps3Key> {
    let chosen = match override_path {
        Some(p) if p.is_file() => p.to_path_buf(),
        Some(p) => return Err(Ps3Error::KeyMissing(p.to_path_buf())),
        None => resolve_sibling_key_path(input)
            .ok_or_else(|| Ps3Error::KeyMissing(input.to_path_buf()))?,
    };
    fs::read(&chosen)
        .map_err(|e| {
            Ps3Error::Io(std::io::Error::new(
                e.kind(),
                format!("{}: {e}", chosen.display()),
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

    #[test]
    fn missing_override_path_reports_key_missing_for_the_override() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.iso");
        std::fs::write(&input, b"").unwrap();
        let override_path = dir.path().join("nope.dkey");

        let err = load_ps3_key(&input, Some(&override_path)).unwrap_err();
        assert!(matches!(err, Ps3Error::KeyMissing(p) if p == override_path));
    }

    #[test]
    fn no_sibling_dkey_reports_key_missing_for_the_input() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.iso");
        std::fs::write(&input, b"").unwrap();

        let err = load_ps3_key(&input, None).unwrap_err();
        assert!(matches!(err, Ps3Error::KeyMissing(p) if p == input));
    }
}
