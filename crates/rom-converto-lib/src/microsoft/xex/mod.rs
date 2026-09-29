//! Xbox 360 XEX2 executable metadata (xenia `xex2_info.h` / `xex_module.cc`).
//!
//! Everything in the XEX2 headers is plaintext and big-endian. The title name
//! and icon live in an XDBF resource inside the basefile, which has to be
//! decrypted and decompressed first ([`basefile`]).

mod basefile;
mod xdbf;

use serde::{Deserialize, Serialize};

use super::read_extent_at as read_xex_range;
use crate::info::Image;
use crate::util::extent_end;

const MAGIC: &[u8; 4] = b"XEX2";
const HEADER_SIZE_OFFSET: usize = 0x08;
const SECURITY_OFFSET_OFFSET: usize = 0x10;
const HEADER_COUNT_OFFSET: usize = 0x14;
const OPT_HEADER_TABLE: usize = 0x18;
const OPT_HEADER_ENTRY: usize = 8;

const KEY_RESOURCE_INFO: u32 = 0x0000_02FF;
const KEY_FILE_FORMAT_INFO: u32 = 0x0000_03FF;
const KEY_ORIGINAL_PE_NAME: u32 = 0x0001_83FF;
const KEY_EXECUTION_INFO: u32 = 0x0004_0006;

const EXEC_MEDIA_ID: usize = 0x00;
const EXEC_VERSION: usize = 0x04;
const EXEC_BASE_VERSION: usize = 0x08;
const EXEC_TITLE_ID: usize = 0x0C;
const EXEC_PLATFORM: usize = 0x10;
const EXEC_DISC_NUMBER: usize = 0x12;
const EXEC_DISC_COUNT: usize = 0x13;

const SEC_IMAGE_SIZE: usize = 0x004;
const SEC_LOAD_ADDRESS: usize = 0x110;
const SEC_AES_KEY: usize = 0x150;
const SEC_REGION: usize = 0x178;
const SEC_ALLOWED_MEDIA: usize = 0x17C;
const SEC_TAIL: usize = 0x180;

const RESOURCE_ENTRY: usize = 0x10;
const RESOURCE_NAME_LEN: usize = 8;

const COMPRESSION_NONE: u16 = 0;
const COMPRESSION_BASIC: u16 = 1;
const COMPRESSION_NORMAL: u16 = 2;

/// Non-overlapping bit groups, so a region value maps to each name at most
/// once. Japan and China are called out of the NTSC-J group, Australia and
/// New Zealand out of the PAL group.
const REGION_FLAGS: &[(u32, &str)] = &[
    (0x0000_00FF, "NTSC-U"),
    (0x0000_0100, "NTSC-J Japan"),
    (0x0000_0200, "NTSC-J China"),
    (0x0000_FC00, "NTSC-J"),
    (0x0001_0000, "PAL Australia/New Zealand"),
    (0x00FE_0000, "PAL"),
    (0xFF00_0000, "Other"),
];

/// Title metadata parsed from a `default.xex`'s optional headers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
pub struct XexInfo {
    pub title_id: u32,
    pub title_id_hex: String,
    pub media_id: u32,
    pub version: String,
    pub version_raw: u32,
    pub base_version: String,
    pub base_version_raw: u32,
    pub disc_number: u8,
    pub disc_count: u8,
    pub platform: u8,
    pub original_pe_name: Option<String>,
    pub region: u32,
    pub region_names: Vec<String>,
    pub allowed_media: u32,
    pub title_name: Option<String>,
    pub icon: Option<Image>,
}

pub(crate) struct ExecutionInfo {
    pub media_id: u32,
    pub version: u32,
    pub base_version: u32,
    pub title_id: u32,
    pub platform: u8,
    pub disc_number: u8,
    pub disc_count: u8,
}

pub(crate) struct SecurityInfo {
    pub image_size: u32,
    pub load_address: u32,
    pub aes_key: [u8; 16],
    pub region: u32,
    pub allowed_media: u32,
}

pub(crate) struct FileFormatInfo {
    pub encryption_type: u16,
    pub compression: Compression,
}

pub(crate) enum Compression {
    None,
    /// Seekable descriptor table for the bounded metadata path.
    BasicAt {
        table_offset: u64,
        count: u64,
    },
    Normal {
        window_size: u32,
        first_block_size: u32,
        first_block_hash: [u8; 20],
    },
}

