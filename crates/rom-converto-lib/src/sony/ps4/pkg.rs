//! PS4 `.pkg` metadata: the `\x7FCNT` container header plus the plaintext
//! `param.sfo` and artwork entries. Everything reads without a key, which
//! is why the reader offers no extraction.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::cnt::{
    self, Cnt, CntEntry, ENTRY_ICON0, ENTRY_PARAM_SFO, ENTRY_PIC0, ENTRY_PIC1, MAX_SFO_BYTES,
};
use crate::info::{ContentKind, Image};
use crate::util::sfo::Sfo;

/// Metadata read from a PS4 `.pkg` header, entry table and plaintext
/// `param.sfo`. Nothing here needs a key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
pub struct Ps4PkgInfo {
    pub content_id: String,
    /// Header flags bit 31: a finalized (retail-style) package.
    pub finalized: bool,
    /// Header `drm_type`: 0 none, 0xF PS4.
    pub drm_type: u32,
    /// Header `content_type`: 0x1A game data, 0x1B additional content, 0x1C additional content without data, 0x1E delta patch.
    pub content_type: u32,
    pub content_type_label: Option<String>,
    pub content_flags: u32,
    /// Decoded `content_flags` bit names, in ascending bit order.
    pub content_flag_labels: Vec<String>,
    pub content_kind: Option<ContentKind>,
    /// Header `version_date`, BCD `yyyymmdd`.
    pub version_date: u32,
    pub title: Option<String>,
    pub title_id: Option<String>,
    /// `CATEGORY` from `param.sfo`, verbatim.
    pub category: Option<String>,
    pub category_label: Option<String>,
    pub app_ver: Option<String>,
    pub version: Option<String>,
    /// `SYSTEM_VER` decoded from BCD `0xAABBCCDD` to `"AA.BB"`.
    pub system_ver: Option<String>,
    /// `APP_TYPE`: 1 paid standalone full, 2 upgradable, 3 demo, 4 freemium.
    pub app_type: Option<u32>,
    pub app_type_label: Option<String>,
    pub parental_level: Option<u32>,
    /// A PS2 Classic wrapper: `CATEGORY` is `gdO`, or `EMU_VERSION` is present.
    pub ps2_classic: bool,
    pub emu_version: Option<u32>,
    /// `icon0.png`.
    pub icon: Option<Image>,
    /// `pic1.png`, else `pic0.png`.
    pub background: Option<Image>,
    pub entry_count: u32,
    pub entries: Vec<CntEntry>,
    pub pfs_image_offset: u64,
    pub pfs_image_size: u64,
    /// Header `package_size`; the file length when the header stores 0.
    pub package_size: u64,
    pub file_size: u64,
}

