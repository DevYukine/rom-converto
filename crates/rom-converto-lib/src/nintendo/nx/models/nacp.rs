//! Switch NACP (`control.nacp`) parser.
//!
//! 16-language ApplicationTitle table followed by fixed-offset fields
//! per the layout at switchbrew.org/wiki/NACP_Format.

use crate::nintendo::nx::error::{NxError, NxResult};

pub const NACP_TITLE_TABLE_SIZE: usize = 0x3000;
pub const NACP_TITLE_ENTRY_SIZE: usize = 0x300;
pub const NACP_NAME_SIZE: usize = 0x200;
pub const NACP_PUBLISHER_SIZE: usize = 0x100;
pub const NACP_LANGUAGE_COUNT: usize = 16;

pub const NACP_OFFSET_STARTUP_USER_ACCOUNT: usize = 0x3025;
pub const NACP_OFFSET_ATTRIBUTE_FLAG: usize = 0x3028;
pub const NACP_OFFSET_SUPPORTED_LANGUAGE: usize = 0x302C;
pub const NACP_OFFSET_PARENTAL_CONTROL: usize = 0x3030;
pub const NACP_OFFSET_SCREENSHOT: usize = 0x3034;
pub const NACP_OFFSET_VIDEO_CAPTURE: usize = 0x3035;
pub const NACP_OFFSET_RATING_AGE: usize = 0x3040;
pub const NACP_OFFSET_DISPLAY_VERSION: usize = 0x3060;
pub const NACP_OFFSET_USER_ACCOUNT_SAVE: usize = 0x3080;
pub const NACP_OFFSET_USER_ACCOUNT_SAVE_JOURNAL: usize = 0x3088;
pub const NACP_OFFSET_DEVICE_SAVE: usize = 0x3090;
pub const NACP_OFFSET_DEVICE_SAVE_JOURNAL: usize = 0x3098;
pub const NACP_OFFSET_BCAT_SAVE: usize = 0x30A0;
pub const NACP_OFFSET_ADDON_INSTALL_POLICY: usize = 0x30F2;
pub const NACP_OFFSET_SCREEN_ORIENTATION: usize = 0x30FC;

/// Language slots in NACP order. Switchbrew indexes them 0..15.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NacpLanguage {
    AmericanEnglish = 0,
    BritishEnglish = 1,
    Japanese = 2,
    French = 3,
    German = 4,
    LatinAmericanSpanish = 5,
    Spanish = 6,
    Italian = 7,
    Dutch = 8,
    CanadianFrench = 9,
    Portuguese = 10,
    Russian = 11,
    Korean = 12,
    TaiwaneseChinese = 13,
    Chinese = 14,
    BrazilianPortuguese = 15,
}

impl NacpLanguage {
    pub const ALL: [NacpLanguage; 16] = [
        Self::AmericanEnglish,
        Self::BritishEnglish,
        Self::Japanese,
        Self::French,
        Self::German,
        Self::LatinAmericanSpanish,
        Self::Spanish,
        Self::Italian,
        Self::Dutch,
        Self::CanadianFrench,
        Self::Portuguese,
        Self::Russian,
        Self::Korean,
        Self::TaiwaneseChinese,
        Self::Chinese,
        Self::BrazilianPortuguese,
    ];

