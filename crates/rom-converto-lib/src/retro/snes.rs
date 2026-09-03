//! SNES internal header parsing.
//!
//! The header has no magic, so the three mapping layouts (each also with
//! and without a 512-byte copier header) are scored and the best-scoring
//! candidate wins.

use super::ascii_trim;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

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
pub struct SnesInfo {
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

/// Locates and parses the SNES internal header in `data`.
///
/// # Errors
/// Returns an error when no candidate location holds a plausible header.
pub fn parse(data: &[u8]) -> Result<SnesInfo> {
    let mut best: Option<(&'static str, bool, usize, [u8; HEADER_LEN])> = None;
    let mut best_score = 0u32;
    for copier_header in [false, true] {
        let body_start = if copier_header { COPIER_HEADER_LEN } else { 0 };
        for (mapping, base) in CANDIDATES {
            let at = body_start + base;
            let Some(header) = data
                .get(at..at + HEADER_LEN)
                .and_then(|s| <[u8; HEADER_LEN]>::try_from(s).ok())
            else {
                continue;
            };
            let score = score(&header);
            if score > best_score {
                best_score = score;
                best = Some((mapping, copier_header, at, header));
            }
        }
    }

    let Some((mapping, copier_header, at, header)) = best.filter(|_| best_score >= MIN_SCORE)
    else {
        return Err(anyhow!("snes: no plausible internal header found"));
    };
    let body = &data[if copier_header { COPIER_HEADER_LEN } else { 0 }..];

    let map_mode = header[0x15];
    let chipset = header[0x16];
    let licensee = header[0x1A];
    // The extended header sits directly below the internal one, and its
    // last byte names the coprocessor when the chipset high nibble is 0xF.
    let chipset_subtype = (licensee == 0x33)
        .then(|| at.checked_sub(1).and_then(|i| data.get(i).copied()))
        .flatten();

    let checksum_complement = u16::from_le_bytes([header[0x1C], header[0x1D]]);
    let checksum = u16::from_le_bytes([header[0x1E], header[0x1F]]);
    let computed_checksum = (mirror_sum(body).0 & 0xFFFF) as u16;

    Ok(SnesInfo {
        mapping: mapping.to_string(),
        copier_header,
        header_offset: at as u64,
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
        licensee,
        version: header[0x1B],
        checksum,
        checksum_complement,
        computed_checksum,
        checksum_valid: checksum == computed_checksum,
    })
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

/// Sums a ROM body the way the SNES memory map sees it: a non-power-of-two
/// image has its tail mirrored up to fill the next power of two. Returns
/// the sum and the size of that mirrored image.
fn mirror_sum(data: &[u8]) -> (u32, usize) {
    if data.is_empty() {
        return (0, 0);
    }
    let full = data.len().next_power_of_two();
    if full == data.len() {
        return (data.iter().map(|&b| u32::from(b)).sum(), full);
    }
    let base = full >> 1;
    let head: u32 = data[..base].iter().map(|&b| u32::from(b)).sum();
    let (tail, tail_size) = mirror_sum(&data[base..]);
    let repeats = (base / tail_size) as u32;
    (head.wrapping_add(tail.wrapping_mul(repeats)), full)
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
        let checksum = ((mirror_sum(&rom).0.wrapping_add(0x1FE)) & 0xFFFF) as u16;
        rom[base + 0x1C..base + 0x1E].copy_from_slice(&(!checksum).to_le_bytes());
        rom[base + 0x1E..base + 0x20].copy_from_slice(&checksum.to_le_bytes());
        rom
    }

    #[test]
    fn reads_lorom_header() {
        let info = parse(&fixture(0x8000, 0x7FC0, 0x20, 0x00)).unwrap();
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
        let info = parse(&fixture(0x10000, 0xFFC0, 0x31, 0x15)).unwrap();
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
        let info = parse(&rom).unwrap();
        assert!(info.copier_header);
        assert_eq!(info.header_offset, (COPIER_HEADER_LEN + 0x7FC0) as u64);
        assert!(info.checksum_valid);
    }

    #[test]
    fn flags_corrupted_checksum() {
        let mut rom = fixture(0x8000, 0x7FC0, 0x20, 0x00);
        rom[0x100] ^= 0xFF;
        assert!(!parse(&rom).unwrap().checksum_valid);
    }

    #[test]
    fn rejects_image_without_header() {
        assert!(parse(&[0u8; 0x8000]).is_err());
        assert!(parse(&[]).is_err());
    }
}
