//! SNES internal header parsing.
//!
//! The header has no magic, so the three mapping layouts (each also with
//! and without a 512-byte copier header) are scored and the best-scoring
//! candidate wins.

use crate::util::bytes::ascii_trim;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};

const HEADER_LEN: usize = 32;
const COPIER_HEADER_LEN: usize = 512;

/// Candidate header locations, as (mapping name, offset in the ROM body).
const CANDIDATES: [(&str, usize); 3] =
    [("LoROM", 0x7FC0), ("HiROM", 0xFFC0), ("ExHiROM", 0x40FFC0)];

/// Below this score the best candidate is treated as noise rather than a
/// header: a real one scores at least a sane map mode plus a printable title.
const MIN_SCORE: u32 = 5;

/// Fields of the SNES internal header, plus the checksum recomputed over
/// the ROM body.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
pub struct ShvcInfo {
    pub mapping: String,
    pub copier_header: bool,
    pub header_offset: u64,
    pub title: String,
    pub map_mode: u8,
    pub fastrom: bool,
    pub chipset: u8,
    pub coprocessor: Option<String>,
    pub rom_size_kb: u32,
    pub sram_size_kb: u32,
    pub country: u8,
    pub region: Option<String>,
    pub licensee: u8,
    pub version: u8,
    pub checksum: u16,
    pub checksum_complement: u16,
    pub computed_checksum: u16,
    pub checksum_valid: bool,
}

