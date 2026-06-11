//! CSO/ZSO metadata extraction.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::cso::error::CsoResult;
use crate::cso::models::CISO_INDEX_UNCOMPRESSED;
use crate::cso::reader::open_cso_sync;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CsoInfo {
    pub format: String,
    pub version: u8,
    pub block_size: u32,
    pub index_shift: u8,
    pub uncompressed_size: u64,
    pub physical_bytes: u64,
    pub compression_ratio: f64,
    pub block_count: u64,
    pub raw_block_count: u64,
}

pub fn read_info(path: &Path) -> CsoResult<CsoInfo> {
    let handle = open_cso_sync(path)?;
    let blocks = handle.header.block_count();
    let raw_block_count = handle.index[..blocks as usize]
        .iter()
        .filter(|e| **e & CISO_INDEX_UNCOMPRESSED != 0)
        .count() as u64;

    Ok(CsoInfo {
        format: handle.format.name().to_string(),
        version: handle.header.version,
        block_size: handle.header.block_size,
        index_shift: handle.header.index_shift,
        uncompressed_size: handle.header.uncompressed_size,
        physical_bytes: handle.file_size,
        compression_ratio: (handle.file_size as f64 / handle.header.uncompressed_size as f64)
            * 100.0,
        block_count: blocks,
        raw_block_count,
    })
}
