//! PS5 `.pkg` metadata: the `\x7FCNT` container header plus the plaintext
//! `param.json` and artwork entries. Everything reads without a key, which
//! is why the reader offers no extraction.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::info::{ContentKind, Image};
use crate::sony::ps4::cnt::{
    self, Cnt, CntEntry, CntImage, ENTRY_ICON0, ENTRY_PARAM_JSON, ENTRY_PIC0, MAX_JSON_BYTES,
};

/// Outer image kind of a PS5 package file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
pub enum Ps5PkgImage {
    /// Bare `\x7FCNT` metadata container, as served by the PlayStation CDN.
    #[default]
    Cnt,
    /// Finalized `\x7FFIH` image with the CNT embedded.
    Fih,
    /// Older `\x7FLIH` image with the CNT embedded.
    Lih,
}

/// Metadata read from a PS5 package header, entry table and plaintext
/// `param.json`. Nothing here needs a key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
pub struct Ps5PkgInfo {
    pub content_id: String,
    pub image: Ps5PkgImage,
    /// FIH or LIH signed byte 0x80; `None` for a bare CNT.
    pub signed: Option<bool>,
    pub finalized: bool,
    pub drm_type: u32,
    /// Header `content_type`: 0x20 is PS5 game data.
    pub content_type: u32,
    pub content_type_label: Option<String>,
    pub content_flags: u32,
    pub content_flag_labels: Vec<String>,
    pub content_kind: Option<ContentKind>,
    pub version_date: u32,
    /// `localizedParameters[defaultLanguage].titleName`, else `en-US`, else the first language.
    pub title: Option<String>,
    pub title_id: Option<String>,
    pub default_language: Option<String>,
    pub content_version: Option<String>,
    pub target_content_version: Option<String>,
    pub master_version: Option<String>,
    /// `requiredSystemSoftwareVersion` hex string `0xAABB...` decoded to `"AA.BB"`.
    pub required_system_version: Option<String>,
    /// `sdkVersion` decoded the same way.
    pub sdk_version: Option<String>,
    pub application_category_type: Option<u32>,
    pub application_category_label: Option<String>,
    /// `applicationDrmType`: `standard`, `upgradable`, `demo`, `free`, `freemium`.
    pub application_drm_type: Option<String>,
    /// `pubtools.creationDate`.
    pub creation_date: Option<String>,
    /// `icon0.png`.
    pub icon: Option<Image>,
    /// `pic0.png`.
    pub background: Option<Image>,
    pub entry_count: u32,
    pub entries: Vec<CntEntry>,
    pub pfs_image_offset: u64,
    pub pfs_image_size: u64,
    pub package_size: u64,
    pub file_size: u64,
}

/// Reads the header, entry table and plaintext metadata of the PS5 `.pkg`
/// file at `path`.
///
/// # Errors
/// Returns an error if the file is not a parseable `\x7FCNT` package or
/// FIH/LIH image.
pub fn read_info(path: &Path) -> Result<Ps5PkgInfo> {
    let mut cnt = Cnt::open(path)?;

    let mut info = Ps5PkgInfo {
        content_id: cnt.content_id.clone(),
        image: match cnt.image {
            CntImage::Cnt => Ps5PkgImage::Cnt,
            CntImage::Fih => Ps5PkgImage::Fih,
            CntImage::Lih => Ps5PkgImage::Lih,
        },
        signed: cnt.signed,
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

    if let Some(param) = cnt
        .read_named("param.json", ENTRY_PARAM_JSON, MAX_JSON_BYTES)
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    {
        let string = |key: &str| param.get(key).and_then(Value::as_str).map(str::to_string);
        info.title_id = string("titleId");
        info.content_version = string("contentVersion");
        info.target_content_version = string("targetContentVersion");
        info.master_version = string("masterVersion");
        info.required_system_version = param
            .get("requiredSystemSoftwareVersion")
            .and_then(Value::as_str)
            .and_then(hex_version);
        info.sdk_version = param
            .get("sdkVersion")
            .and_then(Value::as_str)
            .and_then(hex_version);
        info.application_category_type = param
            .get("applicationCategoryType")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok());
        info.application_drm_type = string("applicationDrmType");
        info.creation_date = param
            .get("pubtools")
            .and_then(|v| v.get("creationDate"))
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Some(localized) = param.get("localizedParameters") {
            info.default_language = localized
                .get("defaultLanguage")
                .and_then(Value::as_str)
                .map(str::to_string);
            info.title = title_name(localized, info.default_language.as_deref());
        }
        if info.content_id.is_empty()
            && let Some(id) = param.get("contentId").and_then(Value::as_str)
        {
            info.content_id = id.to_string();
        }
    }

    info.application_category_label = info.application_category_type.and_then(category_label);
    info.content_kind = content_kind(&info);
    info.icon = cnt.read_image("icon0.png", ENTRY_ICON0);
    info.background = cnt.read_image("pic0.png", ENTRY_PIC0);

    Ok(info)
}