struct ResourceEntry {
    address: u32,
    size: u32,
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    bytes
        .get(offset..offset.checked_add(2)?)?
        .try_into()
        .ok()
        .map(u16::from_be_bytes)
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    bytes
        .get(offset..offset.checked_add(4)?)?
        .try_into()
        .ok()
        .map(u32::from_be_bytes)
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    bytes
        .get(offset..offset.checked_add(8)?)?
        .try_into()
        .ok()
        .map(u64::from_be_bytes)
}

fn flag_names(value: u32, flags: &[(u32, &str)]) -> Vec<String> {
    flags
        .iter()
        .filter(|(bits, _)| value & bits != 0)
        .map(|(_, name)| name.to_string())
        .collect()
}

/// major:4, minor:4, build:16, qfe:8, most significant field first.
fn version_string(version: u32) -> String {
    format!(
        "{}.{}.{}.{}",
        version >> 28,
        (version >> 24) & 0xF,
        (version >> 8) & 0xFFFF,
        version & 0xFF
    )
}

fn parse_execution_info(data: &[u8]) -> Option<ExecutionInfo> {
    Some(ExecutionInfo {
        media_id: read_u32(data, EXEC_MEDIA_ID)?,
        version: read_u32(data, EXEC_VERSION)?,
        base_version: read_u32(data, EXEC_BASE_VERSION)?,
        title_id: read_u32(data, EXEC_TITLE_ID)?,
        platform: *data.get(EXEC_PLATFORM)?,
        disc_number: *data.get(EXEC_DISC_NUMBER)?,
        disc_count: *data.get(EXEC_DISC_COUNT)?,
    })
}

fn parse_security_info(bytes: &[u8], offset: usize) -> Option<SecurityInfo> {
    let sec = bytes.get(offset..offset.checked_add(SEC_TAIL)?)?;
    Some(SecurityInfo {
        image_size: read_u32(sec, SEC_IMAGE_SIZE)?,
        load_address: read_u32(sec, SEC_LOAD_ADDRESS)?,
        aes_key: sec.get(SEC_AES_KEY..SEC_AES_KEY + 16)?.try_into().ok()?,
        region: read_u32(sec, SEC_REGION)?,
        allowed_media: read_u32(sec, SEC_ALLOWED_MEDIA)?,
    })
}

fn valid_opt_header<R: std::io::Read + std::io::Seek>(
    reader: &mut R,
    base: u64,
    length: u64,
    key: u32,
    offset: u64,
) -> bool {
    let size = match key & 0xFF {
        0x01 => return true,
        0xFF => match read_xex_range(reader, base, length, offset, 4)
            .and_then(|bytes| read_u32(&bytes, 0))
        {
            Some(size) => size as u64,
            None => return false,
        },
        fixed => fixed as u64 * 4,
    };
    extent_end(offset, size, length).is_some()
}

