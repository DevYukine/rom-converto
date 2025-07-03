use binrw::{BinRead, BinWrite};

#[derive(BinRead, BinWrite, Debug, Clone, Copy)]
#[brw(little)]
pub struct ExeFSHeader {
    pub fname: [u8; 8],
    pub off: [u8; 4],
    pub size: [u8; 4],
}
