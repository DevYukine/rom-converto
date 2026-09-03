//! Dreamcast IP header parsing, plus the plain-text `.gdi` track index
//! that points at the GD-ROM track the header lives in.

use super::{SegaDiscSystem, ascii_trim, probe_sega_disc};
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Fields of the Dreamcast IP header, with the area and peripheral fields
/// kept raw alongside what is decoded from them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamcastInfo {
    pub sector_size: u32,
    pub hardware_id: String,
    pub maker_id: String,
    pub device_info: String,
    pub area_symbols: String,
    pub regions: Vec<String>,
    pub peripherals_raw: String,
    pub peripherals: Vec<String>,
    pub product_number: String,
    pub version: String,
    pub release_date: String,
    pub boot_filename: String,
    pub maker_name: String,
    pub title: String,
    /// Track index of the `.gdi` the header was reached through, absent
    /// when the header came straight out of a disc image.
    pub gdi: Option<GdiIndex>,
}

/// The track table of a `.gdi` sheet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GdiIndex {
    /// Track count the sheet declares on its first line.
    pub track_count: usize,
    pub tracks: Vec<GdiTrack>,
}

/// One track line of a `.gdi` sheet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GdiTrack {
    pub number: u32,
    pub lba: u64,
    pub track_type: u32,
    pub sector_size: u32,
    pub filename: String,
}

/// Parses the Dreamcast IP header out of `head`, the first sector of a
/// disc image in either the cooked or raw MODE1 layout.
///
/// # Errors
/// Returns an error when the sector carries no `SEGA SEGAKATANA `
/// hardware id, or is too short to hold the header.
pub fn parse(head: &[u8]) -> Result<DreamcastInfo> {
    let found = probe_sega_disc(head)
        .filter(|h| h.system == SegaDiscSystem::Dreamcast)
        .ok_or_else(|| anyhow!("dreamcast: first sector carries no \"SEGA SEGAKATANA \" id"))?;
    let ip = head
        .get(found.offset..found.offset + 0x100)
        .ok_or_else(|| anyhow!("dreamcast: first sector is shorter than the IP header"))?;

    let area_symbols = ascii_trim(&ip[0x30..0x38]);
    let peripherals_raw = ascii_trim(&ip[0x38..0x40]);

    Ok(DreamcastInfo {
        sector_size: found.sector_size,
        hardware_id: ascii_trim(&ip[0x00..0x10]),
        maker_id: ascii_trim(&ip[0x10..0x20]),
        device_info: ascii_trim(&ip[0x20..0x30]),
        regions: regions(&area_symbols),
        area_symbols,
        peripherals: peripherals(&peripherals_raw),
        peripherals_raw,
        product_number: ascii_trim(&ip[0x40..0x4A]),
        version: ascii_trim(&ip[0x4A..0x50]),
        release_date: ascii_trim(&ip[0x50..0x60]),
        boot_filename: ascii_trim(&ip[0x60..0x70]),
        maker_name: ascii_trim(&ip[0x70..0x80]),
        title: super::md::collapse(&ip[0x80..0x100]),
        gdi: None,
    })
}

/// Parses the `.gdi` track index at `path` and reads the IP header from
/// the first sector of its third track, where the GD-ROM high-density
/// area starts.
///
/// # Errors
/// Returns an error when the index is malformed, lists fewer than three
/// tracks, or the third track's file holds no Dreamcast IP header.
pub fn parse_gdi(path: &Path) -> Result<DreamcastInfo> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("retro info: read {}", path.display()))?;
    let index = parse_index(&text)?;
    let third = index
        .tracks
        .get(2)
        .ok_or_else(|| anyhow!("dreamcast: gdi index lists fewer than three tracks"))?;
    let track_path = path
        .parent()
        .unwrap_or(Path::new("."))
        .join(&third.filename);

    let mut info = parse(&super::read_disc_head(&track_path)?)?;
    info.gdi = Some(index);
    Ok(info)
}

