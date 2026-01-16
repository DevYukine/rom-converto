use crate::chd::compression::{ChdCompressor, compress_cd_hunk, tag_to_bytes};
use crate::chd::error::ChdResult;

#[derive(Debug, Clone)]
pub struct CdZsCompressor;

impl ChdCompressor for CdZsCompressor {
    fn name(&self) -> &'static str {
        "CD Zstandard Compressor"
    }

    fn tag_bytes(&self) -> [u8; 4] {
        tag_to_bytes("cdzs")
    }

    fn compress(&self, data: &[u8]) -> ChdResult<Vec<u8>> {
        compress_cd_hunk(
            data,
            |base| Ok(zstd::encode_all(base, 0)?),
            |subcode| Ok(zstd::encode_all(subcode, 0)?),
        )
    }
}
