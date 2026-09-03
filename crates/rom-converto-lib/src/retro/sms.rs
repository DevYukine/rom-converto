//! Master System and Game Gear header parsing. Both consoles share the
//! same `TMR SEGA` header; the caller picks the system by extension.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

const MAGIC: &[u8; 8] = b"TMR SEGA";

/// The header sits at whichever of these offsets holds the magic, largest
/// first, since a 32 KiB image can also mirror into the smaller slots.
const CANDIDATES: [usize; 3] = [0x7FF0, 0x3FF0, 0x1FF0];

/// Fields of the Sega 8-bit cartridge header, with the checksum recomputed
/// over the range the size nibble implies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmsInfo {
    pub header_offset: u64,
    pub product_code: u32,
    pub version: u8,
    pub region_code: u8,
    pub region: Option<String>,
    pub rom_size_code: u8,
    pub rom_size_kb: Option<u32>,
    pub checksum: u16,
    pub computed_checksum: u16,
    pub checksum_valid: bool,
}

/// Locates and parses the `TMR SEGA` header in `data`.
///
/// # Errors
/// Returns an error when none of the three header offsets holds the magic.
pub fn parse(data: &[u8]) -> Result<SmsInfo> {
    let at = CANDIDATES
        .into_iter()
        .find(|&at| data.get(at..at + 16).is_some_and(|h| &h[..8] == MAGIC))
        .ok_or_else(|| anyhow!("sms: no \"TMR SEGA\" header found"))?;
    let header = &data[at..at + 16];

    let region_code = header[0x0F] >> 4;
    let rom_size_code = header[0x0F] & 0x0F;
    let checksum = u16::from_le_bytes([header[0x0A], header[0x0B]]);
    let computed_checksum = compute_checksum(data, at, rom_size_code);

    Ok(SmsInfo {
        header_offset: at as u64,
        product_code: u32::from(header[0x0E] >> 4) * 10_000
            + bcd(header[0x0D]) * 100
            + bcd(header[0x0C]),
        version: header[0x0E] & 0x0F,
        region_code,
        region: region(region_code).map(str::to_string),
        rom_size_code,
        rom_size_kb: rom_size_kb(rom_size_code),
        checksum,
        computed_checksum,
        checksum_valid: checksum == computed_checksum,
    })
}

fn bcd(byte: u8) -> u32 {
    u32::from(byte >> 4) * 10 + u32::from(byte & 0x0F)
}

/// Sums everything below the header, then everything from 0x8000 up to the
/// limit the size nibble sets. Ranges past the end of the file are clipped.
fn compute_checksum(data: &[u8], header_offset: usize, rom_size_code: u8) -> u16 {
    let second_end = match rom_size_code {
        0xA..=0xC => None,
        0xD => Some(0x0C000),
        0xE => Some(0x10000),
        0xF => Some(0x20000),
        0x0 => Some(0x40000),
        0x1 => Some(0x80000),
        _ => Some(0x100000),
    };

    let sum = |range: &[u8]| {
        range
            .iter()
            .fold(0u16, |acc, &b| acc.wrapping_add(u16::from(b)))
    };
    let second = second_end
        .filter(|_| data.len() > 0x8000)
        .map(|end| sum(&data[0x8000..end.min(data.len())]))
        .unwrap_or(0);
    sum(&data[..header_offset.min(data.len())]).wrapping_add(second)
}

fn region(code: u8) -> Option<&'static str> {
    Some(match code {
        0x3 => "Master System (Japan)",
        0x4 => "Master System (Export)",
        0x5 => "Game Gear (Japan)",
        0x6 => "Game Gear (Export)",
        0x7 => "Game Gear (International)",
        _ => return None,
    })
}

fn rom_size_kb(code: u8) -> Option<u32> {
    Some(match code {
        0xA => 8,
        0xB => 16,
        0xC => 32,
        0xD => 48,
        0xE => 64,
        0xF => 128,
        0x0 => 256,
        0x1 => 512,
        0x2 => 1024,
        _ => return None,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a 32 KiB image with the header at 0x7FF0 and a matching
    /// checksum.
    pub(crate) fn fixture(region_and_size: u8) -> Vec<u8> {
        let mut rom = vec![0u8; 32 * 1024];
        for (i, b) in rom.iter_mut().enumerate() {
            *b = (i % 241) as u8;
        }
        let at = 0x7FF0;
        rom[at..at + 16].fill(0);
        rom[at..at + 8].copy_from_slice(MAGIC);
        rom[at + 0x0C] = 0x34;
        rom[at + 0x0D] = 0x12;
        rom[at + 0x0E] = 0x51;
        rom[at + 0x0F] = region_and_size;

        let checksum = compute_checksum(&rom, at, region_and_size & 0x0F);
        rom[at + 0x0A..at + 0x0C].copy_from_slice(&checksum.to_le_bytes());
        rom
    }

    #[test]
    fn reads_header() {
        let info = parse(&fixture(0x4C)).unwrap();
        assert_eq!(info.header_offset, 0x7FF0);
        assert_eq!(info.product_code, 51234);
        assert_eq!(info.version, 1);
        assert_eq!(info.region.as_deref(), Some("Master System (Export)"));
        assert_eq!(info.rom_size_kb, Some(32));
        assert!(info.checksum_valid);
    }

    #[test]
    fn decodes_game_gear_region() {
        let info = parse(&fixture(0x7C)).unwrap();
        assert_eq!(info.region.as_deref(), Some("Game Gear (International)"));
    }

    #[test]
    fn flags_corrupted_checksum() {
        let mut rom = fixture(0x4C);
        rom[0x100] ^= 0xFF;
        assert!(!parse(&rom).unwrap().checksum_valid);
    }

    #[test]
    fn rejects_missing_magic() {
        let mut rom = fixture(0x4C);
        rom[0x7FF0] = b'X';
        assert!(parse(&rom).is_err());
    }
}