/// Parses XEX metadata using the fixed header and individually validated
/// optional-header/resource ranges.
pub(crate) fn read_xex_info_at<R: std::io::Read + std::io::Seek>(
    reader: &mut R,
    base: u64,
    length: u64,
) -> Option<XexInfo> {
    let fixed = read_xex_range(reader, base, length, 0, OPT_HEADER_TABLE)?;
    if fixed.get(0..4)? != MAGIC {
        return None;
    }
    let header_size = read_u32(&fixed, HEADER_SIZE_OFFSET)? as u64;

    let declared_count = read_u32(&fixed, HEADER_COUNT_OFFSET)? as u64;
    let count = declared_count.min((length - OPT_HEADER_TABLE as u64) / OPT_HEADER_ENTRY as u64);
    let mut execution_offset = None;
    let mut format_offset = None;
    let mut resource_offset = None;
    let mut name_offset = None;
    let mut index = 0u64;
    while index < count {
        let batch_count = (count - index).min(512);
        let at = OPT_HEADER_TABLE as u64 + index * OPT_HEADER_ENTRY as u64;
        let pairs = read_xex_range(
            reader,
            base,
            length,
            at,
            (batch_count * OPT_HEADER_ENTRY as u64) as usize,
        )?;
        for pair in pairs.chunks_exact(OPT_HEADER_ENTRY) {
            let key = read_u32(pair, 0)?;
            let value = read_u32(pair, 4)? as u64;
            match key {
                KEY_EXECUTION_INFO if valid_opt_header(reader, base, length, key, value) => {
                    execution_offset = Some(value)
                }
                KEY_FILE_FORMAT_INFO if valid_opt_header(reader, base, length, key, value) => {
                    format_offset = Some(value)
                }
                KEY_RESOURCE_INFO if valid_opt_header(reader, base, length, key, value) => {
                    resource_offset = Some(value)
                }
                KEY_ORIGINAL_PE_NAME if valid_opt_header(reader, base, length, key, value) => {
                    name_offset = Some(value)
                }
                _ => {}
            }
        }
        index += batch_count;
    }

    let exec = execution_offset.and_then(|offset| {
        read_xex_range(reader, base, length, offset, 20)
            .and_then(|bytes| parse_execution_info(&bytes))
    });
    let security_offset = read_u32(&fixed, SECURITY_OFFSET_OFFSET)? as u64;
    let security_bytes = read_xex_range(reader, base, length, security_offset, SEC_TAIL)?;
    let security = parse_security_info(&security_bytes, 0)?;
    let title_id = exec.as_ref().map_or(0, |e| e.title_id);
    let version = exec.as_ref().map_or(0, |e| e.version);
    let base_version = exec.as_ref().map_or(0, |e| e.base_version);
    let original_pe_name =
        name_offset.and_then(|offset| read_xex_original_name(reader, base, length, offset));
    let fmt = format_offset.and_then(|offset| read_xex_file_format(reader, base, length, offset));
    let resource = resource_offset
        .and_then(|offset| find_title_resource(reader, base, length, offset, title_id));

    let mut info = XexInfo {
        title_id,
        title_id_hex: format!("{title_id:08X}"),
        media_id: exec.as_ref().map_or(0, |e| e.media_id),
        version: version_string(version),
        version_raw: version,
        base_version: version_string(base_version),
        base_version_raw: base_version,
        disc_number: exec.as_ref().map_or(0, |e| e.disc_number),
        disc_count: exec.as_ref().map_or(0, |e| e.disc_count),
        platform: exec.as_ref().map_or(0, |e| e.platform),
        original_pe_name,
        region: security.region,
        region_names: flag_names(security.region, REGION_FLAGS),
        allowed_media: security.allowed_media,
        title_name: None,
        icon: None,
    };
    if let (Some(fmt), Some(resource)) = (fmt, resource) {
        let Some(start) = resource
            .address
            .checked_sub(security.load_address)
            .map(u64::from)
        else {
            return Some(info);
        };
        let end = start
            .checked_add(resource.size as u64)?
            .min(security.image_size as u64);
        if start < end
            && let Some(stored_len) = length.checked_sub(header_size)
            && let Some(source_base) = base.checked_add(header_size)
            && let Some(source) =
                basefile::ResourceSource::new(source_base, stored_len, &fmt, &security)
        {
            let mut resource_reader = basefile::ResourceReader::new();
            let meta = xdbf::parse_xdbf_ranges(end - start, |offset, size| {
                let resource_offset = start.checked_add(offset)?;
                resource_reader.read_resource_at(
                    reader,
                    &source,
                    &fmt,
                    &security,
                    resource_offset,
                    size,
                )
            });
            info.title_name = meta.title_name;
            info.icon = meta.icon;
        }
    }
    Some(info)
}

fn read_xex_file_format<R: std::io::Read + std::io::Seek>(
    reader: &mut R,
    base: u64,
    extent: u64,
    offset: u64,
) -> Option<FileFormatInfo> {
    let fixed = read_xex_range(reader, base, extent, offset, 8)?;
    let info_size = read_u32(&fixed, 0)? as u64;
    if info_size < 8 || extent_end(offset, info_size, extent).is_none() {
        return None;
    }
    let encryption_type = read_u16(&fixed, 4)?;
    let compression = match read_u16(&fixed, 6)? {
        COMPRESSION_NONE => Compression::None,
        COMPRESSION_NORMAL => {
            if info_size < 36 {
                return None;
            }
            let data = read_xex_range(reader, base, extent, offset, 36)?;
            Compression::Normal {
                window_size: read_u32(&data, 8)?,
                first_block_size: read_u32(&data, 12)?,
                first_block_hash: data.get(16..36)?.try_into().ok()?,
            }
        }
        COMPRESSION_BASIC => Compression::BasicAt {
            table_offset: base.checked_add(offset)?.checked_add(8)?,
            count: info_size.checked_sub(8)? / 8,
        },
        _ => return None,
    };
    Some(FileFormatInfo {
        encryption_type,
        compression,
    })
}

