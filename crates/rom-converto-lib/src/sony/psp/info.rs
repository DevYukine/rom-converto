//! `EBOOT.PBP` metadata: `PARAM.SFO` fields, the `ICON0.PNG` artwork, the
//! per-segment layout, and the kind of image `DATA.PSAR` holds.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::info::{ContentKind, Image};
use crate::sony::psp::pbp::{DATA_PSAR, ICON0_PNG, PARAM_SFO, Pbp, SEGMENT_NAMES, Segment};
use crate::util::sfo::Sfo;

/// Cap on a segment read whole into memory for metadata, so a corrupt
/// header cannot ask for a huge allocation.
const MAX_METADATA_BYTES: u64 = 4 * 1024 * 1024;

/// The image `DATA.PSAR` carries, identified by its first 8 bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PsarKind {
    /// PSN-distributed UMD image.
    Npumdimg,
    /// PS1 Classic disc image.
    Psisoimg,
    /// PS1 Classic multi-disc container, whose magic is `PSTITLEIMG`.
    Pstitleimg,
    /// A magic matching none of the known kinds.
    Unknown { magic: String },
}

/// One PBP segment as reported by [`read_info`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PbpSegmentInfo {
    pub name: String,
    pub offset: u64,
    pub size: u64,
    pub present: bool,
}

/// Metadata read from an `EBOOT.PBP`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PbpInfo {
    pub physical_bytes: u64,
    pub version: u32,
    pub title: Option<String>,
    pub disc_id: Option<String>,
    pub disc_version: Option<String>,
    /// The raw `CATEGORY` code, kept verbatim even when undecoded.
    pub category: Option<String>,
    pub category_label: Option<String>,
    /// Normalized category, derived from [`PbpInfo::category`]. A UMD
    /// image never carries an update or DLC category.
    #[serde(default)]
    pub content_kind: Option<ContentKind>,
    pub psp_system_ver: Option<String>,
    pub parental_level: Option<u32>,
    pub region: Option<u32>,
    pub icon: Option<Image>,
    pub segments: Vec<PbpSegmentInfo>,
    /// `None` when the container carries no `DATA.PSAR`.
    pub psar_kind: Option<PsarKind>,
}

/// Reads `EBOOT.PBP` metadata from `path`.
///
/// # Errors
/// Returns an error if the file cannot be read or does not carry a valid
/// PBP header. A malformed `PARAM.SFO` or `ICON0.PNG` leaves those fields
/// unset rather than failing the read.
pub fn read_info(path: &Path) -> Result<PbpInfo> {
    let mut file =
        File::open(path).with_context(|| format!("pbp info: open {}", path.display()))?;
    let pbp =
        Pbp::read(&mut file).with_context(|| format!("pbp info: parse {}", path.display()))?;

    let sfo =
        read_capped(&mut file, pbp.segments[PARAM_SFO])?.and_then(|bytes| Sfo::parse(&bytes).ok());
    let category = sfo
        .as_ref()
        .and_then(|s| s.get_str("CATEGORY"))
        .map(str::to_string);
    let category_label = category
        .as_deref()
        .and_then(category_label)
        .map(str::to_string);
    let content_kind = category.as_deref().and_then(content_kind_from_category);

    Ok(PbpInfo {
        physical_bytes: pbp.file_size,
        version: pbp.version,
        title: sfo
            .as_ref()
            .and_then(|s| s.get_str("TITLE"))
            .map(str::to_string),
        disc_id: sfo
            .as_ref()
            .and_then(|s| s.get_str("DISC_ID"))
            .map(str::to_string),
        disc_version: sfo
            .as_ref()
            .and_then(|s| s.get_str("DISC_VERSION"))
            .map(str::to_string),
        category,
        category_label,
        content_kind,
        psp_system_ver: sfo
            .as_ref()
            .and_then(|s| s.get_str("PSP_SYSTEM_VER"))
            .map(str::to_string),
        parental_level: sfo.as_ref().and_then(|s| s.get_u32("PARENTAL_LEVEL")),
        region: sfo.as_ref().and_then(|s| s.get_u32("REGION")),
        icon: read_capped(&mut file, pbp.segments[ICON0_PNG])?.and_then(Image::from_png),
        segments: SEGMENT_NAMES
            .iter()
            .zip(pbp.segments.iter())
            .map(|(name, s)| PbpSegmentInfo {
                name: (*name).to_string(),
                offset: s.offset,
                size: s.size,
                present: s.size > 0,
            })
            .collect(),
        psar_kind: read_psar_kind(&mut file, pbp.segments[DATA_PSAR])?,
    })
}

/// Only codes whose meaning is unambiguous are decoded; everything else
/// stays raw in [`PbpInfo::category`].
fn category_label(code: &str) -> Option<&'static str> {
    match code {
        "UG" => Some("UMD game"),
        "MG" => Some("Memory Stick game"),
        "ME" | "MS" => Some("PS1 Classic"),
        _ => None,
    }
}

/// A UMD/EBOOT `CATEGORY` only ever names a game; there is no update or
/// DLC category on this medium.
fn content_kind_from_category(code: &str) -> Option<ContentKind> {
    match code {
        "UG" | "MG" | "ME" | "MS" => Some(ContentKind::Game),
        _ => None,
    }
}

fn read_capped<R: Read + Seek>(reader: &mut R, segment: Segment) -> Result<Option<Vec<u8>>> {
    if segment.size == 0 || segment.size > MAX_METADATA_BYTES {
        return Ok(None);
    }
    reader.seek(SeekFrom::Start(segment.offset))?;
    let mut buf = vec![0u8; segment.size as usize];
    reader.read_exact(&mut buf)?;
    Ok(Some(buf))
}