/// Picks the title from `localizedParameters`: the default language, else
/// `en-US`, else whichever language carrying a `titleName` sorts first.
fn title_name(localized: &Value, default_language: Option<&str>) -> Option<String> {
    let name = |lang: &str| -> Option<String> {
        Some(localized.get(lang)?.get("titleName")?.as_str()?.to_string())
    };
    default_language
        .and_then(name)
        .or_else(|| name("en-US"))
        .or_else(|| {
            localized
                .as_object()?
                .values()
                .find_map(|v| Some(v.get("titleName")?.as_str()?.to_string()))
        })
}

/// Decodes a `"0xAABB..."` firmware string to `"AA.BB"`.
fn hex_version(raw: &str) -> Option<String> {
    let digits = raw
        .strip_prefix("0x")
        .or_else(|| raw.strip_prefix("0X"))
        .unwrap_or(raw);
    let word = u16::from_str_radix(digits.get(..4)?, 16).ok()?;
    Some(format!("{:02X}.{:02X}", word >> 8, word & 0xFF))
}

/// Human label for an `applicationCategoryType` code.
fn category_label(category: u32) -> Option<String> {
    let label = match category {
        0 => "Native game",
        65536 => "Native media application",
        65792 => "RNPS media application",
        66048 => "Web media application",
        131328 => "System built-in application",
        131584 => "Big daemon",
        _ => return None,
    };
    Some(label.to_string())
}

