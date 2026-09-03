//! Nintendo DS cartridge metadata extraction: header fields, the secure-area
//! encryption state, and the decoded banner (titles + icon), for `nds info`.

use crate::info::{Image, LanguageCode, MultilingualString};
use crate::nintendo::nds::{
    DECRYPTED_MARKER, HEADER_SIZE, SECURE_AREA_END, SECURE_AREA_ID, SECURE_AREA_OFFSET, read_u32,
};
use crate::util::pixel::encode_png;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Decoded icon is always 32x32.
const ICON_DIM: u32 = 32;

/// Metadata read from a Nintendo DS cartridge image: header fields, secure
/// area state, and the decoded banner, if present.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NdsInfo {
    pub physical_bytes: u64,
    pub game_title: String,
    pub game_code: String,
    pub maker_code: String,
    pub unit_code: u8,
    pub unit_code_name: String,
    pub region: u8,
    pub rom_version: u8,
    pub device_capacity: u8,
    pub capacity_bytes: u64,
    pub ntr_rom_size: u32,
    pub arm9: NdsArmInfo,
    pub arm7: NdsArmInfo,
    pub fnt_offset: u32,
    pub fnt_size: u32,
    pub fat_offset: u32,
    pub fat_size: u32,
    pub header_crc16: u16,
    pub header_crc16_computed: u16,
    pub header_crc16_valid: bool,
    pub secure_area: NdsSecureAreaState,
    pub banner: Option<NdsBannerInfo>,
}

/// The rom_offset/entry_address/load_address/size quadruplet the header
/// stores for each of the two on-cart CPUs.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct NdsArmInfo {
    pub rom_offset: u32,
    pub entry_address: u32,
    pub load_address: u32,
    pub size: u32,
}

/// Whether the KEY1-encrypted secure area at `0x4000..0x8000` is present,
/// and if so whether it currently holds plaintext or ciphertext.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NdsSecureAreaState {
    #[default]
    NotPresent,
    Encrypted,
    Decrypted,
}

/// Decoded `banner.bin` header block: title strings per language plus the
/// 32x32 icon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NdsBannerInfo {
    pub banner_version: u16,
    pub titles: MultilingualString,
    pub banner_crc16: u16,
    pub banner_crc16_computed: u16,
    pub banner_crc16_valid: bool,
    pub icon: Image,
}

/// Reads header, secure-area state, and banner metadata from a Nintendo DS
/// cartridge image at `path`. Banner read failures are logged and treated
/// as absent rather than propagated, since not every dump carries one.
pub fn read_info(path: &Path) -> Result<NdsInfo> {
    let physical_bytes = std::fs::metadata(path)
        .with_context(|| format!("nds info: stat {}", path.display()))?
        .len();

    let mut file =
        File::open(path).with_context(|| format!("nds info: open {}", path.display()))?;

    let mut header = [0u8; HEADER_SIZE];
    file.read_exact(&mut header)
        .context("nds info: read header")?;

    let game_title = read_ascii_trim(&header[0x000..0x00C]);
    let game_code = read_ascii_trim(&header[0x00C..0x010]);
    let maker_code = read_ascii_trim(&header[0x010..0x012]);
    let unit_code = header[0x012];
    let device_capacity = header[0x014];
    let region = header[0x01D];
    let rom_version = header[0x01E];

    let arm9 = read_arm_info(&header[0x020..0x030]);
    let arm7 = read_arm_info(&header[0x030..0x040]);
    let fnt_offset = read_u32(&header[0x040..0x044]);
    let fnt_size = read_u32(&header[0x044..0x048]);
    let fat_offset = read_u32(&header[0x048..0x04C]);
    let fat_size = read_u32(&header[0x04C..0x050]);
    let banner_offset = read_u32(&header[0x068..0x06C]);
    let ntr_rom_size = read_u32(&header[0x080..0x084]);

    let header_crc16 = u16::from_le_bytes(header[0x15E..0x160].try_into().expect("2-byte slice"));
    let header_crc16_computed = crc16(&header[..0x15E]);

    let secure_area =
        detect_secure_area(&mut file, arm9.rom_offset).context("nds info: read secure area")?;

    let banner = if banner_offset != 0 {
        read_banner(&mut file, banner_offset as u64).unwrap_or_else(|e| {
            log::debug!("nds info: banner read skipped ({})", e);
            None
        })
    } else {
        None
    };

    Ok(NdsInfo {
        physical_bytes,
        game_title,
        game_code,
        maker_code,
        unit_code,
        unit_code_name: unit_code_name(unit_code).to_string(),
        region,
        rom_version,
        device_capacity,
        capacity_bytes: (128u64 * 1024) << device_capacity as u32,
        ntr_rom_size,
        arm9,
        arm7,
        fnt_offset,
        fnt_size,
        fat_offset,
        fat_size,
        header_crc16,
        header_crc16_computed,
        header_crc16_valid: header_crc16 == header_crc16_computed,
        secure_area,
        banner,
    })
}