fn parse_index(text: &str) -> Result<GdiIndex> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let track_count = lines
        .next()
        .and_then(|l| l.parse::<usize>().ok())
        .ok_or_else(|| anyhow!("dreamcast: gdi does not open with a track count"))?;
    let tracks = lines
        .take(track_count)
        .map(parse_track)
        .collect::<Result<Vec<_>>>()?;
    Ok(GdiIndex {
        track_count,
        tracks,
    })
}

/// A track line is `number lba type sector_size filename offset`, with
/// the filename quoted when it holds spaces.
fn parse_track(line: &str) -> Result<GdiTrack> {
    let bad = || anyhow!("dreamcast: malformed gdi track line {line:?}");
    let fields: Vec<&str> = line.split_whitespace().collect();
    let [number, lba, track_type, sector_size, ..] = fields.as_slice() else {
        return Err(bad());
    };
    let filename = match line.find('"') {
        Some(open) => {
            let rest = &line[open + 1..];
            rest[..rest.find('"').ok_or_else(bad)?].to_string()
        }
        None => fields.get(4).ok_or_else(bad)?.to_string(),
    };

    Ok(GdiTrack {
        number: number.parse().map_err(|_| bad())?,
        lba: lba.parse().map_err(|_| bad())?,
        track_type: track_type.parse().map_err(|_| bad())?,
        sector_size: sector_size.parse().map_err(|_| bad())?,
        filename,
    })
}

fn regions(symbols: &str) -> Vec<String> {
    symbols
        .chars()
        .filter_map(|c| {
            Some(
                match c {
                    'J' => "Japan",
                    'U' => "North America",
                    'E' => "Europe",
                    _ => return None,
                }
                .to_string(),
            )
        })
        .collect()
}

