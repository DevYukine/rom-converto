//! iNES and NES 2.0 header parsing. Neither variant defines a checksum,
//! so no checksum fields are reported.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

const MAGIC: [u8; 4] = *b"NES\x1a";

/// Header fields of an iNES or NES 2.0 image.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NesInfo {
    pub nes2: bool,
    pub prg_rom_bytes: u64,
    pub chr_rom_bytes: u64,
    pub mapper: u16,
    pub submapper: Option<u8>,
    pub mirroring: String,
    pub battery: bool,
    pub trainer: bool,
    pub four_screen: bool,
    pub console_type: String,
    pub timing: String,
    pub prg_ram_bytes: Option<u32>,
    pub prg_nvram_bytes: Option<u32>,
    pub chr_ram_bytes: Option<u32>,
    pub chr_nvram_bytes: Option<u32>,
}

/// Parses the 16-byte iNES/NES 2.0 header at the start of `data`.
///
/// # Errors
/// Returns an error when `data` is shorter than the header or does not
/// start with the iNES magic.
pub fn parse(data: &[u8]) -> Result<NesInfo> {
    let h: [u8; 16] = data
        .get(..16)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| anyhow!("nes: file shorter than the 16-byte iNES header"))?;
    if h[..4] != MAGIC {
        return Err(anyhow!("nes: bad magic, expected \"NES\\x1a\""));
    }

    let nes2 = h[7] & 0x0C == 0x08;
    let (prg_msb, chr_msb) = if nes2 {
        (h[9] & 0x0F, h[9] >> 4)
    } else {
        (0, 0)
    };

    let mut mapper = u16::from(h[6] >> 4) | u16::from(h[7] & 0xF0);
    if nes2 {
        mapper |= u16::from(h[8] & 0x0F) << 8;
    }

    Ok(NesInfo {
        nes2,
        prg_rom_bytes: rom_size(h[4], prg_msb, 16 * 1024),
        chr_rom_bytes: rom_size(h[5], chr_msb, 8 * 1024),
        mapper,
        submapper: nes2.then_some(h[8] >> 4),
        mirroring: if h[6] & 0x01 != 0 {
            "vertical".to_string()
        } else {
            "horizontal".to_string()
        },
        battery: h[6] & 0x02 != 0,
        trainer: h[6] & 0x04 != 0,
        four_screen: h[6] & 0x08 != 0,
        console_type: console_type(h[7] & 0x03).to_string(),
        timing: if nes2 {
            timing(h[12] & 0x03).to_string()
        } else if h[9] & 0x01 != 0 {
            "PAL".to_string()
        } else {
            "NTSC".to_string()
        },
        prg_ram_bytes: nes2.then_some(ram_size(h[10] & 0x0F)),
        prg_nvram_bytes: nes2.then_some(ram_size(h[10] >> 4)),
        chr_ram_bytes: nes2.then_some(ram_size(h[11] & 0x0F)),
        chr_nvram_bytes: nes2.then_some(ram_size(h[11] >> 4)),
    })
}

/// NES 2.0 exponent-multiplier form kicks in when the size MSB nibble is
/// 0xF, in which case the LSB byte carries `2^E * (2M + 1)` bytes.
fn rom_size(lsb: u8, msb: u8, unit: u64) -> u64 {
    if msb == 0x0F {
        let exponent = u32::from(lsb >> 2);
        let multiplier = u64::from(lsb & 0x03) * 2 + 1;
        1u64.checked_shl(exponent)
            .unwrap_or(0)
            .saturating_mul(multiplier)
    } else {
        ((u64::from(msb) << 8) | u64::from(lsb)) * unit
    }
}

fn ram_size(shift: u8) -> u32 {
    if shift == 0 { 0 } else { 64u32 << shift }
}

fn console_type(code: u8) -> &'static str {
    match code {
        0 => "Nintendo Entertainment System",
        1 => "Nintendo Vs. System",
        2 => "Nintendo Playchoice 10",
        _ => "Extended",
    }
}

fn timing(code: u8) -> &'static str {
    match code {
        0 => "NTSC",
        1 => "PAL",
        2 => "Multiple region",
        _ => "Dendy",
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a header-only iNES image with the given flag bytes.
    pub(crate) fn fixture(flags6: u8, flags7: u8, tail: [u8; 8]) -> Vec<u8> {
        let mut rom = vec![0u8; 16];
        rom[..4].copy_from_slice(&MAGIC);
        rom[4] = 2;
        rom[5] = 1;
        rom[6] = flags6;
        rom[7] = flags7;
        rom[8..16].copy_from_slice(&tail);
        rom
    }

    #[test]
    fn reads_ines_header() {
        let info = parse(&fixture(0x03, 0x10, [0, 1, 0, 0, 0, 0, 0, 0])).unwrap();
        assert!(!info.nes2);
        assert_eq!(info.prg_rom_bytes, 32 * 1024);
        assert_eq!(info.chr_rom_bytes, 8 * 1024);
        assert_eq!(info.mapper, 0x10);
        assert_eq!(info.submapper, None);
        assert_eq!(info.mirroring, "vertical");
        assert!(info.battery);
        assert!(!info.trainer);
        assert_eq!(info.console_type, "Nintendo Entertainment System");
        assert_eq!(info.timing, "PAL");
        assert_eq!(info.prg_ram_bytes, None);
    }

    #[test]
    fn reads_nes2_msb_submapper_and_ram() {
        // mapper 0x312, submapper 5, PRG MSB 3, CHR MSB 0, PRG-NVRAM 8 KiB.
        let info = parse(&fixture(
            0x00,
            0x58,
            [0x53, 0x03, 0x70, 0x00, 0x03, 0, 0, 0],
        ))
        .unwrap();
        assert!(info.nes2);
        assert_eq!(info.mapper, 0x350);
        assert_eq!(info.submapper, Some(5));
        assert_eq!(info.prg_rom_bytes, 0x302 * 16 * 1024);
        assert_eq!(info.chr_rom_bytes, 8 * 1024);
        assert_eq!(info.prg_ram_bytes, Some(0));
        assert_eq!(info.prg_nvram_bytes, Some(64 << 7));
        assert_eq!(info.timing, "Dendy");
    }

    #[test]
    fn reads_nes2_exponent_rom_size() {
        // PRG MSB nibble 0xF selects exponent form: 2^5 * (2*1 + 1) bytes.
        let mut rom = fixture(0x00, 0x08, [0x00, 0x0F, 0, 0, 0, 0, 0, 0]);
        rom[4] = (5 << 2) | 1;
        assert_eq!(parse(&rom).unwrap().prg_rom_bytes, 32 * 3);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut rom = fixture(0, 0, [0; 8]);
        rom[0] = b'X';
        assert!(parse(&rom).is_err());
        assert!(parse(&rom[..8]).is_err());
    }
}
