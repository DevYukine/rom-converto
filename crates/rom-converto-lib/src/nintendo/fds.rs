//! Famicom Disk System image parsing: the optional 16-byte FDS wrapper
//! header plus the disk info block that opens every disk side.

use crate::util::bytes::ascii_trim;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};

const FDS_WRAPPER_MAGIC: &[u8; 4] = b"FDS\x1a";
const FDS_WRAPPER_HEADER_LEN: usize = 16;
const SIDE_LEN: usize = 65500;
const INFO_BLOCK_LEN: usize = 0x38;
const VERIFICATION: &[u8; 14] = b"*NINTENDO-HVC*";

/// An FDS image: the wrapper it arrived in, and one entry per disk side.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
pub struct FdsInfo {
    /// True when the image opens with the 16-byte FDS wrapper header.
    #[serde(rename = "fwnes_header")]
    pub wrapper_header: bool,
    pub side_count: usize,
    pub sides: Vec<FdsSide>,
}

/// The disk info block at the start of one disk side.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
pub struct FdsSide {
    pub licensee_code: u8,
    pub game_name: String,
    pub game_type_code: u8,
    pub game_type: Option<String>,
    pub version: u8,
    pub side_number: u8,
    pub disk_number: u8,
    pub disk_type_code: u8,
    pub disk_type: Option<String>,
    pub boot_read_file_code: u8,
    pub manufacture_date_raw: String,
    pub manufacture_date: Option<String>,
}

/// Parses an FDS image, with or without its 16-byte FDS wrapper header.
///
/// # Errors
/// Returns an error when the image holds no whole disk side, or when a
/// side does not open with the `*NINTENDO-HVC*` disk info block.
pub fn parse_reader(reader: &mut (impl Read + Seek), file_len: u64) -> Result<FdsInfo> {
    let mut prefix = [0; FDS_WRAPPER_HEADER_LEN];
    let has_wrapper = file_len >= FDS_WRAPPER_HEADER_LEN as u64 && {
        reader.seek(SeekFrom::Start(0))?;
        reader.read_exact(&mut prefix)?;
        &prefix[..4] == FDS_WRAPPER_MAGIC
    };
    let body_start = if has_wrapper {
        FDS_WRAPPER_HEADER_LEN as u64
    } else {
        0
    };
    let side_count = if has_wrapper {
        usize::from(prefix[4])
    } else {
        ((file_len - body_start) / SIDE_LEN as u64) as usize
    };
    let whole_sides = (file_len - body_start) / SIDE_LEN as u64;
    let mut sides = Vec::new();
    let mut block = [0; INFO_BLOCK_LEN];
    for index in 0..whole_sides {
        reader.seek(SeekFrom::Start(body_start + index * SIDE_LEN as u64))?;
        reader.read_exact(&mut block)?;
        sides.push(parse_side(&block)?);
    }
    if sides.is_empty() {
        return Err(anyhow!("fds: image holds no disk side"));
    }
    Ok(FdsInfo {
        wrapper_header: has_wrapper,
        side_count,
        sides,
    })
}

fn parse_side(block: &[u8]) -> Result<FdsSide> {
    let b = block
        .get(..INFO_BLOCK_LEN)
        .ok_or_else(|| anyhow!("fds: disk side shorter than the 56-byte disk info block"))?;
    if b[0] != 0x01 {
        return Err(anyhow!("fds: disk side does not open with block type 1"));
    }
    if &b[1..15] != VERIFICATION {
        return Err(anyhow!(
            "fds: disk side is missing the \"*NINTENDO-HVC*\" verification string"
        ));
    }

    Ok(FdsSide {
        licensee_code: b[0x0F],
        game_name: ascii_trim(&b[0x10..0x13]),
        game_type_code: b[0x13],
        game_type: game_type(b[0x13]).map(str::to_string),
        version: b[0x14],
        side_number: b[0x15],
        disk_number: b[0x16],
        disk_type_code: b[0x17],
        disk_type: disk_type(b[0x17]).map(str::to_string),
        boot_read_file_code: b[0x19],
        manufacture_date_raw: hex(&b[0x1F..0x22]),
        manufacture_date: showa_date(&b[0x1F..0x22]),
    })
}

fn game_type(code: u8) -> Option<&'static str> {
    Some(match code {
        b' ' => "Normal disk",
        b'E' => "Event disk",
        b'R' => "Reduction in price",
        _ => return None,
    })
}

