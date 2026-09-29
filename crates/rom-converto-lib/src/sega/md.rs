//! Mega Drive / Genesis header parsing, for both raw dumps and the
//! interleaved SMD copier format.

use crate::util::bytes::ascii_trim;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};

const SMD_HEADER_LEN: usize = 512;
const SMD_BLOCK_LEN: usize = 16 * 1024;
const HEADER_END: usize = 0x200;
const IO_BLOCK_LEN: usize = 64 * 1024;

/// Fields of the Mega Drive cartridge header, with the checksum recomputed
/// over the ROM body.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
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

/// Parses the Mega Drive header and streams its checksum from a seekable image.
pub fn parse_reader(reader: &mut (impl Read + Seek), file_len: u64) -> Result<MdInfo> {
    let mut smd_header = [0; SMD_HEADER_LEN];
    let smd = file_len > SMD_HEADER_LEN as u64
        && (file_len - SMD_HEADER_LEN as u64).is_multiple_of(SMD_BLOCK_LEN as u64)
        && {
            reader.seek(SeekFrom::Start(0))?;
            reader.read_exact(&mut smd_header)?;
            smd_header[8] == 0xAA && smd_header[9] == 0xBB
        };
    let body_start = if smd { SMD_HEADER_LEN as u64 } else { 0 };
    let logical_len = file_len - body_start;
    if logical_len < HEADER_END as u64 {
        return Err(anyhow!("md: file shorter than the 0x200-byte header"));
    }

    let mut header = [0; HEADER_END];
    let mut computed_checksum = 0u16;
    if smd {
        let mut block = [0; SMD_BLOCK_LEN];
        reader.seek(SeekFrom::Start(body_start))?;
        let mut offset = 0u64;
        while offset < logical_len {
            reader.read_exact(&mut block)?;
            let (odd, even) = block.split_at(SMD_BLOCK_LEN / 2);
            if offset == 0 {
                for (i, (&o, &e)) in odd.iter().zip(even).enumerate() {
                    let pair = [e, o];
                    let logical_offset = i * 2;
                    if logical_offset < HEADER_END {
                        header[logical_offset..logical_offset + 2].copy_from_slice(&pair);
                    } else {
                        computed_checksum =
                            computed_checksum.wrapping_add(u16::from_be_bytes(pair));
                    }
                }
            } else {
                for (&o, &e) in odd.iter().zip(even) {
                    computed_checksum = computed_checksum.wrapping_add(u16::from_be_bytes([e, o]));
                }
            }
            offset += SMD_BLOCK_LEN as u64;
        }
    } else {
        reader.seek(SeekFrom::Start(0))?;
        reader.read_exact(&mut header)?;
        let mut buf = [0; IO_BLOCK_LEN];
        let mut offset = HEADER_END as u64;
        let mut pending = None;
        while offset < logical_len {
            let count = ((logical_len - offset).min(buf.len() as u64)) as usize;
            reader.read_exact(&mut buf[..count])?;
            let mut iter = buf[..count].iter().copied();
            if let Some(first) = pending.take() {
                if let Some(second) = iter.next() {
                    computed_checksum =
                        computed_checksum.wrapping_add(u16::from_be_bytes([first, second]));
                } else {
                    pending = Some(first);
                }
            }
            while let Some(first) = iter.next() {
                if let Some(second) = iter.next() {
                    computed_checksum =
                        computed_checksum.wrapping_add(u16::from_be_bytes([first, second]));
                } else {
                    pending = Some(first);
                }
            }
            offset += count as u64;
        }
    }

    let console = ascii_trim(&header[0x100..0x110]);
    if !console.contains("SEGA") {
        return Err(anyhow!("md: console field does not name a Sega system"));
    }
    let checksum = u16::from_be_bytes([header[0x18E], header[0x18F]]);
    Ok(MdInfo {
        format: if smd { "SMD" } else { "Raw" }.to_string(),
        console,
        copyright: ascii_trim(&header[0x110..0x120]),
        domestic_title: collapse(&header[0x120..0x150]),
        overseas_title: collapse(&header[0x150..0x180]),
        serial: ascii_trim(&header[0x180..0x18E]),
        device_support: device_support(&header[0x190..0x1A0]),
        region: region(&header[0x1F0..HEADER_END]),
        rom_start: u32::from_be_bytes(
            header[0x1A0..0x1A4]
                .try_into()
                .expect("fixed-size header contains ROM start"),
        ),
        rom_end: u32::from_be_bytes(
            header[0x1A4..0x1A8]
                .try_into()
                .expect("fixed-size header contains ROM end"),
        ),
        checksum,
        computed_checksum,
        checksum_valid: checksum == computed_checksum,
    })
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

    /// Parses a Mega Drive header from a whole in-memory image; only tests
    /// exercise the non-streaming path since real callers use a seekable
    /// source via [`super::parse_reader`].
    fn parse(data: &[u8]) -> Result<MdInfo> {
        use std::io::Cursor;
        super::parse_reader(&mut Cursor::new(data), data.len() as u64)
    }

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
