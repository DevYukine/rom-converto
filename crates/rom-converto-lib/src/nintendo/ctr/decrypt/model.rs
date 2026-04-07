#[repr(C)]
#[derive(Debug)]
pub struct CiaContent {
    pub cid: u32,
    pub cidx: u16,
    pub ctype: u16,
    pub csize: u64,
}

#[derive(Debug)]
pub enum NcchSection {
    ExHeader = 1,
    ExeFS = 2,
    RomFS = 3,
}
