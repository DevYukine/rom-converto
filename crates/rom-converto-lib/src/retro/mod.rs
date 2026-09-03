//! Header-based metadata extraction for cartridge-era consoles.
//!
//! One module per system, each parsing that system's ROM header out of a
//! whole-file read, since the checksums these formats define cover the
//! whole image. [`read_info`] dispatches on the file extension.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub mod a78;
pub mod gb;
pub mod gba;
pub mod lynx;
pub mod md;
pub mod n64;
pub mod nes;
pub mod ngp;
pub mod sms;
pub mod snes;
pub mod vb;
pub mod ws;

pub use a78::A78Info;
pub use gb::GbInfo;
pub use gba::GbaInfo;
pub use lynx::LynxInfo;
pub use md::MdInfo;
pub use n64::N64Info;
pub use nes::NesInfo;
pub use ngp::NgpInfo;
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
}

/// File extensions [`read_info`] accepts. `.bin` is deliberately absent:
/// too many systems use it for the extension to pick a parser.
pub const RETRO_EXTENSIONS: &[&str] = &[
    "nes", "sfc", "smc", "z64", "n64", "v64", "gb", "gbc", "gba", "md", "gen", "smd", "sms", "gg",
    "vb", "ws", "wsc", "ngp", "ngc", "lnx", "a78",
];

/// Reads cartridge header metadata from the ROM at `path`, choosing the
/// parser by file extension.
///
/// # Errors
/// Returns an error when the extension is not one of [`RETRO_EXTENSIONS`],
/// or when the file does not carry the header that extension implies.
pub fn read_info(path: &Path) -> Result<RetroInfo> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let data =
        std::fs::read(path).with_context(|| format!("retro info: read {}", path.display()))?;

    let details = match ext.as_str() {
        "nes" => RetroDetails::Nes(nes::parse(&data)?),
        "sfc" | "smc" => RetroDetails::Snes(snes::parse(&data)?),
        "z64" | "n64" | "v64" => RetroDetails::N64(n64::parse(&data)?),
        "gb" | "gbc" => RetroDetails::GameBoy(gb::parse(&data)?),
        "gba" => RetroDetails::Gba(gba::parse(&data)?),
        "md" | "gen" | "smd" => RetroDetails::MegaDrive(md::parse(&data)?),
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
    fn rejects_unknown_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.bin");
        std::fs::write(&path, [0u8; 64]).unwrap();
        assert!(read_info(&path).is_err());
    }
}
