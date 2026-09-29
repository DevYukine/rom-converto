//! XDBF (SPA) resource parsing, big-endian: the title name string table and
//! the 64x64 PNG icon.

use super::{read_u16, read_u32, read_u64};
use crate::info::Image;
use crate::util::extent_end;

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

/// Real SPA tables hold at most a few thousand entries; the cap only stops
/// hostile counts from growing the Vec without limit through zero-filled
/// reads.
const MAX_XDBF_ENTRIES: usize = 1 << 20;
/// Icon payloads are capped at 16 MiB: the largest real SPA icons are a
/// few hundred KiB, so the cap only stops hostile lengths. Entries
/// declaring more are skipped instead of read.
const MAX_ICON_BYTES: usize = 16 << 20;

struct Entry {
    namespace: u16,
    id: u64,
    table_index: usize,
    length: u32,
}

#[derive(Default)]
pub(crate) struct XdbfMeta {
    pub title_name: Option<String>,
    pub icon: Option<Image>,
}

/// Parses XDBF entries in data-offset order so seekable resource readers can
/// inspect all payloads in one forward pass. Declared counts and payload
/// sizes are capped so hostile values cannot drive large allocations.
pub(crate) fn parse_xdbf_ranges(
    resource_size: u64,
    mut read_range: impl FnMut(u64, usize) -> Option<Vec<u8>>,
) -> XdbfMeta {
    let mut meta = XdbfMeta::default();
    let Some(header) = read_range(0, HEADER_LEN) else {
        return meta;
    };
    if header.get(0..4) != Some(MAGIC.as_slice()) {
        return meta;
    }
    let Some(table_length) = read_u32(&header, ENTRY_TABLE_LENGTH).map(|n| n as usize) else {
        return meta;
    };
    let Some(count) = read_u32(&header, ENTRY_COUNT).map(|n| n as usize) else {
        return meta;
    };
    let Some(free_length) = read_u32(&header, FREE_SPACE_TABLE_LENGTH).map(|n| n as usize) else {
        return meta;
    };
    let Some(entry_table_end) = (count as u64)
        .checked_mul(ENTRY_LEN as u64)
        .and_then(|length| (HEADER_LEN as u64).checked_add(length))
    else {
        return meta;
    };
    if count > table_length || entry_table_end > resource_size {
        return meta;
    }
    let Some(data_base) = table_length
        .checked_mul(ENTRY_LEN)
        .and_then(|length| HEADER_LEN.checked_add(length))
        .and_then(|base| free_length.checked_mul(FREE_ENTRY_LEN)?.checked_add(base))
    else {
        return meta;
    };
    if data_base as u64 > resource_size {
        return meta;
    }

    // The declared count never drives the reservation: it is bounded by
    // the logical resource extent and an absolute cap, and the Vec grows
    // per successfully read batch.
    let count = count
        .min(
            (resource_size.saturating_sub(HEADER_LEN as u64) / ENTRY_LEN as u64).min(count as u64)
                as usize,
        )
        .min(MAX_XDBF_ENTRIES);

    let mut entries = Vec::new();
    let mut index = 0usize;
    while index < count {
        let batch_count = (count - index).min(256);
        let Some(entry_offset) = index.checked_mul(ENTRY_LEN) else {
            break;
        };
        let Some(byte_offset) = HEADER_LEN.checked_add(entry_offset) else {
            break;
        };
        let Some(byte_len) = batch_count.checked_mul(ENTRY_LEN) else {
            break;
        };
        let Some(table_bytes) = read_range(byte_offset as u64, byte_len) else {
            break;
        };
        for (within_batch, entry) in table_bytes.as_chunks::<ENTRY_LEN>().0.iter().enumerate() {
            let (Some(namespace), Some(id), Some(offset), Some(length)) = (
                read_u16(entry, 0),
                read_u64(entry, 2),
                read_u32(entry, 10),
                read_u32(entry, 14),
            ) else {
                continue;
            };
            let Some(start) = (data_base as u64).checked_add(u64::from(offset)) else {
                continue;
            };
            if start
                .checked_add(u64::from(length))
                .is_none_or(|end| end > resource_size)
            {
                continue;
            }
            entries.push((
                start,
                Entry {
                    namespace,
                    id,
                    table_index: index + within_batch,
                    length,
                },
            ));
        }
        index += batch_count;
    }
    entries.sort_unstable_by_key(|(start, _)| *start);

    let mut title_name: Option<(usize, String)> = None;
    let mut icon: Option<(usize, Image)> = None;
    for (start, entry) in entries {
        let needs_icon_header =
            entry.namespace == NAMESPACE_IMAGE && entry.id == ICON_ID && entry.length >= 24;
        let header_len = if needs_icon_header {
            24
        } else {
            entry.length.min(4) as usize
        };
        let header = read_range(start, header_len);
        if entry.length >= 4
            && header
                .as_deref()
                .is_some_and(|bytes| bytes.get(..4) == Some(XSTR_MAGIC.as_slice()))
            && let Some(title) = xstr_title_ranges(entry.length as usize, start, &mut read_range)
            && title_name
                .as_ref()
                .is_none_or(|(table_index, _)| entry.table_index < *table_index)
        {
            title_name = Some((entry.table_index, title));
        }

        if entry.namespace == NAMESPACE_IMAGE
            && entry.id == ICON_ID
            && entry.length >= 24
            && entry.length <= MAX_ICON_BYTES as u32
            && header
                .as_ref()
                .and_then(|bytes| Image::from_png(bytes.clone()))
                .is_some()
                // The payload read stays inside the declared resource
                // extent; the entry length is already capped above.
            && let Some(data) = read_range(
                start,
                (entry.length as u64).min(resource_size.saturating_sub(start)) as usize,
            )
            && let Some(image) = Image::from_png(data)
            && icon
                .as_ref()
                .is_none_or(|(table_index, _)| entry.table_index < *table_index)
        {
            icon = Some((entry.table_index, image));
        }
    }
    meta.title_name = title_name.map(|(_, title)| title);
    meta.icon = icon.map(|(_, image)| image);
    meta
}

