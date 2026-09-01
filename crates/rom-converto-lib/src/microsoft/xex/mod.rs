//! Xbox 360 XEX2 executable metadata (xenia `xex2_info.h` / `xex_module.cc`).
//!
//! Everything in the XEX2 headers is plaintext and big-endian. The title name
//! and icon live in an XDBF resource inside the basefile, which has to be
//! decrypted and decompressed first ([`basefile`]).

mod basefile;
mod xdbf;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::info::Image;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

enum OptHeader<'a> {
    /// Inline value; nothing this module reads uses the inline encoding.
    Value,
    Data(&'a [u8]),
}

struct ExecutionInfo {
    media_id: u32,
    version: u32,
    base_version: u32,
    title_id: u32,
    platform: u8,
    disc_number: u8,
    disc_count: u8,
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
    /// `(data_size, zero_size)` pairs.
    Basic(Vec<(u32, u32)>),
    Normal {
        window_size: u32,
        first_block_size: u32,
        first_block_hash: [u8; 20],
    },
}

struct ResourceEntry {
    name: String,
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

/// The low byte of the key says where the payload lives: `0x01` means the
/// table slot itself is the value, `0xFF` means it points at a self-sized
/// block, anything else means a fixed `low_byte * 4` bytes at that offset.
fn resolve_opt_header(bytes: &[u8], key: u32, value: u32) -> Option<OptHeader<'_>> {
    let at = value as usize;
    let size = match (key & 0xFF) as u8 {
        0x01 => return Some(OptHeader::Value),
        0xFF => read_u32(bytes, at)? as usize,
        low => low as usize * 4,
    };
    bytes.get(at..at.checked_add(size)?).map(OptHeader::Data)
}

fn opt_headers(bytes: &[u8]) -> HashMap<u32, OptHeader<'_>> {
    let count = read_u32(bytes, HEADER_COUNT_OFFSET).unwrap_or(0) as usize;
    let count = count.min(bytes.len() / OPT_HEADER_ENTRY);
    let mut headers = HashMap::new();
    for i in 0..count {
        let at = OPT_HEADER_TABLE + i * OPT_HEADER_ENTRY;
        let (Some(key), Some(value)) = (read_u32(bytes, at), read_u32(bytes, at + 4)) else {
            break;
        };
        if let Some(header) = resolve_opt_header(bytes, key, value) {
            headers.insert(key, header);
        }
    }
    headers
}

fn opt_data<'a>(headers: &HashMap<u32, OptHeader<'a>>, key: u32) -> Option<&'a [u8]> {
    match headers.get(&key)? {
        OptHeader::Data(data) => Some(data),
        OptHeader::Value => None,
    }
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

