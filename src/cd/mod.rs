// src/cd/sector.rs
use binrw::prelude::*;

pub const SECTOR_SIZE: usize = 2352;
pub const SUBCODE_SIZE: usize = 96;
pub const FRAME_SIZE: usize = SECTOR_SIZE + SUBCODE_SIZE;
pub const FRAMES_PER_HUNK: u32 = 8;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrackMode {
    Audio,
    Mode1,
    Mode2Form1,
    Mode2Form2,
    Mode2Raw,
}

#[derive(Debug, BinRead, BinWrite)]
#[br(big)] // preserves big-endian default for other numeric fields, if any
#[bw(big)]
pub struct CdSector {
    pub sync: [u8; 12],
    pub header: [u8; 4],
    pub data: [u8; 2048],
    #[br(little)]
    #[bw(little)]
    pub edc: u32, // explicitly LE
    pub intermediate: [u8; 8],
    pub ecc_p: [u8; 172],
    pub ecc_q: [u8; 104],
}
impl CdSector {
    pub fn from_raw_bytes(data: &[u8], mode: TrackMode) -> Result<Self, anyhow::Error> {
        match mode {
            TrackMode::Audio => {
                // Audio tracks are stored as-is
                Ok(Self {
                    sync: [0; 12],
                    header: [0; 4],
                    data: data[0..2048].try_into()?,
                    edc: 0,
                    intermediate: [0; 8],
                    ecc_p: [0; 172],
                    ecc_q: [0; 104],
                })
            }
            TrackMode::Mode1 => {
                // Parse Mode 1 sector
                let mut cursor = std::io::Cursor::new(data);
                Ok(CdSector::read(&mut cursor)?)
            }
            _ => todo!("Implement other modes"),
        }
    }
}
