use crate::chd::compression::{ChdCompressor, tag_to_bytes};
use crate::chd::error::ChdResult;

#[derive(Debug, Clone)]
pub struct FlacCompressor;

impl ChdCompressor for FlacCompressor {
    fn name(&self) -> &'static str {
        "FLAC Compressor"
    }

    fn tag_bytes(&self) -> [u8; 4] {
        tag_to_bytes("flac")
    }

    fn compress(&self, data: &[u8]) -> ChdResult<Vec<u8>> {
        todo!()
    }
}
