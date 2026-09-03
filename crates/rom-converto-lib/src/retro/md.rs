//! Mega Drive / Genesis header parsing, for both raw dumps and the
//! interleaved SMD copier format.

use super::ascii_trim;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

const SMD_HEADER_LEN: usize = 512;
const SMD_BLOCK_LEN: usize = 16 * 1024;
const HEADER_END: usize = 0x200;

/// Fields of the Mega Drive cartridge header, with the checksum recomputed
/// over the ROM body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MdInfo {
    pub format: String,
    pub console: String,
    pub copyright: String,
    pub domestic_title: String,
    pub overseas_title: String,
    pub serial: String,
    pub device_support: Vec<String>,
    pub region: Vec<String>,
    pub rom_start: u32,
    pub rom_end: u32,
    pub checksum: u16,
    pub computed_checksum: u16,
    pub checksum_valid: bool,
}

/// Parses the Mega Drive header at 0x100, deinterleaving first when `data`
/// is an SMD image.
///
/// # Errors
/// Returns an error when `data` is shorter than the header or the console
/// name field does not identify a Sega system.
pub fn parse(data: &[u8]) -> Result<MdInfo> {
    let (format, rom) = if is_smd(data) {
        ("SMD", deinterleave_smd(data))
    } else {
        ("Raw", data.to_vec())
    };

    if rom.len() < HEADER_END {
        return Err(anyhow!("md: file shorter than the 0x200-byte header"));
    }
    let console = ascii_trim(&rom[0x100..0x110]);
    if !console.contains("SEGA") {
        return Err(anyhow!("md: console field does not name a Sega system"));
    }

    let checksum = u16::from_be_bytes([rom[0x18E], rom[0x18F]]);
    let computed_checksum = rom[HEADER_END..]
        .as_chunks::<2>()
        .0
        .iter()
        .fold(0u16, |acc, w| acc.wrapping_add(u16::from_be_bytes(*w)));

    Ok(MdInfo {
        format: format.to_string(),
        console,
        copyright: ascii_trim(&rom[0x110..0x120]),
        domestic_title: collapse(&rom[0x120..0x150]),
        overseas_title: collapse(&rom[0x150..0x180]),
        serial: ascii_trim(&rom[0x180..0x18E]),
        device_support: device_support(&rom[0x190..0x1A0]),
        region: region(&rom[0x1F0..HEADER_END]),
        rom_start: u32::from_be_bytes([rom[0x1A0], rom[0x1A1], rom[0x1A2], rom[0x1A3]]),
        rom_end: u32::from_be_bytes([rom[0x1A4], rom[0x1A5], rom[0x1A6], rom[0x1A7]]),
        checksum,
        computed_checksum,
        checksum_valid: checksum == computed_checksum,
    })
}

/// An SMD image is a 512-byte copier header followed by whole 16 KiB
/// blocks, with 0xAA 0xBB at offset 8 marking the format.
fn is_smd(data: &[u8]) -> bool {
    data.len() > SMD_HEADER_LEN
        && (data.len() - SMD_HEADER_LEN).is_multiple_of(SMD_BLOCK_LEN)
        && data[8] == 0xAA
        && data[9] == 0xBB
}

/// Each SMD block holds its odd bytes first, then its even bytes.
fn deinterleave_smd(data: &[u8]) -> Vec<u8> {
    let body = &data[SMD_HEADER_LEN..];
    let mut out = Vec::with_capacity(body.len());
    for block in body.chunks(SMD_BLOCK_LEN) {
        let (odd, even) = block.split_at(block.len() / 2);
        for (o, e) in odd.iter().zip(even) {
            out.push(*e);
            out.push(*o);
        }
    }
    out
}