fn xstr_title_ranges(
    length: usize,
    base: u64,
    read_range: &mut impl FnMut(u64, usize) -> Option<Vec<u8>>,
) -> Option<String> {
    let header = read_range(base, XSTR_RECORDS)?;
    if header.get(0..4)? != XSTR_MAGIC {
        return None;
    }
    let count = read_u16(&header, XSTR_STRING_COUNT)? as usize;
    let mut at = XSTR_RECORDS as u64;
    for _ in 0..count {
        // Stay inside this entry: a truncated table must not read into the next.
        extent_end(at, 4, length as u64)?;
        let record = read_range(base.checked_add(at)?, 4)?;
        let id = read_u16(&record, 0)?;
        let text_len = read_u16(&record, 2)? as usize;
        at = at.checked_add(4)?;
        extent_end(at, text_len as u64, length as u64)?;
        if id == TITLE_STRING_ID {
            let text = read_range(base.checked_add(at)?, text_len)?;
            return Some(
                String::from_utf8_lossy(&text)
                    .trim_end_matches('\0')
                    .to_string(),
            );
        }
        at = at.checked_add(text_len as u64)?;
    }
    None
}

#[cfg(test)]
pub(super) fn build_xdbf(title: &str, png: &[u8]) -> Vec<u8> {
    let mut xstr = XSTR_MAGIC.to_vec();
    xstr.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    xstr.extend_from_slice(&0u32.to_be_bytes());
    xstr.extend_from_slice(&2u16.to_be_bytes());
    for (id, text) in [(0x0001u16, "Publisher"), (TITLE_STRING_ID, title)] {
        xstr.extend_from_slice(&id.to_be_bytes());
        xstr.extend_from_slice(&(text.len() as u16).to_be_bytes());
        xstr.extend_from_slice(text.as_bytes());
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

    fn ranged(bytes: &[u8]) -> XdbfMeta {
        parse_xdbf_ranges(bytes.len() as u64, |offset, length| {
            bytes
                .get(offset as usize..(offset as usize).checked_add(length)?)
                .map(<[u8]>::to_vec)
        })
    }

    #[test]
    fn title_and_icon_are_extracted() {
        let png = build_png(64, 64);
        let meta = ranged(&build_xdbf("Halo 3", &png));
        assert_eq!(meta.title_name.as_deref(), Some("Halo 3"));
        let icon = meta.icon.expect("icon entry is present");
        assert_eq!((icon.width, icon.height), (64, 64));
        assert_eq!(icon.png_bytes, png);
    }
    #[test]
    fn icon_payload_larger_than_four_mib_is_retained() {
        let mut png = build_png(1, 1);
        png.resize(4 * 1024 * 1024 + 1, 0);
        let meta = ranged(&build_xdbf("Title", &png));
        assert_eq!(meta.icon.unwrap().png_bytes, png);
    }

    #[test]
    fn a_multi_byte_title_decodes_as_utf8() {
        let meta = ranged(&build_xdbf("Pokémon", &build_png(1, 1)));
        assert_eq!(meta.title_name.as_deref(), Some("Pokémon"));
    }

    #[test]
    fn title_is_found_in_namespace_five_too() {
        let mut xdbf = build_xdbf("Halo 3", &build_png(1, 1));
        // Rewrite the string table's namespace from 3 to 5.
        xdbf[HEADER_LEN..HEADER_LEN + 2].copy_from_slice(&5u16.to_be_bytes());
        assert_eq!(ranged(&xdbf).title_name.as_deref(), Some("Halo 3"));
    }

    #[test]
    fn table_order_is_preserved_when_payload_offsets_are_unsorted() {
        let png = build_png(7, 9);
        let mut bytes = build_xdbf("Ordered First", &png);
        let data_base = HEADER_LEN + 2 * ENTRY_LEN;
        let xstr_len = read_u32(&bytes, HEADER_LEN + 14).unwrap() as usize;
        let xstr = bytes[data_base..data_base + xstr_len].to_vec();
        let png_bytes = bytes[data_base + xstr_len..].to_vec();
        bytes.truncate(data_base);
        bytes.extend_from_slice(&png_bytes);
        bytes.extend_from_slice(&xstr);
        bytes[HEADER_LEN + 10..HEADER_LEN + 14]
            .copy_from_slice(&(png_bytes.len() as u32).to_be_bytes());
        bytes[HEADER_LEN + ENTRY_LEN + 10..HEADER_LEN + ENTRY_LEN + 14]
            .copy_from_slice(&0u32.to_be_bytes());

        let meta = ranged(&bytes);
        assert_eq!(meta.title_name.as_deref(), Some("Ordered First"));
        let icon = meta.icon.unwrap();
        assert_eq!((icon.width, icon.height), (7, 9));
    }

    #[test]
    fn forged_entry_count_past_resource_is_rejected_before_reserving() {
        let mut bytes = vec![0u8; HEADER_LEN];
        bytes[0..4].copy_from_slice(MAGIC);
        bytes[ENTRY_TABLE_LENGTH..ENTRY_TABLE_LENGTH + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        bytes[ENTRY_COUNT..ENTRY_COUNT + 4].copy_from_slice(&u32::MAX.to_be_bytes());

        let meta = ranged(&bytes);
        assert!(meta.title_name.is_none());
        assert!(meta.icon.is_none());
    }

    #[test]
    fn hostile_entry_count_is_capped_before_reserving() {
        // Real SPA tables hold a few thousand entries at most, so a
        // declared count of ~134M inside a 3 GiB resource must be capped
        // at MAX_XDBF_ENTRIES and the Vec grown per batch instead of
        // reserving gigabytes up front.
        let mut header = [0u8; HEADER_LEN];
        header[0..4].copy_from_slice(MAGIC);
        header[ENTRY_TABLE_LENGTH..ENTRY_TABLE_LENGTH + 4]
            .copy_from_slice(&(u32::MAX / 32).to_be_bytes());
        header[ENTRY_COUNT..ENTRY_COUNT + 4].copy_from_slice(&(u32::MAX / 32).to_be_bytes());
        let mut largest_read = 0usize;
        let meta = parse_xdbf_ranges(3 * 1024 * 1024 * 1024, |offset, length| {
            largest_read = largest_read.max(length);
            let at = offset as usize;
            let mut out = vec![0u8; length];
            if at < header.len() {
                let copied = header.len().saturating_sub(at).min(length);
                out[..copied].copy_from_slice(&header[at..at + copied]);
            }
            Some(out)
        });
        assert!(meta.title_name.is_none());
        assert!(meta.icon.is_none());
        assert!(largest_read < 1024 * 1024);
    }

    #[test]
    fn hostile_icon_payload_is_capped_before_reserving() {
        // A 24-byte PNG header must not gate a multi-gigabyte payload
        // allocation: the read is capped at MAX_ICON_BYTES no matter what
        // the entry declares.
        let png = build_png(64, 64);
        let mut bytes = build_xdbf("Capped", &png);
        let entry_len_at = HEADER_LEN + ENTRY_LEN + 14;
        let declared_len = 3u32 * 1024 * 1024 * 1024 / 2;
        bytes[entry_len_at..entry_len_at + 4].copy_from_slice(&declared_len.to_be_bytes());
        let mut largest_read = 0usize;
        let meta = parse_xdbf_ranges(
            bytes.len() as u64 + u64::from(declared_len),
            |offset, length| {
                largest_read = largest_read.max(length);
                let at = offset as usize;
                let mut out = vec![0u8; length];
                if at < bytes.len() {
                    let copied = bytes.len().saturating_sub(at).min(length);
                    out[..copied].copy_from_slice(&bytes[at..at + copied]);
                }
                Some(out)
            },
        );
        // Entries declaring past the cap are skipped outright: the stored
        // PNG bytes are never even read, let alone a 1.5 GiB payload.
        assert!(meta.icon.is_none());
        assert!(largest_read <= MAX_ICON_BYTES);
    }
    #[test]
    fn first_table_title_wins_and_only_icon_id_0x8000_is_selected() {
        fn xstr(title: &str) -> Vec<u8> {
            let mut bytes = XSTR_MAGIC.to_vec();
            bytes.extend_from_slice(&0x0001_0000u32.to_be_bytes());
            bytes.extend_from_slice(&0u32.to_be_bytes());
            bytes.extend_from_slice(&1u16.to_be_bytes());
            bytes.extend_from_slice(&TITLE_STRING_ID.to_be_bytes());
            bytes.extend_from_slice(&(title.len() as u16).to_be_bytes());
            bytes.extend_from_slice(title.as_bytes());
            bytes
        }

        let first_title = xstr("First table title");
        let second_title = xstr("Second physical title");
        let wrong_icon = build_png(1, 1);
        let title_icon = build_png(7, 9);
        let data_base = HEADER_LEN + 4 * ENTRY_LEN;
        let records = [
            (3u16, 1u64, first_title.as_slice()),
            (NAMESPACE_IMAGE, 1, wrong_icon.as_slice()),
            (3, 2, second_title.as_slice()),
            (NAMESPACE_IMAGE, ICON_ID, title_icon.as_slice()),
        ];
        let offsets = [
            second_title.len() as u32 + wrong_icon.len() as u32,
            second_title.len() as u32,
            0,
            (second_title.len() + wrong_icon.len() + first_title.len()) as u32,
        ];

        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        bytes.extend_from_slice(&(records.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&(records.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        for (index, (namespace, id, payload)) in records.iter().enumerate() {
            bytes.extend_from_slice(&namespace.to_be_bytes());
            bytes.extend_from_slice(&id.to_be_bytes());
            bytes.extend_from_slice(&offsets[index].to_be_bytes());
            bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        }
        assert_eq!(bytes.len(), data_base);
        bytes.extend_from_slice(&second_title);
        bytes.extend_from_slice(&wrong_icon);
        bytes.extend_from_slice(&first_title);
        bytes.extend_from_slice(&title_icon);

        let meta = ranged(&bytes);
        assert_eq!(meta.title_name.as_deref(), Some("First table title"));
        let icon = meta.icon.unwrap();
        assert_eq!((icon.width, icon.height), (7, 9));
    }

    #[test]
    fn oversized_entry_count_is_rejected_before_table_read() {
        let mut header = build_xdbf("unused", &build_png(1, 1));
        header.resize(HEADER_LEN, 0);
        header[ENTRY_TABLE_LENGTH..ENTRY_TABLE_LENGTH + 4]
            .copy_from_slice(&100_000u32.to_be_bytes());
        header[ENTRY_COUNT..ENTRY_COUNT + 4].copy_from_slice(&100_000u32.to_be_bytes());
        let mut largest_read = 0usize;
        let meta = parse_xdbf_ranges(HEADER_LEN as u64, |offset, length| {
            largest_read = largest_read.max(length);
            header
                .get(offset as usize..offset as usize + length)
                .map(<[u8]>::to_vec)
        });
        assert!(meta.title_name.is_none());
        assert!(meta.icon.is_none());
        assert_eq!(largest_read, HEADER_LEN);
    }

    #[test]
    fn one_by_one_png_dimensions() {
        let meta = ranged(&build_xdbf("Tiny", &build_png(1, 1)));
        let icon = meta.icon.expect("icon entry is present");
        assert_eq!((icon.width, icon.height), (1, 1));
    }

    #[test]
    fn ranged_parser_reads_only_tables_and_metadata_payloads() {
        let png = build_png(8, 8);
        let mut bytes = build_xdbf("Range Title", &png);
        bytes.resize(1024 * 1024, 0);
        let mut largest_read = 0;
        let mut previous_offset = 0;
        let meta = parse_xdbf_ranges(bytes.len() as u64, |offset, length| {
            assert!(
                offset >= previous_offset,
                "resource reads must be forward ordered"
            );
            previous_offset = offset;
            largest_read = largest_read.max(length);
            Some(
                bytes
                    .get(offset as usize..offset as usize + length)?
                    .to_vec(),
            )
        });
        assert_eq!(meta.title_name.as_deref(), Some("Range Title"));
        assert_eq!(meta.icon.unwrap().png_bytes, png);
        assert!(largest_read < bytes.len());
    }

    #[test]
    fn truncated_entry_table_yields_nothing() {
        let full = build_xdbf("Halo 3", &build_png(64, 64));
        let meta = ranged(&full[..HEADER_LEN + ENTRY_LEN]);
        assert!(meta.title_name.is_none());
        assert!(meta.icon.is_none());
    }

    #[test]
    fn bad_magic_yields_nothing() {
        let mut xdbf = build_xdbf("Halo 3", &build_png(64, 64));
        xdbf[0] = b'Y';
        let meta = ranged(&xdbf);
        assert!(meta.title_name.is_none());
        assert!(meta.icon.is_none());
    }

    #[test]
    fn non_png_icon_payload_is_rejected() {
        let meta = ranged(&build_xdbf("Halo 3", b"not a png at all"));
        assert_eq!(meta.title_name.as_deref(), Some("Halo 3"));
        assert!(meta.icon.is_none());
    }
}
