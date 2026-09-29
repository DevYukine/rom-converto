//! `default.xbe` header parsing: title metadata carried in the XBE
//! certificate (xboxdevwiki.net/Xbe).

use serde::{Deserialize, Serialize};

use super::xpr::{decode_xpr0_parts, xpr0_layout};
use crate::info::Image;
use crate::microsoft::read_extent_at as read_range;

const MAGIC: &[u8; 4] = b"XBEH";
const BASE_ADDRESS_OFFSET: usize = 0x104;
const CERT_ADDRESS_OFFSET: usize = 0x118;
const SECTION_COUNT_OFFSET: usize = 0x11C;
#[cfg(test)]
const SECTION_HEADERS_ADDRESS_OFFSET: usize = 0x120;

const SECTION_HEADER_SIZE: usize = 0x38;
const SECTION_RAW_ADDRESS: usize = 0x0C;
const SECTION_RAW_SIZE: usize = 0x10;
const SECTION_NAME_ADDRESS: usize = 0x14;
const MAX_SECTIONS: u32 = 256;
const MAX_SECTION_NAME_LEN: usize = 64;

/// Sections a title image can live in, best first.
const ICON_SECTIONS: [&str; 2] = ["$$XTIMAGE", "$$XSIMAGE"];

const CERT_TIMEDATE: usize = 0x04;
const CERT_TITLE_ID: usize = 0x08;
const CERT_TITLE_NAME: usize = 0x0C;
const CERT_TITLE_NAME_UNITS: usize = 40;
const CERT_ALTERNATE_TITLE_IDS: usize = 0x5C;
const CERT_ALTERNATE_TITLE_IDS_COUNT: usize = 16;
const CERT_ALLOWED_MEDIA: usize = 0x9C;
const CERT_REGION: usize = 0xA0;
const CERT_RATINGS: usize = 0xA4;
const CERT_DISC_NUMBER: usize = 0xA8;
const CERT_VERSION: usize = 0xAC;
const CERT_TAIL: usize = CERT_VERSION + 4;

const ALLOWED_MEDIA_FLAGS: &[(u32, &str)] = &[
    (0x1, "HARD_DISK"),
    (0x2, "DVD_X2"),
    (0x4, "DVD_CD"),
    (0x8, "CD"),
    (0x10, "DVD_5_RO"),
    (0x20, "DVD_9_RO"),
    (0x40, "DVD_5_RW"),
    (0x80, "DVD_9_RW"),
    (0x100, "DONGLE"),
    (0x200, "MEDIA_BOARD"),
    (0x40000000, "NONSECURE_HARD_DISK"),
    (0x80000000, "NONSECURE_MODE"),
];

const REGION_FLAGS: &[(u32, &str)] = &[
    (0x1, "North America"),
    (0x2, "Japan"),
    (0x4, "Rest of World"),
    (0x80000000, "Manufacturing"),
];

/// Title metadata parsed from a `default.xbe`'s certificate.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
pub struct XbeInfo {
    pub title_id: u32,
    pub title_id_hex: String,
    pub title_id_code: String,
    pub title_name: String,
    pub alternate_title_ids: Vec<u32>,
    pub allowed_media: u32,
    pub allowed_media_names: Vec<String>,
    pub region: u32,
    pub region_names: Vec<String>,
    pub ratings: u32,
    pub disc_number: u32,
    pub version: u32,
    pub cert_timestamp: u32,
    /// Title image decoded from the XBE's `$$XTIMAGE` section, when it holds
    /// an XPR0 texture in a format the decoder supports.
    #[serde(default)]
    pub icon: Option<Image>,
}

pub(super) fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    bytes
        .get(offset..offset.checked_add(4)?)?
        .try_into()
        .ok()
        .map(u32::from_le_bytes)
}

fn flag_names(value: u32, flags: &[(u32, &str)]) -> Vec<String> {
    flags
        .iter()
        .filter(|(bit, _)| value & bit != 0)
        .map(|(_, name)| name.to_string())
        .collect()
}

fn title_id_code(title_id: u32) -> String {
    let c1 = (title_id >> 24) as u8;
    let c2 = ((title_id >> 16) & 0xFF) as u8;
    if !c1.is_ascii_alphabetic() || !c2.is_ascii_alphabetic() {
        return format!("{title_id:08X}");
    }
    let game_number = title_id & 0xFFFF;
    format!("{}{}-{game_number:03}", c1 as char, c2 as char)
}