/// Trims a fixed-width title field and squeezes its internal padding runs
/// down to single spaces.
pub(super) fn collapse(bytes: &[u8]) -> String {
    ascii_trim(bytes)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn device_support(bytes: &[u8]) -> Vec<String> {
    bytes
        .iter()
        .filter_map(|&b| {
            Some(
                match b {
                    b'J' => "3-button controller",
                    b'6' => "6-button controller",
                    b'0' => "Master System controller",
                    b'A' => "Analog joystick",
                    b'4' => "Multitap",
                    b'G' => "Light gun",
                    b'L' => "Activator",
                    b'M' => "Mouse",
                    b'B' => "Trackball",
                    b'T' => "Tablet",
                    b'V' => "Paddle",
                    b'K' => "Keyboard",
                    b'R' => "RS-232",
                    b'P' => "Printer",
                    b'C' => "CD-ROM",
                    b'F' => "Floppy drive",
                    b'D' => "Download",
                    _ => return None,
                }
                .to_string(),
            )
        })
        .collect()
}

/// Decodes the region field, which is either old-style region letters or a
/// single new-style hex digit whose bits select the regions.
pub(super) fn region(bytes: &[u8]) -> Vec<String> {
    let field = ascii_trim(bytes);
    if !field.is_empty() && field.chars().all(|c| matches!(c, 'J' | 'U' | 'E')) {
        return field
            .chars()
            .map(|c| {
                match c {
                    'J' => "Japan",
                    'U' => "Americas",
                    _ => "Europe",
                }
                .to_string()
            })
            .collect();
    }
    let Some(bits) = field.chars().next().and_then(|c| c.to_digit(16)) else {
        return Vec::new();
    };
    [(1, "Japan"), (4, "Americas"), (8, "Europe")]
        .into_iter()
        .filter(|(mask, _)| bits & mask != 0)
        .map(|(_, name)| name.to_string())
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a 32 KiB raw Mega Drive image with a filled header and a
    /// matching checksum.
    pub(crate) fn fixture() -> Vec<u8> {
        let mut rom = vec![0u8; 32 * 1024];
        rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
        rom[0x110..0x120].copy_from_slice(b"(C)TEST 1991.APR");
        rom[0x120..0x150].fill(b' ');
        rom[0x120..0x12D].copy_from_slice(b"DOMESTIC  ONE");
        rom[0x150..0x180].fill(b' ');
        rom[0x150..0x15C].copy_from_slice(b"OVERSEAS ONE");
        rom[0x180..0x18E].copy_from_slice(b"GM 00001009-00");
        rom[0x190..0x1A0].fill(b' ');
        rom[0x190..0x192].copy_from_slice(b"J6");
        rom[0x1A4..0x1A8].copy_from_slice(&0x7FFFu32.to_be_bytes());
        rom[0x1F0..0x1F3].copy_from_slice(b"JUE");
        for (i, b) in rom[HEADER_END..].iter_mut().enumerate() {
            *b = (i % 253) as u8;
        }

        let checksum = rom[HEADER_END..]
            .as_chunks::<2>()
            .0
            .iter()
            .fold(0u16, |acc, w| acc.wrapping_add(u16::from_be_bytes(*w)));
        rom[0x18E..0x190].copy_from_slice(&checksum.to_be_bytes());
        rom
    }

    /// Interleaves a raw image into SMD form.
    fn to_smd(raw: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; SMD_HEADER_LEN];
        out[0] = (raw.len() / SMD_BLOCK_LEN) as u8;
        out[8] = 0xAA;
        out[9] = 0xBB;
        for block in raw.chunks(SMD_BLOCK_LEN) {
            let half = block.len() / 2;
            let mut odd = Vec::with_capacity(half);
            let mut even = Vec::with_capacity(half);
            for pair in block.chunks_exact(2) {
                even.push(pair[0]);
                odd.push(pair[1]);
            }
            out.extend_from_slice(&odd);
            out.extend_from_slice(&even);
        }
        out
    }

    #[test]
    fn reads_raw_header() {
        let info = parse(&fixture()).unwrap();
        assert_eq!(info.format, "Raw");
        assert_eq!(info.console, "SEGA MEGA DRIVE");
        assert_eq!(info.copyright, "(C)TEST 1991.APR");
        assert_eq!(info.domestic_title, "DOMESTIC ONE");
        assert_eq!(info.overseas_title, "OVERSEAS ONE");
        assert_eq!(info.serial, "GM 00001009-00");
        assert_eq!(
            info.device_support,
            ["3-button controller", "6-button controller"]
        );
        assert_eq!(info.region, ["Japan", "Americas", "Europe"]);
        assert_eq!(info.rom_end, 0x7FFF);
        assert!(info.checksum_valid);
    }

    #[test]
    fn reads_smd_image() {
        let info = parse(&to_smd(&fixture())).unwrap();
        assert_eq!(info.format, "SMD");
        assert_eq!(info.serial, "GM 00001009-00");
        assert!(info.checksum_valid);
    }

    #[test]
    fn decodes_new_style_region() {
        let mut rom = fixture();
        rom[0x1F0..0x1F3].copy_from_slice(b"F  ");
        assert_eq!(parse(&rom).unwrap().region, ["Japan", "Americas", "Europe"]);
    }

    #[test]
    fn flags_corrupted_checksum() {
        let mut rom = fixture();
        rom[0x400] ^= 0xFF;
        assert!(!parse(&rom).unwrap().checksum_valid);
    }

    #[test]
    fn rejects_non_sega_header() {
        let mut rom = fixture();
        rom[0x100..0x110].fill(b'X');
        assert!(parse(&rom).is_err());
        assert!(parse(&rom[..0x100]).is_err());
    }
}
