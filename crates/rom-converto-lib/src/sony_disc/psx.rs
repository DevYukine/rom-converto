//! PS1/PS2 disc metadata: the boot executable and normalized title id
//! from `SYSTEM.CNF`, plus the ISO9660 volume identifier.

use std::io;

use serde::{Deserialize, Serialize};

use crate::util::iso9660::{SectorSource, Volume};

/// Metadata read from a PS1 or PS2 disc image.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PsxInfo {
    pub disc_kind: String,
    pub boot_executable: Option<String>,
    pub title_id: Option<String>,
    pub volume_id: Option<String>,
    pub version: Option<String>,
    pub total_sectors: u64,
    /// Logical size of the disc, not of the file or container it came in.
    pub size_bytes: u64,
}

pub(crate) fn read<S: SectorSource>(src: &mut S, volume: &Volume) -> io::Result<PsxInfo> {
    let cnf = volume.read_file(src, "SYSTEM.CNF")?.unwrap_or_default();
    let cnf = String::from_utf8_lossy(&cnf);
    // PS2 boots through BOOT2, PS1 through BOOT; a disc carries one.
    let boot_executable = cnf_value(&cnf, "BOOT2").or_else(|| cnf_value(&cnf, "BOOT"));

    Ok(PsxInfo {
        disc_kind: volume.kind.label().to_string(),
        title_id: boot_executable.as_deref().and_then(normalize_title_id),
        boot_executable,
        volume_id: (!volume.volume_id.is_empty()).then(|| volume.volume_id.clone()),
        version: cnf_value(&cnf, "VER"),
        total_sectors: volume.total_sectors,
        size_bytes: volume.size_bytes(),
    })
}

/// Value of a `KEY = VALUE` line in `SYSTEM.CNF`; keys match exactly, so
/// `BOOT` never picks up a `BOOT2` line.
fn cnf_value(text: &str, key: &str) -> Option<String> {
    text.lines()
        .find_map(|line| {
            let (k, v) = line.split_once('=')?;
            (k.trim() == key).then(|| v.trim().to_string())
        })
        .filter(|v| !v.is_empty())
}

/// `cdrom0:\SLUS_203.12;1` becomes `SLUS-20312`. `None` when the boot
/// path's basename is not the four-letter, three-digit, two-digit serial
/// shape, in which case only the boot path itself is reportable.
fn normalize_title_id(boot: &str) -> Option<String> {
    let base = boot.rsplit(['\\', '/']).next()?;
    let base = base.split(';').next()?;
    let (name, digits) = base.split_once('_')?;
    let (high, low) = digits.split_once('.')?;
    let shaped = name.len() == 4
        && name.bytes().all(|b| b.is_ascii_alphabetic())
        && high.len() == 3
        && low.len() == 2
        && high.bytes().chain(low.bytes()).all(|b| b.is_ascii_digit());
    shaped.then(|| format!("{}-{high}{low}", name.to_ascii_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_boot_paths_to_title_ids() {
        assert_eq!(
            normalize_title_id("cdrom0:\\SLUS_203.12;1").as_deref(),
            Some("SLUS-20312")
        );
        assert_eq!(
            normalize_title_id("cdrom:\\slps_015.67;1").as_deref(),
            Some("SLPS-01567")
        );
        assert_eq!(
            normalize_title_id("cdrom0:/SCES_509.16").as_deref(),
            Some("SCES-50916")
        );
    }

    #[test]
    fn rejects_paths_that_are_not_serials() {
        for boot in [
            "cdrom0:\\BOOT.ELF;1",
            "cdrom0:\\SLUS_20312;1",
            "cdrom0:\\SLUSX_203.12;1",
            "cdrom0:\\SLUS_20.312;1",
            "cdrom0:\\SL1S_203.12;1",
            "cdrom0:\\SLUS_203.1A;1",
        ] {
            assert_eq!(normalize_title_id(boot), None, "boot: {boot}");
        }
    }

    #[test]
    fn cnf_values_match_keys_exactly() {
        let cnf = "BOOT2 = cdrom0:\\SLUS_203.12;1\r\nVER = 1.01\r\nVMODE = NTSC\r\n";
        assert_eq!(
            cnf_value(cnf, "BOOT2").as_deref(),
            Some("cdrom0:\\SLUS_203.12;1")
        );
        assert_eq!(cnf_value(cnf, "BOOT"), None);
        assert_eq!(cnf_value(cnf, "VER").as_deref(), Some("1.01"));
        assert_eq!(cnf_value("VER =\r\n", "VER"), None);
    }
}
