//! `prod.keys` / `title.keys` parser. Format is hactool's: ASCII
//! `name = hex` lines, `#` comments and blank lines ignored. NCZ
//! decompression alone could re-encrypt sections from the keys cached
//! in `NCZSECTN`, but every operation here demands a keyfile so the
//! tool stays gated behind console-key ownership.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::nintendo::nx::error::{NxError, NxResult};

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum KeyAreaKind {
    Application,
    Ocean,
    System,
}

impl KeyAreaKind {
    fn name(self) -> &'static str {
        match self {
            KeyAreaKind::Application => "application",
            KeyAreaKind::Ocean => "ocean",
            KeyAreaKind::System => "system",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "application" => Some(KeyAreaKind::Application),
            "ocean" => Some(KeyAreaKind::Ocean),
            "system" => Some(KeyAreaKind::System),
            _ => None,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct KeySet {
    pub header_key: Option<[u8; 32]>,
    pub master_keys: HashMap<u8, [u8; 16]>,
    pub key_area_keys: HashMap<(KeyAreaKind, u8), [u8; 16]>,
    pub titlekeks: HashMap<u8, [u8; 16]>,
    pub title_keys: HashMap<[u8; 16], [u8; 16]>,
}

impl KeySet {
    pub fn header_key(&self) -> NxResult<&[u8; 32]> {
        self.header_key.as_ref().ok_or_else(|| NxError::MissingKey {
            name: "header_key".into(),
        })
    }

    pub fn key_area_key(&self, kind: KeyAreaKind, idx: u8) -> NxResult<&[u8; 16]> {
        self.key_area_keys
            .get(&(kind, idx))
            .ok_or_else(|| NxError::MissingKey {
                name: format!("key_area_key_{}_{:02x}", kind.name(), idx),
            })
    }

    pub fn master_key(&self, idx: u8) -> NxResult<&[u8; 16]> {
        self.master_keys
            .get(&idx)
            .ok_or_else(|| NxError::MissingKey {
                name: format!("master_key_{:02x}", idx),
            })
    }

    pub fn titlekek(&self, idx: u8) -> NxResult<&[u8; 16]> {
        self.titlekeks.get(&idx).ok_or_else(|| NxError::MissingKey {
            name: format!("titlekek_{:02x}", idx),
        })
    }

    pub fn title_key(&self, rights_id: &[u8; 16]) -> NxResult<&[u8; 16]> {
        self.title_keys
            .get(rights_id)
            .ok_or_else(|| NxError::MissingKey {
                name: format!("title_key for rights_id {}", hex::encode(rights_id)),
            })
    }

    /// Merge a `title.keys` file: each non-comment line is
    /// `<rights_id_hex> = <encrypted_title_key_hex>`, both 16 bytes.
    pub fn merge_title_keys(&mut self, path: &Path) -> NxResult<()> {
        let content = fs::read_to_string(path)?;
        for raw in content.lines() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            let (left, right) = match line.split_once('=') {
                Some((l, r)) => (l.trim(), r.trim()),
                None => return Err(NxError::KeyfileParse { line: raw.into() }),
            };
            let rights_id = decode_fixed::<16>("rights_id", left)?;
            let title_key = decode_fixed::<16>("title_key", right)?;
            self.title_keys.insert(rights_id, title_key);
        }
        Ok(())
    }
}

/// On miss, returns `KeyfileMissing` with every path that was tried so
/// the error message tells the user exactly where to drop the file.
/// When `explicit` is provided only that path is honored: matching nsz,
/// the fallback search is skipped so a typo'd `--keys` flag is loud
/// instead of silently resolving to a different file.
pub fn load_keyset(explicit: Option<&Path>) -> NxResult<KeySet> {
    let candidates = match explicit {
        Some(p) => vec![p.to_path_buf()],
        None => default_candidate_paths(),
    };
    let path = candidates
        .iter()
        .find(|p| p.is_file())
        .cloned()
        .ok_or_else(|| NxError::KeyfileMissing(candidates.clone()))?;
    let content = fs::read_to_string(&path)?;
    let mut keyset = KeySet::default();
    for raw in content.lines() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = match line.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => return Err(NxError::KeyfileParse { line: raw.into() }),
        };
        ingest(&mut keyset, key, value)?;
    }
    Ok(keyset)
}

