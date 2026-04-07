use crate::chd::compression::{
    ChdCompressor, ChdDecompressor, compress_cd_hunk, decompress_cd_hunk, deflate_compress,
    deflate_decompress, tag_to_bytes,
};
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
        compress_cd_hunk(data, deflate_compress, deflate_compress)
    }
}

#[derive(Debug, Clone)]
pub struct CdZlDecompressor;

impl ChdDecompressor for CdZlDecompressor {
    fn tag_bytes(&self) -> [u8; 4] {
        tag_to_bytes("cdzl")
    }

    fn decompress(&self, compressed: &[u8], output_len: usize) -> ChdResult<Vec<u8>> {
        decompress_cd_hunk(
            compressed,
            output_len,
            deflate_decompress,
            deflate_decompress,
        )
    }
}
