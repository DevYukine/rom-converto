use binrw::{BinRead, BinWrite};

pub const NCSD_HEADER_SIZE: usize = 0x200;
pub const NCSD_FIRST_PARTITION_OFFSET: u64 = 0x4000;
pub const NCSD_PARTITION_FS_TYPE_NORMAL: u8 = 0x01;

#[derive(BinRead, BinWrite, Debug, Clone)]
#[brw(little)]
pub struct NcsdPartitionEntry {
    pub offset: u32,
    pub size: u32,
}

impl NcsdPartitionEntry {
    pub const EMPTY: Self = Self { offset: 0, size: 0 };
}

#[derive(BinRead, BinWrite, Debug, Clone)]
#[brw(little)]
pub struct NcsdHeader {
    pub signature: [u8; 0x100],
    pub magic: [u8; 4],
    pub image_size: u32,
    pub media_id: u64,
    pub partition_fs_types: [u8; 8],
    pub partition_crypt_types: [u8; 8],
    pub partition_table: [NcsdPartitionEntry; 8],
    pub ncch_exheader_hash: [u8; 0x20],
    pub additional_header_size: u32,
    pub sector_zero_offset: u32,
    pub partition_flags: [u8; 8],
    pub partition_id_table: [u64; 8],
    pub reserved: [u8; 0x30],
}

impl NcsdHeader {
    pub const MAGIC: [u8; 4] = *b"NCSD";

    pub fn blank() -> Self {
        Self {
            signature: [0u8; 0x100],
            magic: Self::MAGIC,
            image_size: 0,
            media_id: 0,
            partition_fs_types: [0u8; 8],
            partition_crypt_types: [0u8; 8],
            partition_table: [
                NcsdPartitionEntry::EMPTY,
                NcsdPartitionEntry::EMPTY,
                NcsdPartitionEntry::EMPTY,
                NcsdPartitionEntry::EMPTY,
                NcsdPartitionEntry::EMPTY,
                NcsdPartitionEntry::EMPTY,
                NcsdPartitionEntry::EMPTY,
                NcsdPartitionEntry::EMPTY,
            ],
            ncch_exheader_hash: [0u8; 0x20],
            additional_header_size: 0,
            sector_zero_offset: 0,
            partition_flags: [0u8; 8],
            partition_id_table: [0u64; 8],
            reserved: [0u8; 0x30],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use binrw::{BinRead, BinWrite};
    use std::io::Cursor;

    #[test]
    fn blank_header_serializes_to_correct_size() {
        let header = NcsdHeader::blank();
        let mut buf = Vec::new();
        header.write(&mut Cursor::new(&mut buf)).unwrap();
        assert_eq!(buf.len(), NCSD_HEADER_SIZE);
    }

    #[test]
    fn header_roundtrip_preserves_fields() {
        let mut header = NcsdHeader::blank();
        header.media_id = 0x0004000000123456;
        header.image_size = 0x40000;
        header.partition_fs_types[0] = NCSD_PARTITION_FS_TYPE_NORMAL;
        header.partition_table[0] = NcsdPartitionEntry {
            offset: 0x20,
            size: 0x10000,
        };

        let mut buf = Vec::new();
        header.write(&mut Cursor::new(&mut buf)).unwrap();

        let read = NcsdHeader::read(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(read.magic, NcsdHeader::MAGIC);
        assert_eq!(read.media_id, 0x0004000000123456);
        assert_eq!(read.image_size, 0x40000);
        assert_eq!(read.partition_fs_types[0], NCSD_PARTITION_FS_TYPE_NORMAL);
        assert_eq!(read.partition_table[0].offset, 0x20);
        assert_eq!(read.partition_table[0].size, 0x10000);
    }
}
