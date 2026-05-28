//! 3DS SMDH (System Menu Data Header) parser.
//!
//! Layout per 3dbrew.org/wiki/SMDH. 0x36C0 bytes total: a 12-language
//! title table, region lock bitmask, age-rating block, flag bitfield,
//! and two icons (24x24 + 48x48, both RGB565 Morton-tiled).

use anyhow::{Result, anyhow};
use byteorder::{LE, ReadBytesExt};
use std::io::Cursor;

pub const SMDH_TOTAL_SIZE: usize = 0x36C0;
pub const SMDH_MAGIC: [u8; 4] = *b"SMDH";

pub const SMDH_TITLES_OFFSET: usize = 0x0008;
pub const SMDH_TITLE_ENTRY_SIZE: usize = 0x0200;
pub const SMDH_AGE_RATINGS_OFFSET: usize = 0x2008;
pub const SMDH_AGE_RATINGS_SIZE: usize = 16;
pub const SMDH_REGION_LOCK_OFFSET: usize = 0x2018;
pub const SMDH_FLAGS_OFFSET: usize = 0x2028;
pub const SMDH_EULA_VERSION_OFFSET: usize = 0x202C;
pub const SMDH_OPTIMAL_ANIM_FRAME_OFFSET: usize = 0x2030;
pub const SMDH_SMALL_ICON_OFFSET: usize = 0x2040;
pub const SMDH_LARGE_ICON_OFFSET: usize = 0x24C0;
pub const SMDH_SMALL_ICON_BYTES: usize = 0x480;
pub const SMDH_LARGE_ICON_BYTES: usize = 0x1200;
pub const SMDH_SMALL_ICON_DIM: u32 = 24;
pub const SMDH_LARGE_ICON_DIM: u32 = 48;

pub const SMDH_LANGUAGE_COUNT: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SmdhLanguage {
    Japanese = 0,
    English = 1,
    French = 2,
    German = 3,
    Italian = 4,
    Spanish = 5,
    SimplifiedChinese = 6,
    Korean = 7,
    Dutch = 8,
    Portuguese = 9,
    Russian = 10,
    TraditionalChinese = 11,
}

