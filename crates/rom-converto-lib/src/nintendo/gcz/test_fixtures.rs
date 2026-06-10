//! Synthetic GCZ writer for tests: same layout Dolphin's
//! `ConvertToGcz` produces, including the stored-raw fallback for
//! blocks deflate cannot shrink and a partial final block.

use std::io::Write;

use flate2::Compression;
use flate2::write::ZlibEncoder;

use super::format::{GCZ_MAGIC, GCZ_UNCOMPRESSED_FLAG, adler32};

pub(crate) fn make_gcz(iso: &[u8], block_size: u32, sub_type: u32) -> Vec<u8> {
    let num_blocks = iso.len().div_ceil(block_size as usize);
    let mut ptrs: Vec<u64> = Vec::with_capacity(num_blocks);
    let mut hashes: Vec<u32> = Vec::with_capacity(num_blocks);
    let mut data: Vec<u8> = Vec::new();

    for chunk in iso.chunks(block_size as usize) {
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(chunk).unwrap();
        let compressed = enc.finish().unwrap();

        let (stored, flag) = if compressed.len() < chunk.len() {
            (compressed, 0)
        } else {
            (chunk.to_vec(), GCZ_UNCOMPRESSED_FLAG)
        };
        ptrs.push(data.len() as u64 | flag);
        hashes.push(adler32(&stored));
        data.extend_from_slice(&stored);
    }

    let mut out = Vec::new();
    out.extend_from_slice(&GCZ_MAGIC.to_le_bytes());
    out.extend_from_slice(&sub_type.to_le_bytes());
    out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    out.extend_from_slice(&(iso.len() as u64).to_le_bytes());
    out.extend_from_slice(&block_size.to_le_bytes());
    out.extend_from_slice(&(num_blocks as u32).to_le_bytes());
    for p in &ptrs {
        out.extend_from_slice(&p.to_le_bytes());
    }
    for h in &hashes {
        out.extend_from_slice(&h.to_le_bytes());
    }
    out.extend_from_slice(&data);
    out
}
