use crate::chd::compression::lzma::lzma_compress;
use crate::chd::compression::{ChdCompressor, compress_cd_hunk, deflate_compress, tag_to_bytes};
use crate::chd::error::ChdResult;

#[derive(Debug, Clone)]
pub struct CdlzCompressor;

impl ChdCompressor for CdlzCompressor {
    fn name(&self) -> &'static str {
        "CD LZMA Compressor"
    }

    fn tag_bytes(&self) -> [u8; 4] {
        tag_to_bytes("cdlz")
    }

    fn compress(&self, data: &[u8]) -> ChdResult<Vec<u8>> {
        compress_cd_hunk(
            data,
            |base| lzma_compress(base),
            |subcode| deflate_compress(subcode),
        )
    }
}
