//! XDBF (SPA) resource parsing, big-endian: the title name string table and
//! the 64x64 PNG icon.

use super::{read_u16, read_u32, read_u64};
use crate::info::Image;

const MAGIC: &[u8; 4] = b"XDBF";
const HEADER_LEN: usize = 24;
const ENTRY_LEN: usize = 18;
const FREE_ENTRY_LEN: usize = 8;

const ENTRY_TABLE_LENGTH: usize = 0x08;
const ENTRY_COUNT: usize = 0x0C;
const FREE_SPACE_TABLE_LENGTH: usize = 0x10;

const XSTR_MAGIC: &[u8; 4] = b"XSTR";
const XSTR_STRING_COUNT: usize = 12;
const XSTR_RECORDS: usize = 14;
const TITLE_STRING_ID: u16 = 0x8000;

const NAMESPACE_IMAGE: u16 = 2;
const ICON_ID: u64 = 0x8000;

#[derive(Default)]
pub(crate) struct XdbfMeta {
    pub title_name: Option<String>,
    pub icon: Option<Image>,
}

struct Entry {
    namespace: u16,
    id: u64,
    offset: u32,
    length: u32,
}

fn entry_table(bytes: &[u8]) -> Option<(Vec<Entry>, usize)> {
    if bytes.get(0..4)? != MAGIC {
        return None;
    }
    let table_length = read_u32(bytes, ENTRY_TABLE_LENGTH)? as usize;
    let count = read_u32(bytes, ENTRY_COUNT)? as usize;
    let free_length = read_u32(bytes, FREE_SPACE_TABLE_LENGTH)? as usize;
    if count > table_length {
        return None;
    }
    let data_base = HEADER_LEN
        .checked_add(table_length.checked_mul(ENTRY_LEN)?)?
        .checked_add(free_length.checked_mul(FREE_ENTRY_LEN)?)?;

    let mut entries = Vec::new();
    for i in 0..count {
        let at = HEADER_LEN + i * ENTRY_LEN;
        entries.push(Entry {
            namespace: read_u16(bytes, at)?,
            id: read_u64(bytes, at + 2)?,
            offset: read_u32(bytes, at + 10)?,
            length: read_u32(bytes, at + 14)?,
        });
    }
    Some((entries, data_base))
}

fn entry_data<'a>(bytes: &'a [u8], data_base: usize, entry: &Entry) -> Option<&'a [u8]> {
    let start = data_base.checked_add(entry.offset as usize)?;
    bytes.get(start..start.checked_add(entry.length as usize)?)
}

fn xstr_title(data: &[u8]) -> Option<String> {
    let count = read_u16(data, XSTR_STRING_COUNT)? as usize;
    let mut at = XSTR_RECORDS;
    for _ in 0..count {
        let id = read_u16(data, at)?;
        let len = read_u16(data, at + 2)? as usize;
        let text = data.get(at + 4..at + 4 + len)?;
        if id == TITLE_STRING_ID {
            let units: Vec<u16> = text
                .as_chunks::<2>()
                .0
                .iter()
                .copied()
                .map(u16::from_be_bytes)
                .collect();
            return Some(
                String::from_utf16_lossy(&units)
                    .trim_end_matches('\0')
                    .to_string(),
            );
        }
        at += 4 + len;
    }
    None
}

/// SPA files disagree on whether string tables live in namespace 3 or 5, so
/// entries are matched on the `XSTR` magic instead of the namespace number.
pub(crate) fn parse_xdbf(bytes: &[u8]) -> XdbfMeta {
    let mut meta = XdbfMeta::default();
    let Some((entries, data_base)) = entry_table(bytes) else {
        return meta;
    };

    for entry in entries {
        let Some(data) = entry_data(bytes, data_base, &entry) else {
            continue;
        };
        if meta.title_name.is_none() && data.starts_with(XSTR_MAGIC) {
            meta.title_name = xstr_title(data);
        }
        if meta.icon.is_none() && entry.namespace == NAMESPACE_IMAGE && entry.id == ICON_ID {
            meta.icon = Image::from_png(data.to_vec());
        }
    }
    meta
}

