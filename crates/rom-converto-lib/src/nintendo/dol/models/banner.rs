//! GameCube `opening.bnr` parser.
//!
//! Two formats:
//!   - BNR1 (single language, typically Latin-1) used by US/EU titles.
//!   - BNR2 (six languages: Japanese, English, German, French, Spanish,
//!     Italian) used by some PAL/JP titles.
//!
//! Both formats embed a 96x32 RGB5A3 banner image at offset 0x20.

use anyhow::{Result, anyhow};

pub const BANNER_IMAGE_OFFSET: usize = 0x20;
pub const BANNER_IMAGE_WIDTH: u32 = 96;
pub const BANNER_IMAGE_HEIGHT: u32 = 32;
pub const BANNER_IMAGE_BYTES: usize = 6144;

pub const BNR1_MAGIC: [u8; 4] = *b"BNR1";
pub const BNR2_MAGIC: [u8; 4] = *b"BNR2";

pub const BANNER_LANG_BLOCK_SIZE: usize = 0x140;
pub const BNR1_FILE_SIZE: usize = 0x1820 + BANNER_LANG_BLOCK_SIZE;
pub const BNR2_FILE_SIZE: usize = 0x1820 + 6 * BANNER_LANG_BLOCK_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerFormat {
    Bnr1,
    Bnr2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerLanguage {
    /// BNR1 carries a single language slot (region-dependent: typically
    /// English for US, German for German PAL, etc.). We expose it as
    /// `BannerLanguage::Default`.
    Default,
    Japanese,
    English,
    German,
    French,
    Spanish,
    Italian,
}

#[derive(Debug, Clone)]
pub struct BannerTitle {
    pub language: BannerLanguage,
    pub short_game_name: String,
    pub short_maker: String,
    pub long_game_name: String,
    pub long_maker: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct GcBanner {
    pub format: BannerFormat,
    pub titles: Vec<BannerTitle>,
    /// Raw RGB5A3 4x4-tiled pixels (6144 bytes).
    pub image_raw: Vec<u8>,
}

impl GcBanner {
    pub fn parse(buf: &[u8]) -> Result<Self> {
        if buf.len() < BNR1_FILE_SIZE {
            return Err(anyhow!("opening.bnr too small: {} bytes", buf.len()));
        }
        let magic: [u8; 4] = buf[0..4].try_into()?;
        let format = match magic {
            BNR1_MAGIC => BannerFormat::Bnr1,
            BNR2_MAGIC => BannerFormat::Bnr2,
            _ => return Err(anyhow!("opening.bnr has unknown magic")),
        };
        let image_raw =
            buf[BANNER_IMAGE_OFFSET..BANNER_IMAGE_OFFSET + BANNER_IMAGE_BYTES].to_vec();

        let titles_start = BANNER_IMAGE_OFFSET + BANNER_IMAGE_BYTES;
        let titles = match format {
            BannerFormat::Bnr1 => {
                if buf.len() < BNR1_FILE_SIZE {
                    return Err(anyhow!("BNR1 file truncated"));
                }
                let block = &buf[titles_start..titles_start + BANNER_LANG_BLOCK_SIZE];
                vec![parse_block(BannerLanguage::Default, block)]
            }
            BannerFormat::Bnr2 => {
                if buf.len() < BNR2_FILE_SIZE {
                    return Err(anyhow!("BNR2 file truncated"));
                }
                let order = [
                    BannerLanguage::Japanese,
                    BannerLanguage::English,
                    BannerLanguage::German,
                    BannerLanguage::French,
                    BannerLanguage::Spanish,
                    BannerLanguage::Italian,
                ];
                order
                    .iter()
                    .enumerate()
                    .map(|(i, lang)| {
                        let base = titles_start + i * BANNER_LANG_BLOCK_SIZE;
                        parse_block(*lang, &buf[base..base + BANNER_LANG_BLOCK_SIZE])
                    })
                    .collect()
            }
        };

        Ok(Self {
            format,
            titles,
            image_raw,
        })
    }
}

fn parse_block(lang: BannerLanguage, block: &[u8]) -> BannerTitle {
    BannerTitle {
        language: lang,
        short_game_name: trim_latin1(&block[0x00..0x20]),
        short_maker: trim_latin1(&block[0x20..0x40]),
        long_game_name: trim_latin1(&block[0x40..0x80]),
        long_maker: trim_latin1(&block[0x80..0xC0]),
        description: trim_latin1(&block[0xC0..0x140]),
    }
}

fn trim_latin1(buf: &[u8]) -> String {
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    buf[..end].iter().map(|&b| b as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_bnr1() -> Vec<u8> {
        let mut buf = vec![0u8; BNR1_FILE_SIZE];
        buf[0..4].copy_from_slice(&BNR1_MAGIC);
        // Image is left zero
        let titles_off = BANNER_IMAGE_OFFSET + BANNER_IMAGE_BYTES;
        let s = b"Game Short";
        buf[titles_off..titles_off + s.len()].copy_from_slice(s);
        let m = b"MS";
        buf[titles_off + 0x20..titles_off + 0x20 + m.len()].copy_from_slice(m);
        let l = b"My Long Game Title";
        buf[titles_off + 0x40..titles_off + 0x40 + l.len()].copy_from_slice(l);
        buf
    }

    fn build_bnr2() -> Vec<u8> {
        let mut buf = vec![0u8; BNR2_FILE_SIZE];
        buf[0..4].copy_from_slice(&BNR2_MAGIC);
        let titles_off = BANNER_IMAGE_OFFSET + BANNER_IMAGE_BYTES;
        // Block 1 (English) at offset titles_off + 0x140
        let eng = titles_off + BANNER_LANG_BLOCK_SIZE;
        let s = b"English Game";
        buf[eng..eng + s.len()].copy_from_slice(s);
        buf
    }

    #[test]
    fn parses_bnr1() {
        let buf = build_bnr1();
        let b = GcBanner::parse(&buf).unwrap();
        assert_eq!(b.format, BannerFormat::Bnr1);
        assert_eq!(b.titles.len(), 1);
        assert_eq!(b.titles[0].language, BannerLanguage::Default);
        assert_eq!(b.titles[0].short_game_name, "Game Short");
        assert_eq!(b.titles[0].short_maker, "MS");
        assert_eq!(b.titles[0].long_game_name, "My Long Game Title");
        assert_eq!(b.image_raw.len(), BANNER_IMAGE_BYTES);
    }

    #[test]
    fn parses_bnr2_six_languages() {
        let buf = build_bnr2();
        let b = GcBanner::parse(&buf).unwrap();
        assert_eq!(b.format, BannerFormat::Bnr2);
        assert_eq!(b.titles.len(), 6);
        let eng = b
            .titles
            .iter()
            .find(|t| t.language == BannerLanguage::English)
            .unwrap();
        assert_eq!(eng.short_game_name, "English Game");
    }

    #[test]
    fn rejects_bad_magic() {
        let mut buf = build_bnr1();
        buf[0..4].copy_from_slice(b"XXXX");
        assert!(GcBanner::parse(&buf).is_err());
    }
}
