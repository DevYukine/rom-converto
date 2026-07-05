//! CISO/ZISO on-disk structures.
//!
//! Spec source: maxcso (`src/cso.h`, `README_CSO.md`) and PPSSPP's
//! `CISOFileBlockDevice`. Both formats share one 24-byte
//! little-endian header and a u32 index:
//!
//! ```text
//! 0x00  magic               "CISO" (deflate) or "ZISO" (LZ4)
//! 0x04  u32  header_size    0x18; many tools write garbage, never
//!                           trust it on read
//! 0x08  u64  uncompressed_size
//! 0x10  u32  block_size     2048 typical, 16384 for >= 2 GiB inputs
//! 0x14  u8   version        0 or 1
//! 0x15  u8   index_shift    left shift applied to index entries
//! 0x16  [u8; 2] reserved
//! ```
//!
//! The index holds `blocks + 1` u32 entries. An entry's low 31 bits
//! shifted left by `index_shift` give the block's file offset; bit
//! 0x80000000 means the block is stored raw. Block length = next
//! offset minus this offset. The final entry is the end-of-file
//! sentinel and never carries the raw bit. The format embeds no
//! checksums anywhere.

use binrw::binrw;

pub const CISO_MAGIC: [u8; 4] = *b"CISO";
pub const ZISO_MAGIC: [u8; 4] = *b"ZISO";
pub const DAX_MAGIC: [u8; 4] = *b"DAX\0";
pub const CISO_HEADER_SIZE: u32 = 0x18;
pub const CISO_INDEX_UNCOMPRESSED: u32 = 0x8000_0000;

pub const DEFAULT_BLOCK_SIZE: u32 = 2048;
/// maxcso bumps the block size for large inputs so the u32 index
/// stays small and offsets need less shifting.
pub const LARGE_INPUT_BLOCK_SIZE: u32 = 16384;
pub const LARGE_INPUT_THRESHOLD: u64 = 0x8000_0000;

const MAX_BLOCK_SIZE: u32 = 1024 * 1024;

/// DAX is decode-only: it is accepted as an input for decompress and
/// to-chd but is never a compression target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CsoFormat {
    Cso,
    Zso,
    Dax,
}

impl CsoFormat {
    pub fn magic(self) -> [u8; 4] {
        match self {
            CsoFormat::Cso => CISO_MAGIC,
            CsoFormat::Zso => ZISO_MAGIC,
            CsoFormat::Dax => DAX_MAGIC,
        }
    }

    pub fn from_magic(magic: &[u8; 4]) -> Option<Self> {
        match magic {
            m if *m == CISO_MAGIC => Some(CsoFormat::Cso),
            m if *m == ZISO_MAGIC => Some(CsoFormat::Zso),
            m if *m == DAX_MAGIC => Some(CsoFormat::Dax),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            CsoFormat::Cso => "cso",
            CsoFormat::Zso => "zso",
            CsoFormat::Dax => "dax",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            CsoFormat::Cso => "CSO",
            CsoFormat::Zso => "ZSO",
            CsoFormat::Dax => "DAX",
        }
    }
}

#[binrw]
#[brw(little)]
#[derive(Debug, Clone)]
pub struct CisoHeader {
    pub magic: [u8; 4],
    pub header_size: u32,
    pub uncompressed_size: u64,
    pub block_size: u32,
    pub version: u8,
    pub index_shift: u8,
    pub reserved: [u8; 2],
}

impl CisoHeader {
    pub fn new(
        format: CsoFormat,
        uncompressed_size: u64,
        block_size: u32,
        index_shift: u8,
    ) -> Self {
        Self {
            magic: format.magic(),
            header_size: CISO_HEADER_SIZE,
            uncompressed_size,
            block_size,
            version: 1,
            index_shift,
            reserved: [0; 2],
        }
    }

    pub fn format(&self) -> Option<CsoFormat> {
        CsoFormat::from_magic(&self.magic)
    }

    pub fn block_count(&self) -> u64 {
        block_count(self.uncompressed_size, self.block_size)
    }
}

pub fn block_count(uncompressed_size: u64, block_size: u32) -> u64 {
    uncompressed_size.div_ceil(block_size as u64)
}

pub fn valid_block_size(block_size: u32) -> bool {
    block_size.is_power_of_two() && (DEFAULT_BLOCK_SIZE..=MAX_BLOCK_SIZE).contains(&block_size)
}

pub fn pick_block_size(input_size: u64) -> u32 {
    if input_size >= LARGE_INPUT_THRESHOLD {
        LARGE_INPUT_BLOCK_SIZE
    } else {
        DEFAULT_BLOCK_SIZE
    }
}

/// Smallest shift that keeps every possible block offset within the
/// index's 31 offset bits, sized against the worst case of all blocks
/// stored raw (maxcso `Output::SetFile`).
pub fn pick_index_shift(uncompressed_size: u64, block_size: u32) -> u8 {
    let index_bytes = (block_count(uncompressed_size, block_size) + 1) * 4;
    let worst_size = CISO_HEADER_SIZE as u64 + index_bytes + uncompressed_size;
    for bit in (31..=62).rev() {
        if worst_size >= (1u64 << bit) {
            return (bit + 1 - 31) as u8;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use binrw::{BinRead, BinWrite};
    use std::io::Cursor;

    #[test]
    fn header_round_trips_and_is_24_bytes() {
        let header = CisoHeader::new(CsoFormat::Zso, 0x12_3456_7800, 2048, 1);
        let mut buf = Cursor::new(Vec::new());
        header.write(&mut buf).unwrap();
        let bytes = buf.into_inner();
        assert_eq!(bytes.len(), CISO_HEADER_SIZE as usize);
        assert_eq!(&bytes[..4], b"ZISO");

        let back = CisoHeader::read(&mut Cursor::new(&bytes)).unwrap();
        assert_eq!(back.uncompressed_size, 0x12_3456_7800);
        assert_eq!(back.block_size, 2048);
        assert_eq!(back.version, 1);
        assert_eq!(back.index_shift, 1);
        assert_eq!(back.format(), Some(CsoFormat::Zso));
    }

    #[test]
    fn shift_is_zero_for_typical_psp_isos() {
        assert_eq!(pick_index_shift(1_700_000_000, 2048), 0);
    }

    #[test]
    fn shift_grows_past_two_gib_worst_case() {
        // The worst case includes header + index, so inputs close to
        // 2 GiB already need a shift.
        assert_eq!(pick_index_shift(0x7000_0000, 2048), 0);
        assert_eq!(pick_index_shift(0x8000_0000, 2048), 1);
        assert_eq!(pick_index_shift(0x1_0000_0000, 2048), 2);
        assert_eq!(pick_index_shift(0x2_0000_0000, 16384), 3);
    }

    #[test]
    fn block_size_selection_matches_maxcso() {
        assert_eq!(pick_block_size(700 * 1024 * 1024), 2048);
        assert_eq!(pick_block_size(0x8000_0000), 16384);
        assert!(valid_block_size(2048));
        assert!(valid_block_size(16384));
        assert!(!valid_block_size(3000));
        assert!(!valid_block_size(1024));
    }
}