#[cfg(test)]
pub(super) fn build_xdbf(title: &str, png: &[u8]) -> Vec<u8> {
    let mut xstr = XSTR_MAGIC.to_vec();
    xstr.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    xstr.extend_from_slice(&0u32.to_be_bytes());
    xstr.extend_from_slice(&2u16.to_be_bytes());
    for (id, text) in [(0x0001u16, "Publisher"), (TITLE_STRING_ID, title)] {
        let units: Vec<u8> = text
            .encode_utf16()
            .flat_map(|unit| unit.to_be_bytes())
            .collect();
        xstr.extend_from_slice(&id.to_be_bytes());
        xstr.extend_from_slice(&(units.len() as u16).to_be_bytes());
        xstr.extend_from_slice(&units);
    }
    // A string table under namespace 3, matching what SPA files ship.
    let records = [
        (3u16, 1u64, xstr.as_slice()),
        (NAMESPACE_IMAGE, ICON_ID, png),
    ];

    let mut header = MAGIC.to_vec();
    header.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    header.extend_from_slice(&(records.len() as u32).to_be_bytes());
    header.extend_from_slice(&(records.len() as u32).to_be_bytes());
    header.extend_from_slice(&0u32.to_be_bytes());
    header.extend_from_slice(&0u32.to_be_bytes());

    let mut data = Vec::new();
    for (namespace, id, payload) in records {
        header.extend_from_slice(&namespace.to_be_bytes());
        header.extend_from_slice(&id.to_be_bytes());
        header.extend_from_slice(&(data.len() as u32).to_be_bytes());
        header.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        data.extend_from_slice(payload);
    }
    header.extend_from_slice(&data);
    header
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// Only the signature and IHDR are read, so the rest is left minimal.
    pub(in crate::microsoft::xex) fn build_png(width: u32, height: u32) -> Vec<u8> {
        let mut png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&width.to_be_bytes());
        png.extend_from_slice(&height.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        png.extend_from_slice(&0u32.to_be_bytes());
        png
    }

    #[test]
    fn title_and_icon_are_extracted() {
        let png = build_png(64, 64);
        let meta = parse_xdbf(&build_xdbf("Halo 3", &png));
        assert_eq!(meta.title_name.as_deref(), Some("Halo 3"));
        let icon = meta.icon.expect("icon entry is present");
        assert_eq!((icon.width, icon.height), (64, 64));
        assert_eq!(icon.png_bytes, png);
    }

    #[test]
    fn title_is_found_in_namespace_five_too() {
        let mut xdbf = build_xdbf("Halo 3", &build_png(1, 1));
        // Rewrite the string table's namespace from 3 to 5.
        xdbf[HEADER_LEN..HEADER_LEN + 2].copy_from_slice(&5u16.to_be_bytes());
        assert_eq!(parse_xdbf(&xdbf).title_name.as_deref(), Some("Halo 3"));
    }

    #[test]
    fn one_by_one_png_dimensions() {
        let meta = parse_xdbf(&build_xdbf("Tiny", &build_png(1, 1)));
        let icon = meta.icon.expect("icon entry is present");
        assert_eq!((icon.width, icon.height), (1, 1));
    }

    #[test]
    fn truncated_entry_table_yields_nothing() {
        let full = build_xdbf("Halo 3", &build_png(64, 64));
        let meta = parse_xdbf(&full[..HEADER_LEN + ENTRY_LEN]);
        assert!(meta.title_name.is_none());
        assert!(meta.icon.is_none());
    }

    #[test]
    fn bad_magic_yields_nothing() {
        let mut xdbf = build_xdbf("Halo 3", &build_png(64, 64));
        xdbf[0] = b'Y';
        let meta = parse_xdbf(&xdbf);
        assert!(meta.title_name.is_none());
        assert!(meta.icon.is_none());
    }

    #[test]
    fn non_png_icon_payload_is_rejected() {
        let meta = parse_xdbf(&build_xdbf("Halo 3", b"not a png at all"));
        assert_eq!(meta.title_name.as_deref(), Some("Halo 3"));
        assert!(meta.icon.is_none());
    }
}