/// Reads the header, entry table and plaintext metadata of the PS4 `.pkg`
/// file at `path`.
///
/// # Errors
/// Returns an error if the file is not a parseable `\x7FCNT` package.
pub fn read_info(path: &Path) -> Result<Ps4PkgInfo> {
    let mut cnt = Cnt::open(path)?;

    let mut info = Ps4PkgInfo {
        content_id: cnt.content_id.clone(),
        finalized: cnt.finalized,
        drm_type: cnt.drm_type,
        content_type: cnt.content_type,
        content_type_label: cnt::content_type_label(cnt.content_type),
        content_flags: cnt.content_flags,
        content_flag_labels: cnt::content_flag_labels(cnt.content_flags),
        version_date: cnt.version_date,
        entry_count: cnt.entry_count,
        entries: cnt.entries.clone(),
        pfs_image_offset: cnt.pfs_image_offset,
        pfs_image_size: cnt.pfs_image_size,
        package_size: if cnt.package_size == 0 {
            cnt.file_size
        } else {
            cnt.package_size
        },
        file_size: cnt.file_size,
        ..Default::default()
    };

    if let Some(sfo) = cnt
        .read_named("param.sfo", ENTRY_PARAM_SFO, MAX_SFO_BYTES)
        .and_then(|bytes| Sfo::parse(&bytes).ok())
    {
        info.title = sfo.get_str("TITLE").map(str::to_string);
        info.title_id = sfo.get_str("TITLE_ID").map(str::to_string);
        info.category = sfo.get_str("CATEGORY").map(str::to_string);
        info.app_ver = sfo.get_str("APP_VER").map(str::to_string);
        info.version = sfo.get_str("VERSION").map(str::to_string);
        info.system_ver = sfo.get_u32("SYSTEM_VER").map(bcd_version);
        info.app_type = sfo.get_u32("APP_TYPE");
        info.parental_level = sfo.get_u32("PARENTAL_LEVEL");
        info.emu_version = sfo.get_u32("EMU_VERSION");
        if info.content_id.is_empty()
            && let Some(id) = sfo.get_str("CONTENT_ID")
        {
            info.content_id = id.to_string();
        }
    }

    info.category_label = info.category.as_deref().and_then(category_label);
    info.app_type_label = info.app_type.and_then(app_type_label);
    info.ps2_classic = info.category.as_deref() == Some("gdO") || info.emu_version.is_some();
    info.content_kind = content_kind(
        info.category.as_deref(),
        info.app_type,
        info.content_type,
        info.content_flags,
    );
    info.icon = cnt.read_image("icon0.png", ENTRY_ICON0);
    info.background = cnt
        .read_image("pic1.png", ENTRY_PIC1)
        .or_else(|| cnt.read_image("pic0.png", ENTRY_PIC0));

    Ok(info)
}

/// Decodes a BCD `0xAABBCCDD` firmware field to `"AA.BB"`.
fn bcd_version(raw: u32) -> String {
    format!("{:02X}.{:02X}", (raw >> 24) & 0xFF, (raw >> 16) & 0xFF)
}

/// Human label for a `param.sfo` `CATEGORY` code. Matched case
/// sensitively: `gdO` (PS2 Classic) differs from `gdo` only by case.
fn category_label(category: &str) -> Option<String> {
    let label = match category {
        "gd" => "Game",
        "gda" => "System application",
        "gdc" => "Non-game application",
        "gdd" => "Background application",
        "gde" => "Non-game mini application",
        "gdk" => "Video service web application",
        "gdl" => "PS Cloud beta application",
        "gdO" => "PS2 Classic",
        "gp" => "Game patch",
        "gpc" => "Non-game application patch",
        "gpd" => "Background application patch",
        "gpe" => "Non-game mini application patch",
        "gpk" => "Video service web application patch",
        "gpl" => "PS Cloud beta application patch",
        "ac" => "Additional content",
        "sd" => "Save data",
        "bd" => "Blu-ray disc",
        _ => return None,
    };
    Some(label.to_string())
}

/// Human label for an `APP_TYPE` code.
fn app_type_label(app_type: u32) -> Option<String> {
    let label = match app_type {
        1 => "Paid standalone full",
        2 => "Upgradable",
        3 => "Demo",
        4 => "Freemium",
        _ => return None,
    };
    Some(label.to_string())
}

