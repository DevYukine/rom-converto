//! Game Boy and Game Boy Color cartridge header parsing.

use super::ascii_trim;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

const HEADER_END: usize = 0x150;

/// The Nintendo bitmap the boot ROM compares against, at 0x104.
const NINTENDO_LOGO: [u8; 48] = [
    0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00, 0x0D,
    0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD, 0xD9, 0x99,
    0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E, 0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E,
];

/// Fields of the Game Boy cartridge header, with both checksums the format
/// defines recomputed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GbInfo {
    pub logo_valid: bool,
    pub title: String,
    pub manufacturer_code: Option<String>,
    pub cgb_flag: u8,
    pub cgb: Option<String>,
    pub sgb_flag: u8,
    pub cart_type: u8,
    pub cart_type_name: Option<String>,
    pub rom_bytes: Option<u32>,
    pub ram_bytes: Option<u32>,
    pub destination: u8,
    pub destination_name: Option<String>,
    pub licensee: String,
    pub version: u8,
    pub header_checksum: u8,
    pub computed_header_checksum: u8,
    pub header_checksum_valid: bool,
    pub global_checksum: u16,
    pub computed_global_checksum: u16,
    pub global_checksum_valid: bool,
}

/// Parses the Game Boy cartridge header at 0x100.
///
/// # Errors
/// Returns an error when `data` is shorter than the header.
pub fn parse(data: &[u8]) -> Result<GbInfo> {
    if data.len() < HEADER_END {
        return Err(anyhow!("gb: file shorter than the 0x150-byte header"));
    }

    let cgb_flag = data[0x143];
    let cgb_aware = matches!(cgb_flag, 0x80 | 0xC0);
    let old_licensee = data[0x14B];

    let header_checksum = data[0x14D];
    let computed_header_checksum = data[0x134..0x14D]
        .iter()
        .fold(0u8, |acc, &b| acc.wrapping_sub(b).wrapping_sub(1));

    let global_checksum = u16::from_be_bytes([data[0x14E], data[0x14F]]);
    let computed_global_checksum = data
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 0x14E && *i != 0x14F)
        .fold(0u16, |acc, (_, &b)| acc.wrapping_add(u16::from(b)));

    Ok(GbInfo {
        logo_valid: data[0x104..0x134] == NINTENDO_LOGO,
        title: if cgb_aware {
            ascii_trim(&data[0x134..0x13F])
        } else {
            ascii_trim(&data[0x134..0x143])
        },
        manufacturer_code: cgb_aware.then(|| ascii_trim(&data[0x13F..0x143])),
        cgb_flag,
        cgb: match cgb_flag {
            0x80 => Some("compatible".to_string()),
            0xC0 => Some("exclusive".to_string()),
            _ => None,
        },
        sgb_flag: data[0x146],
        cart_type: data[0x147],
        cart_type_name: cart_type(data[0x147]).map(str::to_string),
        rom_bytes: (data[0x148] <= 8).then(|| (32 * 1024u32) << data[0x148]),
        ram_bytes: ram_bytes(data[0x149]),
        destination: data[0x14A],
        destination_name: match data[0x14A] {
            0x00 => Some("Japan".to_string()),
            0x01 => Some("Overseas".to_string()),
            _ => None,
        },
        licensee: if old_licensee == 0x33 {
            ascii_trim(&data[0x144..0x146])
        } else {
            format!("{old_licensee:02X}")
        },
        version: data[0x14C],
        header_checksum,
        computed_header_checksum,
        header_checksum_valid: header_checksum == computed_header_checksum,
        global_checksum,
        computed_global_checksum,
        global_checksum_valid: global_checksum == computed_global_checksum,
    })
}

fn ram_bytes(code: u8) -> Option<u32> {
    Some(match code {
        0x00 => 0,
        0x02 => 8 * 1024,
        0x03 => 32 * 1024,
        0x04 => 128 * 1024,
        0x05 => 64 * 1024,
        _ => return None,
    })
}

