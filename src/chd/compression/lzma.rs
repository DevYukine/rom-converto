use crate::chd::compression::{ChdCompressor, tag_to_bytes};
use crate::chd::error::ChdResult;

#[derive(Debug, Clone)]
pub struct LzmaCompressor;

impl ChdCompressor for LzmaCompressor {
    fn name(&self) -> &'static str {
        "LZMA Compressor"
    }

    fn tag_bytes(&self) -> [u8; 4] {
        tag_to_bytes("lzma")
    }

    fn compress(&self, data: &[u8]) -> ChdResult<Vec<u8>> {
        Ok(liblzma::encode_all(data, 7)?)
    }
}
