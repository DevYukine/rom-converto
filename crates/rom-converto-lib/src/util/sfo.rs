//! `\0PSF` (PARAM.SFO) key/value parser, shared by the PS3 disc and PSP
//! UMD metadata readers, and the writer the PS1 EBOOT builder uses.

use crate::util::bytes::{u16_le, u32_le};
use std::collections::BTreeMap;
use std::io;

const MAGIC: &[u8; 4] = b"\0PSF";
const VERSION: u32 = 0x0101;
const HEADER_LEN: usize = 20;
const TABLE_ALIGN: usize = 4;
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
        let key_table_start = u32_le(data, 8) as usize;
        let data_table_start = u32_le(data, 12) as usize;
        let num_entries = u32_le(data, 16);
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
            let key_offset = u16_le(data, base) as usize;
            let data_fmt = u16_le(data, base + 2);
            let data_len = u32_le(data, base + 4) as usize;
            let data_offset = u32_le(data, base + 12) as usize;

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

/// One `\0PSF` entry to write: its key, its value, and the slot the value
/// is padded to, so a later in-place patch has a fixed place to land.
pub(crate) struct SfoEntry<'a> {
    pub key: &'a str,
    pub value: SfoValue,
    pub slot: u32,
}

/// Encodes `entries` as a `\0PSF` blob, keys sorted as firmware expects.
///
/// # Errors
/// Returns `InvalidInput` when a value with its terminator does not fit its
/// slot.
pub(crate) fn write(entries: &[SfoEntry<'_>]) -> io::Result<Vec<u8>> {
    let mut sorted: Vec<&SfoEntry<'_>> = entries.iter().collect();
    sorted.sort_by_key(|entry| entry.key);

    let key_table_start = HEADER_LEN + sorted.len() * INDEX_ENTRY_LEN;
    let key_bytes: usize = sorted.iter().map(|entry| entry.key.len() + 1).sum();
    let data_table_start = (key_table_start + key_bytes).next_multiple_of(TABLE_ALIGN);
    let data_bytes: usize = sorted.iter().map(|entry| entry.slot as usize).sum();

    let mut out = vec![0u8; data_table_start + data_bytes];
    out[0..4].copy_from_slice(MAGIC);
    out[4..8].copy_from_slice(&VERSION.to_le_bytes());
    out[8..12].copy_from_slice(&(key_table_start as u32).to_le_bytes());
    out[12..16].copy_from_slice(&(data_table_start as u32).to_le_bytes());
    out[16..20].copy_from_slice(&(sorted.len() as u32).to_le_bytes());

    let (mut key_offset, mut data_offset) = (0usize, 0usize);
    for (i, entry) in sorted.iter().enumerate() {
        let (format, payload) = match &entry.value {
            SfoValue::U32(v) => (FMT_U32, v.to_le_bytes().to_vec()),
            SfoValue::Str(s) => {
                let mut bytes = s.as_bytes().to_vec();
                bytes.push(0);
                (FMT_UTF8, bytes)
            }
        };
        if payload.len() > entry.slot as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "PARAM.SFO {} needs {} bytes, its slot holds {}",
                    entry.key,
                    payload.len(),
                    entry.slot
                ),
            ));
        }
        let index = HEADER_LEN + i * INDEX_ENTRY_LEN;
        out[index..index + 2].copy_from_slice(&(key_offset as u16).to_le_bytes());
        out[index + 2..index + 4].copy_from_slice(&format.to_le_bytes());
        out[index + 4..index + 8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        out[index + 8..index + 12].copy_from_slice(&entry.slot.to_le_bytes());
        out[index + 12..index + 16].copy_from_slice(&(data_offset as u32).to_le_bytes());

        let key_at = key_table_start + key_offset;
        out[key_at..key_at + entry.key.len()].copy_from_slice(entry.key.as_bytes());
        let data_at = data_table_start + data_offset;
        out[data_at..data_at + payload.len()].copy_from_slice(&payload);

        key_offset += entry.key.len() + 1;
        data_offset += entry.slot as usize;
    }
    Ok(out)
}

fn invalid(msg: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
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

    /// Builds a minimal `\0PSF` blob holding `entries`, each value in a
    /// slot of exactly its own size.
    pub fn build_sfo(entries: &[(&str, Val)]) -> Vec<u8> {
        let entries: Vec<SfoEntry<'_>> = entries
            .iter()
            .map(|(key, value)| match value {
                Val::Str(s) => SfoEntry {
                    key,
                    value: SfoValue::Str((*s).to_string()),
                    slot: s.len() as u32 + 1,
                },
                Val::U32(n) => SfoEntry {
                    key,
                    value: SfoValue::U32(*n),
                    slot: 4,
                },
            })
            .collect();
        write(&entries).expect("fixture values fit their slots")
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
    fn write_sorts_keys_and_pads_values_to_their_slots() {
        let blob = write(&[
            SfoEntry {
                key: "TITLE",
                value: SfoValue::Str("Game".to_string()),
                slot: 128,
            },
            SfoEntry {
                key: "BOOTABLE",
                value: SfoValue::U32(1),
                slot: 4,
            },
        ])
        .unwrap();
        // Header, two index entries, "BOOTABLE\0TITLE\0" aligned to 4, slots.
        let data_table_start = (HEADER_LEN + 2 * INDEX_ENTRY_LEN + 15).next_multiple_of(4);
        assert_eq!(blob.len(), data_table_start + 4 + 128);
        assert_eq!(u32_le(&blob, 12) as usize, data_table_start);
        // BOOTABLE sorts first, so TITLE's data sits after its 4-byte slot.
        assert_eq!(u32_le(&blob, HEADER_LEN + INDEX_ENTRY_LEN + 12), 4);
        assert_eq!(u32_le(&blob, HEADER_LEN + INDEX_ENTRY_LEN + 8), 128);

        let sfo = Sfo::parse(&blob).unwrap();
        assert_eq!(sfo.get_u32("BOOTABLE"), Some(1));
        assert_eq!(sfo.get_str("TITLE"), Some("Game"));
    }

    #[test]
    fn write_rejects_a_value_longer_than_its_slot() {
        let err = write(&[SfoEntry {
            key: "CATEGORY",
            value: SfoValue::Str("ABCD".to_string()),
            slot: 4,
        }])
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
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
