//! GameCube disc header (`boot.bin`) and region byte (`bi2.bin`).
//!
//! Layout per yagcd.chadderz.co.uk. All multi-byte integers in
//! GameCube headers are big-endian.

use anyhow::{Result, anyhow};
use byteorder::{BE, ReadBytesExt};
use std::io::{Read, Seek, SeekFrom};

pub const BOOT_BIN_GAME_ID_OFFSET: u64 = 0x00;
pub const BOOT_BIN_GAME_NAME_OFFSET: u64 = 0x20;
pub const BOOT_BIN_GAME_NAME_LEN: usize = 64;
pub const BOOT_BIN_MAGIC_OFFSET: u64 = 0x1C;
pub const GC_MAGIC: u32 = 0xC2339F3D;

pub const BI2_REGION_OFFSET: u64 = 0x458;

pub const BOOT_BIN_DOL_OFFSET_FIELD: u64 = 0x420;
pub const BOOT_BIN_FST_OFFSET_FIELD: u64 = 0x424;
pub const BOOT_BIN_FST_SIZE_FIELD: u64 = 0x428;

pub const APPLOADER_OFFSET: u64 = 0x2440;
pub const APPLOADER_DATE_LEN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcRegion {
    Japan,
    Usa,
    Pal,
    Korea,
    Unknown(u32),
}

