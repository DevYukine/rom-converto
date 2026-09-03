//! Header-based metadata extraction for cartridge-era consoles.
//!
//! One module per system. Cartridge headers are parsed out of a whole-file
//! read, since the checksums those formats define cover the whole image;
//! the Sega disc systems instead read only the first sector of the first
//! data track. [`read_info`] dispatches on the file extension.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

pub mod a78;
pub mod dreamcast;
pub mod fds;
pub mod gb;
pub mod gba;
pub mod lynx;
pub mod md;
pub mod n64;
pub mod nes;
pub mod ngp;
pub mod saturn;
pub mod segacd;
pub mod sms;
pub mod snes;
pub mod vb;
pub mod ws;

pub use a78::A78Info;
pub use dreamcast::DreamcastInfo;
pub use fds::FdsInfo;
pub use gb::GbInfo;
pub use gba::GbaInfo;
pub use lynx::LynxInfo;
pub use md::MdInfo;
pub use n64::N64Info;
pub use nes::NesInfo;
pub use ngp::NgpInfo;
pub use saturn::SaturnInfo;
pub use segacd::SegaCdInfo;
pub use sms::SmsInfo;
pub use snes::SnesInfo;
pub use vb::VbInfo;
pub use ws::WsInfo;

/// Metadata read from a cartridge ROM image.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetroInfo {
    pub file_size: u64,
    pub details: RetroDetails,
}

/// Per-system header fields, tagged with the system on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "system", rename_all = "snake_case")]
pub enum RetroDetails {
    Nes(NesInfo),
    Snes(SnesInfo),
    N64(N64Info),
    GameBoy(GbInfo),
    Gba(GbaInfo),
    MegaDrive(MdInfo),
    MasterSystem(SmsInfo),
    GameGear(SmsInfo),
    VirtualBoy(VbInfo),
    WonderSwan(WsInfo),
    NeoGeoPocket(NgpInfo),
    Lynx(LynxInfo),
    Atari7800(A78Info),
    Sega32x(MdInfo),
    Fds(FdsInfo),
    SegaSaturn(SaturnInfo),
    SegaCd(SegaCdInfo),
    Dreamcast(DreamcastInfo),
}

/// File extensions [`read_info`] accepts. `.bin` is deliberately absent:
/// too many systems use it for the extension to pick a parser. `.iso` and
/// `.cue` are absent too: they are shared with other consoles, so
/// `crate::info::detect_console` sniffs them before routing here.
pub const RETRO_EXTENSIONS: &[&str] = &[
    "nes", "sfc", "smc", "z64", "n64", "v64", "gb", "gbc", "gba", "md", "gen", "smd", "32x", "sms",
    "gg", "vb", "ws", "wsc", "ngp", "ngc", "lnx", "a78", "fds", "gdi",
];

/// Sega disc system named by the hardware id at the start of a disc
/// image's first data sector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegaDiscSystem {
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
    let sheet = crate::cue::CueParser::new(path).parse_bytes(
        &std::fs::read(path).with_context(|| format!("retro info: read {}", path.display()))?,
    )?;
    let file = sheet
        .files
        .first()
        .ok_or_else(|| anyhow!("retro info: {} references no files", path.display()))?;
    Ok(path.parent().unwrap_or(Path::new(".")).join(&file.filename))
}

/// Reads header metadata from the ROM or disc image at `path`, choosing
/// the parser by file extension.
///
/// # Errors
/// Returns an error when the extension is not one of [`RETRO_EXTENSIONS`],
/// `.iso`, or `.cue`, or when the file does not carry the header that
/// extension implies.
pub fn read_info(path: &Path) -> Result<RetroInfo> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if matches!(ext.as_str(), "gdi" | "iso" | "cue") {
        let file_size = std::fs::metadata(path)
            .with_context(|| format!("retro info: read {}", path.display()))?
            .len();
        return Ok(RetroInfo {
            file_size,
            details: disc_details(path, &ext)?,
        });
    }

    let data =
        std::fs::read(path).with_context(|| format!("retro info: read {}", path.display()))?;

    let details = match ext.as_str() {
        "nes" => RetroDetails::Nes(nes::parse(&data)?),
        "sfc" | "smc" => RetroDetails::Snes(snes::parse(&data)?),
        "z64" | "n64" | "v64" => RetroDetails::N64(n64::parse(&data)?),
        "gb" | "gbc" => RetroDetails::GameBoy(gb::parse(&data)?),
        "gba" => RetroDetails::Gba(gba::parse(&data)?),
        "md" | "gen" | "smd" => RetroDetails::MegaDrive(md::parse(&data)?),
        // 32X carts carry the plain Mega Drive header, console name apart.
        "32x" => RetroDetails::Sega32x(md::parse(&data)?),
        "fds" => RetroDetails::Fds(fds::parse(&data)?),
        "sms" => RetroDetails::MasterSystem(sms::parse(&data)?),
        "gg" => RetroDetails::GameGear(sms::parse(&data)?),
        "vb" => RetroDetails::VirtualBoy(vb::parse(&data)?),
        "ws" | "wsc" => RetroDetails::WonderSwan(ws::parse(&data)?),
        "ngp" | "ngc" => RetroDetails::NeoGeoPocket(ngp::parse(&data)?),
        "lnx" => RetroDetails::Lynx(lynx::parse(&data)?),
        "a78" => RetroDetails::Atari7800(a78::parse(&data)?),
        other => return Err(anyhow!("retro info: unsupported extension {other:?}")),
    };

    Ok(RetroInfo {
        file_size: data.len() as u64,
        details,
    })
}

