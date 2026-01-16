use crate::chd::compression::{ChdCompressor, compress_cd_hunk, deflate_compress, tag_to_bytes};
use crate::chd::error::ChdResult;

#[derive(Debug, Clone)]
pub struct CdZlCompressor;

impl ChdCompressor for CdZlCompressor {
    fn name(&self) -> &'static str {
        "CD Deflate Compressor"
    }

    fn tag_bytes(&self) -> [u8; 4] {
        tag_to_bytes("cdzl")
    }

    fn compress(&self, data: &[u8]) -> ChdResult<Vec<u8>> {
        compress_cd_hunk(
            data,
            |base| deflate_compress(base),
            |subcode| deflate_compress(subcode),
        )
    }
}
