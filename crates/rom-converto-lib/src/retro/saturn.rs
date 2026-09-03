//! Sega Saturn IP header parsing, read from the first sector of the first
//! data track.

use super::{SegaDiscSystem, ascii_trim, probe_sega_disc};
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

/// Fields of the Saturn IP header, with the area and peripheral fields
/// kept raw alongside the symbols decoded from them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaturnInfo {
    pub sector_size: u32,
    pub hardware_id: String,
    pub maker_id: String,
    pub product_number: String,
    pub version: String,
    pub release_date: String,
    pub device_info: String,
    pub area_symbols: String,
    pub regions: Vec<String>,
    pub peripheral_symbols: String,
    pub peripherals: Vec<String>,
    pub title: String,
}

/// Parses the Saturn IP header out of `head`, the first sector of a disc
/// image in either the cooked or raw MODE1 layout.
///
/// # Errors
/// Returns an error when the sector carries no `SEGA SEGASATURN `
/// hardware id, or is too short to hold the header.
pub fn parse(head: &[u8]) -> Result<SaturnInfo> {
    let found = probe_sega_disc(head)
        .filter(|h| h.system == SegaDiscSystem::SegaSaturn)
        .ok_or_else(|| anyhow!("saturn: first sector carries no \"SEGA SEGASATURN \" id"))?;
    let ip = head
        .get(found.offset..found.offset + 0xD0)
        .ok_or_else(|| anyhow!("saturn: first sector is shorter than the IP header"))?;

    let area_symbols = ascii_trim(&ip[0x40..0x4A]);
    let peripheral_symbols = ascii_trim(&ip[0x50..0x60]);

    Ok(SaturnInfo {
        sector_size: found.sector_size,
        hardware_id: ascii_trim(&ip[0x00..0x10]),
        maker_id: ascii_trim(&ip[0x10..0x20]),
        product_number: ascii_trim(&ip[0x20..0x2A]),
        version: ascii_trim(&ip[0x2A..0x30]),
        release_date: ascii_trim(&ip[0x30..0x38]),
        device_info: ascii_trim(&ip[0x38..0x40]),
        regions: regions(&area_symbols),
        area_symbols,
        peripherals: peripherals(&peripheral_symbols),
        peripheral_symbols,
        title: super::md::collapse(&ip[0x60..0xD0]),
    })
}

fn regions(symbols: &str) -> Vec<String> {
    symbols
        .chars()
        .filter_map(|c| {
            Some(
                match c {
                    'J' => "Japan",
                    'T' => "Asia NTSC",
                    'U' => "North America",
                    'B' => "Central/South America NTSC",
                    'K' => "Korea",
                    'A' => "Asia PAL",
                    'E' => "Europe",
                    'L' => "Central/South America PAL",
                    _ => return None,
                }
                .to_string(),
            )
        })
        .collect()
}

fn peripherals(symbols: &str) -> Vec<String> {
    symbols
        .chars()
        .filter_map(|c| {
            Some(
                match c {
                    'J' => "Control pad",
                    'A' => "Analog controller",
                    'M' => "Mouse",
                    'K' => "Keyboard",
                    'S' => "Steering controller",
                    'T' => "Multitap",
                    'G' => "Virtua Gun",
                    'F' => "Floppy drive",
                    _ => return None,
                }
                .to_string(),
            )
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a 2048-byte cooked first sector with a filled IP header.
    pub(crate) fn cooked_sector() -> Vec<u8> {
        let mut ip = vec![b' '; 2048];
        ip[0x00..0x10].copy_from_slice(b"SEGA SEGASATURN ");
        ip[0x10..0x20].copy_from_slice(b"SEGA TP T-000   ");
        ip[0x20..0x2A].copy_from_slice(b"T-0001G   ");
        ip[0x2A..0x30].copy_from_slice(b"V1.000");
        ip[0x30..0x38].copy_from_slice(b"19950301");
        ip[0x38..0x40].copy_from_slice(b"CD-1/1  ");
        ip[0x40..0x4A].copy_from_slice(b"JTUE      ");
        ip[0x50..0x60].copy_from_slice(b"JAM             ");
        ip[0x60..0xD0].fill(b' ');
        ip[0x60..0x69].copy_from_slice(b"TEST GAME");
        ip
    }

    /// Wraps a cooked sector in the 2352-byte raw MODE1 layout.
    pub(crate) fn raw_sector() -> Vec<u8> {
        let mut out = vec![0u8; 2352];
        out[1..12].fill(0xFF);
        out[0x10..0x10 + 2048].copy_from_slice(&cooked_sector());
        out
    }

    #[test]
    fn reads_cooked_sector() {
        let info = parse(&cooked_sector()).unwrap();
        assert_eq!(info.sector_size, 2048);
        assert_eq!(info.hardware_id, "SEGA SEGASATURN");
        assert_eq!(info.maker_id, "SEGA TP T-000");
        assert_eq!(info.product_number, "T-0001G");
        assert_eq!(info.version, "V1.000");
        assert_eq!(info.release_date, "19950301");
        assert_eq!(info.device_info, "CD-1/1");
        assert_eq!(info.area_symbols, "JTUE");
        assert_eq!(
            info.regions,
            ["Japan", "Asia NTSC", "North America", "Europe"]
        );
        assert_eq!(info.peripheral_symbols, "JAM");
        assert_eq!(
            info.peripherals,
            ["Control pad", "Analog controller", "Mouse"]
        );
        assert_eq!(info.title, "TEST GAME");
    }

    #[test]
    fn reads_raw_mode1_sector() {
        let info = parse(&raw_sector()).unwrap();
        assert_eq!(info.sector_size, 2352);
        assert_eq!(info.product_number, "T-0001G");
        assert_eq!(info.title, "TEST GAME");
    }

    #[test]
    fn reads_raw_mode2_sector() {
        let mut out = vec![0u8; 2352];
        out[1..12].fill(0xFF);
        out[0x18..0x18 + 2048].copy_from_slice(&cooked_sector());
        let info = parse(&out).unwrap();
        assert_eq!(info.sector_size, 2352);
        assert_eq!(info.title, "TEST GAME");
    }

    #[test]
    fn rejects_wrong_hardware_id() {
        let mut sector = cooked_sector();
        sector[0..16].copy_from_slice(b"SEGA SEGAKATANA ");
        assert!(parse(&sector).is_err());
    }

    #[test]
    fn rejects_truncated_sector() {
        assert!(parse(&cooked_sector()[..0x80]).is_err());
    }
}