/// Normalizes a package into the shared [`ContentKind`] vocabulary.
fn content_kind(info: &Ps5PkgInfo) -> Option<ContentKind> {
    if cnt::is_patch(info.content_flags) || info.target_content_version.is_some() {
        return Some(ContentKind::Update);
    }
    if info.application_drm_type.as_deref() == Some("demo") {
        return Some(ContentKind::Demo);
    }
    match info.application_category_type {
        Some(131328) | Some(131584) => Some(ContentKind::System),
        Some(0) => Some(ContentKind::Game),
        _ if info.content_type == 0x20 => Some(ContentKind::Game),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sony::ps4::cnt::test_fixtures::{
        CNT_PFS_OFFSET, CNT_PFS_SIZE, Entry, build_cnt, wrap_fih, wrap_lih,
    };

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

    fn param_json() -> Vec<u8> {
        br#"{
            "applicationCategoryType": 0,
            "applicationDrmType": "standard",
            "contentId": "UP9000-PPSA00001_00-SYNTHETICPS5000",
            "contentVersion": "01.000.000",
            "localizedParameters": {
                "defaultLanguage": "ja-JP",
                "en-US": { "titleName": "Synthetic Game" },
                "ja-JP": { "titleName": "Gousei Game" }
            },
            "masterVersion": "01.00",
            "pubtools": { "creationDate": "2023-01-15 09:41:00" },
            "requiredSystemSoftwareVersion": "0x0114000000000000",
            "sdkVersion": "0x0400000000000000",
            "targetContentVersion": "01.001.000",
            "titleId": "PPSA00001"
        }"#
        .to_vec()
    }

    fn entry(id: u32, name: &'static str, data: Vec<u8>) -> Entry {
        Entry {
            id,
            name: Some(name),
            data,
            encrypted: false,
        }
    }

    fn entries() -> Vec<Entry> {
        vec![
            entry(ENTRY_PARAM_JSON, "param.json", param_json()),
            entry(ENTRY_ICON0, "icon0.png", png(512, 512)),
            entry(ENTRY_PIC0, "pic0.png", png(3840, 2160)),
        ]
    }

    #[test]
    fn reads_param_json_from_a_bare_container() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.pkg");
        std::fs::write(&path, build_cnt(0x20, 0, &entries())).unwrap();

        let info = read_info(&path).unwrap();
        assert_eq!(info.image, Ps5PkgImage::Cnt);
        assert_eq!(info.signed, None);
        assert_eq!(info.content_type, 0x20);
        assert_eq!(info.content_type_label.as_deref(), Some("PS5 game data"));
        // The default language wins over en-US.
        assert_eq!(info.title.as_deref(), Some("Gousei Game"));
        assert_eq!(info.default_language.as_deref(), Some("ja-JP"));
        assert_eq!(info.title_id.as_deref(), Some("PPSA00001"));
        assert_eq!(info.content_version.as_deref(), Some("01.000.000"));
        assert_eq!(info.target_content_version.as_deref(), Some("01.001.000"));
        assert_eq!(info.master_version.as_deref(), Some("01.00"));
        assert_eq!(info.required_system_version.as_deref(), Some("01.14"));
        assert_eq!(info.sdk_version.as_deref(), Some("04.00"));
        assert_eq!(info.application_category_type, Some(0));
        assert_eq!(
            info.application_category_label.as_deref(),
            Some("Native game")
        );
        assert_eq!(info.application_drm_type.as_deref(), Some("standard"));
        assert_eq!(info.creation_date.as_deref(), Some("2023-01-15 09:41:00"));
        // targetContentVersion marks the package as a patch.
        assert_eq!(info.content_kind, Some(ContentKind::Update));
        assert_eq!(info.entry_count, 4);
        assert_eq!(info.pfs_image_offset, CNT_PFS_OFFSET);
        assert_eq!(info.pfs_image_size, CNT_PFS_SIZE);
        assert_eq!(info.icon.expect("icon0.png").width, 512);
        assert_eq!(info.background.expect("pic0.png").height, 2160);
    }

    #[test]
    fn fih_image_reports_signing_and_the_outer_pfs_span() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.pkg");
        let cnt = build_cnt(0x20, 0, &entries());
        std::fs::write(&path, wrap_fih(&cnt, 0x80, 0x20000, 0x10000)).unwrap();

        let info = read_info(&path).unwrap();
        assert_eq!(info.image, Ps5PkgImage::Fih);
        assert_eq!(info.signed, Some(true));
        assert_eq!(info.title.as_deref(), Some("Gousei Game"));
        assert!(
            info.entries.iter().any(|e| e.id == ENTRY_PARAM_JSON),
            "embedded container not found"
        );
        assert_eq!(info.pfs_image_offset, 0x20000);
        assert_eq!(info.pfs_image_size, 0x10000);
    }

    #[test]
    fn lih_image_finds_the_container_at_the_version_specific_slot() {
        let dir = tempfile::tempdir().unwrap();
        let cnt = build_cnt(0x20, 0, &entries());
        for version in [1u16, 3] {
            let path = dir.path().join("game.pkg");
            std::fs::write(&path, wrap_lih(&cnt, version)).unwrap();
            let info = read_info(&path).unwrap();
            assert_eq!(info.image, Ps5PkgImage::Lih);
            assert_eq!(info.signed, Some(false));
            assert_eq!(info.title.as_deref(), Some("Gousei Game"), "v{version}");
        }
    }

    #[test]
    fn truncated_container_still_detects_as_ps5() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.pkg");
        let cnt = build_cnt(0x20, 0, &entries());
        // Cut the file inside the param.json payload so its record is dropped.
        let cut = cnt.len() - 8;
        std::fs::write(&path, &cnt[..cut]).unwrap();
        assert_eq!(
            cnt::detect(&path).unwrap(),
            Some(crate::sony::ps4::CntPlatform::Ps5)
        );
    }

    #[test]
    fn drm_type_and_category_drive_content_kind() {
        let dir = tempfile::tempdir().unwrap();
        for (json, want) in [
            (
                r#"{"applicationDrmType":"demo","applicationCategoryType":0}"#,
                ContentKind::Demo,
            ),
            (r#"{"applicationCategoryType":131584}"#, ContentKind::System),
            (r#"{"applicationCategoryType":0}"#, ContentKind::Game),
        ] {
            let path = dir.path().join("game.pkg");
            std::fs::write(
                &path,
                build_cnt(
                    0x20,
                    0,
                    &[entry(
                        ENTRY_PARAM_JSON,
                        "param.json",
                        json.as_bytes().to_vec(),
                    )],
                ),
            )
            .unwrap();
            assert_eq!(read_info(&path).unwrap().content_kind, Some(want), "{json}");
        }
    }

    #[test]
    fn title_falls_back_to_en_us_then_any_language() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.pkg");
        std::fs::write(
            &path,
            build_cnt(
                0x20,
                0,
                &[entry(
                    ENTRY_PARAM_JSON,
                    "param.json",
                    br#"{"localizedParameters":{"defaultLanguage":"fr-FR","en-US":{"titleName":"Fallback"}}}"#
                        .to_vec(),
                )],
            ),
        )
        .unwrap();

        assert_eq!(read_info(&path).unwrap().title.as_deref(), Some("Fallback"));
    }
}
