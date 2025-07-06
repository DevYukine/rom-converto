use crate::cd::SECTOR_SIZE;
use crate::chd::compression::{ChdCompressor, tag_to_bytes};
use crate::chd::error::ChdResult;
use flate2::Compression;
use flate2::write::ZlibEncoder;
use std::io::Write;

#[derive(Debug, Clone)]
pub struct CdZlCompressor;

impl ChdCompressor for CdZlCompressor {
    fn name(&self) -> &'static str {
        "CD Zlib Compressor"
    }

    fn tag_bytes(&self) -> [u8; 4] {
        tag_to_bytes("cdzl")
    }

    fn compress(&self, data: &[u8]) -> ChdResult<Vec<u8>> {
        // IMPORTANT: CD compression has a specific format!
        let sector_count = data.len() / SECTOR_SIZE;
        let mut frames = Vec::with_capacity(sector_count * 2048);
        let mut subcode = Vec::with_capacity(sector_count * 96);

        for i in 0..sector_count {
            let sector_start = i * SECTOR_SIZE;
            let sector = &data[sector_start..sector_start + SECTOR_SIZE];

            // Extract frame data (2048 bytes after sync/header)
            frames.extend_from_slice(&sector[16..16 + 2048]);

            // Extract subcode data (last 96 bytes) if present in raw sectors
            // For standard Mode1/Mode2 sectors, generate empty subcode
            subcode.extend_from_slice(&[0u8; 96]);
        }

        // Compress with zlib
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(6));
        encoder.write_all(frames.as_slice())?;
        let compressed_frames = encoder.finish()?;

        // Build result: subcode + compressed frames
        let mut result = Vec::new();
        result.extend_from_slice(&subcode);
        result.extend_from_slice(&compressed_frames);

        Ok(result)
    }
}
