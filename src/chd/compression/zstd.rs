use crate::chd::compression::{ChdCompressor, tag_to_bytes};
use crate::chd::error::ChdResult;

#[derive(Debug, Clone)]
pub struct ZstdCompressor;

impl ChdCompressor for ZstdCompressor {
    fn name(&self) -> &'static str {
        "ZSTD Compressor"
    }

    fn tag_bytes(&self) -> [u8; 4] {
        tag_to_bytes("zstd")
    }

    fn compress(&self, data: &[u8]) -> ChdResult<Vec<u8>> {
        Ok(zstd::encode_all(data, 0)?)
    }
}