fn cart_type(code: u8) -> Option<&'static str> {
    Some(match code {
        0x00 => "ROM ONLY",
        0x01 => "MBC1",
        0x02 => "MBC1+RAM",
        0x03 => "MBC1+RAM+BATTERY",
        0x05 => "MBC2",
        0x06 => "MBC2+BATTERY",
        0x08 => "ROM+RAM",
        0x09 => "ROM+RAM+BATTERY",
        0x0B => "MMM01",
        0x0C => "MMM01+RAM",
        0x0D => "MMM01+RAM+BATTERY",
        0x0F => "MBC3+TIMER+BATTERY",
        0x10 => "MBC3+TIMER+RAM+BATTERY",
        0x11 => "MBC3",
        0x12 => "MBC3+RAM",
        0x13 => "MBC3+RAM+BATTERY",
        0x19 => "MBC5",
        0x1A => "MBC5+RAM",
        0x1B => "MBC5+RAM+BATTERY",
        0x1C => "MBC5+RUMBLE",
        0x1D => "MBC5+RUMBLE+RAM",
        0x1E => "MBC5+RUMBLE+RAM+BATTERY",
        0x20 => "MBC6",
        0x22 => "MBC7+SENSOR+RUMBLE+RAM+BATTERY",
        0xFC => "POCKET CAMERA",
        0xFD => "BANDAI TAMA5",
        0xFE => "HuC3",
        0xFF => "HuC1+RAM+BATTERY",
        _ => return None,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a 32 KiB Game Boy image with a valid logo and both checksums
    /// fixed up.
    pub(crate) fn fixture(cgb_flag: u8) -> Vec<u8> {
        let mut rom = vec![0u8; 32 * 1024];
        rom[0x104..0x134].copy_from_slice(&NINTENDO_LOGO);
        rom[0x134..0x143].fill(b' ');
        rom[0x134..0x13B].copy_from_slice(b"TESTROM");
        if cgb_flag != 0 {
            rom[0x13F..0x143].copy_from_slice(b"ABCD");
        }
        rom[0x143] = cgb_flag;
        rom[0x144..0x146].copy_from_slice(b"01");
        rom[0x146] = 0x03;
        rom[0x147] = 0x1B;
        rom[0x148] = 0x00;
        rom[0x149] = 0x03;
        rom[0x14A] = 0x01;
        rom[0x14B] = 0x33;
        rom[0x14C] = 0x02;

        rom[0x14D] = rom[0x134..0x14D]
            .iter()
            .fold(0u8, |acc, &b| acc.wrapping_sub(b).wrapping_sub(1));
        let global = rom
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 0x14E && *i != 0x14F)
            .fold(0u16, |acc, (_, &b)| acc.wrapping_add(u16::from(b)));
        rom[0x14E..0x150].copy_from_slice(&global.to_be_bytes());
        rom
    }

    #[test]
    fn reads_dmg_header() {
        let info = parse(&fixture(0x00)).unwrap();
        assert!(info.logo_valid);
        assert_eq!(info.title, "TESTROM");
        assert_eq!(info.manufacturer_code, None);
        assert_eq!(info.cgb, None);
        assert_eq!(info.sgb_flag, 0x03);
        assert_eq!(info.cart_type_name.as_deref(), Some("MBC5+RAM+BATTERY"));
        assert_eq!(info.rom_bytes, Some(32 * 1024));
        assert_eq!(info.ram_bytes, Some(32 * 1024));
        assert_eq!(info.destination_name.as_deref(), Some("Overseas"));
        assert_eq!(info.licensee, "01");
        assert_eq!(info.version, 2);
        assert!(info.header_checksum_valid);
        assert!(info.global_checksum_valid);
    }

    #[test]
    fn splits_title_and_manufacturer_for_cgb() {
        let info = parse(&fixture(0xC0)).unwrap();
        assert_eq!(info.title, "TESTROM");
        assert_eq!(info.manufacturer_code.as_deref(), Some("ABCD"));
        assert_eq!(info.cgb.as_deref(), Some("exclusive"));
        assert!(info.header_checksum_valid);
    }

    #[test]
    fn flags_corrupted_checksums() {
        let mut rom = fixture(0x00);
        rom[0x134] ^= 0xFF;
        let info = parse(&rom).unwrap();
        assert!(!info.header_checksum_valid);
        assert!(!info.global_checksum_valid);
    }

    #[test]
    fn flags_bad_logo_and_rejects_short_file() {
        let mut rom = fixture(0x00);
        rom[0x104] ^= 0xFF;
        assert!(!parse(&rom).unwrap().logo_valid);
        assert!(parse(&rom[..0x140]).is_err());
    }
}
