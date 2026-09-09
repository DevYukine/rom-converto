//! Sega systems: [`md`] (Mega Drive, and the 32X sharing its header),
//! [`sms`] (Master System and Game Gear), and the disc systems [`mcd`]
//! (Sega CD), [`saturn`], and [`katana`] (Dreamcast).

use anyhow::{Context, Result, anyhow};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

pub mod katana;
pub mod mcd;
pub mod md;
pub mod saturn;
pub mod sms;

/// Sega disc system named by the hardware id at the start of a disc
/// image's first data sector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SegaDiscSystem {
    SegaSaturn,
    SegaCd,
    Dreamcast,
}

/// Where a Sega disc header sits in the first sector, and how wide the
/// sectors around it are.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SegaDiscHeader {
    pub system: SegaDiscSystem,
    pub offset: usize,
    pub sector_size: u32,
}

/// First-sector bytes [`probe_sega_disc`] needs: a raw MODE2 sector's
/// 0x18-byte preamble plus the widest header read here.
pub(crate) const DISC_HEAD_LEN: usize = 0x18 + 0x200;

/// Probes the sector layouts, cooked at 0, raw MODE1 at 0x10, and raw
/// MODE2/FORM1 at 0x18, for a Sega disc hardware id.
pub(crate) fn probe_sega_disc(head: &[u8]) -> Option<SegaDiscHeader> {
    for (offset, sector_size) in [(0usize, 2048u32), (0x10, 2352), (0x18, 2352)] {
        let Some(id) = head.get(offset..offset + 16) else {
            continue;
        };
        let system = if id == b"SEGA SEGASATURN " {
            SegaDiscSystem::SegaSaturn
        } else if id.starts_with(b"SEGADISCSYSTEM") {
            SegaDiscSystem::SegaCd
        } else if id == b"SEGA SEGAKATANA " {
            SegaDiscSystem::Dreamcast
        } else {
            continue;
        };
        return Some(SegaDiscHeader {
            system,
            offset,
            sector_size,
        });
    }
    None
}

/// Reads the first sector of the disc image at `path`, or as much of it
/// as the file holds.
pub(crate) fn read_disc_head(path: &Path) -> Result<Vec<u8>> {
    let mut head = Vec::new();
    File::open(path)
        .and_then(|f| f.take(DISC_HEAD_LEN as u64).read_to_end(&mut head))
        .with_context(|| format!("retro info: read {}", path.display()))?;
    Ok(head)
}

/// Path of the first file a cue sheet references, resolved against the
/// sheet's own directory.
pub(crate) fn cue_first_file(path: &Path) -> Result<PathBuf> {
    let sheet = crate::disc::cue::CueParser::new(path).parse_bytes(
        &std::fs::read(path).with_context(|| format!("retro info: read {}", path.display()))?,
    )?;
    let file = sheet
        .files
        .first()
        .ok_or_else(|| anyhow!("retro info: {} references no files", path.display()))?;
    Ok(path.parent().unwrap_or(Path::new(".")).join(&file.filename))
}
