//! ZArchive `CompressionOffsetRecord` (40 bytes, big-endian).
//!
//! One record covers [`ENTRIES_PER_OFFSET_RECORD`] compressed blocks.
//! `base_offset` is the absolute byte position of the first block in
//! the record; each `size_minus_one[i]` is the compressed byte length
//! of block `base_offset + i`, stored as `length - 1` so that a full
//! 64 KiB incompressible block (length 65536) fits in a u16.

use binrw::{BinRead, BinWrite};

use crate::nintendo::wup::constants::{COMPRESSED_BLOCK_SIZE, ENTRIES_PER_OFFSET_RECORD};

/// Compression offset record mapping block index to compressed byte
/// offset. Serialised as one big-endian u64 followed by 16 big-endian
/// u16s for a fixed 40-byte layout.
#[derive(Debug, Clone, BinRead, BinWrite, PartialEq, Eq)]
#[brw(big)]
pub struct CompressionOffsetRecord {
    /// Absolute byte offset (from the start of the archive) of the
    /// first compressed block covered by this record.
    pub base_offset: u64,
    /// For each of the 16 blocks, the compressed size minus one. An
    /// incompressible 64 KiB block stores `0xFFFF` here.
    pub size_minus_one: [u16; ENTRIES_PER_OFFSET_RECORD],
}

impl CompressionOffsetRecord {
    /// Build a new record starting at `base_offset` with every slot
    /// initialised to zero.
    pub fn new(base_offset: u64) -> Self {
        Self {
            base_offset,
            size_minus_one: [0u16; ENTRIES_PER_OFFSET_RECORD],
        }
    }

    /// Store the compressed byte length of block `slot` (0..16).
    /// `size` must be 1..=65536; the on-disk value is `size - 1`.
    /// An incompressible block (raw-stored 64 KiB) uses `size = 65536`
    /// which encodes to `size_minus_one = 0xFFFF`.
    pub fn set_block_size(&mut self, slot: usize, size: usize) {
        debug_assert!(slot < ENTRIES_PER_OFFSET_RECORD);
        debug_assert!(size >= 1);
        debug_assert!(size <= COMPRESSED_BLOCK_SIZE);
        self.size_minus_one[slot] = (size - 1) as u16;
    }

    /// Recover the compressed byte length of block `slot`.
    pub fn block_size(&self, slot: usize) -> usize {
        debug_assert!(slot < ENTRIES_PER_OFFSET_RECORD);
        self.size_minus_one[slot] as usize + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::wup::constants::COMPRESSION_OFFSET_RECORD_SIZE;
    use binrw::{BinRead, BinWrite};
    use std::io::Cursor;

    #[test]
    fn offset_record_serialises_to_40_bytes() {
        let record = CompressionOffsetRecord::new(0);
        let mut buf = Cursor::new(Vec::new());
        record.write(&mut buf).unwrap();
        assert_eq!(
            buf.into_inner().len(),
            COMPRESSION_OFFSET_RECORD_SIZE,
            "upstream spec: sizeof(CompressionOffsetRecord) == 40"
        );
    }

    #[test]
    fn offset_record_byte_layout_is_big_endian() {
        let mut record = CompressionOffsetRecord::new(0x1234_5678_9ABC_DEF0);
        // Set each slot to a distinct value so byte ordering is
        // easy to verify.
        for i in 0..ENTRIES_PER_OFFSET_RECORD {
            record.size_minus_one[i] = 0x0100 + i as u16;
        }
        let mut buf = Cursor::new(Vec::new());
        record.write(&mut buf).unwrap();
        let bytes = buf.into_inner();
        assert_eq!(&bytes[0..8], &0x1234_5678_9ABC_DEF0u64.to_be_bytes());
        for i in 0..ENTRIES_PER_OFFSET_RECORD {
            let start = 8 + i * 2;
            let expected: u16 = 0x0100 + i as u16;
            assert_eq!(&bytes[start..start + 2], &expected.to_be_bytes());
        }
    }

    #[test]
    fn offset_record_round_trip() {
        let mut original = CompressionOffsetRecord::new(0xDEAD_BEEF_CAFE_BABE);
        for i in 0..ENTRIES_PER_OFFSET_RECORD {
            original.size_minus_one[i] = (i as u16) * 0x1111;
        }
        let mut buf = Cursor::new(Vec::new());
        original.write(&mut buf).unwrap();
        let bytes = buf.into_inner();

        let mut reader = Cursor::new(&bytes);
        let parsed = CompressionOffsetRecord::read(&mut reader).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn set_block_size_stores_size_minus_one() {
        let mut record = CompressionOffsetRecord::new(0);
        record.set_block_size(0, 1);
        record.set_block_size(1, 0x4000);
        record.set_block_size(2, COMPRESSED_BLOCK_SIZE);
        assert_eq!(record.size_minus_one[0], 0);
        assert_eq!(record.size_minus_one[1], 0x3FFF);
        assert_eq!(
            record.size_minus_one[2], 0xFFFF,
            "incompressible 64 KiB block must encode to 0xFFFF"
        );
    }

    #[test]
    fn block_size_round_trips_through_setter() {
        let mut record = CompressionOffsetRecord::new(0);
        for (slot, size) in [(0, 1), (1, 0x2345), (2, COMPRESSED_BLOCK_SIZE)] {
            record.set_block_size(slot, size);
            assert_eq!(record.block_size(slot), size);
        }
    }
}
