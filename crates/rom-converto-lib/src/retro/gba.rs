//! Game Boy Advance cartridge header parsing.

use super::ascii_trim;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

const HEADER_END: usize = 0xC0;

/// The compressed Nintendo bitmap the BIOS checks, at 0x04.
const NINTENDO_LOGO: [u8; 156] = [
    0x24, 0xFF, 0xAE, 0x51, 0x69, 0x9A, 0xA2, 0x21, 0x3D, 0x84, 0x82, 0x0A, 0x84, 0xE4, 0x09, 0xAD,
    0x11, 0x24, 0x8B, 0x98, 0xC0, 0x81, 0x7F, 0x21, 0xA3, 0x52, 0xBE, 0x19, 0x93, 0x09, 0xCE, 0x20,
    0x10, 0x46, 0x4A, 0x4A, 0xF8, 0x27, 0x31, 0xEC, 0x58, 0xC7, 0xE8, 0x33, 0x82, 0xE3, 0xCE, 0xBF,
    0x85, 0xF4, 0xDF, 0x94, 0xCE, 0x4B, 0x09, 0xC1, 0x94, 0x56, 0x8A, 0xC0, 0x13, 0x72, 0xA7, 0xFC,
    0x9F, 0x84, 0x4D, 0x73, 0xA3, 0xCA, 0x9A, 0x61, 0x58, 0x97, 0xA3, 0x27, 0xFC, 0x03, 0x98, 0x76,
    0x23, 0x1D, 0xC7, 0x61, 0x03, 0x04, 0xAE, 0x56, 0xBF, 0x38, 0x84, 0x00, 0x40, 0xA7, 0x0E, 0xFD,
    0xFF, 0x52, 0xFE, 0x03, 0x6F, 0x95, 0x30, 0xF1, 0x97, 0xFB, 0xC0, 0x85, 0x60, 0xD6, 0x80, 0x25,
    0xA9, 0x63, 0xBE, 0x03, 0x01, 0x4E, 0x38, 0xE2, 0xF9, 0xA2, 0x34, 0xFF, 0xBB, 0x3E, 0x03, 0x44,
    0x78, 0x00, 0x90, 0xCB, 0x88, 0x11, 0x3A, 0x94, 0x65, 0xC0, 0x7C, 0x63, 0x87, 0xF0, 0x3C, 0xAF,
    0xD6, 0x25, 0xE4, 0x8B, 0x38, 0x0A, 0xAC, 0x72, 0x21, 0xD4, 0xF8, 0x07,
];

/// Fields of the GBA cartridge header, with the complement check recomputed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GbaInfo {
    pub title: String,
    pub game_code: String,
    pub region: Option<String>,
    pub maker_code: String,
    pub version: u8,
    pub header_checksum: u8,
    pub computed_header_checksum: u8,
    pub header_checksum_valid: bool,
    pub logo_valid: bool,
}

/// Parses the GBA cartridge header at the start of `data`.
///
/// # Errors
/// Returns an error when `data` is shorter than the header or the fixed
/// 0x96 byte at 0xB2 is missing.
pub fn parse(data: &[u8]) -> Result<GbaInfo> {
    if data.len() < HEADER_END {
        return Err(anyhow!("gba: file shorter than the 0xC0-byte header"));
    }
    if data[0xB2] != 0x96 {
        return Err(anyhow!("gba: fixed header byte at 0xB2 is not 0x96"));
    }

    let header_checksum = data[0xBD];
    let computed_header_checksum = data[0xA0..0xBD]
        .iter()
        .fold(0u8, |acc, &b| acc.wrapping_sub(b))
        .wrapping_sub(0x19);

    Ok(GbaInfo {
        title: ascii_trim(&data[0xA0..0xAC]),
        game_code: ascii_trim(&data[0xAC..0xB0]),
        region: region(data[0xAF]).map(str::to_string),
        maker_code: ascii_trim(&data[0xB0..0xB2]),
        version: data[0xBC],
        header_checksum,
        computed_header_checksum,
        header_checksum_valid: header_checksum == computed_header_checksum,
        logo_valid: data[0x04..0xA0] == NINTENDO_LOGO,
    })
}

fn region(code: u8) -> Option<&'static str> {
    Some(match code {
        b'J' => "Japan",
        b'E' => "USA",
        b'P' => "Europe",
        b'D' => "Germany",
        b'F' => "France",
        b'I' => "Italy",
        b'S' => "Spain",
        b'K' => "South Korea",
        _ => return None,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a header-only GBA image with a valid logo and complement check.
    pub(crate) fn fixture() -> Vec<u8> {
        let mut rom = vec![0u8; HEADER_END];
        rom[0x04..0xA0].copy_from_slice(&NINTENDO_LOGO);
        rom[0xA0..0xAC].fill(b' ');
        rom[0xA0..0xA7].copy_from_slice(b"TESTROM");
        rom[0xAC..0xB0].copy_from_slice(b"ATRP");
        rom[0xB0..0xB2].copy_from_slice(b"01");
        rom[0xB2] = 0x96;
        rom[0xBC] = 0x03;
        rom[0xBD] = rom[0xA0..0xBD]
            .iter()
            .fold(0u8, |acc, &b| acc.wrapping_sub(b))
            .wrapping_sub(0x19);
        rom
    }

    #[test]
    fn reads_header() {
        let info = parse(&fixture()).unwrap();
        assert!(info.logo_valid);
        assert_eq!(info.title, "TESTROM");
        assert_eq!(info.game_code, "ATRP");
        assert_eq!(info.region.as_deref(), Some("Europe"));
        assert_eq!(info.maker_code, "01");
        assert_eq!(info.version, 3);
        assert!(info.header_checksum_valid);
    }

    #[test]
    fn flags_corrupted_checksum() {
        let mut rom = fixture();
        rom[0xA0] ^= 0xFF;
        assert!(!parse(&rom).unwrap().header_checksum_valid);
    }

    #[test]
    fn rejects_missing_fixed_byte() {
        let mut rom = fixture();
        rom[0xB2] = 0x00;
        assert!(parse(&rom).is_err());
        assert!(parse(&rom[..0x80]).is_err());
    }
}