/// The peripheral field is eight hex digits of flags. Only the flags that
/// say something about the hardware a disc needs are named; the button
/// bits are left to the raw string.
fn peripherals(field: &str) -> Vec<String> {
    let Ok(bits) = u32::from_str_radix(field, 16) else {
        return Vec::new();
    };
    [
        (0x0000_0001, "Windows CE"),
        (0x0000_0010, "VGA box"),
        (0x0000_0200, "Jump pack"),
        (0x0000_0400, "Microphone"),
        (0x0000_0800, "Memory card"),
        (0x0200_0000, "Light gun"),
        (0x0400_0000, "Keyboard"),
        (0x0800_0000, "Mouse"),
    ]
    .into_iter()
    .filter(|(mask, _)| bits & mask != 0)
    .map(|(_, name)| name.to_string())
    .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a 2048-byte cooked first sector with a filled IP header.
    pub(crate) fn cooked_sector() -> Vec<u8> {
        let mut ip = vec![b' '; 2048];
        ip[0x00..0x10].copy_from_slice(b"SEGA SEGAKATANA ");
        ip[0x10..0x20].copy_from_slice(b"SEGA ENTERPRISES");
        ip[0x20..0x30].copy_from_slice(b"1234 GD-ROM1/1  ");
        ip[0x30..0x38].copy_from_slice(b"JUE     ");
        ip[0x38..0x40].copy_from_slice(b"00000E10");
        ip[0x40..0x4A].copy_from_slice(b"T-00001N  ");
        ip[0x4A..0x50].copy_from_slice(b"V1.002");
        ip[0x50..0x60].copy_from_slice(b"20000317        ");
        ip[0x60..0x70].copy_from_slice(b"1ST_READ.BIN    ");
        ip[0x70..0x80].copy_from_slice(b"TEST PUBLISHER  ");
        ip[0x80..0x100].fill(b' ');
        ip[0x80..0x89].copy_from_slice(b"TEST GAME");
        ip
    }

    /// Writes a `.gdi` sheet and its four track files into `dir`, and
    /// returns the sheet's path.
    pub(crate) fn gdi_fixture(dir: &Path) -> std::path::PathBuf {
        for name in ["track01.bin", "track02.raw", "track04.bin"] {
            std::fs::write(dir.join(name), [0u8; 16]).expect("write track");
        }
        std::fs::write(dir.join("track03.bin"), cooked_sector()).expect("write data track");

        let gdi = dir.join("game.gdi");
        std::fs::write(
            &gdi,
            "4\n\
             1 0 4 2352 track01.bin 0\n\
             2 1798 0 2352 track02.raw 0\n\
             3 45000 4 2048 track03.bin 0\n\
             4 250000 0 2352 track04.bin 0\n",
        )
        .expect("write gdi");
        gdi
    }

    #[test]
    fn reads_ip_header() {
        let info = parse(&cooked_sector()).unwrap();
        assert_eq!(info.sector_size, 2048);
        assert_eq!(info.hardware_id, "SEGA SEGAKATANA");
        assert_eq!(info.maker_id, "SEGA ENTERPRISES");
        assert_eq!(info.device_info, "1234 GD-ROM1/1");
        assert_eq!(info.area_symbols, "JUE");
        assert_eq!(info.regions, ["Japan", "North America", "Europe"]);
        assert_eq!(info.peripherals_raw, "00000E10");
        assert_eq!(
            info.peripherals,
            ["VGA box", "Jump pack", "Microphone", "Memory card"]
        );
        assert_eq!(info.product_number, "T-00001N");
        assert_eq!(info.version, "V1.002");
        assert_eq!(info.release_date, "20000317");
        assert_eq!(info.boot_filename, "1ST_READ.BIN");
        assert_eq!(info.maker_name, "TEST PUBLISHER");
        assert_eq!(info.title, "TEST GAME");
        assert!(info.gdi.is_none());
    }

    #[test]
    fn reads_raw_mode1_sector() {
        let mut raw = vec![0u8; 2352];
        raw[1..12].fill(0xFF);
        raw[0x10..0x10 + 2048].copy_from_slice(&cooked_sector());
        let info = parse(&raw).unwrap();
        assert_eq!(info.sector_size, 2352);
        assert_eq!(info.title, "TEST GAME");
    }

    #[test]
    fn reads_third_track_of_a_gdi() {
        let dir = tempfile::tempdir().unwrap();
        let info = parse_gdi(&gdi_fixture(dir.path())).unwrap();
        assert_eq!(info.product_number, "T-00001N");

        let index = info.gdi.expect("gdi index");
        assert_eq!(index.track_count, 4);
        assert_eq!(index.tracks.len(), 4);
        assert_eq!(index.tracks[2].number, 3);
        assert_eq!(index.tracks[2].lba, 45000);
        assert_eq!(index.tracks[2].track_type, 4);
        assert_eq!(index.tracks[2].sector_size, 2048);
        assert_eq!(index.tracks[2].filename, "track03.bin");
    }

    #[test]
    fn reads_a_quoted_track_filename() {
        let track = parse_track("3 45000 4 2048 \"my track 03.bin\" 0").unwrap();
        assert_eq!(track.filename, "my track 03.bin");
    }

    #[test]
    fn rejects_malformed_gdi() {
        assert!(parse_index("").is_err());
        assert!(parse_index("two\n1 0 4 2352 a.bin 0\n").is_err());
        assert!(parse_index("1\n1 0 4\n").is_err());
        assert!(parse_index("1\n1 0 4 2352 a.bin 0\n").is_ok());
    }

    #[test]
    fn rejects_gdi_with_too_few_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let gdi = dir.path().join("short.gdi");
        std::fs::write(&gdi, "2\n1 0 4 2352 a.bin 0\n2 1798 0 2352 b.raw 0\n").unwrap();
        assert!(parse_gdi(&gdi).is_err());
    }

    #[test]
    fn rejects_wrong_hardware_id() {
        let mut sector = cooked_sector();
        sector[0..16].copy_from_slice(b"SEGA SEGASATURN ");
        assert!(parse(&sector).is_err());
        assert!(parse(&cooked_sector()[..0x80]).is_err());
    }
}
