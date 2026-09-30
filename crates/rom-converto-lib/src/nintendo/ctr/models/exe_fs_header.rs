use binrw::{BinRead, BinWrite};

/// There are a maximum of 10 file headers in the ExeFS format. This maximum is
/// disputable: some references indicate a maximum of 8 sections, others a
/// maximum of 10. From a non-SDK point of view, the ExeFS header format can
/// hold no more than 10 file headers within the currently defined size of
/// 0x200 bytes.
#[derive(BinRead, BinWrite, Debug, Clone, Copy)]
#[brw(little)]
pub struct ExeFSHeader {
    pub file_name: [u8; 8],

    pub file_offset: [u8; 4],

    pub file_size: [u8; 4],
}
