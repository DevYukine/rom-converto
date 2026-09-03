//! PlayStation 1, 2, and PSP disc metadata (the `info` feature).
//!
//! One ISO9660 probe covers all three: PS1 and PS2 identify themselves
//! through `SYSTEM.CNF`, PSP through `PSP_GAME/PARAM.SFO`. The probe runs
//! over [`crate::util::iso9660::SectorSource`], so the same reader serves
//! a plain `.iso`, a `.cue`/`.bin` pair, and the discs carried inside CSO
//! and CHD containers. Missing or malformed metadata leaves fields `None`
//! rather than failing the read; only real I/O errors propagate.

use std::io;
use std::path::Path;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::util::iso9660::{DiscKind, SectorSource, Volume, read_volume};

pub mod psp;
pub mod psx;
pub(crate) mod source;

pub use psp::PspInfo;
pub use psx::PsxInfo;

/// A PlayStation-family disc found inside a container image.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiscContent {
    Psx(PsxInfo),
    Psp(PspInfo),
}

/// Reads PS1/PS2/PSP metadata from a logical-sector source. `Ok(None)`
/// when the image holds no PlayStation-family disc.
pub fn read_disc_content<S: SectorSource>(src: &mut S) -> io::Result<Option<DiscContent>> {
    let Some(volume) = read_volume(src)? else {
        return Ok(None);
    };
    Ok(match volume.kind {
        DiscKind::Psp => Some(DiscContent::Psp(psp::read(src, &volume)?)),
        DiscKind::UnknownIso => None,
        _ => Some(DiscContent::Psx(psx::read(src, &volume)?)),
    })
}

/// Reads PS1/PS2 metadata from a plain ISO, or from the data track of a
/// `.cue` sheet.
///
/// # Errors
/// Returns an error if the image cannot be read, is not ISO9660, or holds
/// a disc that is not PS1 or PS2.
pub fn read_psx_info(path: &Path) -> Result<PsxInfo> {
    if is_cue(path) {
        let mut src = source::CueSectors::open(path)?;
        let volume = psx_volume_of(&mut src, path)?;
        Ok(psx::read(&mut src, &volume)?)
    } else {
        let file = std::fs::File::open(path)?;
        let mut src = &file;
        let volume = psx_volume_of(&mut src, path)?;
        Ok(psx::read(&mut src, &volume)?)
    }
}

/// Reads PSP metadata from a plain ISO.
///
/// # Errors
/// Returns an error if the image cannot be read or is not ISO9660.
pub fn read_psp_info(path: &Path) -> Result<PspInfo> {
    let file = std::fs::File::open(path)?;
    let mut src = &file;
    let volume = volume_of(&mut src, path)?;
    Ok(psp::read(&mut src, &volume)?)
}

/// Best-effort probe of the disc a CHD holds; `None` when it carries no
/// PlayStation-family disc or cannot be decoded.
pub(crate) fn chd_disc_content(path: &Path) -> Option<DiscContent> {
    let mut src = source::ChdSectors::open(path).ok()?;
    read_disc_content(&mut src).ok().flatten()
}

/// CSO/ZSO/DAX twin of [`chd_disc_content`].
pub(crate) fn cso_disc_content(path: &Path) -> Option<DiscContent> {
    let mut src = source::CsoSectors::open(path).ok()?;
    read_disc_content(&mut src).ok().flatten()
}

fn is_cue(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cue"))
}

fn volume_of<S: SectorSource>(src: &mut S, path: &Path) -> Result<Volume> {
    read_volume(src)?.ok_or_else(|| anyhow!("{} is not an ISO9660 disc image", path.display()))
}

