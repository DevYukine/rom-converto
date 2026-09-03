//! Sega CD / Mega-CD boot sector parsing: the `SEGADISCSYSTEM` hardware
//! id, then the Mega Drive style game header the disc carries at 0x100.

use super::{SegaDiscSystem, ascii_trim, md, probe_sega_disc};
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

/// Fields of a Sega CD boot sector. The Mega Drive header the disc
/// embeds defines no checksum over disc contents, so none is reported.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegaCdInfo {
    pub sector_size: u32,
    pub hardware_id: String,
    pub console: String,
    pub copyright: String,
    pub domestic_title: String,
    pub overseas_title: String,
    pub serial: String,
    pub device_support: Vec<String>,
    pub region: Vec<String>,
}

/// Parses the Sega CD boot sector out of `head`, the first sector of a
/// disc image in either the cooked or raw MODE1 layout.
///
/// # Errors
/// Returns an error when the sector carries no `SEGADISCSYSTEM` hardware
/// id, or is too short to hold the header at 0x100.
pub fn parse(head: &[u8]) -> Result<SegaCdInfo> {
    let found = probe_sega_disc(head)
        .filter(|h| h.system == SegaDiscSystem::SegaCd)
        .ok_or_else(|| anyhow!("segacd: first sector carries no \"SEGADISCSYSTEM\" id"))?;
    let boot = head
        .get(found.offset..found.offset + 0x200)
        .ok_or_else(|| anyhow!("segacd: first sector is shorter than the boot header"))?;

    Ok(SegaCdInfo {
        sector_size: found.sector_size,
        hardware_id: ascii_trim(&boot[0x000..0x010]),
        console: ascii_trim(&boot[0x100..0x110]),
        copyright: ascii_trim(&boot[0x110..0x120]),
        domestic_title: md::collapse(&boot[0x120..0x150]),
        overseas_title: md::collapse(&boot[0x150..0x180]),
        serial: ascii_trim(&boot[0x180..0x18E]),
        device_support: md::device_support(&boot[0x190..0x1A0]),
        region: md::region(&boot[0x1F0..0x200]),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a 2048-byte cooked boot sector with a filled header.
    pub(crate) fn cooked_sector() -> Vec<u8> {
        let mut boot = vec![0u8; 2048];
        boot[0x000..0x010].copy_from_slice(b"SEGADISCSYSTEM  ");
        boot[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
        boot[0x110..0x120].copy_from_slice(b"(C)TEST 1993.SEP");
        boot[0x120..0x180].fill(b' ');
        boot[0x120..0x12C].copy_from_slice(b"DOMESTIC  CD");
        boot[0x150..0x15B].copy_from_slice(b"OVERSEAS CD");
        boot[0x180..0x18E].copy_from_slice(b"GM T-00001-00 ");
        boot[0x190..0x1A0].fill(b' ');
        boot[0x190..0x192].copy_from_slice(b"JC");
        boot[0x1F0..0x1F3].copy_from_slice(b"JUE");
        boot
    }

    #[test]
    fn reads_boot_sector() {
        let info = parse(&cooked_sector()).unwrap();
        assert_eq!(info.sector_size, 2048);
        assert_eq!(info.hardware_id, "SEGADISCSYSTEM");
        assert_eq!(info.console, "SEGA MEGA DRIVE");
        assert_eq!(info.copyright, "(C)TEST 1993.SEP");
        assert_eq!(info.domestic_title, "DOMESTIC CD");
        assert_eq!(info.overseas_title, "OVERSEAS CD");
        assert_eq!(info.serial, "GM T-00001-00");
        assert_eq!(info.device_support, ["3-button controller", "CD-ROM"]);
        assert_eq!(info.region, ["Japan", "Americas", "Europe"]);
    }

    #[test]
    fn reads_raw_mode1_sector() {
        let mut raw = vec![0u8; 2352];
        raw[1..12].fill(0xFF);
        raw[0x10..0x10 + 2048].copy_from_slice(&cooked_sector());
        let info = parse(&raw).unwrap();
        assert_eq!(info.sector_size, 2352);
        assert_eq!(info.serial, "GM T-00001-00");
    }

    #[test]
    fn rejects_wrong_hardware_id() {
        let mut sector = cooked_sector();
        sector[0..16].copy_from_slice(b"SEGA SEGASATURN ");
        assert!(parse(&sector).is_err());
    }

    #[test]
    fn rejects_truncated_sector() {
        assert!(parse(&cooked_sector()[..0x120]).is_err());
    }
}