fn default_candidate_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(".switch").join("prod.keys"));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        out.push(PathBuf::from(profile).join(".switch").join("prod.keys"));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        out.push(dir.join("prod.keys"));
    }
    out
}

fn ingest(set: &mut KeySet, key: &str, value: &str) -> NxResult<()> {
    if key == "header_key" {
        set.header_key = Some(decode_fixed::<32>(key, value)?);
        return Ok(());
    }
    if let Some(rest) = key.strip_prefix("master_key_") {
        if let Some(idx) = try_parse_index(rest) {
            set.master_keys.insert(idx, decode_fixed::<16>(key, value)?);
        }
        return Ok(());
    }
    if let Some(rest) = key.strip_prefix("titlekek_") {
        if let Some(idx) = try_parse_index(rest) {
            set.titlekeks.insert(idx, decode_fixed::<16>(key, value)?);
        }
        return Ok(());
    }
    if let Some(rest) = key.strip_prefix("key_area_key_")
        && let Some((kind_str, idx_str)) = rest.rsplit_once('_')
        && let Some(kind) = KeyAreaKind::from_name(kind_str)
        && let Some(idx) = try_parse_index(idx_str)
    {
        set.key_area_keys
            .insert((kind, idx), decode_fixed::<16>(key, value)?);
        return Ok(());
    }
    // Unknown keys (eticket_rsa_kek, RSA components, key_area_key_*_source,
    // future suffixes) silently passthrough; only the subset of entries
    // needed for NCA crypto matters here. Real prod.keys files carry many
    // entries that go unused.
    Ok(())
}

fn try_parse_index(hex_idx: &str) -> Option<u8> {
    u8::from_str_radix(hex_idx, 16).ok()
}

fn decode_fixed<const N: usize>(name: &str, value: &str) -> NxResult<[u8; N]> {
    let bytes = hex::decode(value).map_err(|_| NxError::InvalidKeyHex {
        name: name.into(),
        value: value.into(),
    })?;
    if bytes.len() != N {
        return Err(NxError::InvalidKeyHex {
            name: name.into(),
            value: value.into(),
        });
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(i) => &line[..i],
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_keys(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    #[test]
    fn parses_basic_keys() {
        let f = write_keys(
            "header_key = 00112233445566778899AABBCCDDEEFF112233445566778899AABBCCDDEEFF00\n\
             master_key_00 = 00010203040506070809000102030405\n\
             key_area_key_application_00 = AABBCCDDEEFF00112233445566778899\n\
             # comment\n",
        );
        let set = load_keyset(Some(f.path())).unwrap();
        assert_eq!(set.header_key.unwrap()[0], 0x00);
        assert_eq!(set.header_key.unwrap()[1], 0x11);
        assert_eq!(set.master_key(0).unwrap()[0], 0x00);
        assert_eq!(set.master_key(0).unwrap()[15], 0x05);
        assert_eq!(
            set.key_area_key(KeyAreaKind::Application, 0).unwrap()[0],
            0xAA
        );
    }

    #[test]
    fn rejects_malformed_lines() {
        let f = write_keys("garbage\n");
        let err = load_keyset(Some(f.path())).unwrap_err();
        assert!(matches!(err, NxError::KeyfileParse { .. }));
    }

    #[test]
    fn rejects_bad_hex_length() {
        let f = write_keys("master_key_00 = 0011\n");
        let err = load_keyset(Some(f.path())).unwrap_err();
        assert!(matches!(err, NxError::InvalidKeyHex { .. }));
    }

    #[test]
    fn missing_index_returns_descriptive_error() {
        let f = write_keys("master_key_00 = 00010203040506070809000102030405\n");
        let set = load_keyset(Some(f.path())).unwrap();
        let err = set.master_key(5).unwrap_err();
        assert!(matches!(err, NxError::MissingKey { name } if name == "master_key_05"));
    }

    #[test]
    fn unknown_keys_ignored() {
        let f = write_keys(
            "header_key = 00112233445566778899AABBCCDDEEFF112233445566778899AABBCCDDEEFF00\n\
             eticket_rsa_kek = 00010203040506070809000102030405\n",
        );
        load_keyset(Some(f.path())).unwrap();
    }

    #[test]
    fn missing_file_lists_candidates() {
        let bogus = PathBuf::from("/definitely/not/here/prod.keys");
        let err = load_keyset(Some(&bogus)).unwrap_err();
        match err {
            NxError::KeyfileMissing(paths) => assert!(paths.contains(&bogus)),
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
