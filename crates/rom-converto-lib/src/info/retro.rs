//! Extension dispatch for the cartridge-era and Sega disc systems.
//!
//! The per-system header parsers live under their maker
//! ([`crate::nintendo`], [`crate::sega`], [`crate::atari`],
//! [`crate::snk`], [`crate::bandai`]); [`read_info`] picks one by file
//! extension. Cartridge headers are parsed out of a whole-file read,
//! since the checksums those formats define cover the whole image; the
//! Sega disc systems instead read only the first sector of the first
//! data track.

use crate::atari::a78::{self, A78Info};
use crate::atari::handy::{self, HandyInfo};
use crate::bandai::ws::{self, WsInfo};
use crate::nintendo::agb::{self, AgbInfo};
use crate::nintendo::dmg::{self, DmgInfo};
use crate::nintendo::fds::{self, FdsInfo};
use crate::nintendo::hvc::{self, HvcInfo};
use crate::nintendo::nus::{self, NusInfo};
use crate::nintendo::shvc::{self, ShvcInfo};
use crate::nintendo::vue::{self, VueInfo};
use crate::sega::katana::{self, KatanaInfo};
use crate::sega::mcd::{self, McdInfo};
use crate::sega::md::{self, MdInfo};
use crate::sega::saturn::{self, SaturnInfo};
use crate::sega::sms::{self, SmsInfo};
use crate::sega::{SegaDiscSystem, cue_first_file, probe_sega_disc, read_disc_head};
use crate::snk::ngp::{self, NgpInfo};
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Metadata read from a cartridge ROM image.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
pub struct RetroInfo {
    pub file_size: u64,
    pub details: RetroDetails,
}

/// Per-system header fields, tagged with the system on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "system", rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
pub enum RetroDetails {
    Nes(HvcInfo),
    Snes(ShvcInfo),
    N64(NusInfo),
    GameBoy(DmgInfo),
    Gba(AgbInfo),
    MegaDrive(MdInfo),
    MasterSystem(SmsInfo),
    GameGear(SmsInfo),
    VirtualBoy(VueInfo),
    WonderSwan(WsInfo),
    NeoGeoPocket(NgpInfo),
    Lynx(HandyInfo),
    Atari7800(A78Info),
    Sega32x(MdInfo),
    Fds(FdsInfo),
    SegaSaturn(SaturnInfo),
    SegaCd(McdInfo),
    Dreamcast(KatanaInfo),
}

/// File extensions [`read_info`] accepts. `.bin` is deliberately absent:
/// too many systems use it for the extension to pick a parser. `.iso` and
/// `.cue` are absent too: they are shared with other consoles, so
/// `crate::info::detect_console` sniffs them before routing here.
pub const RETRO_EXTENSIONS: &[&str] = &[
    "nes", "sfc", "smc", "z64", "n64", "v64", "gb", "gbc", "gba", "md", "gen", "smd", "32x", "sms",
    "gg", "vb", "ws", "wsc", "ngp", "ngc", "lnx", "a78", "fds", "gdi",
];

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
        "nes" => RetroDetails::Nes(hvc::parse(&data)?),
        "sfc" | "smc" => RetroDetails::Snes(shvc::parse(&data)?),
        "z64" | "n64" | "v64" => RetroDetails::N64(nus::parse(&data)?),
        "gb" | "gbc" => RetroDetails::GameBoy(dmg::parse(&data)?),
        "gba" => RetroDetails::Gba(agb::parse(&data)?),
        "md" | "gen" | "smd" => RetroDetails::MegaDrive(md::parse(&data)?),
        // 32X carts carry the plain Mega Drive header, console name apart.
        "32x" => RetroDetails::Sega32x(md::parse(&data)?),
        "fds" => RetroDetails::Fds(fds::parse(&data)?),
        "sms" => RetroDetails::MasterSystem(sms::parse(&data)?),
        "gg" => RetroDetails::GameGear(sms::parse(&data)?),
        "vb" => RetroDetails::VirtualBoy(vue::parse(&data)?),
        "ws" | "wsc" => RetroDetails::WonderSwan(ws::parse(&data)?),
        "ngp" | "ngc" => RetroDetails::NeoGeoPocket(ngp::parse(&data)?),
        "lnx" => RetroDetails::Lynx(handy::parse(&data)?),
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
        return Ok(RetroDetails::Dreamcast(katana::parse_gdi(path)?));
    }
    let source = if ext == "cue" {
        cue_first_file(path)?
    } else {
        path.to_path_buf()
    };
    let head = read_disc_head(&source)?;
    match probe_sega_disc(&head).map(|h| h.system) {
        Some(SegaDiscSystem::SegaSaturn) => Ok(RetroDetails::SegaSaturn(saturn::parse(&head)?)),
        Some(SegaDiscSystem::SegaCd) => Ok(RetroDetails::SegaCd(mcd::parse(&head)?)),
        Some(SegaDiscSystem::Dreamcast) => Ok(RetroDetails::Dreamcast(katana::parse(&head)?)),
        None => Err(anyhow!(
            "retro info: {} carries no Sega disc hardware id",
            source.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatches_on_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.lnx");
        std::fs::write(&path, handy::tests::fixture()).unwrap();

        let info = read_info(&path).unwrap();
        assert_eq!(info.file_size, 64);
        assert!(matches!(info.details, RetroDetails::Lynx(_)));
    }

    #[test]
    fn dispatches_32x_and_fds_on_extension() {
        let dir = tempfile::tempdir().unwrap();

        let path = dir.path().join("game.32x");
        let mut rom = md::tests::fixture();
        rom[0x100..0x110].copy_from_slice(b"SEGA 32X        ");
        std::fs::write(&path, &rom).unwrap();
        match read_info(&path).unwrap().details {
            RetroDetails::Sega32x(md) => assert_eq!(md.console, "SEGA 32X"),
            other => panic!("expected Sega32x, got {other:?}"),
        }

        let path = dir.path().join("game.fds");
        std::fs::write(&path, fds::tests::fixture(true)).unwrap();
        assert!(matches!(
            read_info(&path).unwrap().details,
            RetroDetails::Fds(_)
        ));
    }

    #[test]
    fn dispatches_sega_disc_images_and_cue_sheets() {
        let dir = tempfile::tempdir().unwrap();

        for (name, sector) in [
            ("saturn.iso", saturn::tests::cooked_sector()),
            ("segacd.iso", mcd::tests::cooked_sector()),
            ("dc.iso", katana::tests::cooked_sector()),
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

        std::fs::write(dir.path().join("saturn.bin"), saturn::tests::raw_sector()).unwrap();
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
