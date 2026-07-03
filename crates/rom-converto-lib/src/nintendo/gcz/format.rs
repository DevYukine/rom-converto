//! GCZ (Dolphin CompressedBlob) on-disk structures.
//!
//! Spec source: Dolphin's `Source/Core/DiscIO/CompressedBlob.{h,cpp}`.
//! Layout: a 32-byte little-endian header, `num_blocks` u64 block
//! pointers, `num_blocks` u32 Adler-32 checksums, then the block data.
//! Block pointers are relative to the start of the data section; bit 63
//! set means the block is stored without compression. Compressed blocks
//! use zlib framing (not raw deflate, not gzip). Checksums cover the
//! bytes exactly as stored, before inflation.

use binrw::{BinRead, BinWrite};

use super::error::{GczError, GczResult};

pub const GCZ_MAGIC: u32 = 0xB10B_C001;
pub const GCZ_HEADER_SIZE: u64 = 32;
pub const GCZ_UNCOMPRESSED_FLAG: u64 = 1 << 63;

#[derive(Debug, Clone, Copy, BinRead, BinWrite)]
#[brw(little)]
pub struct GczHeader {
    pub magic: u32,
    /// 0 = GameCube, 1 = Wii. Dolphin writes it but never reads it.
    pub sub_type: u32,
    pub compressed_data_size: u64,
    pub data_size: u64,
    pub block_size: u32,
    pub num_blocks: u32,
}

impl GczHeader {
    pub fn validate(&self) -> GczResult<()> {
        if self.magic != GCZ_MAGIC {
            return Err(GczError::InvalidMagic(self.magic));
        }
        if self.block_size == 0 || self.block_size > 0x100_0000 {
            return Err(GczError::InvalidHeader(format!(
                "implausible block size {:#x}",
                self.block_size
            )));
        }
        if self.data_size == 0 {
            return Err(GczError::InvalidHeader("zero data size".into()));
        }
        let expected_blocks = self.data_size.div_ceil(self.block_size as u64);
        if expected_blocks != self.num_blocks as u64 {
            return Err(GczError::InvalidHeader(format!(
                "block count {} does not match data size (expected {expected_blocks})",
                self.num_blocks
            )));
        }
        Ok(())
    }
}

/// Adler-32 as zlib computes it; GCZ stores one per block.
pub fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    // Largest n with 255n(n+1)/2 + (n+1)(MOD-1) < 2^32, per zlib.
    const NMAX: usize = 5552;
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for chunk in data.chunks(NMAX) {
        for &byte in chunk {
            a += byte as u32;
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;
    use binrw::io::Cursor;

    #[test]
    fn header_round_trips_at_32_bytes() {
        let h = GczHeader {
            magic: GCZ_MAGIC,
            sub_type: 1,
            compressed_data_size: 12345,
            data_size: 0x40000,
            block_size: 0x8000,
            num_blocks: 8,
        };
        let mut buf = Cursor::new(Vec::new());
        h.write(&mut buf).unwrap();
        assert_eq!(buf.get_ref().len() as u64, GCZ_HEADER_SIZE);
        let back = GczHeader::read(&mut Cursor::new(buf.into_inner())).unwrap();
        assert_eq!(back.data_size, h.data_size);
        back.validate().unwrap();
    }

    #[test]
    fn validate_rejects_inconsistent_block_count() {
        let h = GczHeader {
            magic: GCZ_MAGIC,
            sub_type: 0,
            compressed_data_size: 0,
            data_size: 0x40001,
            block_size: 0x8000,
            num_blocks: 8,
        };
        assert!(h.validate().is_err());
    }

    #[test]
    fn adler32_matches_zlib_vectors() {
        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        let big = vec![0xFFu8; 100_000];
        assert_eq!(adler32(&big), {
            let mut a: u64 = 1;
            let mut b: u64 = 0;
            for &x in &big {
                a = (a + x as u64) % 65521;
                b = (b + a) % 65521;
            }
            ((b as u32) << 16) | a as u32
        });
    }
}