fn parse_original_pe_name(data: &[u8]) -> Option<String> {
    let name = data.get(4..)?;
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    if end == 0 {
        return None;
    }
    Some(String::from_utf8_lossy(&name[..end]).into_owned())
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

fn parse_file_format_info(data: &[u8]) -> Option<FileFormatInfo> {
    let info_size = read_u32(data, 0)? as usize;
    let encryption_type = read_u16(data, 4)?;
    let compression = match read_u16(data, 6)? {
        COMPRESSION_NONE => Compression::None,
        COMPRESSION_BASIC => {
            let count = info_size.checked_sub(8)? / 8;
            let mut blocks = Vec::new();
            for i in 0..count {
                let at = 8 + i * 8;
                blocks.push((read_u32(data, at)?, read_u32(data, at + 4)?));
            }
            Compression::Basic(blocks)
        }
        COMPRESSION_NORMAL => Compression::Normal {
            window_size: read_u32(data, 8)?,
            first_block_size: read_u32(data, 12)?,
            first_block_hash: data.get(16..36)?.try_into().ok()?,
        },
        _ => return None,
    };
    Some(FileFormatInfo {
        encryption_type,
        compression,
    })
}

fn parse_resource_info(data: &[u8]) -> Vec<ResourceEntry> {
    let count = data.len().saturating_sub(4) / RESOURCE_ENTRY;
    (0..count)
        .filter_map(|i| {
            let at = 4 + i * RESOURCE_ENTRY;
            let name = data.get(at..at + RESOURCE_NAME_LEN)?;
            Some(ResourceEntry {
                name: String::from_utf8_lossy(name)
                    .trim_matches(|c| c == ' ' || c == '\0')
                    .to_string(),
                address: read_u32(data, at + 8)?,
                size: read_u32(data, at + 12)?,
            })
        })
        .collect()
}

/// Best effort: any failure here just means no title name or icon.
fn read_xdbf_meta(
    bytes: &[u8],
    header_size: usize,
    security: &SecurityInfo,
    headers: &HashMap<u32, OptHeader<'_>>,
    title_id: u32,
) -> Option<xdbf::XdbfMeta> {
    let fmt = parse_file_format_info(opt_data(headers, KEY_FILE_FORMAT_INFO)?)?;
    let pe_data = bytes.get(header_size..)?;
    let basefile = basefile::decrypt_and_decompress(pe_data, &fmt, security)?;

    let wanted = format!("{title_id:08X}");
    let entry = parse_resource_info(opt_data(headers, KEY_RESOURCE_INFO)?)
        .into_iter()
        .find(|entry| entry.name == wanted)?;
    let start = entry.address.checked_sub(security.load_address)? as usize;
    let end = start.checked_add(entry.size as usize)?.min(basefile.len());
    Some(xdbf::parse_xdbf(basefile.get(start..end)?))
}

pub fn read_xex_info(bytes: &[u8]) -> Option<XexInfo> {
    if bytes.get(0..4)? != MAGIC {
        return None;
    }
    let header_size = read_u32(bytes, HEADER_SIZE_OFFSET)? as usize;
    let security = parse_security_info(bytes, read_u32(bytes, SECURITY_OFFSET_OFFSET)? as usize)?;
    let headers = opt_headers(bytes);

    let exec = opt_data(&headers, KEY_EXECUTION_INFO).and_then(parse_execution_info);
    let title_id = exec.as_ref().map_or(0, |e| e.title_id);
    let version = exec.as_ref().map_or(0, |e| e.version);
    let base_version = exec.as_ref().map_or(0, |e| e.base_version);

    let meta =
        read_xdbf_meta(bytes, header_size, &security, &headers, title_id).unwrap_or_default();

    Some(XexInfo {
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
        original_pe_name: opt_data(&headers, KEY_ORIGINAL_PE_NAME).and_then(parse_original_pe_name),
        region: security.region,
        region_names: flag_names(security.region, REGION_FLAGS),
        allowed_media: security.allowed_media,
        title_name: meta.title_name,
        icon: meta.icon,
    })
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
    fn opt_table_walks_all_size_encodings() {
        let mut buf = vec![0u8; 0x200];
        put32(&mut buf, HEADER_COUNT_OFFSET, 3);
        // inline value
        put32(&mut buf, OPT_HEADER_TABLE, 0x0001_0001);
        put32(&mut buf, OPT_HEADER_TABLE + 4, 0xDEAD_BEEF);
        // self-sized block at 0x100
        put32(&mut buf, OPT_HEADER_TABLE + 8, 0x0000_02FF);
        put32(&mut buf, OPT_HEADER_TABLE + 12, 0x100);
        put32(&mut buf, 0x100, 0x14);
        // fixed 6 * 4 bytes at 0x180
        put32(&mut buf, OPT_HEADER_TABLE + 16, KEY_EXECUTION_INFO);
        put32(&mut buf, OPT_HEADER_TABLE + 20, 0x180);

        let headers = opt_headers(&buf);
        assert!(matches!(headers.get(&0x0001_0001), Some(OptHeader::Value)));
        assert_eq!(opt_data(&headers, 0x0000_02FF).map(<[u8]>::len), Some(0x14));
        assert_eq!(
            opt_data(&headers, KEY_EXECUTION_INFO).map(<[u8]>::len),
            Some(0x18)
        );
    }

    #[test]
    fn opt_header_out_of_bounds_is_dropped() {
        let mut buf = vec![0u8; 0x40];
        put32(&mut buf, HEADER_COUNT_OFFSET, 1);
        put32(&mut buf, OPT_HEADER_TABLE, KEY_EXECUTION_INFO);
        put32(&mut buf, OPT_HEADER_TABLE + 4, 0x3000);
        assert!(opt_headers(&buf).is_empty());
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

    #[test]
    fn original_pe_name_stops_at_null() {
        let mut data = vec![0u8; 4];
        put32(&mut data, 0, 0x18);
        data.extend_from_slice(b"default.xex\0trailing");
        assert_eq!(
            parse_original_pe_name(&data).as_deref(),
            Some("default.xex")
        );
        assert!(parse_original_pe_name(&[0, 0, 0, 4]).is_none());
    }

    #[test]
    fn resource_entry_name_is_trimmed() {
        let mut data = vec![0u8; 4 + RESOURCE_ENTRY];
        put32(&mut data, 0, 4 + RESOURCE_ENTRY as u32);
        data[4..12].copy_from_slice(b"45410A\0\0");
        put32(&mut data, 12, LOAD_ADDRESS + XDBF_AT);
        put32(&mut data, 16, 0x200);

        let entries = parse_resource_info(&data);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "45410A");
        assert_eq!(entries[0].address, LOAD_ADDRESS + XDBF_AT);
        assert_eq!(entries[0].size, 0x200);
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
}
