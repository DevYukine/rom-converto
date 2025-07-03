use binrw::{BinRead, BinWrite};

/// There are a maximum of 10 file headers in the ExeFS format. (This maximum number of file headers is disputable, with makerom indicating a maximum of 8 sections and makecia indicating a maximum of 10. From a non-SDK point of view, the ExeFS header format can hold no more than 10 file headers within the currently define size of 0x200 bytes.)
#[derive(BinRead, BinWrite, Debug, Clone, Copy)]
#[brw(little)]
pub struct ExeFSHeader {
    /// File Name
    pub file_name: [u8; 8],

    /// File Offset
    pub file_offset: [u8; 4],

    /// File Size
    pub file_size: [u8; 4],
}
