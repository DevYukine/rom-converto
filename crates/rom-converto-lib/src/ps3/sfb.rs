//! `.SFB` (PS3_DISC.SFB) key/value parser. All values are strings.

use std::collections::BTreeMap;

use crate::ps3::error::{Ps3Error, Ps3Result};

const MAGIC: &[u8; 4] = b".SFB";
const HEADER_LEN: usize = 0x20;
const ENTRY_LEN: usize = 0x20;
const KEY_LEN: usize = 0x10;
const MAX_ENTRIES: usize = 256;
const MAX_VALUE_LEN: usize = 4096;

pub struct Sfb(BTreeMap<String, String>);

impl Sfb {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    pub fn parse(data: &[u8]) -> Ps3Result<Sfb> {
        if data.len() < HEADER_LEN || &data[0..4] != MAGIC {
            return Err(Ps3Error::InvalidSfb("missing .SFB header".into()));
        }
        let mut map = BTreeMap::new();
        let mut off = HEADER_LEN;
        for _ in 0..MAX_ENTRIES {
            let Some(entry) = data.get(off..off + ENTRY_LEN) else {
                break;
            };
            let key_raw = &entry[0..KEY_LEN];
            let key_end = key_raw.iter().position(|&b| b == 0).unwrap_or(KEY_LEN);
            if key_end == 0 {
                break;
            }
            let key = String::from_utf8_lossy(&key_raw[..key_end]).into_owned();
            let value_off =
                u32::from_be_bytes(entry[0x10..0x14].try_into().expect("4-byte slice")) as usize;
            let value_len =
                u32::from_be_bytes(entry[0x14..0x18].try_into().expect("4-byte slice")) as usize;
            let capped = value_len.min(MAX_VALUE_LEN);
            if let Some(raw) = data.get(value_off..value_off.saturating_add(capped)) {
                let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
                map.insert(key, String::from_utf8_lossy(&raw[..end]).into_owned());
            }
            off += ENTRY_LEN;
        }
        Ok(Sfb(map))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_sfb(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut data_table = Vec::new();
        let mut meta = Vec::new();
        for (_, v) in entries {
            let offset = (HEADER_LEN + entries.len() * ENTRY_LEN + data_table.len()) as u32;
            let mut b = v.as_bytes().to_vec();
            b.push(0);
            meta.push((offset, b.len() as u32));
            data_table.extend_from_slice(&b);
        }

        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.resize(HEADER_LEN, 0);
        for ((k, _), (offset, len)) in entries.iter().zip(meta.iter()) {
            let mut entry = vec![0u8; ENTRY_LEN];
            let kb = k.as_bytes();
            entry[..kb.len().min(KEY_LEN)].copy_from_slice(&kb[..kb.len().min(KEY_LEN)]);
            entry[0x10..0x14].copy_from_slice(&offset.to_be_bytes());
            entry[0x14..0x18].copy_from_slice(&len.to_be_bytes());
            out.extend_from_slice(&entry);
        }
        out.extend_from_slice(&data_table);
        out
    }

    #[test]
    fn parses_version_and_title_id() {
        let sfb = Sfb::parse(&build_sfb(&[
            ("TITLE_ID", "BLES02247"),
            ("VERSION", "01.00"),
        ]))
        .unwrap();
        assert_eq!(sfb.get("VERSION"), Some("01.00"));
        assert_eq!(sfb.get("TITLE_ID"), Some("BLES02247"));
        assert_eq!(sfb.get("MISSING"), None);
    }

    #[test]
    fn rejects_bad_magic() {
        assert!(Sfb::parse(&vec![0u8; HEADER_LEN]).is_err());
    }

    #[test]
    fn truncated_input_does_not_panic() {
        let full = build_sfb(&[("VERSION", "01.00")]);
        for len in 0..full.len() {
            let _ = Sfb::parse(&full[..len]);
        }
    }
}
