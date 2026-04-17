//! Per-disc master key loading.
//!
//! Wii U optical discs have no universal master key, so each disc
//! has its own 128 bit key the user supplies. The usual form is 16
//! raw bytes in `game.key`. This module also accepts a 32-character
//! hex string (whitespace tolerated).

use std::fs;
use std::path::{Path, PathBuf};

use crate::nintendo::wup::error::{WupError, WupResult};

/// Sixteen byte AES-128 key used for disc-level decryption.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiscKey(pub [u8; 16]);

impl DiscKey {
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Parse key material from raw bytes (exactly 16) or from an
    /// ASCII hex representation (32 hex digits, whitespace tolerated).
    /// Rejects anything else.
    pub fn from_file_contents(contents: &[u8]) -> WupResult<Self> {
        if contents.len() == 16 {
            let mut out = [0u8; 16];
            out.copy_from_slice(contents);
            return Ok(Self(out));
        }

        // Text form: strip ASCII whitespace and require 32 hex chars.
        let text: String = contents
            .iter()
            .filter(|b| !b.is_ascii_whitespace())
            .map(|b| *b as char)
            .collect();
        if text.len() == 32 && text.chars().all(|c| c.is_ascii_hexdigit()) {
            let mut out = [0u8; 16];
            for (i, chunk) in text.as_bytes().chunks(2).enumerate() {
                let hi = hex_val(chunk[0]);
                let lo = hex_val(chunk[1]);
                out[i] = (hi << 4) | lo;
            }
            return Ok(Self(out));
        }
        Err(WupError::DiscKeyMalformed(format!(
            "expected 16 raw bytes or 32 hex chars, got {} bytes",
            contents.len()
        )))
    }
}

fn hex_val(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

/// Resolve and load a disc key for a given disc file.
///
/// Precedence:
/// 1. Explicit `override_path` when supplied.
/// 2. Sibling `<disc>.key`.
/// 3. Sibling `game.key` in the same directory.
///
/// Returns [`WupError::DiscKeyMissing`] if no candidate exists.
pub fn load_disc_key(disc_path: &Path, override_path: Option<&Path>) -> WupResult<DiscKey> {
    let chosen = resolve_key_path(disc_path, override_path)
        .ok_or_else(|| WupError::DiscKeyMissing(disc_path.to_path_buf()))?;
    let contents = fs::read(&chosen).map_err(|_| WupError::DiscKeyMissing(chosen.clone()))?;
    DiscKey::from_file_contents(&contents)
}

fn resolve_key_path(disc_path: &Path, override_path: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = override_path {
        if p.is_file() {
            return Some(p.to_path_buf());
        }
        return None;
    }

    let mut stem = disc_path.to_path_buf();
    stem.set_extension("key");
    if stem.is_file() {
        return Some(stem);
    }

    if let Some(parent) = disc_path.parent() {
        let game_key = parent.join("game.key");
        if game_key.is_file() {
            return Some(game_key);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

    #[test]
    fn parses_raw_16_bytes() {
        let bytes: Vec<u8> = (0u8..16).collect();
        let key = DiscKey::from_file_contents(&bytes).unwrap();
        assert_eq!(
            key.as_bytes(),
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
        );
    }

    #[test]
    fn parses_lowercase_hex() {
        let hex = b"000102030405060708090a0b0c0d0e0f";
        let key = DiscKey::from_file_contents(hex).unwrap();
        assert_eq!(
            key.as_bytes(),
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
        );
    }

    #[test]
    fn parses_uppercase_hex_with_trailing_newline() {
        let hex = b"AABBCCDDEEFF00112233445566778899\n";
        let key = DiscKey::from_file_contents(hex).unwrap();
        assert_eq!(
            key.as_bytes(),
            &[
                0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,
                0x88, 0x99
            ]
        );
    }

    #[test]
    fn rejects_short_input() {
        let result = DiscKey::from_file_contents(&[0u8; 15]);
        assert!(matches!(result, Err(WupError::DiscKeyMalformed(_))));
    }

    #[test]
    fn rejects_non_hex_ascii() {
        // 32 chars but not valid hex.
        let result = DiscKey::from_file_contents(b"gggggggggggggggggggggggggggggggg");
        assert!(matches!(result, Err(WupError::DiscKeyMalformed(_))));
    }

    #[test]
    fn loader_reads_sibling_key_file() {
        let dir = TempDir::new().unwrap();
        let disc = dir.path().join("game.wud");
        std::fs::write(&disc, [0u8; 64]).unwrap();
        let keyfile = dir.path().join("game.key");
        std::fs::write(&keyfile, [0x77u8; 16]).unwrap();
        let key = load_disc_key(&disc, None).unwrap();
        assert_eq!(key.as_bytes(), &[0x77; 16]);
    }

    #[test]
    fn loader_falls_back_to_game_dot_key() {
        let dir = TempDir::new().unwrap();
        let disc = dir.path().join("mystery.wud");
        std::fs::write(&disc, [0u8; 64]).unwrap();
        let keyfile = dir.path().join("game.key");
        std::fs::write(&keyfile, [0x88u8; 16]).unwrap();
        let key = load_disc_key(&disc, None).unwrap();
        assert_eq!(key.as_bytes(), &[0x88; 16]);
    }

    #[test]
    fn loader_honors_override_when_valid() {
        let dir = TempDir::new().unwrap();
        let disc = dir.path().join("game.wud");
        std::fs::write(&disc, [0u8; 64]).unwrap();
        let mut override_key = NamedTempFile::new().unwrap();
        override_key.write_all(&[0x55u8; 16]).unwrap();
        let key = load_disc_key(&disc, Some(override_key.path())).unwrap();
        assert_eq!(key.as_bytes(), &[0x55; 16]);
    }

    #[test]
    fn loader_errors_when_key_absent() {
        let dir = TempDir::new().unwrap();
        let disc = dir.path().join("game.wud");
        std::fs::write(&disc, [0u8; 64]).unwrap();
        let result = load_disc_key(&disc, None);
        assert!(matches!(result, Err(WupError::DiscKeyMissing(_))));
    }
}
