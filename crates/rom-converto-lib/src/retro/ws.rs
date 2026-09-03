//! WonderSwan and WonderSwan Color footer parsing. The last 16 bytes hold
//! the V30MZ reset vector followed by the cartridge header.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

const FOOTER_LEN: usize = 16;

/// The reset vector that opens the footer is always a far jump.
const FAR_JMP: u8 = 0xEA;

/// Fields of the WonderSwan cartridge footer, with the checksum recomputed
/// over the ROM body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsInfo {
    pub publisher_id: u8,
    pub color: bool,
    pub game_id: u8,
    pub save_type: u8,
    pub save: Option<String>,
    pub version: u8,
    pub checksum: u16,
    pub computed_checksum: u16,
    pub checksum_valid: bool,
}

/// Parses the WonderSwan footer from the tail of `data`.
///
/// # Errors
/// Returns an error when `data` is shorter than the footer or does not open
/// it with the far-jump reset vector.
pub fn parse(data: &[u8]) -> Result<WsInfo> {
    let base = data
        .len()
        .checked_sub(FOOTER_LEN)
        .ok_or_else(|| anyhow!("ws: file shorter than the 16-byte footer"))?;
    let footer = &data[base..];
    if footer[0] != FAR_JMP {
        return Err(anyhow!("ws: footer does not start with a far-jump vector"));
    }

    let checksum = u16::from_le_bytes([footer[14], footer[15]]);
    let computed_checksum = data[..data.len() - 2]
        .iter()
        .fold(0u16, |acc, &b| acc.wrapping_add(u16::from(b)));

    Ok(WsInfo {
        publisher_id: footer[6],
        color: footer[7] != 0,
        game_id: footer[8],
        save_type: footer[11],
        save: save(footer[11]).map(str::to_string),
        version: footer[9],
        checksum,
        computed_checksum,
        checksum_valid: checksum == computed_checksum,
    })
}

fn save(code: u8) -> Option<&'static str> {
    Some(match code {
        0x00 => "none",
        0x01 => "8 KiB SRAM",
        0x02 => "32 KiB SRAM",
        0x03 => "128 KiB SRAM",
        0x04 => "256 KiB SRAM",
        0x05 => "512 KiB SRAM",
        0x10 => "128 byte EEPROM",
        0x20 => "2 KiB EEPROM",
        0x50 => "1 KiB EEPROM",
        _ => return None,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a 64 KiB image with a filled footer and a matching checksum.
    pub(crate) fn fixture(color: u8) -> Vec<u8> {
        let mut rom = vec![0u8; 64 * 1024];
        for (i, b) in rom.iter_mut().enumerate() {
            *b = (i % 239) as u8;
        }
        let base = rom.len() - FOOTER_LEN;
        rom[base..].fill(0);
        rom[base] = FAR_JMP;
        rom[base + 6] = 0x01;
        rom[base + 7] = color;
        rom[base + 8] = 0x2A;
        rom[base + 9] = 0x02;
        rom[base + 10] = 0x04;
        rom[base + 11] = 0x02;

        let checksum = rom[..rom.len() - 2]
            .iter()
            .fold(0u16, |acc, &b| acc.wrapping_add(u16::from(b)));
        let end = rom.len();
        rom[end - 2..].copy_from_slice(&checksum.to_le_bytes());
        rom
    }

    #[test]
    fn reads_footer() {
        let info = parse(&fixture(0x00)).unwrap();
        assert_eq!(info.publisher_id, 0x01);
        assert!(!info.color);
        assert_eq!(info.game_id, 0x2A);
        assert_eq!(info.version, 0x02);
        assert_eq!(info.save_type, 0x02);
        assert_eq!(info.save.as_deref(), Some("32 KiB SRAM"));
        assert!(info.checksum_valid);
    }

    #[test]
    fn flags_color_system() {
        assert!(parse(&fixture(0x01)).unwrap().color);
    }

    #[test]
    fn flags_corrupted_checksum() {
        let mut rom = fixture(0x00);
        rom[0x100] ^= 0xFF;
        assert!(!parse(&rom).unwrap().checksum_valid);
    }

    #[test]
    fn rejects_missing_reset_vector() {
        let mut rom = fixture(0x00);
        let base = rom.len() - FOOTER_LEN;
        rom[base] = 0x00;
        assert!(parse(&rom).is_err());
        assert!(parse(&[]).is_err());
    }
}