fn disk_type(code: u8) -> Option<&'static str> {
    Some(match code {
        0 => "FMC (normal card)",
        1 => "FSC (card with shutter)",
        _ => return None,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// The manufacture date is three BCD bytes counting from the start of the
/// Showa era, so year 0 is 1925. Digits outside BCD, or a month or day
/// outside range, leave only the raw bytes.
fn showa_date(bcd: &[u8]) -> Option<String> {
    let year = from_bcd(bcd[0])?;
    let month = from_bcd(bcd[1])?;
    let day = from_bcd(bcd[2])?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(format!("{}-{month:02}-{day:02}", 1925 + u32::from(year)))
}

fn from_bcd(byte: u8) -> Option<u8> {
    let (high, low) = (byte >> 4, byte & 0x0F);
    (high <= 9 && low <= 9).then_some(high * 10 + low)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds one 65500-byte disk side with a filled info block.
    fn side(side_number: u8, disk_number: u8) -> Vec<u8> {
        let mut disk = vec![0u8; SIDE_LEN];
        disk[0] = 0x01;
        disk[1..15].copy_from_slice(VERIFICATION);
        disk[0x0F] = 0x01;
        disk[0x10..0x13].copy_from_slice(b"TST");
        disk[0x13] = b' ';
        disk[0x14] = 2;
        disk[0x15] = side_number;
        disk[0x16] = disk_number;
        disk[0x17] = 0;
        disk[0x19] = 5;
        disk[0x1F..0x22].copy_from_slice(&[0x61, 0x04, 0x01]);
        disk
    }

    /// Builds a two-sided image, optionally wrapped in the 16-byte FDS
    /// wrapper header.
    pub(crate) fn fixture(wrapper: bool) -> Vec<u8> {
        let mut out = Vec::new();
        if wrapper {
            out.extend_from_slice(FDS_WRAPPER_MAGIC);
            out.push(2);
            out.resize(FDS_WRAPPER_HEADER_LEN, 0);
        }
        out.extend_from_slice(&side(0, 0));
        out.extend_from_slice(&side(1, 0));
        out
    }

    #[test]
    fn reads_fds_wrapper_image() {
        let rom = fixture(true);
        let info = parse_reader(&mut std::io::Cursor::new(&rom), rom.len() as u64).unwrap();
        assert!(info.wrapper_header);
        assert_eq!(info.side_count, 2);
        assert_eq!(info.sides.len(), 2);

        let first = &info.sides[0];
        assert_eq!(first.licensee_code, 0x01);
        assert_eq!(first.game_name, "TST");
        assert_eq!(first.game_type.as_deref(), Some("Normal disk"));
        assert_eq!(first.version, 2);
        assert_eq!(first.side_number, 0);
        assert_eq!(first.disk_number, 0);
        assert_eq!(first.disk_type.as_deref(), Some("FMC (normal card)"));
        assert_eq!(first.boot_read_file_code, 5);
        assert_eq!(first.manufacture_date_raw, "610401");
        assert_eq!(first.manufacture_date.as_deref(), Some("1986-04-01"));
        assert_eq!(info.sides[1].side_number, 1);
    }

    #[test]
    fn reads_headerless_image_and_derives_side_count() {
        let rom = fixture(false);
        let info = parse_reader(&mut std::io::Cursor::new(&rom), rom.len() as u64).unwrap();
        assert!(!info.wrapper_header);
        assert_eq!(info.side_count, 2);
    }

    #[test]
    fn keeps_raw_bytes_for_a_non_bcd_date() {
        let mut rom = fixture(true);
        rom[FDS_WRAPPER_HEADER_LEN + 0x1F] = 0xAB;
        let info = parse_reader(&mut std::io::Cursor::new(&rom), rom.len() as u64).unwrap();
        assert_eq!(info.sides[0].manufacture_date, None);
        assert_eq!(info.sides[0].manufacture_date_raw, "AB0401");
    }

    #[test]
    fn rejects_missing_verification_string() {
        let mut rom = fixture(true);
        rom[FDS_WRAPPER_HEADER_LEN + 1] = b'X';
        assert!(parse_reader(&mut std::io::Cursor::new(&rom), rom.len() as u64).is_err());

        let mut rom = fixture(true);
        rom[FDS_WRAPPER_HEADER_LEN] = 0x02;
        assert!(parse_reader(&mut std::io::Cursor::new(&rom), rom.len() as u64).is_err());
    }

    #[test]
    fn rejects_truncated_image() {
        let short = &fixture(true)[..FDS_WRAPPER_HEADER_LEN + 8];
        assert!(parse_reader(&mut std::io::Cursor::new(short), short.len() as u64).is_err());
        assert!(parse_reader(&mut std::io::Cursor::new(&[]), 0).is_err());
    }
}