fn read_xex_original_name<R: std::io::Read + std::io::Seek>(
    reader: &mut R,
    base: u64,
    extent: u64,
    offset: u64,
) -> Option<String> {
    let size = read_xex_range(reader, base, extent, offset, 4)
        .and_then(|bytes| read_u32(&bytes, 0))? as u64;
    if size <= 4 || extent_end(offset, size, extent).is_none() {
        return None;
    }
    let mut at = 4u64;
    let mut name = Vec::new();
    while at < size {
        let length = (size - at).min(64 * 1024) as usize;
        let bytes = read_xex_range(reader, base, extent, offset.checked_add(at)?, length)?;
        let end = bytes
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(bytes.len());
        name.extend_from_slice(&bytes[..end]);
        if end < bytes.len() {
            break;
        }
        at += length as u64;
    }
    if name.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&name).into_owned())
}

fn find_title_resource<R: std::io::Read + std::io::Seek>(
    reader: &mut R,
    base: u64,
    extent: u64,
    offset: u64,
    title_id: u32,
) -> Option<ResourceEntry> {
    let size = read_xex_range(reader, base, extent, offset, 4)
        .and_then(|bytes| read_u32(&bytes, 0))? as u64;
    if size < 4 || extent_end(offset, size, extent).is_none() {
        return None;
    }
    let count = (size - 4) / RESOURCE_ENTRY as u64;
    let wanted = format!("{title_id:08X}");
    for index in 0..count {
        let entry_offset = offset
            .checked_add(4)?
            .checked_add(index.checked_mul(RESOURCE_ENTRY as u64)?)?;
        let entry = read_xex_range(reader, base, extent, entry_offset, RESOURCE_ENTRY)?;
        let raw_name = entry.get(..RESOURCE_NAME_LEN)?;
        let start = raw_name
            .iter()
            .position(|&byte| byte != b' ' && byte != 0)
            .unwrap_or(raw_name.len());
        let end = raw_name
            .iter()
            .rposition(|&byte| byte != b' ' && byte != 0)
            .map_or(start, |index| index + 1);
        if raw_name.get(start..end)? == wanted.as_bytes() {
            return Some(ResourceEntry {
                address: read_u32(&entry, 8)?,
                size: read_u32(&entry, 12)?,
            });
        }
    }
    None
}
/// Test convenience: parses XEX metadata from an in-memory image.
#[cfg(test)]
pub fn read_xex_info(bytes: &[u8]) -> Option<XexInfo> {
    read_xex_info_at(&mut std::io::Cursor::new(bytes), 0, bytes.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOAD_ADDRESS: u32 = 0x8200_0000;
    const TITLE_ID: u32 = 0x4541_1234;
    const SESSION_KEY: [u8; 16] = [
        0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0xFE, 0xDC, 0xBA, 0x98, 0x76, 0x54, 0x32,
        0x10,
    ];

    const HEADER_SIZE: usize = 0x1000;
    const SECURITY_OFFSET: usize = 0x400;
    const EXEC_AT: usize = 0x100;
    const RESOURCE_AT: usize = 0x200;
    const FORMAT_AT: usize = 0x300;
    const PE_NAME_AT: usize = 0x380;
    const XDBF_AT: u32 = 0x40;

    fn put32(buf: &mut [u8], at: usize, value: u32) {
        buf[at..at + 4].copy_from_slice(&value.to_be_bytes());
    }

    fn put16(buf: &mut [u8], at: usize, value: u16) {
        buf[at..at + 2].copy_from_slice(&value.to_be_bytes());
    }

    #[test]
    fn execution_info_fields_and_version_string() {
        let mut data = vec![0u8; 0x18];
        put32(&mut data, EXEC_MEDIA_ID, 0x1122_3344);
        put32(&mut data, EXEC_VERSION, 0x2105_1234);
        put32(&mut data, EXEC_BASE_VERSION, 0x1000_0001);
        put32(&mut data, EXEC_TITLE_ID, TITLE_ID);
        data[EXEC_PLATFORM] = 2;
        data[EXEC_DISC_NUMBER] = 1;
        data[EXEC_DISC_COUNT] = 3;

        let exec = parse_execution_info(&data).expect("fixture is 0x18 bytes");
        assert_eq!(exec.media_id, 0x1122_3344);
        assert_eq!(exec.title_id, TITLE_ID);
        assert_eq!(exec.platform, 2);
        assert_eq!(exec.disc_number, 1);
        assert_eq!(exec.disc_count, 3);
        assert_eq!(version_string(exec.version), "2.1.1298.52");
        assert_eq!(version_string(exec.base_version), "1.0.0.1");
    }

    #[test]
    fn execution_info_rejects_short_buffer() {
        assert!(parse_execution_info(&[0u8; 0x10]).is_none());
    }

    #[test]
    fn security_info_offsets() {
        let mut buf = vec![0u8; SECURITY_OFFSET + SEC_TAIL];
        put32(&mut buf, SECURITY_OFFSET + SEC_IMAGE_SIZE, 0x0010_0000);
        put32(&mut buf, SECURITY_OFFSET + SEC_LOAD_ADDRESS, LOAD_ADDRESS);
        buf[SECURITY_OFFSET + SEC_AES_KEY..SECURITY_OFFSET + SEC_AES_KEY + 16]
            .copy_from_slice(&[0xAB; 16]);
        put32(&mut buf, SECURITY_OFFSET + SEC_REGION, 0x0000_FF00);
        put32(&mut buf, SECURITY_OFFSET + SEC_ALLOWED_MEDIA, 0x0000_0F00);

        let sec = parse_security_info(&buf, SECURITY_OFFSET).expect("fixture is long enough");
        assert_eq!(sec.image_size, 0x0010_0000);
        assert_eq!(sec.load_address, LOAD_ADDRESS);
        assert_eq!(sec.aes_key, [0xAB; 16]);
        assert_eq!(sec.region, 0x0000_FF00);
        assert_eq!(sec.allowed_media, 0x0000_0F00);
        assert!(parse_security_info(&buf, SECURITY_OFFSET + 1).is_none());
    }

    #[test]
    fn region_names_cover_each_bit_once() {
        assert_eq!(flag_names(0x0000_00FF, REGION_FLAGS), ["NTSC-U"]);
        assert_eq!(
            flag_names(0x0000_0300, REGION_FLAGS),
            ["NTSC-J Japan", "NTSC-J China"]
        );
        assert_eq!(
            flag_names(0xFFFF_FFFF, REGION_FLAGS).len(),
            REGION_FLAGS.len()
        );
    }

    /// ENCRYPTION_NORMAL + COMPRESSION_NONE, basefile carrying an XDBF at
    /// `XDBF_AT` that the RESOURCE_INFO entry points at.
    fn build_xex(title: &str, png: &[u8], corrupt_key: bool) -> Vec<u8> {
        let xdbf = xdbf::build_xdbf(title, png);
        let mut basefile = vec![0u8; XDBF_AT as usize];
        basefile.extend_from_slice(&xdbf);
        basefile.resize(basefile.len().next_multiple_of(16), 0);
        let image_size = basefile.len() as u32;

        let mut pe_data = basefile;
        basefile::cbc_encrypt_zero_iv(&SESSION_KEY, &mut pe_data);

        let mut aes_key = SESSION_KEY;
        basefile::cbc_encrypt_zero_iv(&basefile::RETAIL_KEY, &mut aes_key);
        if corrupt_key {
            aes_key[0] ^= 0xFF;
        }

        let mut buf = vec![0u8; HEADER_SIZE];
        buf[0..4].copy_from_slice(MAGIC);
        put32(&mut buf, HEADER_SIZE_OFFSET, HEADER_SIZE as u32);
        put32(&mut buf, SECURITY_OFFSET_OFFSET, SECURITY_OFFSET as u32);
        put32(&mut buf, HEADER_COUNT_OFFSET, 4);

        for (i, (key, value)) in [
            (KEY_EXECUTION_INFO, EXEC_AT),
            (KEY_RESOURCE_INFO, RESOURCE_AT),
            (KEY_FILE_FORMAT_INFO, FORMAT_AT),
            (KEY_ORIGINAL_PE_NAME, PE_NAME_AT),
        ]
        .into_iter()
        .enumerate()
        {
            let at = OPT_HEADER_TABLE + i * OPT_HEADER_ENTRY;
            put32(&mut buf, at, key);
            put32(&mut buf, at + 4, value as u32);
        }

        put32(&mut buf, EXEC_AT + EXEC_MEDIA_ID, 0x0BAD_F00D);
        put32(&mut buf, EXEC_AT + EXEC_VERSION, 0x2105_1234);
        put32(&mut buf, EXEC_AT + EXEC_BASE_VERSION, 0x1000_0001);
        put32(&mut buf, EXEC_AT + EXEC_TITLE_ID, TITLE_ID);
        buf[EXEC_AT + EXEC_PLATFORM] = 2;
        buf[EXEC_AT + EXEC_DISC_NUMBER] = 1;
        buf[EXEC_AT + EXEC_DISC_COUNT] = 2;

        put32(&mut buf, RESOURCE_AT, 4 + RESOURCE_ENTRY as u32);
        buf[RESOURCE_AT + 4..RESOURCE_AT + 12]
            .copy_from_slice(format!("{TITLE_ID:08X}").as_bytes());
        put32(&mut buf, RESOURCE_AT + 12, LOAD_ADDRESS + XDBF_AT);
        put32(&mut buf, RESOURCE_AT + 16, xdbf.len() as u32);

        put32(&mut buf, FORMAT_AT, 8);
        put16(&mut buf, FORMAT_AT + 4, 1);
        put16(&mut buf, FORMAT_AT + 6, COMPRESSION_NONE);

        put32(&mut buf, PE_NAME_AT, 0x18);
        buf[PE_NAME_AT + 4..PE_NAME_AT + 16].copy_from_slice(b"default.xex\0");

        put32(&mut buf, SECURITY_OFFSET + SEC_IMAGE_SIZE, image_size);
        put32(&mut buf, SECURITY_OFFSET + SEC_LOAD_ADDRESS, LOAD_ADDRESS);
        buf[SECURITY_OFFSET + SEC_AES_KEY..SECURITY_OFFSET + SEC_AES_KEY + 16]
            .copy_from_slice(&aes_key);
        put32(&mut buf, SECURITY_OFFSET + SEC_REGION, 0x0000_00FF);
        put32(&mut buf, SECURITY_OFFSET + SEC_ALLOWED_MEDIA, 0x0000_0F00);

        buf.extend_from_slice(&pe_data);
        buf
    }

    #[test]
    fn read_xex_info_extracts_title_and_icon() {
        let png = xdbf::tests::build_png(64, 64);
        let info = read_xex_info(&build_xex("Test Title", &png, false)).expect("valid xex");

        assert_eq!(info.title_id, TITLE_ID);
        assert_eq!(info.title_id_hex, format!("{TITLE_ID:08X}"));
        assert_eq!(info.media_id, 0x0BAD_F00D);
        assert_eq!(info.version, "2.1.1298.52");
        assert_eq!(info.base_version, "1.0.0.1");
        assert_eq!(info.disc_number, 1);
        assert_eq!(info.disc_count, 2);
        assert_eq!(info.platform, 2);
        assert_eq!(info.original_pe_name.as_deref(), Some("default.xex"));
        assert_eq!(info.region_names, ["NTSC-U"]);
        assert_eq!(info.allowed_media, 0x0000_0F00);
        assert_eq!(info.title_name.as_deref(), Some("Test Title"));
        let icon = info.icon.expect("icon entry is present");
        assert_eq!((icon.width, icon.height), (64, 64));
    }

    #[test]
    fn seekable_xex_reader_preserves_encrypted_title_and_icon() {
        let png = xdbf::tests::build_png(64, 64);
        let bytes = build_xex("Range Title", &png, false);
        let expected = read_xex_info(&bytes).expect("slice parser reads fixture");
        let mut cursor = std::io::Cursor::new(bytes.clone());
        let actual = read_xex_info_at(&mut cursor, 0, bytes.len() as u64)
            .expect("seekable parser reads fixture");
        assert_eq!(actual.title_name, expected.title_name);
        assert_eq!(
            actual.icon.expect("title icon exists").png_bytes,
            expected.icon.expect("slice icon exists").png_bytes
        );
    }

    #[test]
    fn read_xex_info_keeps_plaintext_when_key_is_wrong() {
        let png = xdbf::tests::build_png(64, 64);
        let info = read_xex_info(&build_xex("Test Title", &png, true)).expect("valid xex");

        assert_eq!(info.title_id, TITLE_ID);
        assert_eq!(info.original_pe_name.as_deref(), Some("default.xex"));
        assert_eq!(info.region_names, ["NTSC-U"]);
        assert!(info.title_name.is_none());
        assert!(info.icon.is_none());
    }

    #[test]
    fn read_xex_info_rejects_bad_magic() {
        assert!(read_xex_info(b"XEX1").is_none());
        assert!(read_xex_info(&[]).is_none());
    }
    #[test]
    fn large_seekable_xex_reads_only_required_stored_data_and_preserves_icon() {
        use std::io::{Read, Seek, SeekFrom};

        struct Sparse {
            data: Vec<u8>,
            position: u64,
            length: u64,
            bytes_read: u64,
        }
        impl Read for Sparse {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                let count = out.len().min((self.length - self.position) as usize);
                if count == 0 {
                    return Ok(0);
                }
                let start = self.position as usize;
                let prefix_end = (start + count).min(self.data.len());
                if start < self.data.len() {
                    let prefix_len = prefix_end - start;
                    out[..prefix_len].copy_from_slice(&self.data[start..prefix_end]);
                    out[prefix_len..count].fill(0);
                } else {
                    out[..count].fill(0);
                }
                self.position += count as u64;
                self.bytes_read += count as u64;
                Ok(count)
            }
        }
        impl Seek for Sparse {
            fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
                let next = match from {
                    SeekFrom::Start(pos) => pos as i128,
                    SeekFrom::Current(delta) => self.position as i128 + delta as i128,
                    SeekFrom::End(delta) => self.length as i128 + delta as i128,
                };
                if next < 0 || next > self.length as i128 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "seek outside sparse file",
                    ));
                }
                self.position = next as u64;
                Ok(self.position)
            }
        }

        let png = xdbf::tests::build_png(64, 64);
        let mut fixture = build_xex("Range Header", &png, false);
        let expected = read_xex_info(&fixture).expect("fixture parses");
        put32(
            &mut fixture,
            SECURITY_OFFSET + SEC_IMAGE_SIZE,
            256 * 1024 * 1024 + 1,
        );
        let stored_len = fixture.len() as u64;
        let length = (256 * 1024 * 1024 + 1) as u64;
        let mut source = Sparse {
            data: fixture,
            position: 0,
            length,
            bytes_read: 0,
        };
        let actual = read_xex_info_at(&mut source, 0, length).expect("large XEX parses");
        assert_eq!(actual.title_id, expected.title_id);
        assert_eq!(actual.title_name, expected.title_name);
        assert_eq!(
            actual.icon.expect("icon is preserved").png_bytes,
            expected.icon.expect("reference icon exists").png_bytes
        );
        assert!(source.bytes_read <= stored_len + 12);
    }

    #[test]
    fn seekable_xex_does_not_materialize_declared_header_extent() {
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
                out[..copied].copy_from_slice(&self.data[start..start + copied]);
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

        let mut bytes = build_xex("Header Range", &xdbf::tests::build_png(1, 1), false);
        let expected = read_xex_info(&bytes).unwrap();
        let count = read_u32(&bytes, HEADER_COUNT_OFFSET).unwrap() as usize;
        for index in 0..count {
            let at = OPT_HEADER_TABLE + index * OPT_HEADER_ENTRY;
            let key = read_u32(&bytes, at).unwrap();
            if key == KEY_FILE_FORMAT_INFO || key == KEY_RESOURCE_INFO {
                put32(&mut bytes, at, 0x0000_7777);
            }
        }
        let header_size = 32 * 1024 * 1024u32;
        put32(&mut bytes, HEADER_SIZE_OFFSET, header_size);
        let mut source = Sparse {
            data: bytes,
            length: header_size as u64 + 4096,
            position: 0,
            bytes_read: 0,
        };
        let length = source.length;
        let actual = read_xex_info_at(&mut source, 0, length).unwrap();
        assert_eq!(actual.original_pe_name, expected.original_pe_name);
        assert_eq!(actual.title_id, expected.title_id);
        assert!(source.bytes_read < 4096);
    }
    #[test]
    fn plaintext_metadata_survives_header_extent_mismatches() {
        let mut bytes = build_xex("Header Bounds", &xdbf::tests::build_png(1, 1), false);
        for header_size in [0x10, 0x40, bytes.len() as u32 + 1] {
            put32(&mut bytes, HEADER_SIZE_OFFSET, header_size);
            let info = read_xex_info(&bytes).expect("plaintext XEX metadata remains available");
            assert_eq!(info.title_id, TITLE_ID);
            assert_eq!(info.original_pe_name.as_deref(), Some("default.xex"));
        }
    }
}