impl SmdhLanguage {
    pub const ACTIVE: [SmdhLanguage; 12] = [
        Self::Japanese,
        Self::English,
        Self::French,
        Self::German,
        Self::Italian,
        Self::Spanish,
        Self::SimplifiedChinese,
        Self::Korean,
        Self::Dutch,
        Self::Portuguese,
        Self::Russian,
        Self::TraditionalChinese,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgeRatingRegion {
    Cero = 0,
    Esrb = 1,
    Usk = 3,
    PegiGen = 4,
    PegiPrt = 6,
    PegiBbfc = 7,
    Cob = 8,
    Grb = 9,
    Cgsrr = 10,
}

impl AgeRatingRegion {
    pub const ALL: [AgeRatingRegion; 9] = [
        Self::Cero,
        Self::Esrb,
        Self::Usk,
        Self::PegiGen,
        Self::PegiPrt,
        Self::PegiBbfc,
        Self::Cob,
        Self::Grb,
        Self::Cgsrr,
    ];
}

#[derive(Debug, Clone)]
pub struct Smdh {
    pub titles: Vec<SmdhTitle>,
    pub region_lock: u32,
    pub flags: u32,
    pub eula_version_major: u8,
    pub eula_version_minor: u8,
    pub age_ratings: [u8; SMDH_AGE_RATINGS_SIZE],
    pub small_icon: Vec<u8>,
    pub large_icon: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct SmdhTitle {
    pub language: SmdhLanguage,
    pub short_description: String,
    pub long_description: String,
    pub publisher: String,
}

#[derive(Debug, Clone, Copy)]
pub struct AgeRating {
    pub region: AgeRatingRegion,
    /// 0..=31; the actual age value, only meaningful when `enabled`.
    pub age: u8,
    pub enabled: bool,
    pub pending: bool,
    pub banned: bool,
}

impl Smdh {
    pub fn parse(buf: &[u8]) -> Result<Self> {
        if buf.len() < SMDH_TOTAL_SIZE {
            return Err(anyhow!(
                "SMDH input too short: {} bytes (need {})",
                buf.len(),
                SMDH_TOTAL_SIZE
            ));
        }
        if buf[0..4] != SMDH_MAGIC {
            return Err(anyhow!("SMDH magic missing"));
        }

        let mut titles = Vec::with_capacity(12);
        for (i, lang) in SmdhLanguage::ACTIVE.iter().enumerate() {
            let off = SMDH_TITLES_OFFSET + i * SMDH_TITLE_ENTRY_SIZE;
            let short_description = read_utf16_string(&buf[off..off + 0x80]);
            let long_description = read_utf16_string(&buf[off + 0x80..off + 0x180]);
            let publisher = read_utf16_string(&buf[off + 0x180..off + 0x200]);
            if short_description.is_empty() && long_description.is_empty() && publisher.is_empty() {
                continue;
            }
            titles.push(SmdhTitle {
                language: *lang,
                short_description,
                long_description,
                publisher,
            });
        }

        let mut age_ratings = [0u8; SMDH_AGE_RATINGS_SIZE];
        age_ratings.copy_from_slice(
            &buf[SMDH_AGE_RATINGS_OFFSET..SMDH_AGE_RATINGS_OFFSET + SMDH_AGE_RATINGS_SIZE],
        );

        let region_lock = (&buf[SMDH_REGION_LOCK_OFFSET..SMDH_REGION_LOCK_OFFSET + 4])
            .read_u32::<LE>()
            .map_err(|e| anyhow!("smdh region lock: {e}"))?;
        let flags = (&buf[SMDH_FLAGS_OFFSET..SMDH_FLAGS_OFFSET + 4])
            .read_u32::<LE>()
            .map_err(|e| anyhow!("smdh flags: {e}"))?;
        let eula_version_minor = buf[SMDH_EULA_VERSION_OFFSET];
        let eula_version_major = buf[SMDH_EULA_VERSION_OFFSET + 1];

        let small_icon =
            buf[SMDH_SMALL_ICON_OFFSET..SMDH_SMALL_ICON_OFFSET + SMDH_SMALL_ICON_BYTES].to_vec();
        let large_icon =
            buf[SMDH_LARGE_ICON_OFFSET..SMDH_LARGE_ICON_OFFSET + SMDH_LARGE_ICON_BYTES].to_vec();

        Ok(Self {
            titles,
            region_lock,
            flags,
            eula_version_major,
            eula_version_minor,
            age_ratings,
            small_icon,
            large_icon,
        })
    }

    pub fn enabled_age_ratings(&self) -> Vec<AgeRating> {
        AgeRatingRegion::ALL
            .iter()
            .filter_map(|r| {
                let raw = self.age_ratings[*r as usize];
                let enabled = raw & 0x80 != 0;
                if !enabled {
                    return None;
                }
                Some(AgeRating {
                    region: *r,
                    age: raw & 0x1F,
                    enabled,
                    pending: raw & 0x40 != 0,
                    banned: raw & 0x20 != 0,
                })
            })
            .collect()
    }
}

fn read_utf16_string(slice: &[u8]) -> String {
    let mut cur = Cursor::new(slice);
    let mut units = Vec::new();
    while let Ok(u) = cur.read_u16::<LE>() {
        if u == 0 {
            break;
        }
        units.push(u);
    }
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_minimal_smdh() -> Vec<u8> {
        let mut buf = vec![0u8; SMDH_TOTAL_SIZE];
        buf[0..4].copy_from_slice(&SMDH_MAGIC);
        // English (index 1) short = "Hi", long = "Hello", publisher = "Pub"
        let entry_off = SMDH_TITLES_OFFSET + SMDH_TITLE_ENTRY_SIZE;
        let short_bytes: Vec<u16> = "Hi".encode_utf16().collect();
        for (i, u) in short_bytes.iter().enumerate() {
            let off = entry_off + i * 2;
            buf[off..off + 2].copy_from_slice(&u.to_le_bytes());
        }
        let long_bytes: Vec<u16> = "Hello".encode_utf16().collect();
        for (i, u) in long_bytes.iter().enumerate() {
            let off = entry_off + 0x80 + i * 2;
            buf[off..off + 2].copy_from_slice(&u.to_le_bytes());
        }
        let pub_bytes: Vec<u16> = "Pub".encode_utf16().collect();
        for (i, u) in pub_bytes.iter().enumerate() {
            let off = entry_off + 0x180 + i * 2;
            buf[off..off + 2].copy_from_slice(&u.to_le_bytes());
        }
        // Region: Japan (0x01)
        buf[SMDH_REGION_LOCK_OFFSET..SMDH_REGION_LOCK_OFFSET + 4]
            .copy_from_slice(&0x00000001u32.to_le_bytes());
        // Flags: visible + auto_save
        buf[SMDH_FLAGS_OFFSET..SMDH_FLAGS_OFFSET + 4].copy_from_slice(&0x00000003u32.to_le_bytes());
        // CERO 12 enabled
        buf[SMDH_AGE_RATINGS_OFFSET + AgeRatingRegion::Cero as usize] = 0x80 | 12;
        // PEGI 7
        buf[SMDH_AGE_RATINGS_OFFSET + AgeRatingRegion::PegiGen as usize] = 0x80 | 7;
        buf
    }

    #[test]
    fn parses_minimal_smdh() {
        let buf = build_minimal_smdh();
        let s = Smdh::parse(&buf).unwrap();
        assert_eq!(s.region_lock, 0x1);
        assert_eq!(s.flags, 0x3);
        assert_eq!(s.titles.len(), 1);
        assert_eq!(s.titles[0].language, SmdhLanguage::English);
        assert_eq!(s.titles[0].short_description, "Hi");
        assert_eq!(s.titles[0].long_description, "Hello");
        assert_eq!(s.titles[0].publisher, "Pub");
        assert_eq!(s.small_icon.len(), SMDH_SMALL_ICON_BYTES);
        assert_eq!(s.large_icon.len(), SMDH_LARGE_ICON_BYTES);
    }

    #[test]
    fn parses_age_ratings() {
        let buf = build_minimal_smdh();
        let s = Smdh::parse(&buf).unwrap();
        let ratings = s.enabled_age_ratings();
        assert_eq!(ratings.len(), 2);
        let cero = ratings
            .iter()
            .find(|r| r.region == AgeRatingRegion::Cero)
            .unwrap();
        assert_eq!(cero.age, 12);
        assert!(cero.enabled);
        assert!(!cero.banned);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut buf = vec![0u8; SMDH_TOTAL_SIZE];
        buf[0..4].copy_from_slice(b"XXXX");
        assert!(Smdh::parse(&buf).is_err());
    }

    #[test]
    fn rejects_too_small() {
        assert!(Smdh::parse(&[0u8; 100]).is_err());
    }
}
