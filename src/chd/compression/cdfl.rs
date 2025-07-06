use crate::chd::compression::{ChdCompressor, tag_to_bytes};
use crate::chd::error::ChdResult;

#[derive(Debug, Clone)]
pub struct CdFlCompressor;

impl ChdCompressor for CdFlCompressor {
    fn name(&self) -> &'static str {
        "CD Flac Compressor"
    }

    fn tag_bytes(&self) -> [u8; 4] {
        tag_to_bytes("cdfl")
    }

    fn compress(&self, data: &[u8]) -> ChdResult<Vec<u8>> {
        todo!()
    }
}