fn title_name(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

/// Reads XBE certificate and icon ranges without loading the executable
/// image as a whole.
pub(crate) fn parse_xbe_at<R: std::io::Read + std::io::Seek>(
    reader: &mut R,
    base: u64,
    length: u64,
) -> Option<XbeInfo> {
    let header = read_range(reader, base, length, 0, CERT_ADDRESS_OFFSET + 4)?;
    if header.get(0..4)? != MAGIC {
        return None;
    }
    let base_address = read_u32(&header, BASE_ADDRESS_OFFSET)?;
    let cert_offset = read_u32(&header, CERT_ADDRESS_OFFSET)?.checked_sub(base_address)? as u64;
    let cert = read_range(reader, base, length, cert_offset, CERT_TAIL)?;
    let title_id = read_u32(&cert, CERT_TITLE_ID)?;
    let title_name_bytes =
        cert.get(CERT_TITLE_NAME..CERT_TITLE_NAME + CERT_TITLE_NAME_UNITS * 2)?;
    let alternate_title_ids = (0..CERT_ALTERNATE_TITLE_IDS_COUNT)
        .map(|i| read_u32(&cert, CERT_ALTERNATE_TITLE_IDS + i * 4))
        .collect::<Option<Vec<u32>>>()?
        .into_iter()
        .filter(|&id| id != 0)
        .collect();
    let allowed_media = read_u32(&cert, CERT_ALLOWED_MEDIA)?;
    let region = read_u32(&cert, CERT_REGION)?;
    let ratings = read_u32(&cert, CERT_RATINGS)?;
    let disc_number = read_u32(&cert, CERT_DISC_NUMBER)?;
    let version = read_u32(&cert, CERT_VERSION)?;
    let cert_timestamp = read_u32(&cert, CERT_TIMEDATE)?;

    let icon = (|| {
        let section_fields = read_range(reader, base, length, SECTION_COUNT_OFFSET as u64, 8)?;
        let section_count = read_u32(&section_fields, 0)?;
        if section_count > MAX_SECTIONS {
            return None;
        }
        let section_table = read_u32(&section_fields, 4)?.checked_sub(base_address)? as u64;
        // One batched pass over the header table records the first section
        // matching each icon name; decoding happens best-priority first.
        let mut matches = [None; ICON_SECTIONS.len()];
        let mut index = 0u64;
        while index < u64::from(section_count) {
            let at = section_table.checked_add(index.checked_mul(SECTION_HEADER_SIZE as u64)?)?;
            let batch_count = (u64::from(section_count) - index)
                .min(32)
                .min(length.saturating_sub(at) / SECTION_HEADER_SIZE as u64);
            if batch_count == 0 {
                break;
            }
            let Some(headers) = read_range(
                reader,
                base,
                length,
                at,
                (batch_count * SECTION_HEADER_SIZE as u64) as usize,
            ) else {
                break;
            };
            for (within, header) in headers.chunks_exact(SECTION_HEADER_SIZE).enumerate() {
                let Some(name_address) = read_u32(header, SECTION_NAME_ADDRESS)
                    .and_then(|address| address.checked_sub(base_address))
                else {
                    continue;
                };
                let name_offset = u64::from(name_address);
                let name_len =
                    (length.saturating_sub(name_offset)).min(MAX_SECTION_NAME_LEN as u64) as usize;
                let Some(name) = read_range(reader, base, length, name_offset, name_len) else {
                    continue;
                };
                let Some(name_end) = name.iter().position(|&byte| byte == 0) else {
                    continue;
                };
                let Some(priority) = ICON_SECTIONS
                    .iter()
                    .position(|wanted| &name[..name_end] == wanted.as_bytes())
                else {
                    continue;
                };
                if matches[priority].is_none() {
                    matches[priority] = Some(index + within as u64);
                }
            }
            index += batch_count;
        }
        for section_index in matches.into_iter().flatten() {
            let section_offset = section_table
                .checked_add(section_index.checked_mul(SECTION_HEADER_SIZE as u64)?)?;
            let Some(section) =
                read_range(reader, base, length, section_offset, SECTION_HEADER_SIZE)
            else {
                continue;
            };
            let raw_address = read_u32(&section, SECTION_RAW_ADDRESS)? as u64;
            let raw_size = read_u32(&section, SECTION_RAW_SIZE)? as usize;
            let Some(xpr_header) = read_range(reader, base, length, raw_address, 0x20) else {
                continue;
            };
            let Some(layout) = xpr0_layout(&xpr_header, raw_size) else {
                continue;
            };
            let pixel_offset = raw_address.checked_add(layout.data_offset as u64)?;
            let pixel_end = (layout.data_offset as u64).checked_add(layout.pixels_len as u64)?;
            if pixel_end > raw_size as u64 {
                continue;
            }
            let Some(pixels) = read_range(reader, base, length, pixel_offset, layout.pixels_len)
            else {
                continue;
            };
            if let Some(icon) = decode_xpr0_parts(layout, &pixels) {
                return Some(icon);
            }
        }
        None
    })();

    Some(XbeInfo {
        title_id,
        title_id_hex: format!("{title_id:08X}"),
        title_id_code: title_id_code(title_id),
        title_name: title_name(title_name_bytes),
        alternate_title_ids,
        allowed_media,
        allowed_media_names: flag_names(allowed_media, ALLOWED_MEDIA_FLAGS),
        region,
        region_names: flag_names(region, REGION_FLAGS),
        ratings,
        disc_number,
        version,
        cert_timestamp,
        icon,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::microsoft::xbox::xpr::tests::build_xpr0_dxt1_4x4;
    use std::io::Cursor;

    fn parse(bytes: &[u8]) -> Option<XbeInfo> {
        parse_xbe_at(&mut Cursor::new(bytes), 0, bytes.len() as u64)
    }

    const BASE_ADDRESS: u32 = 0x10000;
    const CERT_OFFSET: usize = 0x180;
    const SECTION_HEADERS_OFFSET: usize = 0x300;
    const SECTION_NAME_OFFSET: usize = 0x400;
    const SECTION_DATA_OFFSET: usize = 0x500;

    fn build_xbe(title_name_units: &[u16]) -> Vec<u8> {
        let mut buf = vec![0u8; 0x1000];
        buf[0..4].copy_from_slice(MAGIC);
        buf[BASE_ADDRESS_OFFSET..BASE_ADDRESS_OFFSET + 4]
            .copy_from_slice(&BASE_ADDRESS.to_le_bytes());
        let cert_rva = BASE_ADDRESS + CERT_OFFSET as u32;
        buf[CERT_ADDRESS_OFFSET..CERT_ADDRESS_OFFSET + 4].copy_from_slice(&cert_rva.to_le_bytes());

        let cert = CERT_OFFSET;
        buf[cert + CERT_TIMEDATE..cert + CERT_TIMEDATE + 4]
            .copy_from_slice(&0x5F5E100u32.to_le_bytes());
        let title_id = 0x4D53_0004u32; // "MS" + game 4
        buf[cert + CERT_TITLE_ID..cert + CERT_TITLE_ID + 4]
            .copy_from_slice(&title_id.to_le_bytes());
        for (i, &unit) in title_name_units.iter().enumerate() {
            let at = cert + CERT_TITLE_NAME + i * 2;
            buf[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        }
        let alt_ids: [u32; CERT_ALTERNATE_TITLE_IDS_COUNT] = {
            let mut ids = [0u32; CERT_ALTERNATE_TITLE_IDS_COUNT];
            ids[0] = 0x1234_5678;
            ids[3] = 0x0000_0001;
            ids
        };
        for (i, id) in alt_ids.iter().enumerate() {
            let at = cert + CERT_ALTERNATE_TITLE_IDS + i * 4;
            buf[at..at + 4].copy_from_slice(&id.to_le_bytes());
        }
        let allowed_media: u32 = 0x1 | 0x10; // HARD_DISK | DVD_5_RO
        buf[cert + CERT_ALLOWED_MEDIA..cert + CERT_ALLOWED_MEDIA + 4]
            .copy_from_slice(&allowed_media.to_le_bytes());
        let region = 0x1 | 0x80000000u32; // NA | Manufacturing
        buf[cert + CERT_REGION..cert + CERT_REGION + 4].copy_from_slice(&region.to_le_bytes());
        buf[cert + CERT_RATINGS..cert + CERT_RATINGS + 4].copy_from_slice(&0x2u32.to_le_bytes());
        buf[cert + CERT_DISC_NUMBER..cert + CERT_DISC_NUMBER + 4]
            .copy_from_slice(&1u32.to_le_bytes());
        buf[cert + CERT_VERSION..cert + CERT_VERSION + 4].copy_from_slice(&3u32.to_le_bytes());

        write_icon_section(&mut buf, ICON_SECTIONS[0], &build_xpr0_dxt1_4x4());
        buf
    }

    /// Writes a one-entry section table whose only section is `name`,
    /// carrying `data`.
    fn write_icon_section(buf: &mut [u8], name: &str, data: &[u8]) {
        buf[SECTION_COUNT_OFFSET..SECTION_COUNT_OFFSET + 4].copy_from_slice(&1u32.to_le_bytes());
        let headers_rva = BASE_ADDRESS + SECTION_HEADERS_OFFSET as u32;
        buf[SECTION_HEADERS_ADDRESS_OFFSET..SECTION_HEADERS_ADDRESS_OFFSET + 4]
            .copy_from_slice(&headers_rva.to_le_bytes());

        let h = SECTION_HEADERS_OFFSET;
        buf[h + SECTION_RAW_ADDRESS..h + SECTION_RAW_ADDRESS + 4]
            .copy_from_slice(&(SECTION_DATA_OFFSET as u32).to_le_bytes());
        buf[h + SECTION_RAW_SIZE..h + SECTION_RAW_SIZE + 4]
            .copy_from_slice(&(data.len() as u32).to_le_bytes());
        let name_rva = BASE_ADDRESS + SECTION_NAME_OFFSET as u32;
        buf[h + SECTION_NAME_ADDRESS..h + SECTION_NAME_ADDRESS + 4]
            .copy_from_slice(&name_rva.to_le_bytes());

        buf[SECTION_NAME_OFFSET..SECTION_NAME_OFFSET + name.len()].copy_from_slice(name.as_bytes());
        buf[SECTION_DATA_OFFSET..SECTION_DATA_OFFSET + data.len()].copy_from_slice(data);
    }

    fn utf16_units(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn parses_every_field_from_a_synthetic_xbe() {
        let buf = build_xbe(&utf16_units("Test Game"));
        let info = parse(&buf).unwrap();

        assert_eq!(info.title_id, 0x4D53_0004);
        assert_eq!(info.title_id_hex, "4D530004");
        assert_eq!(info.title_id_code, "MS-004");
        assert_eq!(info.title_name, "Test Game");
        assert_eq!(info.alternate_title_ids, vec![0x1234_5678, 0x0000_0001]);
        assert_eq!(info.allowed_media, 0x11);
        assert_eq!(info.allowed_media_names, vec!["HARD_DISK", "DVD_5_RO"]);
        assert_eq!(info.region, 0x8000_0001);
        assert_eq!(info.region_names, vec!["North America", "Manufacturing"]);
        assert_eq!(info.ratings, 0x2);
        assert_eq!(info.disc_number, 1);
        assert_eq!(info.version, 3);
        assert_eq!(info.cert_timestamp, 0x5F5E100);
    }

    #[test]
    fn bad_magic_returns_none() {
        let mut buf = build_xbe(&utf16_units("Test Game"));
        buf[0] = b'X' + 1;
        assert!(parse(&buf).is_none());
    }

    #[test]
    fn cert_past_eof_returns_none() {
        let mut buf = build_xbe(&utf16_units("Test Game"));
        let bogus_rva = BASE_ADDRESS + buf.len() as u32 + 0x100;
        buf[CERT_ADDRESS_OFFSET..CERT_ADDRESS_OFFSET + 4].copy_from_slice(&bogus_rva.to_le_bytes());
        assert!(parse(&buf).is_none());
    }

    #[test]
    fn full_unterminated_title_name_is_parsed() {
        let units: Vec<u16> = "A".repeat(CERT_TITLE_NAME_UNITS).encode_utf16().collect();
        let buf = build_xbe(&units);
        let info = parse(&buf).unwrap();
        assert_eq!(info.title_name, "A".repeat(CERT_TITLE_NAME_UNITS));
    }

    #[test]
    fn truncated_buffer_returns_none() {
        let buf = build_xbe(&utf16_units("Test Game"));
        assert!(parse(&buf[..CERT_OFFSET]).is_none());
    }

    #[test]
    fn icon_is_decoded_from_the_xtimage_section() {
        let buf = build_xbe(&utf16_units("Test Game"));
        let icon = parse(&buf).unwrap().icon.expect("icon decoded");
        assert_eq!((icon.width, icon.height), (4, 4));
        assert_eq!(&icon.png_bytes[..4], &[0x89, b'P', b'N', b'G']);
    }

    #[test]
    fn icon_is_decoded_from_the_xsimage_section() {
        let mut buf = build_xbe(&utf16_units("Test Game"));
        buf[SECTION_NAME_OFFSET..SECTION_NAME_OFFSET + ICON_SECTIONS[1].len()]
            .copy_from_slice(ICON_SECTIONS[1].as_bytes());
        assert!(parse(&buf).unwrap().icon.is_some());
    }

    #[test]
    fn unnamed_icon_section_yields_no_icon() {
        let mut buf = build_xbe(&utf16_units("Test Game"));
        buf[SECTION_NAME_OFFSET] = b'X';
        let info = parse(&buf).unwrap();
        assert!(info.icon.is_none());
        assert_eq!(info.title_name, "Test Game");
    }

    #[test]
    fn malformed_section_table_yields_no_icon() {
        let mut buf = build_xbe(&utf16_units("Test Game"));
        // Section headers below the image base: not addressable.
        buf[SECTION_HEADERS_ADDRESS_OFFSET..SECTION_HEADERS_ADDRESS_OFFSET + 4]
            .copy_from_slice(&(BASE_ADDRESS - 0x100).to_le_bytes());
        let info = parse(&buf).unwrap();
        assert!(info.icon.is_none());
        assert_eq!(info.title_name, "Test Game");
    }

    #[test]
    fn bad_xpr_magic_yields_no_icon() {
        let mut buf = build_xbe(&utf16_units("Test Game"));
        buf[SECTION_DATA_OFFSET] = b'Y';
        let info = parse(&buf).unwrap();
        assert!(info.icon.is_none());
        assert_eq!(info.title_name, "Test Game");
    }

    #[test]
    fn absurd_section_count_yields_no_icon() {
        let mut buf = build_xbe(&utf16_units("Test Game"));
        buf[SECTION_COUNT_OFFSET..SECTION_COUNT_OFFSET + 4]
            .copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse(&buf).unwrap().icon.is_none());
    }

    #[test]
    fn seekable_reader_parses_certificate_and_icon() {
        let mut buf = build_xbe(&utf16_units("Range Game"));
        buf.resize((1 << 20) + 1, 0);
        let length = buf.len() as u64;
        let mut cursor = Cursor::new(buf);
        let actual = parse_xbe_at(&mut cursor, 0, length).unwrap();
        assert_eq!(actual.title_id, 0x4D53_0004);
        assert_eq!(actual.title_name, "Range Game");
        let icon = actual.icon.unwrap();
        assert_eq!((icon.width, icon.height), (4, 4));
    }

    #[test]
    fn seekable_icon_read_ignores_oversized_section_extent() {
        use std::io::{Read, Seek, SeekFrom};

        struct Sparse {
            data: Vec<u8>,
            length: u64,
            position: u64,
            bytes_read: u64,
        }
        impl Read for Sparse {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                let count = out.len().min((self.length - self.position) as usize);
                if count == 0 {
                    return Ok(0);
                }
                let start = self.position as usize;
                let copied = count.min(self.data.len().saturating_sub(start));
                if copied > 0 {
                    out[..copied].copy_from_slice(&self.data[start..start + copied]);
                }
                out[copied..count].fill(0);
                self.position += count as u64;
                self.bytes_read += count as u64;
                Ok(count)
            }
        }
        impl Seek for Sparse {
            fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
                let next = match from {
                    SeekFrom::Start(position) => position as i128,
                    SeekFrom::Current(offset) => self.position as i128 + offset as i128,
                    SeekFrom::End(offset) => self.length as i128 + offset as i128,
                };
                if next < 0 || next > self.length as i128 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "bad seek",
                    ));
                }
                self.position = next as u64;
                Ok(self.position)
            }
        }

        let mut bytes = build_xbe(&utf16_units("Bounded Icon"));
        let icon = parse(&bytes).unwrap().icon.unwrap();
        let section_size = 1u32 << 30;
        bytes[SECTION_HEADERS_OFFSET + SECTION_RAW_SIZE
            ..SECTION_HEADERS_OFFSET + SECTION_RAW_SIZE + 4]
            .copy_from_slice(&section_size.to_le_bytes());
        let length = SECTION_DATA_OFFSET as u64 + section_size as u64;
        let mut source = Sparse {
            data: bytes,
            length,
            position: 0,
            bytes_read: 0,
        };
        let actual = parse_xbe_at(&mut source, 0, length).unwrap();
        assert_eq!(actual.icon.unwrap().png_bytes, icon.png_bytes);
        assert!(source.bytes_read < 4096);
    }

    #[test]
    fn non_ascii_publisher_bytes_fall_back_to_hex_code() {
        let mut buf = build_xbe(&utf16_units("Test Game"));
        let title_id = 0x0102_0004u32;
        buf[CERT_OFFSET + CERT_TITLE_ID..CERT_OFFSET + CERT_TITLE_ID + 4]
            .copy_from_slice(&title_id.to_le_bytes());
        let info = parse(&buf).unwrap();
        assert_eq!(info.title_id_code, "01020004");
    }
}
