//! `\0PSF` (PARAM.SFO) key/value parser, shared by the PS3 disc and PSP
//! UMD metadata readers.

use std::collections::BTreeMap;
use std::io;

const MAGIC: &[u8; 4] = b"\0PSF";
const HEADER_LEN: usize = 20;
const INDEX_ENTRY_LEN: usize = 16;
const MAX_ENTRIES: u32 = 1024;
const MAX_VALUE_LEN: usize = 64 * 1024;

const FMT_UTF8_SPECIAL: u16 = 0x0004;
const FMT_UTF8: u16 = 0x0204;
const FMT_U32: u16 = 0x0404;

/// One `\0PSF` entry value; only the UTF-8 and `u32` formats are kept.
#[derive(Debug, Clone)]
pub enum SfoValue {
    Str(String),
    U32(u32),
}

/// A parsed `\0PSF` key/value table.
pub struct Sfo(BTreeMap<String, SfoValue>);

impl Sfo {
    /// Returns the string value for `key`, or `None` when it is absent or
    /// holds another type.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        match self.0.get(key) {
            Some(SfoValue::Str(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Returns the `u32` value for `key`, or `None` when it is absent or
    /// holds another type.
    pub fn get_u32(&self, key: &str) -> Option<u32> {
        match self.0.get(key) {
            Some(SfoValue::U32(v)) => Some(*v),
            _ => None,
        }
    }

    /// Parses a `\0PSF` blob. Malformed individual entries are skipped;
    /// only a bad header or an oversized index table errors.
    pub fn parse(data: &[u8]) -> io::Result<Sfo> {
        if data.len() < HEADER_LEN || &data[0..4] != MAGIC {
            return Err(invalid("missing \\0PSF header"));
        }
        let key_table_start = read_u32(data, 8) as usize;
        let data_table_start = read_u32(data, 12) as usize;
        let num_entries = read_u32(data, 16);
        if num_entries > MAX_ENTRIES {
            return Err(invalid(format!(
                "entry count {num_entries} exceeds cap {MAX_ENTRIES}"
            )));
        }
        let index_end = HEADER_LEN + num_entries as usize * INDEX_ENTRY_LEN;
        if index_end > data.len() {
            return Err(invalid("index table exceeds input"));
        }

        // Offsets past this point come from the file; every access is
        // bounds-checked and a bad entry is skipped, never fatal.
        let mut map = BTreeMap::new();
        for i in 0..num_entries as usize {
            let base = HEADER_LEN + i * INDEX_ENTRY_LEN;
            let key_offset = read_u16(data, base) as usize;
            let data_fmt = read_u16(data, base + 2);
            let data_len = read_u32(data, base + 4) as usize;
            let data_offset = read_u32(data, base + 12) as usize;

            let Some(key) = read_c_string(data, key_table_start.saturating_add(key_offset)) else {
                continue;
            };
            let value_pos = data_table_start.saturating_add(data_offset);
            let capped = data_len.min(MAX_VALUE_LEN);
            let Some(raw) = data.get(value_pos..value_pos.saturating_add(capped)) else {
                continue;
            };

            let value = match data_fmt {
                FMT_U32 => {
                    if raw.len() < 4 {
                        continue;
                    }
                    SfoValue::U32(u32::from_le_bytes(
                        raw[0..4].try_into().expect("raw[0..4] is always 4 bytes"),
                    ))
                }
                FMT_UTF8 | FMT_UTF8_SPECIAL => {
                    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
                    SfoValue::Str(String::from_utf8_lossy(&raw[..end]).into_owned())
                }
                _ => continue,
            };
            map.insert(key, value);
        }
        Ok(Sfo(map))
    }
}

fn invalid(msg: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

fn read_u16(data: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(data[off..off + 2].try_into().expect("2-byte slice"))
}

fn read_u32(data: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(data[off..off + 4].try_into().expect("4-byte slice"))
}

fn read_c_string(data: &[u8], pos: usize) -> Option<String> {
    let slice = data.get(pos..)?;
    let end = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    Some(String::from_utf8_lossy(&slice[..end]).into_owned())
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    use super::*;

    pub enum Val {
        Str(&'static str),
        U32(u32),
    }

    /// Builds a minimal `\0PSF` blob holding `entries`.
    pub fn build_sfo(entries: &[(&str, Val)]) -> Vec<u8> {
        let mut key_table = Vec::new();
        let mut key_offsets = Vec::new();
        for (k, _) in entries {
            key_offsets.push(key_table.len() as u16);
            key_table.extend_from_slice(k.as_bytes());
            key_table.push(0);
        }

        let mut data_table = Vec::new();
        let mut meta = Vec::new();
        for (_, v) in entries {
            let offset = data_table.len() as u32;
            match v {
                Val::Str(s) => {
                    let mut b = s.as_bytes().to_vec();
                    b.push(0);
                    let len = b.len() as u32;
                    data_table.extend_from_slice(&b);
                    meta.push((FMT_UTF8, len, offset));
                }
                Val::U32(n) => {
                    data_table.extend_from_slice(&n.to_le_bytes());
                    meta.push((FMT_U32, 4, offset));
                }
            }
        }

        let num = entries.len() as u32;
        let key_table_start = HEADER_LEN as u32 + num * INDEX_ENTRY_LEN as u32;
        let data_table_start = key_table_start + key_table.len() as u32;

        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&0x0101u32.to_le_bytes());
        out.extend_from_slice(&key_table_start.to_le_bytes());
        out.extend_from_slice(&data_table_start.to_le_bytes());
        out.extend_from_slice(&num.to_le_bytes());
        for (i, (fmt, len, offset)) in meta.iter().enumerate() {
            out.extend_from_slice(&key_offsets[i].to_le_bytes());
            out.extend_from_slice(&fmt.to_le_bytes());
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&offset.to_le_bytes());
        }
        out.extend_from_slice(&key_table);
        out.extend_from_slice(&data_table);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::test_fixtures::{Val, build_sfo};
    use super::*;

    #[test]
    fn parses_strings_and_u32() {
        let sfo = Sfo::parse(&build_sfo(&[
            ("TITLE", Val::Str("Persona 5")),
            ("TITLE_ID", Val::Str("BLES02247")),
            ("PARENTAL_LEVEL", Val::U32(5)),
        ]))
        .unwrap();
        assert_eq!(sfo.get_str("TITLE"), Some("Persona 5"));
        assert_eq!(sfo.get_str("TITLE_ID"), Some("BLES02247"));
        assert_eq!(sfo.get_u32("PARENTAL_LEVEL"), Some(5));
        assert_eq!(sfo.get_str("PARENTAL_LEVEL"), None);
        assert_eq!(sfo.get_u32("TITLE"), None);
        assert_eq!(sfo.get_str("MISSING"), None);
    }

    #[test]
    fn rejects_bad_magic() {
        assert!(Sfo::parse(b"XXXX and more bytes here").is_err());
    }

    #[test]
    fn truncated_input_errors_without_panic() {
        let full = build_sfo(&[("TITLE", Val::Str("X"))]);
        for len in 0..full.len() {
            let _ = Sfo::parse(&full[..len]);
        }
    }

    #[test]
    fn hostile_offsets_do_not_panic() {
        let mut blob = build_sfo(&[("TITLE", Val::Str("X"))]);
        // Corrupt the key/data table starts to point far out of bounds.
        blob[8..12].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        blob[12..16].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        let sfo = Sfo::parse(&blob).unwrap();
        assert_eq!(sfo.get_str("TITLE"), None);
    }
}
