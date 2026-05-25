//! `info` extractor for Wii U (WUP) titles. WUD/WUX disc images are
//! not yet supported.

use crate::info::{Image, MultilingualString};
use crate::nintendo::wup::app_xml::AppXml;
use crate::nintendo::wup::loadiine::LoadiineTitle;
use crate::nintendo::wup::meta_image::decode_meta_tga;
use crate::nintendo::wup::meta_source::{DirSource, MetaSource, WuaSource};
use crate::nintendo::wup::meta_xml::MetaXml;
use crate::nintendo::wup::nus::source::NusSource;
use crate::nintendo::wup::wua::ZArchiveReader;
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WupInfo {
    pub title_id: u64,
    pub title_id_hex: String,
    pub title_type: String,
    pub title_version: u32,
    pub group_id: u16,
    pub access_rights: u32,
    pub content_count: usize,
    pub total_content_size: u64,
    pub os_version: Option<u64>,
    pub sdk_version: Option<u32>,
    pub meta: Option<WupMetaInfo>,
    pub source_kind: String,
    /// All titles bundled in the input. Empty for plain directory
    /// inputs; populated when a `.wua` packs base + update + DLC.
    pub bundled_titles: Vec<BundledTitle>,
    /// Highest update title's version, when an update is bundled
    /// alongside the base. The effective game version is `base +
    /// update` so this is shown in preference to `title_version`.
    pub update_version: Option<u32>,
    pub image: Option<Image>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundledTitle {
    pub title_id: u64,
    pub title_id_hex: String,
    pub title_type: String,
    pub title_version: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WupMetaInfo {
    pub long_names: MultilingualString,
    pub short_names: MultilingualString,
    pub publishers: MultilingualString,
    pub product_code: Option<String>,
    pub company_code: Option<String>,
    pub company_name: Option<String>,
    pub region: Option<u32>,
    pub region_names: Vec<String>,
    pub title_id: Option<u64>,
    pub os_version: Option<u64>,
    pub app_size: Option<u64>,
    pub group_id: Option<u32>,
    pub boss_id: Option<u64>,
    pub mastering_date: Option<String>,
    pub content_platform: Option<String>,
    pub logo_type: Option<u32>,
    pub app_launch_type: Option<u32>,
    pub invisible_flag: Option<bool>,
    pub no_managed_flag: Option<bool>,
    pub eula_version: Option<u32>,
    pub drc_use: Option<bool>,
    pub e_manual: Option<bool>,
    pub e_manual_version: Option<u32>,
    pub ext_dev_nunchaku: Option<bool>,
    pub ext_dev_classic: Option<bool>,
    pub ext_dev_urcc: Option<bool>,
    pub ext_dev_board: Option<bool>,
    pub ext_dev_usb_keyboard: Option<bool>,
    pub ext_dev_etc: Option<bool>,
    pub ext_dev_etc_name: Option<String>,
    pub save_size: Option<u64>,
    pub common_save_size: Option<u64>,
    pub account_save_size: Option<u64>,
    pub boss_size: Option<u64>,
    pub common_boss_size: Option<u64>,
    pub account_boss_size: Option<u64>,
    pub network_use: Option<bool>,
    pub online_account_use: Option<bool>,
    pub age_ratings: HashMap<String, u8>,
}

pub fn read_info(path: &Path) -> Result<WupInfo> {
    if path.is_file() {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if ext == "wua" {
            return read_wua(path);
        }
        return Err(anyhow!(
            "wup info: WUD/WUX disc images are not yet supported; extract NUS or loadiine first"
        ));
    }

    let mut src = DirSource::new(path.to_path_buf());
    if let Some(loadiine) = detect_loadiine_from_source(&mut src)? {
        return read_loadiine(&mut src, loadiine, "loadiine");
    }
    read_nus(path)
}

fn read_wua(path: &Path) -> Result<WupInfo> {
    let mut reader =
        ZArchiveReader::open(path).map_err(|e| anyhow!("wup info: open .wua: {}", e))?;
    let titles = reader.top_level_names();
    let bundled = parse_bundled_titles(&titles);
    let title_dir = pick_primary_title_dir(&titles, &bundled)
        .ok_or_else(|| anyhow!("wup info: .wua archive contains no titles"))?;

    let source_kind = if titles.len() > 1 {
        format!("wua ({} titles, showing {})", titles.len(), title_dir)
    } else {
        format!("wua ({})", title_dir)
    };

    let mut src = WuaSource::new(&mut reader, &title_dir);
    let loadiine = detect_loadiine_from_source(&mut src)?.ok_or_else(|| {
        anyhow!(
            "wup info: WUA archive title {} is not loadiine layout (NUS-in-WUA is not yet supported)",
            title_dir
        )
    })?;

    let mut info = read_loadiine(&mut src, loadiine, &source_kind)?;
    info.update_version = bundled
        .iter()
        .filter(|t| is_update_title_id(t.title_id))
        .map(|t| t.title_version)
        .max();
    info.bundled_titles = bundled;
    Ok(info)
}

fn parse_bundled_titles(top_level: &[String]) -> Vec<BundledTitle> {
    let mut out = Vec::with_capacity(top_level.len());
    for name in top_level {
        let Some((tid_hex, version_str)) = name.split_once("_v") else {
            continue;
        };
        if tid_hex.len() != 16 {
            continue;
        }
        let Ok(title_id) = u64::from_str_radix(tid_hex, 16) else {
            continue;
        };
        let Ok(title_version) = version_str.parse::<u32>() else {
            continue;
        };
        out.push(BundledTitle {
            title_id,
            title_id_hex: format!("{:016X}", title_id),
            title_type: title_type_name(title_id),
            title_version,
        });
    }
    out
}

fn pick_primary_title_dir(top_level: &[String], bundled: &[BundledTitle]) -> Option<String> {
    // Pick the base title over Update/DLC so the bundled-update info
    // still describes the actual game when an archive packs all three.
    let preferred = bundled
        .iter()
        .find(|t| is_base_title_id(t.title_id))
        .map(|t| format!("{:016x}_v{}", t.title_id, t.title_version));
    if let Some(name) = preferred
        && top_level.iter().any(|n| n == &name)
    {
        return Some(name);
    }
    top_level.iter().find(|n| !n.is_empty()).cloned()
}

fn is_base_title_id(title_id: u64) -> bool {
    let high = (title_id >> 32) as u32;
    matches!(high, 0x00050000 | 0x00050002)
}

fn is_update_title_id(title_id: u64) -> bool {
    let high = (title_id >> 32) as u32;
    high == 0x0005000E
}

fn read_loadiine(
    src: &mut dyn MetaSource,
    loadiine: LoadiineTitle,
    source_kind: &str,
) -> Result<WupInfo> {
    let app_bytes = src
        .read("code/app.xml")?
        .ok_or_else(|| anyhow!("wup info: code/app.xml missing"))?;
    let app = AppXml::from_bytes(&app_bytes, Path::new("code/app.xml"))
        .context("wup info: parse code/app.xml")?;

    let meta_bytes = src
        .read("meta/meta.xml")?
        .ok_or_else(|| anyhow!("wup info: meta/meta.xml missing"))?;
    let meta = MetaXml::from_bytes(&meta_bytes).context("wup info: parse meta/meta.xml")?;

    let image = read_meta_icon(src);

    let title_id = loadiine.title_id;
    Ok(WupInfo {
        title_id,
        title_id_hex: format!("{:016X}", title_id),
        title_type: title_type_name(title_id),
        title_version: loadiine.title_version,
        group_id: 0,
        access_rights: 0,
        content_count: 0,
        total_content_size: 0,
        os_version: app.os_version,
        sdk_version: app.sdk_version,
        meta: Some(meta_info_from_parsed(meta)),
        source_kind: source_kind.to_string(),
        bundled_titles: Vec::new(),
        update_version: None,
        image,
    })
}

fn read_meta_icon(src: &mut dyn MetaSource) -> Option<Image> {
    let bytes = match src.read("meta/iconTex.tga") {
        Ok(Some(b)) => b,
        Ok(None) => return None,
        Err(e) => {
            log::warn!("wup info: read meta/iconTex.tga failed: {}", e);
            return None;
        }
    };
    match decode_meta_tga(&bytes) {
        Ok(img) => Some(img),
        Err(e) => {
            log::warn!("wup info: decode meta/iconTex.tga failed: {}", e);
            None
        }
    }
}

fn detect_loadiine_from_source(src: &mut dyn MetaSource) -> Result<Option<LoadiineTitle>> {
    if !src.exists("meta/meta.xml")?
        || !src.exists("code/app.xml")?
        || !src.exists("code/cos.xml")?
    {
        return Ok(None);
    }
    let app_bytes = src
        .read("code/app.xml")?
        .ok_or_else(|| anyhow!("wup info: code/app.xml went missing"))?;
    let parsed = AppXml::from_bytes(&app_bytes, Path::new("code/app.xml"))?;
    Ok(Some(LoadiineTitle {
        dir: PathBuf::new(),
        title_id: parsed.title_id,
        title_version: parsed.title_version,
    }))
}

fn read_nus(dir: &Path) -> Result<WupInfo> {
    let mut src = NusSource::open(dir).context("wup info: open NUS source")?;

    let title_id = src.tmd().title_id;
    let title_version = src.tmd().title_version as u32;
    let group_id = src.tmd().group_id;
    let access_rights = src.tmd().access_rights;
    let content_count = src.tmd().contents.len();
    let total_content_size: u64 = src.tmd().contents.iter().map(|c| c.size).sum();

    let meta = match src.read("meta/meta.xml") {
        Ok(Some(bytes)) => match MetaXml::from_bytes(&bytes) {
            Ok(parsed) => Some(meta_info_from_parsed(parsed)),
            Err(e) => {
                log::warn!("wup info: parse meta/meta.xml failed: {}", e);
                None
            }
        },
        Ok(None) => None,
        Err(e) => {
            log::warn!("wup info: read meta/meta.xml from NUS failed: {}", e);
            None
        }
    };

    let image = read_meta_icon(&mut src);

    Ok(WupInfo {
        title_id,
        title_id_hex: format!("{:016X}", title_id),
        title_type: title_type_name(title_id),
        title_version,
        group_id,
        access_rights,
        content_count,
        total_content_size,
        os_version: None,
        sdk_version: None,
        meta,
        source_kind: "nus".to_string(),
        bundled_titles: Vec::new(),
        update_version: None,
        image,
    })
}

fn title_type_name(title_id: u64) -> String {
    let high = (title_id >> 32) as u32;
    match high {
        0x00050000 => "Game".to_string(),
        0x00050002 => "Demo".to_string(),
        0x0005000C => "DLC".to_string(),
        0x0005000E => "Update".to_string(),
        0x00050010 => "System".to_string(),
        other => format!("Unknown(0x{:08X})", other),
    }
}

fn meta_info_from_parsed(meta: MetaXml) -> WupMetaInfo {
    let region_names = meta.region.map(region_mask_names).unwrap_or_default();
    let age_ratings = meta
        .age_ratings
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    let company_name = meta
        .company_code
        .as_deref()
        .and_then(|c| {
            let trimmed = c.trim();
            if trimmed.len() >= 2 {
                crate::util::maker_codes::lookup_maker(&trimmed[..2])
            } else {
                None
            }
        })
        .map(|s| s.to_string());

    WupMetaInfo {
        long_names: meta.long_names,
        short_names: meta.short_names,
        publishers: meta.publishers,
        product_code: meta.product_code,
        company_code: meta.company_code,
        company_name,
        region: meta.region,
        region_names,
        title_id: meta.title_id,
        os_version: meta.os_version,
        app_size: meta.app_size,
        group_id: meta.group_id,
        boss_id: meta.boss_id,
        mastering_date: meta.mastering_date,
        content_platform: meta.content_platform,
        logo_type: meta.logo_type,
        app_launch_type: meta.app_launch_type,
        invisible_flag: meta.invisible_flag,
        no_managed_flag: meta.no_managed_flag,
        eula_version: meta.eula_version,
        drc_use: meta.drc_use,
        e_manual: meta.e_manual,
        e_manual_version: meta.e_manual_version,
        ext_dev_nunchaku: meta.ext_dev_nunchaku,
        ext_dev_classic: meta.ext_dev_classic,
        ext_dev_urcc: meta.ext_dev_urcc,
        ext_dev_board: meta.ext_dev_board,
        ext_dev_usb_keyboard: meta.ext_dev_usb_keyboard,
        ext_dev_etc: meta.ext_dev_etc,
        ext_dev_etc_name: meta.ext_dev_etc_name,
        save_size: meta.save_size,
        common_save_size: meta.common_save_size,
        account_save_size: meta.account_save_size,
        boss_size: meta.boss_size,
        common_boss_size: meta.common_boss_size,
        account_boss_size: meta.account_boss_size,
        network_use: meta.network_use,
        online_account_use: meta.online_account_use,
        age_ratings,
    }
}

fn region_mask_names(mask: u32) -> Vec<String> {
    if mask == 0xFFFFFFFF || mask == 0x7FFFFFFF {
        return vec!["RegionFree".to_string()];
    }
    let mut out = Vec::new();
    if mask & 0x01 != 0 {
        out.push("Japan".to_string());
    }
    if mask & 0x02 != 0 {
        out.push("USA".to_string());
    }
    if mask & 0x04 != 0 {
        out.push("Europe".to_string());
    }
    if mask & 0x08 != 0 {
        out.push("Australia".to_string());
    }
    if mask & 0x10 != 0 {
        out.push("China".to_string());
    }
    if mask & 0x20 != 0 {
        out.push("Korea".to_string());
    }
    if mask & 0x40 != 0 {
        out.push("Taiwan".to_string());
    }
    out
}
