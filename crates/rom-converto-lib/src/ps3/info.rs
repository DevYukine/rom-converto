//! PS3 disc `info`: region-table stats plus metadata from the plaintext
//! `PS3_DISC.SFB` and `PARAM.SFO`. Missing or malformed metadata leaves
//! those fields `None` without failing the read.

use std::io::{BufReader, Read};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ps3::error::Ps3Result;
use crate::ps3::fs::read_plain_files;
use crate::ps3::region::{SECTOR_SIZE, parse_region_table};
use crate::ps3::sfb::Sfb;
use crate::util::sfo::Sfo;

/// Summary of a PS3 disc's region table and plaintext `PARAM.SFO`/
/// `PS3_DISC.SFB` metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ps3Info {
    pub title: Option<String>,
    pub title_id: Option<String>,
    pub region: Option<String>,
    pub version: Option<String>,
    pub app_ver: Option<String>,
    pub resolution: Option<String>,
    pub sound_format: Option<String>,
    pub firmware: Option<String>,
    pub parental_level: Option<u32>,
    pub region_count: usize,
    pub total_sectors: u32,
    pub encrypted_sectors: u64,
    pub size_bytes: u64,
}

/// Reads a PS3 disc's region-table stats and plaintext metadata; no
/// disc key is needed since it never touches encrypted sectors.
///
/// # Errors
/// Returns an error if the sector 0 region table cannot be parsed.
pub fn read_ps3_info(path: &Path) -> Ps3Result<Ps3Info> {
    let size_bytes = std::fs::metadata(path)?.len();
    let mut file = std::fs::File::open(path)?;

    let mut sector0 = [0u8; SECTOR_SIZE];
    file.read_exact(&mut sector0)?;
    let (regions, total_sectors) = parse_region_table(&sector0)?;
    let encrypted_sectors = regions
        .iter()
        .filter(|r| !r.plain)
        .map(|r| (r.last - r.start + 1) as u64)
        .sum();

    let mut info = Ps3Info {
        region_count: regions.len(),
        total_sectors,
        encrypted_sectors,
        size_bytes,
        ..Default::default()
    };

    let mut reader = BufReader::new(file);
    if let Ok(files) = read_plain_files(&mut reader) {
        if let Some(sfo) = files.param_sfo.as_deref().and_then(|b| Sfo::parse(b).ok()) {
            info.title = sfo.get_str("TITLE").map(str::to_string);
            info.title_id = sfo.get_str("TITLE_ID").map(str::to_string);
            info.version = sfo.get_str("VERSION").map(str::to_string);
            info.app_ver = sfo.get_str("APP_VER").map(str::to_string);
            info.firmware = sfo.get_str("PS3_SYSTEM_VER").map(str::to_string);
            info.parental_level = sfo.get_u32("PARENTAL_LEVEL");
            info.resolution = sfo.get_u32("RESOLUTION").and_then(decode_resolution);
            info.sound_format = sfo.get_u32("SOUND_FORMAT").and_then(decode_sound_format);
        }
        if let Some(sfb) = files.disc_sfb.as_deref().and_then(|b| Sfb::parse(b).ok()) {
            // The SFB carries the authoritative physical-disc VERSION.
            if let Some(v) = sfb.get("VERSION") {
                info.version = Some(v.to_string());
            }
            if info.title_id.is_none() {
                info.title_id = sfb.get("TITLE_ID").map(str::to_string);
            }
        }
    }

    info.region = info
        .title_id
        .as_deref()
        .and_then(region_from_title_id)
        .map(str::to_string);
    Ok(info)
}

fn region_from_title_id(id: &str) -> Option<&'static str> {
    let b = id.as_bytes();
    if b.len() < 4 || (&b[0..2] != b"BL" && &b[0..2] != b"BC") {
        return None;
    }
    match b[2] {
        b'E' => Some("Europe"),
        b'U' => Some("USA"),
        b'J' => Some("Japan"),
        b'A' | b'K' => Some("Asia"),
        _ => None,
    }
}

fn decode_flags(bits: u32, flags: &[(u32, &str)]) -> Option<String> {
    let parts: Vec<&str> = flags
        .iter()
        .filter(|(b, _)| bits & b != 0)
        .map(|(_, l)| *l)
        .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn decode_resolution(bits: u32) -> Option<String> {
    decode_flags(
        bits,
        &[(0x01, "480"), (0x02, "576"), (0x04, "720"), (0x08, "1080")],
    )
}

fn decode_sound_format(bits: u32) -> Option<String> {
    decode_flags(
        bits,
        &[
            (0x01, "LPCM 2.0"),
            (0x04, "LPCM 5.1"),
            (0x10, "LPCM 7.1"),
            (0x100, "Dolby Digital 5.1"),
            (0x200, "DTS 5.1"),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_mapping() {
        assert_eq!(region_from_title_id("BLES02247"), Some("Europe"));
        assert_eq!(region_from_title_id("BCES00001"), Some("Europe"));
        assert_eq!(region_from_title_id("BLUS30000"), Some("USA"));
        assert_eq!(region_from_title_id("BCUS98111"), Some("USA"));
        assert_eq!(region_from_title_id("BLJS10001"), Some("Japan"));
        assert_eq!(region_from_title_id("BLJM60001"), Some("Japan"));
        assert_eq!(region_from_title_id("BLAS50001"), Some("Asia"));
        assert_eq!(region_from_title_id("BCKS10001"), Some("Asia"));
        assert_eq!(region_from_title_id("MRTC00001"), None);
        assert_eq!(region_from_title_id("BL"), None);
    }

    #[test]
    fn region_from_title_id_handles_multibyte_prefix_without_panicking() {
        // "€a" is 4 bytes (E2 82 AC 61) but only 2 chars, so a naive str
        // slice at byte index 2 would land mid-codepoint and panic.
        assert_eq!(region_from_title_id("\u{20AC}a"), None);
        assert_eq!(region_from_title_id("BLES02247"), Some("Europe"));
        assert_eq!(region_from_title_id("BLUS12345"), Some("USA"));
        assert_eq!(region_from_title_id("BLJS00001"), Some("Japan"));
    }

    #[test]
    fn resolution_and_sound_decode() {
        assert_eq!(
            decode_resolution(0x0F).as_deref(),
            Some("480, 576, 720, 1080")
        );
        assert_eq!(decode_resolution(0x08).as_deref(), Some("1080"));
        assert_eq!(decode_resolution(0), None);
        assert_eq!(decode_sound_format(0x01).as_deref(), Some("LPCM 2.0"));
        assert_eq!(
            decode_sound_format(0x101).as_deref(),
            Some("LPCM 2.0, Dolby Digital 5.1")
        );
    }
}
