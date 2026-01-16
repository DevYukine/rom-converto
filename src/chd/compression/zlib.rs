use crate::chd::compression::{ChdCompressor, deflate_compress, tag_to_bytes};
use crate::chd::error::ChdResult;

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
        deflate_compress(data)
    }
}