fn read_arm_info(bytes: &[u8]) -> NdsArmInfo {
    NdsArmInfo {
        rom_offset: read_u32(&bytes[0x00..0x04]),
        entry_address: read_u32(&bytes[0x04..0x08]),
        load_address: read_u32(&bytes[0x08..0x0C]),
        size: read_u32(&bytes[0x0C..0x10]),
    }
}

fn unit_code_name(code: u8) -> &'static str {
    match code {
        0x00 => "NDS",
        0x02 => "NDS+DSi",
        0x03 => "DSi",
        _ => "Unknown",
    }
}

/// Classifies the secure area from the ARM9 rom_offset and, when the offset
/// falls inside the `0x4000..0x8000` window, the first 8 bytes of the
/// window itself. Reuses [`DECRYPTED_MARKER`] and [`SECURE_AREA_ID`] from
/// the crypto module rather than re-deriving what "decrypted" looks like.
fn detect_secure_area(file: &mut File, arm9_rom_offset: u32) -> Result<NdsSecureAreaState> {
    if !(SECURE_AREA_OFFSET..SECURE_AREA_END).contains(&(arm9_rom_offset as usize)) {
        return Ok(NdsSecureAreaState::NotPresent);
    }

    file.seek(SeekFrom::Start(SECURE_AREA_OFFSET as u64))?;
    let mut head = [0u8; 8];
    file.read_exact(&mut head)?;

    if head == *DECRYPTED_MARKER || head == *SECURE_AREA_ID {
        Ok(NdsSecureAreaState::Decrypted)
    } else {
        Ok(NdsSecureAreaState::Encrypted)
    }
}

/// Language table offset within the banner block.
const BANNER_TITLES_OFFSET: usize = 0x240;
/// Bytes per language title (256 UTF-16LE code units incl. terminator).
const BANNER_TITLE_SIZE: usize = 0x100;
/// End of the CRC16-covered range (icon + palette + the original six
/// languages), fixed regardless of banner version.
const BANNER_CRC_END: usize = 0x840;

fn read_banner<R: Read + Seek>(reader: &mut R, offset: u64) -> Result<Option<NdsBannerInfo>> {
    reader.seek(SeekFrom::Start(offset))?;
    let mut prefix = [0u8; 4];
    reader.read_exact(&mut prefix)?;
    let banner_version = u16::from_le_bytes([prefix[0], prefix[1]]);
    let banner_crc16 = u16::from_le_bytes([prefix[2], prefix[3]]);

    let mut languages = vec![
        LanguageCode::Japanese,
        LanguageCode::English,
        LanguageCode::French,
        LanguageCode::German,
        LanguageCode::Italian,
        LanguageCode::Spanish,
    ];
    if banner_version >= 2 {
        languages.push(LanguageCode::Chinese);
    }
    if banner_version >= 3 {
        languages.push(LanguageCode::Korean);
    }

    let total_size = BANNER_TITLES_OFFSET + BANNER_TITLE_SIZE * languages.len();
    reader.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; total_size];
    reader.read_exact(&mut buf)?;

    let banner_crc16_computed = crc16(&buf[0x020..BANNER_CRC_END]);

    let mut palette = [0u16; 16];
    for (i, slot) in palette.iter_mut().enumerate() {
        let off = 0x220 + i * 2;
        *slot = u16::from_le_bytes([buf[off], buf[off + 1]]);
    }
    let rgba = decode_icon(&buf[0x020..0x220], &palette);
    let png = encode_png(&rgba, ICON_DIM, ICON_DIM)?;

    let entries = languages.into_iter().enumerate().map(|(i, lang)| {
        let off = BANNER_TITLES_OFFSET + BANNER_TITLE_SIZE * i;
        (lang, read_utf16_string(&buf[off..off + BANNER_TITLE_SIZE]))
    });

    Ok(Some(NdsBannerInfo {
        banner_version,
        titles: MultilingualString::from_pairs(entries),
        banner_crc16,
        banner_crc16_computed,
        banner_crc16_valid: banner_crc16 == banner_crc16_computed,
        icon: Image::new(png, ICON_DIM, ICON_DIM),
    }))
}

fn read_ascii_trim(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf[..end].iter().map(|&b| b as char).collect()
}