    pub fn icon_file_name(self) -> &'static str {
        match self {
            Self::AmericanEnglish => "icon_AmericanEnglish.dat",
            Self::BritishEnglish => "icon_BritishEnglish.dat",
            Self::Japanese => "icon_Japanese.dat",
            Self::French => "icon_French.dat",
            Self::German => "icon_German.dat",
            Self::LatinAmericanSpanish => "icon_LatinAmericanSpanish.dat",
            Self::Spanish => "icon_Spanish.dat",
            Self::Italian => "icon_Italian.dat",
            Self::Dutch => "icon_Dutch.dat",
            Self::CanadianFrench => "icon_CanadianFrench.dat",
            Self::Portuguese => "icon_Portuguese.dat",
            Self::Russian => "icon_Russian.dat",
            Self::Korean => "icon_Korean.dat",
            Self::TaiwaneseChinese => "icon_TaiwaneseChinese.dat",
            Self::Chinese => "icon_Chinese.dat",
            Self::BrazilianPortuguese => "icon_BrazilianPortuguese.dat",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Nacp {
    pub titles: Vec<NacpTitle>,
    pub display_version: String,
    pub startup_user_account: u8,
    pub screenshot: u8,
    pub video_capture: u8,
    pub attribute_flag: u32,
    pub supported_language_bitmask: u32,
    pub parental_control_flag: u32,
    pub user_account_save: i64,
    pub user_account_save_journal: i64,
    pub device_save: i64,
    pub device_save_journal: i64,
    pub bcat_save: i64,
    pub rating_age: [i8; 32],
    pub addon_install_policy: u8,
    pub screen_orientation: u8,
}

#[derive(Debug, Clone)]
pub struct NacpTitle {
    pub language: NacpLanguage,
    pub name: String,
    pub publisher: String,
}

impl Nacp {
    pub fn parse(buf: &[u8]) -> NxResult<Self> {
        if buf.len() < 0x4000 {
            return Err(NxError::InvalidNcaHeader);
        }

        let mut titles = Vec::with_capacity(NACP_LANGUAGE_COUNT);
        for (idx, lang) in NacpLanguage::ALL.iter().enumerate() {
            let off = idx * NACP_TITLE_ENTRY_SIZE;
            let name = read_utf8_string(&buf[off..off + NACP_NAME_SIZE]);
            let publisher = read_utf8_string(
                &buf[off + NACP_NAME_SIZE..off + NACP_NAME_SIZE + NACP_PUBLISHER_SIZE],
            );
            if name.is_empty() && publisher.is_empty() {
                continue;
            }
            titles.push(NacpTitle {
                language: *lang,
                name,
                publisher,
            });
        }

        let display_version =
            read_utf8_string(&buf[NACP_OFFSET_DISPLAY_VERSION..NACP_OFFSET_DISPLAY_VERSION + 0x10]);

        let mut rating_age = [0i8; 32];
        for (i, slot) in rating_age.iter_mut().enumerate() {
            *slot = buf[NACP_OFFSET_RATING_AGE + i] as i8;
        }

        Ok(Self {
            titles,
            display_version,
            startup_user_account: buf[NACP_OFFSET_STARTUP_USER_ACCOUNT],
            screenshot: buf[NACP_OFFSET_SCREENSHOT],
            video_capture: buf[NACP_OFFSET_VIDEO_CAPTURE],
            attribute_flag: read_u32_at(buf, NACP_OFFSET_ATTRIBUTE_FLAG),
            supported_language_bitmask: read_u32_at(buf, NACP_OFFSET_SUPPORTED_LANGUAGE),
            parental_control_flag: read_u32_at(buf, NACP_OFFSET_PARENTAL_CONTROL),
            user_account_save: read_i64_at(buf, NACP_OFFSET_USER_ACCOUNT_SAVE),
            user_account_save_journal: read_i64_at(buf, NACP_OFFSET_USER_ACCOUNT_SAVE_JOURNAL),
            device_save: read_i64_at(buf, NACP_OFFSET_DEVICE_SAVE),
            device_save_journal: read_i64_at(buf, NACP_OFFSET_DEVICE_SAVE_JOURNAL),
            bcat_save: read_i64_at(buf, NACP_OFFSET_BCAT_SAVE),
            rating_age,
            addon_install_policy: buf[NACP_OFFSET_ADDON_INSTALL_POLICY],
            screen_orientation: buf[NACP_OFFSET_SCREEN_ORIENTATION],
        })
    }
}

fn read_utf8_string(slice: &[u8]) -> String {
    let end = slice.iter().position(|b| *b == 0).unwrap_or(slice.len());
    String::from_utf8_lossy(&slice[..end]).into_owned()
}

fn read_u32_at(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

fn read_i64_at(buf: &[u8], off: usize) -> i64 {
    i64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_test_nacp() -> Vec<u8> {
        let mut buf = vec![0u8; 0x4000];
        // English (idx 0): name "Test Game", publisher "Test Publisher"
        buf[0..9].copy_from_slice(b"Test Game");
        buf[NACP_NAME_SIZE..NACP_NAME_SIZE + 14].copy_from_slice(b"Test Publisher");
        // Japanese (idx 2)
        let jp_off = 2 * NACP_TITLE_ENTRY_SIZE;
        let jp_bytes = "ジャパン".as_bytes();
        buf[jp_off..jp_off + jp_bytes.len()].copy_from_slice(jp_bytes);
        buf[NACP_OFFSET_DISPLAY_VERSION..NACP_OFFSET_DISPLAY_VERSION + 5]
            .copy_from_slice(b"1.0.0");
        buf[NACP_OFFSET_ATTRIBUTE_FLAG..NACP_OFFSET_ATTRIBUTE_FLAG + 4]
            .copy_from_slice(&0x00000003u32.to_le_bytes());
        buf[NACP_OFFSET_USER_ACCOUNT_SAVE..NACP_OFFSET_USER_ACCOUNT_SAVE + 8]
            .copy_from_slice(&0x00010000i64.to_le_bytes());
        buf
    }

    #[test]
    fn parses_titles_and_fields() {
        let buf = build_test_nacp();
        let n = Nacp::parse(&buf).unwrap();
        assert_eq!(n.display_version, "1.0.0");
        assert_eq!(n.attribute_flag, 3);
        assert_eq!(n.user_account_save, 0x10000);
        assert_eq!(n.titles.len(), 2);
        assert_eq!(n.titles[0].language, NacpLanguage::AmericanEnglish);
        assert_eq!(n.titles[0].name, "Test Game");
        assert_eq!(n.titles[0].publisher, "Test Publisher");
        assert_eq!(n.titles[1].language, NacpLanguage::Japanese);
    }

    #[test]
    fn rejects_too_small_buffer() {
        assert!(Nacp::parse(&[0u8; 100]).is_err());
    }

    #[test]
    fn icon_file_name_round_trips() {
        for lang in NacpLanguage::ALL {
            let name = lang.icon_file_name();
            assert!(name.starts_with("icon_"));
            assert!(name.ends_with(".dat"));
        }
    }
}