/// Locates and parses the SNES internal header, probing each candidate
/// mapping with a bounded read and streaming the checksum.
///
/// # Errors
/// Returns an error when no candidate location holds a plausible header.
pub fn parse_reader(reader: &mut (impl Read + Seek), file_len: u64) -> Result<ShvcInfo> {
    let mut best: Option<(&'static str, bool, u64, [u8; HEADER_LEN])> = None;
    let mut best_score = 0u32;
    for copier_header in [false, true] {
        let body_start = if copier_header {
            COPIER_HEADER_LEN as u64
        } else {
            0
        };
        for (mapping, base) in CANDIDATES {
            let at = body_start + base as u64;
            if at + HEADER_LEN as u64 > file_len {
                continue;
            }
            let mut header = [0; HEADER_LEN];
            reader.seek(SeekFrom::Start(at))?;
            reader.read_exact(&mut header)?;
            let candidate_score = score(&header);
            if candidate_score > best_score {
                best_score = candidate_score;
                best = Some((mapping, copier_header, at, header));
            }
        }
    }
    let Some((mapping, copier_header, at, header)) = best.filter(|_| best_score >= MIN_SCORE)
    else {
        return Err(anyhow!("shvc: no plausible internal header found"));
    };

    let chipset_subtype = if header[0x1A] == 0x33 && at > 0 {
        let mut subtype = [0];
        reader.seek(SeekFrom::Start(at - 1))?;
        reader.read_exact(&mut subtype)?;
        Some(subtype[0])
    } else {
        None
    };
    let body_start = if copier_header {
        COPIER_HEADER_LEN as u64
    } else {
        0
    };
    let (sum, _) = mirror_sum_reader(reader, body_start, file_len - body_start)?;
    let computed_checksum = (sum & 0xFFFF) as u16;
    let map_mode = header[0x15];
    let chipset = header[0x16];
    let checksum_complement = u16::from_le_bytes([header[0x1C], header[0x1D]]);
    let checksum = u16::from_le_bytes([header[0x1E], header[0x1F]]);

    Ok(ShvcInfo {
        mapping: mapping.to_string(),
        copier_header,
        header_offset: at,
        title: ascii_trim(&header[..21]),
        map_mode,
        fastrom: map_mode & 0x10 != 0,
        chipset,
        coprocessor: coprocessor(chipset, chipset_subtype),
        rom_size_kb: 1u32 << header[0x17].min(31),
        sram_size_kb: if header[0x18] == 0 {
            0
        } else {
            1u32 << header[0x18].min(31)
        },
        country: header[0x19],
        region: region(header[0x19]).map(str::to_string),
        licensee: header[0x1A],
        version: header[0x1B],
        checksum,
        checksum_complement,
        computed_checksum,
        checksum_valid: checksum == computed_checksum,
    })
}

fn mirror_sum_reader(reader: &mut (impl Read + Seek), start: u64, len: u64) -> Result<(u64, u64)> {
    if len == 0 {
        return Ok((0, 0));
    }
    let full = len.next_power_of_two();
    if full == len {
        return Ok((sum_range(reader, start, len)?, full));
    }
    let base = full >> 1;
    let (tail_sum, tail_size) = mirror_sum_reader(reader, start + base, len - base)?;
    let head = sum_range(reader, start, base)?;
    Ok((
        head.wrapping_add(tail_sum.wrapping_mul(base / tail_size)),
        full,
    ))
}

fn sum_range(reader: &mut (impl Read + Seek), start: u64, len: u64) -> Result<u64> {
    let mut buf = [0; 64 * 1024];
    let mut offset = 0;
    let mut sum = 0u64;
    reader.seek(SeekFrom::Start(start))?;
    while offset < len {
        let count = (len - offset).min(buf.len() as u64) as usize;
        reader.read_exact(&mut buf[..count])?;
        sum = buf[..count].iter().fold(sum, |acc, &b| acc + u64::from(b));
        offset += count as u64;
    }
    Ok(sum)
}

fn score(header: &[u8]) -> u32 {
    let mut score = 0;
    let complement = u16::from_le_bytes([header[0x1C], header[0x1D]]);
    let checksum = u16::from_le_bytes([header[0x1E], header[0x1F]]);
    if checksum.wrapping_add(complement) == 0xFFFF {
        score += 4;
    }
    let map_mode = header[0x15];
    if map_mode & 0xE0 == 0x20 {
        score += 2;
    }
    if matches!(map_mode & 0x0F, 0x0 | 0x1 | 0x2 | 0x3 | 0x5 | 0xA) {
        score += 1;
    }
    if header[..21].iter().all(|&b| (0x20..=0x7E).contains(&b)) {
        score += 3;
    }
    if (0x08..=0x0D).contains(&header[0x17]) {
        score += 1;
    }
    score
}

/// Decodes the coprocessor named by the chipset byte. The low nibble must
/// be 3 or higher for a coprocessor to be present at all.
fn coprocessor(chipset: u8, subtype: Option<u8>) -> Option<String> {
    if chipset & 0x0F < 0x3 {
        return None;
    }
    let name = match chipset >> 4 {
        0x0 => "DSP",
        0x1 => "SuperFX",
        0x2 => "OBC1",
        0x3 => "SA-1",
        0x4 => "S-DD1",
        0x5 => "S-RTC",
        0xF => match subtype? {
            0x00 => "SPC7110",
            0x01 => "ST010/ST011",
            0x02 => "ST018",
            0x10 => "CX4",
            _ => return None,
        },
        _ => return None,
    };
    Some(name.to_string())
}

fn region(country: u8) -> Option<&'static str> {
    Some(match country {
        0x00 => "Japan",
        0x01 => "USA and Canada",
        0x02 => "Europe, Oceania, and Asia",
        0x03 => "Sweden and Scandinavia",
        0x04 => "Finland",
        0x05 => "Denmark",
        0x06 => "France",
        0x07 => "Netherlands",
        0x08 => "Spain",
        0x09 => "Germany",
        0x0A => "Italy",
        0x0B => "China",
        0x0D => "South Korea",
        0x0E => "Common",
        0x0F => "Canada",
        0x10 => "Brazil",
        0x11 => "Australia",
        _ => return None,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a ROM of `size` bytes carrying a valid internal header at
    /// `base`, with the checksum pair fixed up to match the body.
    pub(crate) fn fixture(size: usize, base: usize, map_mode: u8, chipset: u8) -> Vec<u8> {
        let mut rom = vec![0u8; size];
        rom[base..base + 21].fill(b' ');
        rom[base..base + 14].copy_from_slice(b"TEST CARTRIDGE");
        rom[base + 0x15] = map_mode;
        rom[base + 0x16] = chipset;
        rom[base + 0x17] = 0x0A;
        rom[base + 0x18] = 0x03;
        rom[base + 0x19] = 0x01;
        rom[base + 0x1A] = 0x33;
        rom[base + 0x1B] = 0x02;

        // The checksum pair always contributes 0x1FE to the total, so the
        // sum with it zeroed plus 0x1FE is the value to store.
        let checksum = ((mirror_sum_reader(
            &mut std::io::Cursor::new(rom.as_slice()),
            0,
            rom.len() as u64,
        )
        .unwrap()
        .0
        .wrapping_add(0x1FE))
            & 0xFFFF) as u16;
        rom[base + 0x1C..base + 0x1E].copy_from_slice(&(!checksum).to_le_bytes());
        rom[base + 0x1E..base + 0x20].copy_from_slice(&checksum.to_le_bytes());
        rom
    }

    #[test]
    fn reads_lorom_header() {
        let rom = fixture(0x8000, 0x7FC0, 0x20, 0x00);
        let info = parse_reader(&mut std::io::Cursor::new(&rom), rom.len() as u64).unwrap();
        assert_eq!(info.mapping, "LoROM");
        assert!(!info.copier_header);
        assert_eq!(info.header_offset, 0x7FC0);
        assert_eq!(info.title, "TEST CARTRIDGE");
        assert!(!info.fastrom);
        assert_eq!(info.rom_size_kb, 1024);
        assert_eq!(info.sram_size_kb, 8);
        assert_eq!(info.region.as_deref(), Some("USA and Canada"));
        assert_eq!(info.version, 2);
        assert!(info.checksum_valid);
        assert_eq!(info.coprocessor, None);
    }

    #[test]
    fn reads_hirom_fastrom_with_coprocessor() {
        let rom = fixture(0x10000, 0xFFC0, 0x31, 0x15);
        let info = parse_reader(&mut std::io::Cursor::new(&rom), rom.len() as u64).unwrap();
        assert_eq!(info.mapping, "HiROM");
        assert_eq!(info.header_offset, 0xFFC0);
        assert!(info.fastrom);
        assert_eq!(info.coprocessor.as_deref(), Some("SuperFX"));
        assert!(info.checksum_valid);
    }

    #[test]
    fn detects_copier_header() {
        let mut rom = vec![0u8; COPIER_HEADER_LEN];
        rom.extend_from_slice(&fixture(0x8000, 0x7FC0, 0x20, 0x00));
        let info = parse_reader(&mut std::io::Cursor::new(&rom), rom.len() as u64).unwrap();
        assert!(info.copier_header);
        assert_eq!(info.header_offset, (COPIER_HEADER_LEN + 0x7FC0) as u64);
        assert!(info.checksum_valid);
    }

    #[test]
    fn reads_exhirom_header_from_reader() {
        let rom = fixture(0x800000, 0x40FFC0, 0x25, 0x00);
        let mut reader = std::io::Cursor::new(rom.clone());
        let info = parse_reader(&mut reader, rom.len() as u64).unwrap();
        assert_eq!(info.mapping, "ExHiROM");
        assert_eq!(info.header_offset, 0x40FFC0);
        assert!(info.checksum_valid);
    }

    #[test]
    fn flags_corrupted_checksum() {
        let mut rom = fixture(0x8000, 0x7FC0, 0x20, 0x00);
        rom[0x100] ^= 0xFF;
        assert!(
            !parse_reader(&mut std::io::Cursor::new(&rom), rom.len() as u64)
                .unwrap()
                .checksum_valid
        );
    }

    #[test]
    fn rejects_image_without_header() {
        let invalid = [0u8; 0x8000];
        assert!(parse_reader(&mut std::io::Cursor::new(&invalid), invalid.len() as u64).is_err());
        assert!(parse_reader(&mut std::io::Cursor::new(&[]), 0).is_err());
    }
}