/// Normalizes a package into the shared [`ContentKind`] vocabulary:
/// `APP_TYPE` 3 wins, then the `CATEGORY` code, then the header's
/// `content_type` and patch flags.
fn content_kind(
    category: Option<&str>,
    app_type: Option<u32>,
    content_type: u32,
    content_flags: u32,
) -> Option<ContentKind> {
    if app_type == Some(3) {
        return Some(ContentKind::Demo);
    }
    if let Some(cat) = category {
        return match cat {
            "gd" | "gdc" | "gde" | "gdk" | "gdl" | "gdO" => Some(ContentKind::Game),
            "gda" | "gdd" => Some(ContentKind::System),
            "ac" => Some(ContentKind::Dlc),
            _ if cat.starts_with("gp") => Some(ContentKind::Update),
            _ => None,
        };
    }

    if content_type == 0x1A && cnt::is_patch(content_flags) {
        return Some(ContentKind::Update);
    }
    match content_type {
        0x1A => Some(ContentKind::Game),
        0x1B | 0x1C => Some(ContentKind::Dlc),
        0x1E => Some(ContentKind::Update),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::cnt::test_fixtures::{
        CNT_PFS_OFFSET, CNT_PFS_SIZE, Entry, build_cnt, wrap_fih,
    };
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

    fn sfo(category: &'static str) -> Vec<u8> {
        build_sfo(&[
            ("APP_TYPE", Val::U32(1)),
            ("APP_VER", Val::Str("01.02")),
            ("CATEGORY", Val::Str(category)),
            ("PARENTAL_LEVEL", Val::U32(5)),
            ("SYSTEM_VER", Val::U32(0x0505_0000)),
            ("TITLE", Val::Str("Synthetic Game")),
            ("TITLE_ID", Val::Str("CUSA00001")),
            ("VERSION", Val::Str("01.00")),
        ])
    }

    fn entry(id: u32, name: Option<&'static str>, data: Vec<u8>) -> Entry {
        Entry {
            id,
            name,
            data,
            encrypted: false,
        }
    }

    fn write(dir: &Path, name: &str, bytes: Vec<u8>) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn reads_header_param_sfo_and_artwork() {
        let dir = tempfile::tempdir().unwrap();
        let entries = vec![
            entry(ENTRY_PARAM_SFO, Some("param.sfo"), sfo("gd")),
            entry(ENTRY_ICON0, Some("icon0.png"), png(512, 512)),
            entry(ENTRY_PIC1, Some("pic1.png"), png(1920, 1080)),
            // No name, so the reader has to fall back to the id map.
            entry(0x1007, None, vec![0xAB; 16]),
        ];
        let path = write(
            dir.path(),
            "game.pkg",
            build_cnt(0x1A, 0x0002_0000 | 0x0400_0000, &entries),
        );

        let info = read_info(&path).unwrap();
        assert_eq!(info.content_id, "UP9000-CUSA00001_00-SYNTHETICPS4000");
        assert!(info.finalized);
        assert_eq!(info.drm_type, 0xF);
        assert_eq!(info.content_type, 0x1A);
        assert_eq!(info.content_type_label.as_deref(), Some("PS4 game data"));
        assert_eq!(info.content_flag_labels, vec!["GD_BASE", "NON_GAME"]);
        assert_eq!(info.version_date, 0x2023_0115);
        assert_eq!(info.title.as_deref(), Some("Synthetic Game"));
        assert_eq!(info.title_id.as_deref(), Some("CUSA00001"));
        assert_eq!(info.category.as_deref(), Some("gd"));
        assert_eq!(info.category_label.as_deref(), Some("Game"));
        assert_eq!(info.app_ver.as_deref(), Some("01.02"));
        assert_eq!(info.version.as_deref(), Some("01.00"));
        assert_eq!(info.system_ver.as_deref(), Some("05.05"));
        assert_eq!(info.app_type, Some(1));
        assert_eq!(info.app_type_label.as_deref(), Some("Paid standalone full"));
        assert_eq!(info.parental_level, Some(5));
        assert_eq!(info.content_kind, Some(ContentKind::Game));
        assert!(!info.ps2_classic);

        // The name table entry counts too, so five records in all.
        assert_eq!(info.entry_count, 5);
        assert_eq!(info.entries.len(), 5);
        let named: Vec<&str> = info
            .entries
            .iter()
            .filter_map(|e| e.name.as_deref())
            .collect();
        assert_eq!(
            named,
            vec![
                "entry_names",
                "param.sfo",
                "icon0.png",
                "pic1.png",
                "pubtoolinfo.dat",
            ]
        );
        assert!(info.entries.iter().all(|e| !e.encrypted));

        assert_eq!(info.pfs_image_offset, CNT_PFS_OFFSET);
        assert_eq!(info.pfs_image_size, CNT_PFS_SIZE);
        // The synthetic header stores no package size, so the file length
        // stands in for it.
        assert_eq!(info.package_size, info.file_size);

        let icon = info.icon.expect("icon0.png");
        assert_eq!((icon.width, icon.height), (512, 512));
        let background = info.background.expect("pic1.png");
        assert_eq!((background.width, background.height), (1920, 1080));
    }

    #[test]
    fn background_falls_back_to_pic0() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "game.pkg",
            build_cnt(
                0x1A,
                0,
                &[entry(ENTRY_PIC0, Some("pic0.png"), png(960, 544))],
            ),
        );

        let background = read_info(&path).unwrap().background.expect("pic0.png");
        assert_eq!((background.width, background.height), (960, 544));
    }

    #[test]
    fn encrypted_param_sfo_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "game.pkg",
            build_cnt(
                0x1B,
                0,
                &[Entry {
                    id: ENTRY_PARAM_SFO,
                    name: Some("param.sfo"),
                    data: sfo("gd"),
                    encrypted: true,
                }],
            ),
        );

        let info = read_info(&path).unwrap();
        assert!(info.title.is_none());
        assert!(info.category.is_none());
        assert!(info.entries.iter().any(|e| e.encrypted));
        // With no readable category the header content type decides.
        assert_eq!(info.content_kind, Some(ContentKind::Dlc));
    }

    #[test]
    fn category_and_app_type_drive_content_kind() {
        let dir = tempfile::tempdir().unwrap();
        for (category, want) in [
            ("gd", ContentKind::Game),
            ("gp", ContentKind::Update),
            ("ac", ContentKind::Dlc),
        ] {
            let path = write(
                dir.path(),
                &format!("{category}.pkg"),
                build_cnt(
                    0x1A,
                    0,
                    &[entry(
                        ENTRY_PARAM_SFO,
                        Some("param.sfo"),
                        build_sfo(&[("CATEGORY", Val::Str(category))]),
                    )],
                ),
            );
            assert_eq!(
                read_info(&path).unwrap().content_kind,
                Some(want),
                "{category}"
            );
        }

        let path = write(
            dir.path(),
            "demo.pkg",
            build_cnt(
                0x1A,
                0,
                &[entry(
                    ENTRY_PARAM_SFO,
                    Some("param.sfo"),
                    build_sfo(&[("APP_TYPE", Val::U32(3)), ("CATEGORY", Val::Str("gd"))]),
                )],
            ),
        );
        let info = read_info(&path).unwrap();
        assert_eq!(info.content_kind, Some(ContentKind::Demo));
        assert_eq!(info.app_type_label.as_deref(), Some("Demo"));
    }

    #[test]
    fn ps2_classic_is_flagged_by_category_and_emu_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "ps2.pkg",
            build_cnt(
                0x1A,
                0,
                &[entry(
                    ENTRY_PARAM_SFO,
                    Some("param.sfo"),
                    build_sfo(&[
                        ("CATEGORY", Val::Str("gdO")),
                        ("EMU_VERSION", Val::U32(0x0101_0000)),
                    ]),
                )],
            ),
        );

        let info = read_info(&path).unwrap();
        assert!(info.ps2_classic);
        assert_eq!(info.emu_version, Some(0x0101_0000));
        assert_eq!(info.category_label.as_deref(), Some("PS2 Classic"));
        assert_eq!(info.content_kind, Some(ContentKind::Game));
    }

    /// A FIH image is a PS5 shape, but the container reader is shared, so
    /// the PS4 reader still resolves the embedded base and outer PFS span.
    #[test]
    fn fih_wrapper_resolves_the_embedded_container() {
        let dir = tempfile::tempdir().unwrap();
        let cnt = build_cnt(
            0x1A,
            0,
            &[entry(ENTRY_PARAM_SFO, Some("param.sfo"), sfo("gd"))],
        );
        let path = write(dir.path(), "fih.pkg", wrap_fih(&cnt, 0x80, 0x9000, 0x5000));

        let info = read_info(&path).unwrap();
        assert_eq!(info.title.as_deref(), Some("Synthetic Game"));
        assert_eq!(info.pfs_image_offset, 0x9000);
        assert_eq!(info.pfs_image_size, 0x5000);
    }
}
