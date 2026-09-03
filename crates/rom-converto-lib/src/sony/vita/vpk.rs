//! PS Vita `.vpk` packages: a ZIP archive whose `sce_sys` directory holds
//! the title metadata (`param.sfo`) and the bubble icon (`icon0.png`).

use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::info::{ContentKind, Image};
use crate::util::sfo::Sfo;

const SFO_MEMBER: &str = "sce_sys/param.sfo";
const ICON_MEMBER: &str = "sce_sys/icon0.png";

/// Cap on a `param.sfo` read out of an untrusted archive.
const MAX_SFO_BYTES: u64 = 1 << 20;
/// Cap on an `icon0.png` read out of an untrusted archive.
const MAX_ICON_BYTES: u64 = 4 << 20;

/// Metadata read from a `.vpk` package.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VpkInfo {
    pub title: Option<String>,
    pub title_id: Option<String>,
    pub content_id: Option<String>,
    pub app_ver: Option<String>,
    /// `CATEGORY` exactly as stored in `param.sfo`.
    pub category: Option<String>,
    /// Human label for [`VpkInfo::category`], when the code is a known one.
    pub category_label: Option<String>,
    /// Normalized category, derived from [`VpkInfo::category`].
    #[serde(default)]
    pub content_kind: Option<ContentKind>,
    pub icon: Option<Image>,
    /// Number of file members, not counting directory entries.
    pub file_count: u32,
    /// Total uncompressed size of all file members.
    pub total_size: u64,
}

/// Reads the `sce_sys` metadata and icon from the `.vpk` archive at `path`.
pub fn read_info(path: &Path) -> Result<VpkInfo> {
    let file = File::open(path).with_context(|| format!("vpk info: open {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("vpk info: read zip {}", path.display()))?;

    let mut info = VpkInfo::default();
    let mut sfo_index = None;
    let mut icon_index = None;
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        // enclosed_name is None for absolute or parent-traversing paths.
        if entry.enclosed_name().is_none() {
            continue;
        }
        if entry.is_dir() {
            continue;
        }
        info.file_count += 1;
        info.total_size += entry.size();

        let name = entry.name().replace('\\', "/").to_ascii_lowercase();
        match name.as_str() {
            SFO_MEMBER => sfo_index = Some(i),
            ICON_MEMBER => icon_index = Some(i),
            _ => {}
        }
    }

    if let Some(i) = sfo_index
        && let Some(sfo) = read_member(&mut zip, i, MAX_SFO_BYTES)?
            .as_deref()
            .and_then(|bytes| Sfo::parse(bytes).ok())
    {
        info.title = sfo.get_str("TITLE").map(str::to_string);
        info.title_id = sfo.get_str("TITLE_ID").map(str::to_string);
        info.content_id = sfo.get_str("CONTENT_ID").map(str::to_string);
        info.app_ver = sfo.get_str("APP_VER").map(str::to_string);
        info.category = sfo.get_str("CATEGORY").map(str::to_string);
        info.category_label = info.category.as_deref().and_then(category_label);
        info.content_kind = info
            .category
            .as_deref()
            .and_then(content_kind_from_category);
    }
    if let Some(i) = icon_index {
        info.icon = read_member(&mut zip, i, MAX_ICON_BYTES)?.and_then(Image::from_png);
    }

    Ok(info)
}

/// Reads member `index` whole, or `None` when it is larger than `max`.
fn read_member(zip: &mut zip::ZipArchive<File>, index: usize, max: u64) -> Result<Option<Vec<u8>>> {
    let mut entry = zip.by_index(index)?;
    if entry.size() > max {
        return Ok(None);
    }
    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut buf)?;
    Ok(Some(buf))
}

/// Label for the `param.sfo` `CATEGORY` codes a Vita package can carry.
fn category_label(category: &str) -> Option<String> {
    let label = match category {
        "gd" => "Application",
        "gp" => "Patch",
        "ac" => "Additional content",
        "gda" => "Application (bundled)",
        _ => return None,
    };
    Some(label.to_string())
}

/// Maps a Vita `param.sfo` `CATEGORY` code to the shared content vocabulary.
fn content_kind_from_category(category: &str) -> Option<ContentKind> {
    match category {
        "gd" | "gda" => Some(ContentKind::Game),
        "gp" => Some(ContentKind::Update),
        "ac" => Some(ContentKind::Dlc),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
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

    fn write_vpk(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn reads_param_sfo_icon_and_totals() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.vpk");
        let sfo = build_sfo(&[
            ("APP_VER", Val::Str("01.02")),
            ("CATEGORY", Val::Str("gd")),
            (
                "CONTENT_ID",
                Val::Str("EP9000-PCSF00001_00-EXAMPLE000000000"),
            ),
            ("TITLE", Val::Str("Example Game")),
            ("TITLE_ID", Val::Str("PCSF00001")),
        ]);
        let icon = png(128, 128);
        write_vpk(
            &path,
            &[
                ("sce_sys/param.sfo", &sfo),
                ("sce_sys/icon0.png", &icon),
                ("eboot.bin", &[0u8; 64]),
            ],
        );

        let info = read_info(&path).unwrap();
        assert_eq!(info.title.as_deref(), Some("Example Game"));
        assert_eq!(info.title_id.as_deref(), Some("PCSF00001"));
        assert_eq!(
            info.content_id.as_deref(),
            Some("EP9000-PCSF00001_00-EXAMPLE000000000")
        );
        assert_eq!(info.app_ver.as_deref(), Some("01.02"));
        assert_eq!(info.category.as_deref(), Some("gd"));
        assert_eq!(info.category_label.as_deref(), Some("Application"));
        assert_eq!(info.content_kind, Some(ContentKind::Game));
        assert_eq!(info.file_count, 3);
        assert_eq!(info.total_size, sfo.len() as u64 + icon.len() as u64 + 64);

        let icon = info.icon.expect("icon0.png");
        assert_eq!((icon.width, icon.height), (128, 128));
    }

    #[test]
    fn maps_category_to_content_kind() {
        for (category, want) in [("gp", ContentKind::Update), ("ac", ContentKind::Dlc)] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("game.vpk");
            let sfo = build_sfo(&[("CATEGORY", Val::Str(category))]);
            write_vpk(&path, &[("sce_sys/param.sfo", &sfo)]);

            let info = read_info(&path).unwrap();
            assert_eq!(info.category.as_deref(), Some(category));
            assert_eq!(info.content_kind, Some(want), "category {category}");
        }
    }

    #[test]
    fn vpk_without_sce_sys_still_reports_totals() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bare.vpk");
        write_vpk(&path, &[("readme.txt", b"hi")]);

        let info = read_info(&path).unwrap();
        assert_eq!(info.file_count, 1);
        assert_eq!(info.total_size, 2);
        assert!(info.title.is_none());
        assert!(info.icon.is_none());
    }

    #[test]
    fn rejects_non_zip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.vpk");
        std::fs::write(&path, b"not a zip archive at all").unwrap();
        assert!(read_info(&path).is_err());
    }
}