/// Reads the Sega disc header out of the first sector a `.gdi`, `.iso`,
/// or `.cue` points at.
fn disc_details(path: &Path, ext: &str) -> Result<RetroDetails> {
    if ext == "gdi" {
        return Ok(RetroDetails::Dreamcast(dreamcast::parse_gdi(path)?));
    }
    let source = if ext == "cue" {
        cue_first_file(path)?
    } else {
        path.to_path_buf()
    };
    let head = read_disc_head(&source)?;
    match probe_sega_disc(&head).map(|h| h.system) {
        Some(SegaDiscSystem::SegaSaturn) => Ok(RetroDetails::SegaSaturn(saturn::parse(&head)?)),
        Some(SegaDiscSystem::SegaCd) => Ok(RetroDetails::SegaCd(segacd::parse(&head)?)),
        Some(SegaDiscSystem::Dreamcast) => Ok(RetroDetails::Dreamcast(dreamcast::parse(&head)?)),
        None => Err(anyhow!(
            "retro info: {} carries no Sega disc hardware id",
            source.display()
        )),
    }
}

/// Renders a fixed-width header name field as a trimmed string, dropping
/// the zero, 0xFF, and control bytes used as padding.
pub(crate) fn ascii_trim(bytes: &[u8]) -> String {
    bytes
        .iter()
        .filter(|&&b| (0x20..=0x7E).contains(&b))
        .map(|&b| b as char)
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatches_on_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.lnx");
        std::fs::write(&path, super::lynx::tests::fixture()).unwrap();

        let info = read_info(&path).unwrap();
        assert_eq!(info.file_size, 64);
        assert!(matches!(info.details, RetroDetails::Lynx(_)));
    }

    #[test]
    fn dispatches_32x_and_fds_on_extension() {
        let dir = tempfile::tempdir().unwrap();

        let path = dir.path().join("game.32x");
        let mut rom = super::md::tests::fixture();
        rom[0x100..0x110].copy_from_slice(b"SEGA 32X        ");
        std::fs::write(&path, &rom).unwrap();
        match read_info(&path).unwrap().details {
            RetroDetails::Sega32x(md) => assert_eq!(md.console, "SEGA 32X"),
            other => panic!("expected Sega32x, got {other:?}"),
        }

        let path = dir.path().join("game.fds");
        std::fs::write(&path, super::fds::tests::fixture(true)).unwrap();
        assert!(matches!(
            read_info(&path).unwrap().details,
            RetroDetails::Fds(_)
        ));
    }

    #[test]
    fn dispatches_sega_disc_images_and_cue_sheets() {
        let dir = tempfile::tempdir().unwrap();

        for (name, sector) in [
            ("saturn.iso", super::saturn::tests::cooked_sector()),
            ("segacd.iso", super::segacd::tests::cooked_sector()),
            ("dc.iso", super::dreamcast::tests::cooked_sector()),
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, &sector).unwrap();
            let details = read_info(&path).unwrap().details;
            assert!(
                matches!(
                    details,
                    RetroDetails::SegaSaturn(_)
                        | RetroDetails::SegaCd(_)
                        | RetroDetails::Dreamcast(_)
                ),
                "{name}: {details:?}"
            );
        }

        std::fs::write(
            dir.path().join("saturn.bin"),
            super::saturn::tests::raw_sector(),
        )
        .unwrap();
        let cue = dir.path().join("saturn.cue");
        std::fs::write(
            &cue,
            b"FILE \"saturn.bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\n",
        )
        .unwrap();
        match read_info(&cue).unwrap().details {
            RetroDetails::SegaSaturn(s) => assert_eq!(s.sector_size, 2352),
            other => panic!("expected SegaSaturn, got {other:?}"),
        }
    }

    #[test]
    fn rejects_a_disc_image_without_a_sega_hardware_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("other.iso");
        std::fs::write(&path, [0u8; 2048]).unwrap();
        assert!(read_info(&path).is_err());
    }

    #[test]
    fn rejects_unknown_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.bin");
        std::fs::write(&path, [0u8; 64]).unwrap();
        assert!(read_info(&path).is_err());
    }
}