fn read_utf16_string(slice: &[u8]) -> String {
    let units: Vec<u16> = slice
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Decodes the 32x32 4bpp indexed icon (a 4x4 grid of 8x8 tiles, low
/// nibble first) into RGBA8, using `palette` (16 BGR555 entries) with
/// index 0 forced transparent.
fn decode_icon(tiles: &[u8], palette: &[u16; 16]) -> Vec<u8> {
    let mut rgba_palette = [[0u8; 4]; 16];
    for (i, entry) in rgba_palette.iter_mut().enumerate() {
        let (r, g, b) = bgr555_to_rgb8(palette[i]);
        *entry = [r, g, b, if i == 0 { 0 } else { 0xFF }];
    }

    let dim = ICON_DIM as usize;
    let mut out = vec![0u8; dim * dim * 4];
    for tile_idx in 0..16usize {
        let tile_x = (tile_idx % 4) * 8;
        let tile_y = (tile_idx / 4) * 8;
        let tile_off = tile_idx * 32;
        for row in 0..8usize {
            for pair in 0..4usize {
                let byte = tiles[tile_off + row * 4 + pair];
                let x = tile_x + pair * 2;
                let y = tile_y + row;
                let px_off = (y * dim + x) * 4;
                out[px_off..px_off + 4].copy_from_slice(&rgba_palette[(byte & 0x0F) as usize]);
                out[px_off + 4..px_off + 8].copy_from_slice(&rgba_palette[(byte >> 4) as usize]);
            }
        }
    }
    out
}

fn bgr555_to_rgb8(pixel: u16) -> (u8, u8, u8) {
    let r5 = (pixel & 0x1F) as u8;
    let g5 = ((pixel >> 5) & 0x1F) as u8;
    let b5 = ((pixel >> 10) & 0x1F) as u8;
    let expand = |v: u8| (v << 3) | (v >> 2);
    (expand(r5), expand(g5), expand(b5))
}

/// CRC-16 used for both the header checksum (`0x15E`) and the banner
/// checksum (`banner+0x002`): reflected polynomial 0x8005 (table form
/// 0xA001), init 0xFFFF, no output xor. Equivalent to CRC-16/MODBUS.
pub(crate) fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= byte as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::nds::test_fixtures::{
        SYNTH_TITLE, make_nds_info_fixture, make_nds_info_fixture_no_banner,
    };

    #[test]
    fn crc16_matches_modbus_check_vector() {
        assert_eq!(crc16(b"123456789"), 0x4B37);
    }

    #[test]
    fn reads_header_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.nds");
        std::fs::write(&path, make_nds_info_fixture(NdsSecureAreaState::Decrypted)).unwrap();

        let info = read_info(&path).unwrap();
        assert_eq!(info.game_title, SYNTH_TITLE);
        assert_eq!(info.game_code, "ARCE");
        assert_eq!(info.maker_code, "01");
        assert_eq!(info.unit_code, 0x00);
        assert_eq!(info.unit_code_name, "NDS");
        assert_eq!(info.device_capacity, 7);
        assert_eq!(info.capacity_bytes, 128 * 1024 << 7);
        assert_eq!(info.arm9.rom_offset, 0x4000);
        assert_eq!(info.fnt_offset, 0);
        assert_eq!(info.fat_offset, 0);
    }

    #[test]
    fn header_crc_valid_on_fresh_fixture() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.nds");
        std::fs::write(&path, make_nds_info_fixture(NdsSecureAreaState::Decrypted)).unwrap();

        let info = read_info(&path).unwrap();
        assert!(info.header_crc16_valid);
        assert_eq!(info.header_crc16, info.header_crc16_computed);
    }

    #[test]
    fn header_crc_invalid_when_corrupted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.nds");
        let mut rom = make_nds_info_fixture(NdsSecureAreaState::Decrypted);
        rom[0x000] ^= 0xFF; // corrupt a byte the checksum covers
        std::fs::write(&path, &rom).unwrap();

        let info = read_info(&path).unwrap();
        assert!(!info.header_crc16_valid);
    }

    #[test]
    fn banner_titles_primary_is_english() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.nds");
        std::fs::write(&path, make_nds_info_fixture(NdsSecureAreaState::Decrypted)).unwrap();

        let info = read_info(&path).unwrap();
        let banner = info.banner.expect("banner present");
        assert_eq!(banner.titles.primary(), Some("Test Game"));
        assert!(banner.banner_crc16_valid);
    }

    #[test]
    fn icon_decodes_to_32x32_png() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.nds");
        std::fs::write(&path, make_nds_info_fixture(NdsSecureAreaState::Decrypted)).unwrap();

        let info = read_info(&path).unwrap();
        let icon = info.banner.expect("banner present").icon;
        assert_eq!(icon.width, 32);
        assert_eq!(icon.height, 32);
        assert_eq!(&icon.png_bytes[..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
    }

    #[test]
    fn no_banner_pointer_yields_no_banner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.nds");
        std::fs::write(&path, make_nds_info_fixture_no_banner()).unwrap();

        let info = read_info(&path).unwrap();
        assert!(info.banner.is_none());
    }

    #[test]
    fn secure_area_not_present_when_arm9_offset_outside_window() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.nds");
        std::fs::write(&path, make_nds_info_fixture(NdsSecureAreaState::NotPresent)).unwrap();

        let info = read_info(&path).unwrap();
        assert_eq!(info.secure_area, NdsSecureAreaState::NotPresent);
    }

    #[test]
    fn secure_area_detected_encrypted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.nds");
        std::fs::write(&path, make_nds_info_fixture(NdsSecureAreaState::Encrypted)).unwrap();

        let info = read_info(&path).unwrap();
        assert_eq!(info.secure_area, NdsSecureAreaState::Encrypted);
    }

    #[test]
    fn secure_area_detected_decrypted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.nds");
        std::fs::write(&path, make_nds_info_fixture(NdsSecureAreaState::Decrypted)).unwrap();

        let info = read_info(&path).unwrap();
        assert_eq!(info.secure_area, NdsSecureAreaState::Decrypted);
    }
}