pub(crate) fn read_psar_kind<R: Read + Seek>(
    reader: &mut R,
    segment: Segment,
) -> Result<Option<PsarKind>> {
    if segment.size < 8 {
        return Ok(None);
    }
    reader.seek(SeekFrom::Start(segment.offset))?;
    let mut magic = [0u8; 8];
    reader.read_exact(&mut magic)?;
    Ok(Some(match &magic {
        b"NPUMDIMG" => PsarKind::Npumdimg,
        b"PSISOIMG" => PsarKind::Psisoimg,
        b"PSTITLEI" => PsarKind::Pstitleimg,
        other => PsarKind::Unknown {
            magic: other
                .iter()
                .map(|&b| if b.is_ascii_graphic() { b as char } else { '.' })
                .collect(),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sony::psp::pbp::test_fixtures::build_pbp;
    use crate::util::sfo::test_fixtures::{Val, build_sfo};

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

    fn eboot(category: &'static str, psar: &[u8]) -> Vec<u8> {
        let sfo = build_sfo(&[
            ("CATEGORY", Val::Str(category)),
            ("DISC_ID", Val::Str("NPUH10041")),
            ("DISC_VERSION", Val::Str("1.02")),
            ("PARENTAL_LEVEL", Val::U32(5)),
            ("PSP_SYSTEM_VER", Val::Str("6.20")),
            ("REGION", Val::U32(0x8000)),
            ("TITLE", Val::Str("Test EBOOT")),
        ]);
        let icon = png(144, 80);
        build_pbp(
            0x10000,
            &[
                &sfo,
                &icon,
                &[],
                &[],
                &png(480, 272),
                &[],
                b"DATA.PSP",
                psar,
            ],
        )
    }

    fn write(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("EBOOT.PBP");
        std::fs::write(&path, bytes).expect("write eboot");
        (dir, path)
    }

    #[test]
    fn reads_param_sfo_icon_and_layout() {
        let (_dir, path) = write(&eboot("UG", b"NPUMDIMG\0\0\0\0\0\0\0\0"));
        let info = read_info(&path).expect("read info");

        assert_eq!(info.version, 0x10000);
        assert_eq!(info.title.as_deref(), Some("Test EBOOT"));
        assert_eq!(info.disc_id.as_deref(), Some("NPUH10041"));
        assert_eq!(info.disc_version.as_deref(), Some("1.02"));
        assert_eq!(info.category.as_deref(), Some("UG"));
        assert_eq!(info.category_label.as_deref(), Some("UMD game"));
        assert_eq!(info.content_kind, Some(ContentKind::Game));
        assert_eq!(info.psp_system_ver.as_deref(), Some("6.20"));
        assert_eq!(info.parental_level, Some(5));
        assert_eq!(info.region, Some(0x8000));
        assert_eq!(info.psar_kind, Some(PsarKind::Npumdimg));

        let icon = info.icon.expect("icon0.png");
        assert_eq!((icon.width, icon.height), (144, 80));

        assert_eq!(info.segments.len(), 8);
        let present: Vec<&str> = info
            .segments
            .iter()
            .filter(|s| s.present)
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(
            present,
            vec![
                "PARAM.SFO",
                "ICON0.PNG",
                "PIC1.PNG",
                "DATA.PSP",
                "DATA.PSAR"
            ]
        );
        assert_eq!(info.segments[6].size, b"DATA.PSP".len() as u64);
        assert!(info.segments.iter().all(|s| s.present == (s.size > 0)));
        assert_eq!(
            info.physical_bytes,
            std::fs::metadata(&path).expect("stat").len()
        );
    }

    #[test]
    fn decodes_only_known_category_codes() {
        for (code, want, want_kind) in [
            ("UG", Some("UMD game"), Some(ContentKind::Game)),
            ("MG", Some("Memory Stick game"), Some(ContentKind::Game)),
            ("ME", Some("PS1 Classic"), Some(ContentKind::Game)),
            ("MS", Some("PS1 Classic"), Some(ContentKind::Game)),
            ("XX", None, None),
        ] {
            let (_dir, path) = write(&eboot(code, b"NPUMDIMG"));
            let info = read_info(&path).expect("read info");
            assert_eq!(info.category.as_deref(), Some(code));
            assert_eq!(info.category_label.as_deref(), want, "category {code}");
            assert_eq!(info.content_kind, want_kind, "category {code}");
        }
    }

    #[test]
    fn identifies_psar_kinds() {
        for (magic, want) in [
            (&b"NPUMDIMG"[..], PsarKind::Npumdimg),
            (&b"PSISOIMG"[..], PsarKind::Psisoimg),
            (&b"PSTITLEIMG"[..], PsarKind::Pstitleimg),
            (
                &b"WHATEVER"[..],
                PsarKind::Unknown {
                    magic: "WHATEVER".to_string(),
                },
            ),
        ] {
            let (_dir, path) = write(&eboot("UG", magic));
            assert_eq!(read_info(&path).expect("read info").psar_kind, Some(want));
        }
    }

    #[test]
    fn missing_psar_reports_absent() {
        let (_dir, path) = write(&eboot("UG", &[]));
        let info = read_info(&path).expect("read info");
        assert_eq!(info.psar_kind, None);
        assert!(!info.segments[7].present);
    }

    #[test]
    fn malformed_param_sfo_leaves_fields_unset() {
        let bytes = build_pbp(
            1,
            &[b"not an sfo", &[], &[], &[], &[], &[], &[], b"NPUMDIMG"],
        );
        let (_dir, path) = write(&bytes);
        let info = read_info(&path).expect("read info");
        assert_eq!(info.title, None);
        assert_eq!(info.category, None);
        assert!(info.icon.is_none());
        assert_eq!(info.psar_kind, Some(PsarKind::Npumdimg));
    }
}
