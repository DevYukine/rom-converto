//! `default.xbe` header parsing: title metadata carried in the XBE
//! certificate (xboxdevwiki.net/Xbe).

use serde::{Deserialize, Serialize};

const MAGIC: &[u8; 4] = b"XBEH";
const BASE_ADDRESS_OFFSET: usize = 0x104;
const CERT_ADDRESS_OFFSET: usize = 0x118;

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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
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

pub(crate) fn parse_xbe(bytes: &[u8]) -> Option<XbeInfo> {
    if bytes.get(0..4)? != MAGIC {
        return None;
    }
    let base_address = read_u32(bytes, BASE_ADDRESS_OFFSET)?;
    let cert_rva = read_u32(bytes, CERT_ADDRESS_OFFSET)?;
    let cert = cert_rva.checked_sub(base_address)? as usize;

    let cert_end = cert.checked_add(CERT_TAIL)?;
    if bytes.len() < cert_end {
        return None;
    }

    let title_id = read_u32(bytes, cert + CERT_TITLE_ID)?;
    let title_name_bytes =
        bytes.get(cert + CERT_TITLE_NAME..cert + CERT_TITLE_NAME + CERT_TITLE_NAME_UNITS * 2)?;
    let alternate_title_ids = (0..CERT_ALTERNATE_TITLE_IDS_COUNT)
        .map(|i| read_u32(bytes, cert + CERT_ALTERNATE_TITLE_IDS + i * 4))
        .collect::<Option<Vec<u32>>>()?
        .into_iter()
        .filter(|&id| id != 0)
        .collect();
    let allowed_media = read_u32(bytes, cert + CERT_ALLOWED_MEDIA)?;
    let region = read_u32(bytes, cert + CERT_REGION)?;
    let ratings = read_u32(bytes, cert + CERT_RATINGS)?;
    let disc_number = read_u32(bytes, cert + CERT_DISC_NUMBER)?;
    let version = read_u32(bytes, cert + CERT_VERSION)?;
    let cert_timestamp = read_u32(bytes, cert + CERT_TIMEDATE)?;

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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE_ADDRESS: u32 = 0x10000;
    const CERT_OFFSET: usize = 0x180;

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
        buf
    }

    fn utf16_units(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn parses_every_field_from_a_synthetic_xbe() {
        let buf = build_xbe(&utf16_units("Test Game"));
        let info = parse_xbe(&buf).unwrap();

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
        assert!(parse_xbe(&buf).is_none());
    }

    #[test]
    fn cert_past_eof_returns_none() {
        let mut buf = build_xbe(&utf16_units("Test Game"));
        let bogus_rva = BASE_ADDRESS + buf.len() as u32 + 0x100;
        buf[CERT_ADDRESS_OFFSET..CERT_ADDRESS_OFFSET + 4].copy_from_slice(&bogus_rva.to_le_bytes());
        assert!(parse_xbe(&buf).is_none());
    }

    #[test]
    fn full_unterminated_title_name_is_parsed() {
        let units: Vec<u16> = "A".repeat(CERT_TITLE_NAME_UNITS).encode_utf16().collect();
        let buf = build_xbe(&units);
        let info = parse_xbe(&buf).unwrap();
        assert_eq!(info.title_name, "A".repeat(CERT_TITLE_NAME_UNITS));
    }

    #[test]
    fn truncated_buffer_returns_none() {
        let buf = build_xbe(&utf16_units("Test Game"));
        assert!(parse_xbe(&buf[..CERT_OFFSET]).is_none());
    }

    #[test]
    fn non_ascii_publisher_bytes_fall_back_to_hex_code() {
        let mut buf = build_xbe(&utf16_units("Test Game"));
        let title_id = 0x0102_0004u32;
        buf[CERT_OFFSET + CERT_TITLE_ID..CERT_OFFSET + CERT_TITLE_ID + 4]
            .copy_from_slice(&title_id.to_le_bytes());
        let info = parse_xbe(&buf).unwrap();
        assert_eq!(info.title_id_code, "01020004");
    }
}
