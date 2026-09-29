//! GameCube `opening.bnr` parser.
//!
//! Two formats:
//!   - BNR1 (single language) used by NTSC discs, plus some single-language
//!     PAL discs.
//!   - BNR2 (six languages: English, German, French, Spanish, Italian,
//!     Dutch) used by PAL titles.
//!
//! Text fields are Shift-JIS on NTSC-J discs and Windows-1252 otherwise.
//!
//! Both formats embed a 96x32 RGB5A3 banner image at offset 0x20.

use crate::nintendo::dol::models::boot_bin::GcRegion;
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

/// Which `opening.bnr` layout a banner uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerFormat {
    Bnr1,
    Bnr2,
}

/// Language of one banner title block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerLanguage {
    /// BNR1 carries a single language slot (region-dependent: English for
    /// US, German for German PAL, Japanese on NTSC-J discs, and so on). It
    /// is exposed as `BannerLanguage::Default`.
    Default,
    English,
    German,
    French,
    Spanish,
    Italian,
    Dutch,
}

/// One decoded title block: short and long game/maker names plus the
/// description text, decoded in the disc region's encoding and trimmed at
/// the first NUL.
#[derive(Debug, Clone)]
pub struct BannerTitle {
    pub language: BannerLanguage,
    pub short_game_name: String,
    pub short_maker: String,
    pub long_game_name: String,
    pub long_maker: String,
    pub description: String,
}

/// Parsed `opening.bnr`: its format, one title block per language, and
/// the raw banner image.
#[derive(Debug, Clone)]
pub struct GcBanner {
    pub format: BannerFormat,
    pub titles: Vec<BannerTitle>,
    /// Raw RGB5A3 4x4-tiled pixels (6144 bytes).
    pub image_raw: Vec<u8>,
}

