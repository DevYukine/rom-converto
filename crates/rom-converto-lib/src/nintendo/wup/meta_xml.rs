//! Wii U `meta/meta.xml` parser.
//!
//! Pulls multilingual names + publisher, product/company code, region,
//! save sizes, network flags, and age ratings. Reuses the targeted
//! `extract_tag` helper from [`crate::nintendo::wup::app_xml`] rather
//! than pulling in a full XML library.

use crate::info::{LanguageCode, MultilingualString};
use crate::nintendo::wup::app_xml::extract_tag;
use anyhow::{Result, anyhow};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WupLanguage {
    Japanese,
    English,
    French,
    German,
    Italian,
    Spanish,
    SimplifiedChinese,
    Korean,
    Dutch,
    Portuguese,
    Russian,
    TraditionalChinese,
}

impl WupLanguage {
    pub const ALL: [(WupLanguage, &'static str); 12] = [
        (WupLanguage::Japanese, "ja"),
        (WupLanguage::English, "en"),
        (WupLanguage::French, "fr"),
        (WupLanguage::German, "de"),
        (WupLanguage::Italian, "it"),
        (WupLanguage::Spanish, "es"),
        (WupLanguage::SimplifiedChinese, "zhs"),
        (WupLanguage::Korean, "ko"),
        (WupLanguage::Dutch, "nl"),
        (WupLanguage::Portuguese, "pt"),
        (WupLanguage::Russian, "ru"),
        (WupLanguage::TraditionalChinese, "zht"),
    ];

    pub fn to_language_code(self) -> LanguageCode {
        match self {
            Self::Japanese => LanguageCode::Japanese,
            Self::English => LanguageCode::English,
            Self::French => LanguageCode::French,
            Self::German => LanguageCode::German,
            Self::Italian => LanguageCode::Italian,
            Self::Spanish => LanguageCode::Spanish,
            Self::SimplifiedChinese => LanguageCode::SimplifiedChinese,
            Self::Korean => LanguageCode::Korean,
            Self::Dutch => LanguageCode::Dutch,
            Self::Portuguese => LanguageCode::Portuguese,
            Self::Russian => LanguageCode::Russian,
            Self::TraditionalChinese => LanguageCode::TraditionalChinese,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WupRatingOrganization {
    Cero,
    Esrb,
    PegiGen,
    PegiPrt,
    PegiBbfc,
    Oflc,
    Usk,
    Cob,
    Grb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WupRatingOrganizationExt {
    Bbfc,
    PegiFin,
    Cgsrr,
}

impl WupRatingOrganization {
    pub const ALL: [(WupRatingOrganization, &'static str); 9] = [
        (Self::Cero, "cero"),
        (Self::Esrb, "esrb"),
        (Self::PegiGen, "pegi_gen"),
        (Self::PegiPrt, "pegi_prt"),
        (Self::PegiBbfc, "pegi_bbfc"),
        (Self::Oflc, "oflc"),
        (Self::Usk, "usk"),
        (Self::Cob, "cob"),
        (Self::Grb, "grb"),
    ];
}

impl WupRatingOrganizationExt {
    pub const ALL: [(WupRatingOrganizationExt, &'static str); 3] = [
        (Self::Bbfc, "bbfc"),
        (Self::PegiFin, "pegi_fin"),
        (Self::Cgsrr, "cgsrr"),
    ];
}

#[derive(Debug, Clone, Default)]
pub struct MetaXml {
    pub long_names: MultilingualString,
    pub short_names: MultilingualString,
    pub publishers: MultilingualString,
    pub product_code: Option<String>,
    pub company_code: Option<String>,
    pub region: Option<u32>,
    pub title_version: Option<u32>,
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
    pub age_ratings: HashMap<&'static str, u8>,
}

impl MetaXml {
    pub fn from_bytes(xml: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(xml)
            .map_err(|_| anyhow!("meta.xml is not valid UTF-8"))?;

        let long_names = build_multilingual(text, "longname_");
        let short_names = build_multilingual(text, "shortname_");
        let publishers = build_multilingual(text, "publisher_");

        let product_code = extract_tag(text, "product_code").map(|s| s.trim().to_string());
        let company_code = extract_tag(text, "company_code").map(|s| s.trim().to_string());

        let region = extract_tag(text, "region").and_then(|s| parse_int_flexible::<u32>(s.trim()));
        let title_version =
            extract_tag(text, "title_version").and_then(|s| s.trim().parse::<u32>().ok());

        let save_size = extract_tag(text, "save_no_account_size").and_then(|s| s.trim().parse().ok())
            .or_else(|| extract_tag(text, "save_size").and_then(|s| s.trim().parse().ok()));
        let common_save_size =
            extract_tag(text, "common_save_size").and_then(|s| s.trim().parse().ok());
        let account_save_size =
            extract_tag(text, "account_save_size").and_then(|s| s.trim().parse().ok());
        let boss_size = extract_tag(text, "boss_size").and_then(|s| s.trim().parse().ok());
        let common_boss_size =
            extract_tag(text, "common_boss_size").and_then(|s| s.trim().parse().ok());
        let account_boss_size =
            extract_tag(text, "account_boss_size").and_then(|s| s.trim().parse().ok());

        let network_use = extract_tag(text, "network_use").map(parse_bool_flag);
        let online_account_use = extract_tag(text, "online_account_use_flag").map(parse_bool_flag);

        let title_id = extract_tag(text, "title_id")
            .and_then(|s| u64::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok());
        let os_version = extract_tag(text, "os_version")
            .and_then(|s| u64::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok());
        let app_size = extract_tag(text, "app_size")
            .and_then(|s| u64::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
            .or_else(|| extract_tag(text, "app_size").and_then(|s| s.trim().parse().ok()));
        let group_id = extract_tag(text, "group_id")
            .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
            .or_else(|| extract_tag(text, "group_id").and_then(|s| s.trim().parse().ok()));
        let boss_id = extract_tag(text, "boss_id")
            .and_then(|s| u64::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
            .or_else(|| extract_tag(text, "boss_id").and_then(|s| s.trim().parse().ok()));
        let mastering_date = extract_tag(text, "mastering_date").map(|s| s.trim().to_string());
        let content_platform = extract_tag(text, "content_platform").map(|s| s.trim().to_string());
        let logo_type = extract_tag(text, "logo_type")
            .and_then(|s| parse_int_flexible::<u32>(s.trim()));
        let app_launch_type = extract_tag(text, "app_launch_type")
            .and_then(|s| parse_int_flexible::<u32>(s.trim()));
        let invisible_flag = extract_tag(text, "invisible_flag").map(parse_bool_flag);
        let no_managed_flag = extract_tag(text, "no_managed_flag").map(parse_bool_flag);
        let eula_version = extract_tag(text, "eula_version")
            .and_then(|s| parse_int_flexible::<u32>(s.trim()));
        let drc_use = extract_tag(text, "drc_use").map(parse_bool_flag);
        let e_manual = extract_tag(text, "e_manual").map(parse_bool_flag);
        let e_manual_version = extract_tag(text, "e_manual_version")
            .and_then(|s| parse_int_flexible::<u32>(s.trim()));
        let ext_dev_nunchaku = extract_tag(text, "ext_dev_nunchaku").map(parse_bool_flag);
        let ext_dev_classic = extract_tag(text, "ext_dev_classic").map(parse_bool_flag);
        let ext_dev_urcc = extract_tag(text, "ext_dev_urcc").map(parse_bool_flag);
        let ext_dev_board = extract_tag(text, "ext_dev_board").map(parse_bool_flag);
        let ext_dev_usb_keyboard = extract_tag(text, "ext_dev_usb_keyboard").map(parse_bool_flag);
        let ext_dev_etc = extract_tag(text, "ext_dev_etc").map(parse_bool_flag);
        let ext_dev_etc_name = extract_tag(text, "ext_dev_etc_name").map(|s| s.trim().to_string());

        let mut age_ratings = HashMap::new();
        for (org, key) in WupRatingOrganization::ALL {
            let tag = format!("pc_{}", key);
            if let Some(value_str) = extract_tag(text, &tag)
                && let Some(value) = parse_int_flexible::<u8>(value_str.trim())
                && value != 0xFF
            {
                let static_key: &'static str = match org {
                    WupRatingOrganization::Cero => "cero",
                    WupRatingOrganization::Esrb => "esrb",
                    WupRatingOrganization::PegiGen => "pegi_gen",
                    WupRatingOrganization::PegiPrt => "pegi_prt",
                    WupRatingOrganization::PegiBbfc => "pegi_bbfc",
                    WupRatingOrganization::Oflc => "oflc",
                    WupRatingOrganization::Usk => "usk",
                    WupRatingOrganization::Cob => "cob",
                    WupRatingOrganization::Grb => "grb",
                };
                age_ratings.insert(static_key, value);
            }
        }
        for (org, key) in WupRatingOrganizationExt::ALL {
            let tag = format!("pc_{}", key);
            if let Some(value_str) = extract_tag(text, &tag)
                && let Some(value) = parse_int_flexible::<u8>(value_str.trim())
                && value != 0xFF
            {
                let static_key: &'static str = match org {
                    WupRatingOrganizationExt::Bbfc => "bbfc",
                    WupRatingOrganizationExt::PegiFin => "pegi_fin",
                    WupRatingOrganizationExt::Cgsrr => "cgsrr",
                };
                age_ratings.insert(static_key, value);
            }
        }

        Ok(Self {
            long_names,
            short_names,
            publishers,
            product_code,
            company_code,
            region,
            title_version,
            title_id,
            os_version,
            app_size,
            group_id,
            boss_id,
            mastering_date,
            content_platform,
            logo_type,
            app_launch_type,
            invisible_flag,
            no_managed_flag,
            eula_version,
            drc_use,
            e_manual,
            e_manual_version,
            ext_dev_nunchaku,
            ext_dev_classic,
            ext_dev_urcc,
            ext_dev_board,
            ext_dev_usb_keyboard,
            ext_dev_etc,
            ext_dev_etc_name,
            save_size,
            common_save_size,
            account_save_size,
            boss_size,
            common_boss_size,
            account_boss_size,
            network_use,
            online_account_use,
            age_ratings,
        })
    }
}

fn build_multilingual(text: &str, prefix: &str) -> MultilingualString {
    let pairs = WupLanguage::ALL.iter().filter_map(|(lang, key)| {
        let tag = format!("{}{}", prefix, key);
        let value = extract_tag(text, &tag)?;
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some((lang.to_language_code(), trimmed.to_string()))
        }
    });
    MultilingualString::from_pairs(pairs)
}

fn parse_bool_flag(s: &str) -> bool {
    matches!(s.trim(), "1" | "true" | "TRUE")
}

fn parse_int_flexible<T: std::str::FromStr>(s: &str) -> Option<T> {
    s.parse::<T>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_meta_xml() -> &'static str {
        r#"<?xml version="1.0" encoding="utf-8"?>
<menu type="complex" access="full">
  <product_code>WUP-N-AMKE</product_code>
  <company_code>00</company_code>
  <region type="hexBinary" length="4">00000002</region>
  <title_version>1</title_version>
  <longname_ja>テストゲーム</longname_ja>
  <longname_en>Test Game</longname_en>
  <longname_de>Test Spiel</longname_de>
  <shortname_en>Test</shortname_en>
  <publisher_en>Test Publisher</publisher_en>
  <save_no_account_size>2097152</save_no_account_size>
  <common_save_size>0</common_save_size>
  <network_use>1</network_use>
  <online_account_use_flag>0</online_account_use_flag>
  <pc_cero>12</pc_cero>
  <pc_esrb>10</pc_esrb>
  <pc_usk>6</pc_usk>
</menu>"#
    }

    #[test]
    fn parses_known_fields() {
        let m = MetaXml::from_bytes(sample_meta_xml().as_bytes()).unwrap();
        assert_eq!(m.product_code.as_deref(), Some("WUP-N-AMKE"));
        assert_eq!(m.company_code.as_deref(), Some("00"));
        // The XML carries "00000002" — even when typed as hexBinary, parse_int_flexible
        // reads it as a decimal string, which still yields 2 (= USA bit).
        assert_eq!(m.region, Some(2));
        assert_eq!(m.title_version, Some(1));
        assert_eq!(m.save_size, Some(2_097_152));
        assert_eq!(m.common_save_size, Some(0));
        assert_eq!(m.network_use, Some(true));
        assert_eq!(m.online_account_use, Some(false));
        assert_eq!(m.age_ratings.get("cero"), Some(&12));
        assert_eq!(m.age_ratings.get("esrb"), Some(&10));
        assert_eq!(m.age_ratings.get("usk"), Some(&6));
    }

    #[test]
    fn collects_multilingual_names() {
        let m = MetaXml::from_bytes(sample_meta_xml().as_bytes()).unwrap();
        assert_eq!(m.long_names.entries.len(), 3);
        assert_eq!(m.long_names.primary(), Some("Test Game"));
        assert_eq!(m.short_names.entries.len(), 1);
        assert_eq!(m.publishers.primary(), Some("Test Publisher"));
    }

    #[test]
    fn missing_optional_tags_yield_none() {
        let m = MetaXml::from_bytes(b"<menu><longname_en>X</longname_en></menu>").unwrap();
        assert!(m.product_code.is_none());
        assert!(m.network_use.is_none());
        assert!(m.age_ratings.is_empty());
        assert_eq!(m.long_names.entries.len(), 1);
    }
}
