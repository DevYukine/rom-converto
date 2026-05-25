//! Wii channel / game IMET banner header parser.
//!
//! IMET sits at the top of the decompressed `opening.bnr` for Wii game
//! discs and `00000000.app` for channels. Layout per
//! wiibrew.org/wiki/Opening.bnr#IMET_header.

use anyhow::{Result, anyhow};
use byteorder::{BE, ReadBytesExt};
use std::io::Cursor;

pub const IMET_MAGIC: [u8; 4] = *b"IMET";
pub const IMET_HEADER_OFFSET: usize = 0x40;
pub const IMET_NAMES_OFFSET: usize = 0x5C;
pub const IMET_NAME_LANG_COUNT: usize = 7;
pub const IMET_NAME_CHARS: usize = 0x54;
pub const IMET_NAME_BYTES: usize = IMET_NAME_CHARS * 2;
pub const IMET_TOTAL_NAMES_BYTES: usize = IMET_NAME_LANG_COUNT * IMET_NAME_BYTES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImetLanguage {
    Japanese,
    English,
    German,
    French,
    Spanish,
    Italian,
    Dutch,
}

impl ImetLanguage {
    pub const ALL: [ImetLanguage; 7] = [
        Self::Japanese,
        Self::English,
        Self::German,
        Self::French,
        Self::Spanish,
        Self::Italian,
        Self::Dutch,
    ];
}

#[derive(Debug, Clone)]
pub struct ImetHeader {
    pub icon_size: u32,
    pub banner_size: u32,
    pub sound_size: u32,
    pub names: Vec<ImetName>,
}

#[derive(Debug, Clone)]
pub struct ImetName {
    pub language: ImetLanguage,
    pub name: String,
}

impl ImetHeader {
    pub fn parse(buf: &[u8]) -> Result<Self> {
        if buf.len() < IMET_NAMES_OFFSET + IMET_TOTAL_NAMES_BYTES {
            return Err(anyhow!("IMET header truncated"));
        }
        if buf[IMET_HEADER_OFFSET..IMET_HEADER_OFFSET + 4] != IMET_MAGIC {
            return Err(anyhow!("IMET magic missing"));
        }

        // Sizes at offset 0x4C, three u32 BE.
        let mut cur = Cursor::new(&buf[0x4C..0x58]);
        let icon_size = cur.read_u32::<BE>()?;
        let banner_size = cur.read_u32::<BE>()?;
        let sound_size = cur.read_u32::<BE>()?;

        let mut names = Vec::with_capacity(IMET_NAME_LANG_COUNT);
        for (i, lang) in ImetLanguage::ALL.iter().enumerate() {
            let off = IMET_NAMES_OFFSET + i * IMET_NAME_BYTES;
            let name_bytes = &buf[off..off + IMET_NAME_BYTES];
            let name = read_utf16be_string(name_bytes);
            if !name.is_empty() {
                names.push(ImetName {
                    language: *lang,
                    name,
                });
            }
        }

        Ok(Self {
            icon_size,
            banner_size,
            sound_size,
            names,
        })
    }
}

fn read_utf16be_string(bytes: &[u8]) -> String {
    let mut units = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        let u = u16::from_be_bytes([chunk[0], chunk[1]]);
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
    use byteorder::WriteBytesExt;

    fn build_imet() -> Vec<u8> {
        let mut buf = vec![0u8; IMET_NAMES_OFFSET + IMET_TOTAL_NAMES_BYTES + 16];
        buf[IMET_HEADER_OFFSET..IMET_HEADER_OFFSET + 4].copy_from_slice(&IMET_MAGIC);
        (&mut buf[0x4C..0x50]).write_u32::<BE>(0x1000).unwrap();
        (&mut buf[0x50..0x54]).write_u32::<BE>(0x4000).unwrap();
        (&mut buf[0x54..0x58]).write_u32::<BE>(0x2000).unwrap();

        // English (idx 1) name: "Test Title"
        let eng_off = IMET_NAMES_OFFSET + IMET_NAME_BYTES;
        for (i, ch) in "Test Title".encode_utf16().enumerate() {
            let off = eng_off + i * 2;
            buf[off..off + 2].copy_from_slice(&ch.to_be_bytes());
        }
        buf
    }

    #[test]
    fn parses_sizes_and_english_name() {
        let buf = build_imet();
        let h = ImetHeader::parse(&buf).unwrap();
        assert_eq!(h.icon_size, 0x1000);
        assert_eq!(h.banner_size, 0x4000);
        assert_eq!(h.sound_size, 0x2000);
        assert_eq!(h.names.len(), 1);
        assert_eq!(h.names[0].language, ImetLanguage::English);
        assert_eq!(h.names[0].name, "Test Title");
    }

    #[test]
    fn rejects_missing_magic() {
        let mut buf = build_imet();
        buf[IMET_HEADER_OFFSET..IMET_HEADER_OFFSET + 4].copy_from_slice(b"XXXX");
        assert!(ImetHeader::parse(&buf).is_err());
    }
}