impl GcBanner {
    /// Parses an `opening.bnr` buffer, detecting BNR1 vs BNR2 from its
    /// magic and decoding one title block per language it carries.
    /// `region` selects the text encoding of the title blocks.
    pub fn parse(buf: &[u8], region: GcRegion) -> Result<Self> {
        if buf.len() < BNR1_FILE_SIZE {
            return Err(anyhow!("opening.bnr too small: {} bytes", buf.len()));
        }
        let magic: [u8; 4] = buf[0..4].try_into()?;
        let format = match magic {
            BNR1_MAGIC => BannerFormat::Bnr1,
            BNR2_MAGIC => BannerFormat::Bnr2,
            _ => return Err(anyhow!("opening.bnr has unknown magic")),
        };
        let image_raw = buf[BANNER_IMAGE_OFFSET..BANNER_IMAGE_OFFSET + BANNER_IMAGE_BYTES].to_vec();

        let titles_start = BANNER_IMAGE_OFFSET + BANNER_IMAGE_BYTES;
        let titles = match format {
            BannerFormat::Bnr1 => {
                if buf.len() < BNR1_FILE_SIZE {
                    return Err(anyhow!("BNR1 file truncated"));
                }
                let block = &buf[titles_start..titles_start + BANNER_LANG_BLOCK_SIZE];
                vec![parse_block(BannerLanguage::Default, region, block)]
            }
            BannerFormat::Bnr2 => {
                if buf.len() < BNR2_FILE_SIZE {
                    return Err(anyhow!("BNR2 file truncated"));
                }
                let order = [
                    BannerLanguage::English,
                    BannerLanguage::German,
                    BannerLanguage::French,
                    BannerLanguage::Spanish,
                    BannerLanguage::Italian,
                    BannerLanguage::Dutch,
                ];
                order
                    .iter()
                    .enumerate()
                    .map(|(i, lang)| {
                        let base = titles_start + i * BANNER_LANG_BLOCK_SIZE;
                        parse_block(*lang, region, &buf[base..base + BANNER_LANG_BLOCK_SIZE])
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

fn parse_block(lang: BannerLanguage, region: GcRegion, block: &[u8]) -> BannerTitle {
    BannerTitle {
        language: lang,
        short_game_name: region.decode_text(&block[0x00..0x20]),
        short_maker: region.decode_text(&block[0x20..0x40]),
        long_game_name: region.decode_text(&block[0x40..0x80]),
        long_maker: region.decode_text(&block[0x80..0xC0]),
        description: region.decode_text(&block[0xC0..0x140]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_bnr1() -> Vec<u8> {
        let mut buf = vec![0u8; BNR1_FILE_SIZE];
        buf[0..4].copy_from_slice(&BNR1_MAGIC);
        let titles_off = BANNER_IMAGE_OFFSET + BANNER_IMAGE_BYTES;
        let s = b"Game Short";
        buf[titles_off..titles_off + s.len()].copy_from_slice(s);
        let m = b"MS";
        buf[titles_off + 0x20..titles_off + 0x20 + m.len()].copy_from_slice(m);
        let l = b"My Long Game Title";
        buf[titles_off + 0x40..titles_off + 0x40 + l.len()].copy_from_slice(l);
        buf
    }

    const BNR2_SLOT_NAMES: [&str; 6] =
        ["English", "German", "French", "Spanish", "Italian", "Dutch"];

    fn build_bnr2() -> Vec<u8> {
        let mut buf = vec![0u8; BNR2_FILE_SIZE];
        buf[0..4].copy_from_slice(&BNR2_MAGIC);
        let titles_off = BANNER_IMAGE_OFFSET + BANNER_IMAGE_BYTES;
        for (i, name) in BNR2_SLOT_NAMES.iter().enumerate() {
            let off = titles_off + i * BANNER_LANG_BLOCK_SIZE;
            buf[off..off + name.len()].copy_from_slice(name.as_bytes());
        }
        buf
    }

    #[test]
    fn parses_bnr1() {
        let buf = build_bnr1();
        let b = GcBanner::parse(&buf, GcRegion::Usa).unwrap();
        assert_eq!(b.format, BannerFormat::Bnr1);
        assert_eq!(b.titles.len(), 1);
        assert_eq!(b.titles[0].language, BannerLanguage::Default);
        assert_eq!(b.titles[0].short_game_name, "Game Short");
        assert_eq!(b.titles[0].short_maker, "MS");
        assert_eq!(b.titles[0].long_game_name, "My Long Game Title");
        assert_eq!(b.image_raw.len(), BANNER_IMAGE_BYTES);
    }

    #[test]
    fn parses_bnr2_in_pal_language_order() {
        let buf = build_bnr2();
        let b = GcBanner::parse(&buf, GcRegion::Pal).unwrap();
        assert_eq!(b.format, BannerFormat::Bnr2);
        let parsed: Vec<_> = b
            .titles
            .iter()
            .map(|t| (t.language, t.short_game_name.as_str()))
            .collect();
        assert_eq!(
            parsed,
            [
                (BannerLanguage::English, "English"),
                (BannerLanguage::German, "German"),
                (BannerLanguage::French, "French"),
                (BannerLanguage::Spanish, "Spanish"),
                (BannerLanguage::Italian, "Italian"),
                (BannerLanguage::Dutch, "Dutch"),
            ]
        );
    }

    #[test]
    fn rejects_bad_magic() {
        let mut buf = build_bnr1();
        buf[0..4].copy_from_slice(b"XXXX");
        assert!(GcBanner::parse(&buf, GcRegion::Usa).is_err());
    }

    #[test]
    fn decodes_bnr1_shift_jis_titles_on_japanese_disc() {
        const KURURIN: [u8; 8] = [0x82, 0xAD, 0x82, 0xE9, 0x82, 0xE8, 0x82, 0xF1];
        let mut buf = build_bnr1();
        let titles_off = BANNER_IMAGE_OFFSET + BANNER_IMAGE_BYTES;
        buf[titles_off..titles_off + KURURIN.len()].copy_from_slice(&KURURIN);
        buf[titles_off + KURURIN.len()] = 0;
        buf[titles_off + 0xC0..titles_off + 0xC0 + KURURIN.len()].copy_from_slice(&KURURIN);

        let b = GcBanner::parse(&buf, GcRegion::Japan).unwrap();
        assert_eq!(b.titles[0].short_game_name, "くるりん");
        assert_eq!(b.titles[0].description, "くるりん");
    }
}
