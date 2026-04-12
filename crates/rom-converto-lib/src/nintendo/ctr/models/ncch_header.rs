use crate::nintendo::ctr::constants::NCCH_FLAGS7_NOCRYPTO;
use binrw::{BinRead, BinWrite};

#[derive(BinRead, BinWrite, Debug)]
#[brw(little)]
pub struct NcchHeader {
    pub signature: [u8; 256],
    pub magic: [u8; 4],
    pub ncchsize: u32,
    pub titleid: [u8; 8],
    pub makercode: u16,
    pub formatversion: u8,
    pub formatversion2: u8,
    pub seedcheck: [u8; 4],
    pub programid: [u8; 8],
    padding1: [u8; 16],
    pub logohash: [u8; 32],
    pub productcode: [u8; 16],
    pub exhdrhash: [u8; 32],
    pub exhdrsize: u32,
    padding2: u32,
    pub flags: [u8; 8],
    pub plainregionoffset: u32,
    pub plainregionsize: u32,
    pub logooffset: u32,
    pub logosize: u32,
    pub exefsoffset: u32,
    pub exefssize: u32,
    pub exefshashsize: u32,
    padding4: u32,
    pub romfsoffset: u32,
    pub romfssize: u32,
    pub romfshashsize: u32,
    padding5: u32,
    pub exefshash: [u8; 32],
    pub romfshash: [u8; 32],
}

impl NcchHeader {
    pub fn is_encrypted(&self) -> bool {
        (self.flags[7] & NCCH_FLAGS7_NOCRYPTO) == 0
    }
}
