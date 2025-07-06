use crate::chd::compression::{ChdCompressor, tag_to_bytes};
use crate::chd::error::ChdResult;
use flate2::Compression;
use flate2::write::ZlibEncoder;
use std::io::Write;

#[derive(Debug, Clone)]
pub struct ZlibCompressor;

impl ChdCompressor for ZlibCompressor {
    fn name(&self) -> &'static str {
        "Zlib Compressor"
    }

    fn tag_bytes(&self) -> [u8; 4] {
        tag_to_bytes("zlib")
    }

    fn compress(&self, data: &[u8]) -> ChdResult<Vec<u8>> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(6));
        encoder.write_all(data)?;
        Ok(encoder.finish()?)
    }
}