impl GcRegion {
    pub fn from_code(code: u32) -> Self {
        match code {
            0 => Self::Japan,
            1 => Self::Usa,
            2 => Self::Pal,
            4 => Self::Korea,
            other => Self::Unknown(other),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GcBootBin {
    pub game_id: String,
    pub maker_code: String,
    pub disc_number: u8,
    pub disc_version: u8,
    pub audio_streaming: bool,
    pub stream_buffer_size: u8,
    pub game_name: String,
    pub region: GcRegion,
    pub fst_offset: u32,
    pub fst_size: u32,
    pub apploader_date: Option<String>,
}

impl GcBootBin {
    pub fn read<R: Read + Seek>(reader: &mut R) -> Result<Self> {
        let start = reader.stream_position()?;

        let mut id_buf = [0u8; 6];
        reader.seek(SeekFrom::Start(start + BOOT_BIN_GAME_ID_OFFSET))?;
        reader.read_exact(&mut id_buf)?;
        let game_id = read_latin1_trim(&id_buf);
        let maker_code = String::from_utf8_lossy(&id_buf[4..6]).into_owned();

        reader.seek(SeekFrom::Start(start + 0x06))?;
        let disc_number = reader.read_u8()?;
        let disc_version = reader.read_u8()?;
        let audio_byte = reader.read_u8()?;
        let stream_buffer_size = reader.read_u8()?;

        reader.seek(SeekFrom::Start(start + BOOT_BIN_MAGIC_OFFSET))?;
        let magic = reader.read_u32::<BE>()?;
        if magic != GC_MAGIC {
            return Err(anyhow!("not a GameCube disc image (magic mismatch)"));
        }

        let mut name_buf = [0u8; BOOT_BIN_GAME_NAME_LEN];
        reader.seek(SeekFrom::Start(start + BOOT_BIN_GAME_NAME_OFFSET))?;
        reader.read_exact(&mut name_buf)?;
        let game_name = read_latin1_trim(&name_buf);

        reader.seek(SeekFrom::Start(start + BOOT_BIN_FST_OFFSET_FIELD))?;
        let fst_offset = reader.read_u32::<BE>()?;
        let fst_size = reader.read_u32::<BE>()?;

        reader.seek(SeekFrom::Start(start + BI2_REGION_OFFSET))?;
        let region_code = reader.read_u32::<BE>().unwrap_or(0xFFFF_FFFF);
        let region = GcRegion::from_code(region_code);

        let apploader_date = read_apploader_date(reader, start).ok();

        Ok(Self {
            game_id,
            maker_code,
            disc_number,
            disc_version,
            audio_streaming: audio_byte != 0,
            stream_buffer_size,
            game_name,
            region,
            fst_offset,
            fst_size,
            apploader_date,
        })
    }
}

fn read_apploader_date<R: Read + Seek>(reader: &mut R, base: u64) -> Result<String> {
    reader.seek(SeekFrom::Start(base + APPLOADER_OFFSET))?;
    let mut date = [0u8; APPLOADER_DATE_LEN];
    reader.read_exact(&mut date)?;
    Ok(read_latin1_trim(&date))
}

fn read_latin1_trim(buf: &[u8]) -> String {
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    buf[..end].iter().map(|&b| b as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use byteorder::WriteBytesExt;
    use std::io::Cursor;

    fn build_min_disc_image(name: &str, region: u32, fst_off: u32, fst_size: u32) -> Vec<u8> {
        let mut buf = vec![0u8; 0x2500];
        buf[0..6].copy_from_slice(b"GALE01");
        buf[6] = 0; // disc 0
        buf[7] = 1; // version 1
        buf[8] = 1; // audio streaming on
        buf[9] = 16; // stream buf
        (&mut buf[0x1C..0x20])
            .write_u32::<BE>(GC_MAGIC)
            .unwrap();
        let name_bytes = name.as_bytes();
        buf[0x20..0x20 + name_bytes.len()].copy_from_slice(name_bytes);
        (&mut buf[BOOT_BIN_FST_OFFSET_FIELD as usize..BOOT_BIN_FST_OFFSET_FIELD as usize + 4])
            .write_u32::<BE>(fst_off)
            .unwrap();
        (&mut buf[BOOT_BIN_FST_SIZE_FIELD as usize..BOOT_BIN_FST_SIZE_FIELD as usize + 4])
            .write_u32::<BE>(fst_size)
            .unwrap();
        (&mut buf[BI2_REGION_OFFSET as usize..BI2_REGION_OFFSET as usize + 4])
            .write_u32::<BE>(region)
            .unwrap();
        let date = b"2003/05/27 17:27";
        buf[APPLOADER_OFFSET as usize..APPLOADER_OFFSET as usize + date.len()]
            .copy_from_slice(date);
        buf
    }

    #[test]
    fn parses_known_disc() {
        let buf = build_min_disc_image("Animal Crossing", 1, 0x100000, 0x500);
        let mut cur = Cursor::new(&buf);
        let boot = GcBootBin::read(&mut cur).unwrap();
        assert_eq!(boot.game_id, "GALE01");
        assert_eq!(boot.maker_code, "01");
        assert_eq!(boot.disc_number, 0);
        assert_eq!(boot.disc_version, 1);
        assert!(boot.audio_streaming);
        assert_eq!(boot.game_name, "Animal Crossing");
        assert_eq!(boot.region, GcRegion::Usa);
        assert_eq!(boot.fst_offset, 0x100000);
        assert_eq!(boot.fst_size, 0x500);
        assert_eq!(boot.apploader_date.as_deref(), Some("2003/05/27 17:27"));
    }

    #[test]
    fn rejects_non_gamecube_magic() {
        let mut buf = build_min_disc_image("Test", 0, 0x100, 0x10);
        // Clobber the magic
        buf[0x1C..0x20].copy_from_slice(&[0u8; 4]);
        let mut cur = Cursor::new(&buf);
        assert!(GcBootBin::read(&mut cur).is_err());
    }

    #[test]
    fn maps_region_codes() {
        assert_eq!(GcRegion::from_code(0), GcRegion::Japan);
        assert_eq!(GcRegion::from_code(1), GcRegion::Usa);
        assert_eq!(GcRegion::from_code(2), GcRegion::Pal);
        assert_eq!(GcRegion::from_code(4), GcRegion::Korea);
        match GcRegion::from_code(99) {
            GcRegion::Unknown(99) => (),
            _ => panic!("expected Unknown(99)"),
        }
    }
}