/// [`volume_of`] restricted to the two families [`PsxInfo`] describes, so
/// a PSP or unrecognized ISO9660 disc never comes back labelled as one.
fn psx_volume_of<S: SectorSource>(src: &mut S, path: &Path) -> Result<Volume> {
    let volume = volume_of(src, path)?;
    match volume.kind {
        DiscKind::Ps1 | DiscKind::Ps2Cd | DiscKind::Ps2Dvd => Ok(volume),
        other => Err(anyhow!(
            "{} is not a PS1 or PS2 disc image ({})",
            path.display(),
            other.label()
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::cue::models::TrackType;
    use crate::util::iso9660::test_fixtures::{
        IsoSpec, SubDir, make_iso, make_iso_with_subdir, set_volume_id,
    };
    use crate::util::sfo::test_fixtures::{Val, build_sfo};

    const PS1_CNF: &[u8] = b"BOOT = cdrom:\\SLUS_000.01;1\r\nTCB = 4\r\n";
    const PS2_CNF: &[u8] = b"BOOT2 = cdrom0:\\SLUS_203.12;1\r\nVER = 1.01\r\nVMODE = NTSC\r\n";

    fn ps1_iso() -> Vec<u8> {
        make_iso(&IsoSpec {
            system_id: b"PLAYSTATION",
            volume_sectors: 250_000,
            root_entries: &[(b"SYSTEM.CNF;1", false)],
            file_content: PS1_CNF,
        })
    }

    fn ps2_iso() -> Vec<u8> {
        let mut iso = make_iso(&IsoSpec {
            system_id: b"PLAYSTATION",
            volume_sectors: 2_000_000,
            root_entries: &[(b"SYSTEM.CNF;1", false)],
            file_content: PS2_CNF,
        });
        set_volume_id(&mut iso, b"SLUS_203.12");
        iso
    }

    fn psp_iso() -> Vec<u8> {
        let sfo = build_sfo(&[
            ("CATEGORY", Val::Str("UG")),
            ("DISC_ID", Val::Str("ULUS10041")),
            ("DISC_VERSION", Val::Str("1.00")),
            ("PARENTAL_LEVEL", Val::U32(1)),
            ("PSP_SYSTEM_VER", Val::Str("3.71")),
            ("TITLE", Val::Str("Test UMD")),
        ]);
        make_iso_with_subdir(
            &IsoSpec {
                system_id: b"PSP GAME",
                volume_sectors: 800_000,
                root_entries: &[],
                file_content: &[],
            },
            &SubDir {
                name: b"PSP_GAME",
                files: &[
                    (b"PARAM.SFO;1", &sfo),
                    (b"ICON0.PNG;1", &png(144, 80)),
                    (b"PIC1.PNG;1", &png(480, 272)),
                ],
            },
        )
    }

    /// A PNG whose signature and IHDR are real; the rest is not read.
    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        out.extend_from_slice(&13u32.to_be_bytes());
        out.extend_from_slice(b"IHDR");
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&[8, 6, 0, 0, 0]);
        out
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("write image");
        path
    }

    /// Wrap each 2048-byte sector of `iso` in a raw CD sector of
    /// `track_type`, the way a bin file carries a data track.
    fn wrap_sectors(iso: &[u8], track_type: TrackType) -> Vec<u8> {
        let block = track_type.block_size() as usize;
        let payload_start = match track_type {
            TrackType::Mode1_2352 => 16,
            TrackType::Mode2_2352 => 24,
            other => panic!("unsupported wrap mode {}", other.cue_string()),
        };
        let mut out = Vec::with_capacity(iso.len() / 2048 * block);
        for chunk in iso.chunks(2048) {
            let mut sector = vec![0u8; block];
            sector[1..12].fill(0xFF);
            sector[payload_start..payload_start + chunk.len()].copy_from_slice(chunk);
            out.extend_from_slice(&sector);
        }
        out
    }

    #[test]
    fn reads_ps1_iso_title_id() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = write(dir.path(), "game.iso", &ps1_iso());

        let info = read_psx_info(&path).expect("read ps1 info");
        assert_eq!(info.console, "PS1");
        assert_eq!(info.media, "CD");
        assert_eq!(info.title_id.as_deref(), Some("SLUS-00001"));
        assert_eq!(
            info.boot_executable.as_deref(),
            Some("cdrom:\\SLUS_000.01;1")
        );
        assert_eq!(info.version, None);
        assert_eq!(info.total_sectors, 250_000);
        assert_eq!(info.size_bytes, 250_000 * 2048);
    }

    #[test]
    fn reads_ps2_iso_version_and_volume_id() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = write(dir.path(), "game.iso", &ps2_iso());

        let info = read_psx_info(&path).expect("read ps2 info");
        assert_eq!(info.console, "PS2");
        assert_eq!(info.media, "DVD");
        assert_eq!(info.title_id.as_deref(), Some("SLUS-20312"));
        assert_eq!(info.version.as_deref(), Some("1.01"));
        assert_eq!(info.volume_id.as_deref(), Some("SLUS_203.12"));
        assert_eq!(info.total_sectors, 2_000_000);
    }

    #[test]
    fn reads_psp_iso_param_sfo_and_icon() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = write(dir.path(), "game.iso", &psp_iso());

        let info = read_psp_info(&path).expect("read psp info");
        assert_eq!(info.title.as_deref(), Some("Test UMD"));
        assert_eq!(info.title_id.as_deref(), Some("ULUS10041"));
        assert_eq!(info.version.as_deref(), Some("1.00"));
        assert_eq!(info.firmware.as_deref(), Some("3.71"));
        assert_eq!(info.category.as_deref(), Some("UG"));
        assert_eq!(info.content_kind, Some(crate::info::ContentKind::Game));
        assert_eq!(info.total_sectors, 800_000);
        let icon = info.icon.expect("icon0.png");
        assert_eq!((icon.width, icon.height), (144, 80));
        let background = info.background.expect("pic1.png");
        assert_eq!((background.width, background.height), (480, 272));
    }

    #[test]
    fn reads_ps2_disc_from_cue_and_bin() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(
            dir.path(),
            "game.bin",
            &wrap_sectors(&ps2_iso(), TrackType::Mode2_2352),
        );
        let cue = write(
            dir.path(),
            "game.cue",
            b"FILE \"game.bin\" BINARY\r\n  TRACK 01 MODE2/2352\r\n    INDEX 01 00:00:00\r\n",
        );

        let info = read_psx_info(&cue).expect("read ps2 info from cue");
        assert_eq!(info.title_id.as_deref(), Some("SLUS-20312"));
        assert_eq!(info.version.as_deref(), Some("1.01"));
        // Logical disc bytes, not the 2352-byte-sector bin the cue points at.
        assert_eq!(info.size_bytes, 2_000_000 * 2048);
    }

    #[tokio::test]
    async fn reads_psp_disc_inside_a_cso() {
        use crate::cso::CsoCompressOptions;
        use crate::util::NoProgress;

        let dir = tempfile::tempdir().expect("temp dir");
        let iso = write(dir.path(), "game.iso", &psp_iso());
        let cso = dir.path().join("game.cso");
        crate::cso::compress_to_cso(&NoProgress, iso, cso.clone(), CsoCompressOptions::default())
            .await
            .expect("compress to cso");

        let info = crate::cso::info::read_info(&cso).expect("read cso info");
        match info.content.expect("cso content") {
            DiscContent::Psp(psp) => {
                assert_eq!(psp.title_id.as_deref(), Some("ULUS10041"));
                assert_eq!(psp.title.as_deref(), Some("Test UMD"));
            }
            other => panic!("expected PSP content, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn reads_ps2_dvd_and_ps1_cd_inside_a_chd() {
        use crate::chd::{ChdOptions, convert_disc_to_chd};
        use crate::util::NoProgress;

        let dir = tempfile::tempdir().expect("temp dir");
        for (name, iso, want) in [
            ("ps2", ps2_iso(), "SLUS-20312"),
            ("ps1", ps1_iso(), "SLUS-00001"),
        ] {
            let sub = dir.path().join(name);
            std::fs::create_dir(&sub).expect("create dir");
            let iso_path = write(&sub, "game.iso", &iso);
            let chd = sub.join("game.chd");
            convert_disc_to_chd(
                &NoProgress,
                iso_path,
                chd.clone(),
                None,
                ChdOptions::default(),
            )
            .await
            .expect("convert to chd");

            let info = crate::chd::info::read_info(&chd).expect("read chd info");
            match info.content.expect("chd content") {
                DiscContent::Psx(psx) => assert_eq!(psx.title_id.as_deref(), Some(want)),
                other => panic!("expected PSX content, got {other:?}"),
            }
        }
    }
}
