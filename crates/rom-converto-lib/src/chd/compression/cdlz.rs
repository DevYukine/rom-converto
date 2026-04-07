use crate::chd::compression::lzma::{lzma_compress, lzma_decompress};
use crate::chd::compression::{
    ChdCompressor, ChdDecompressor, compress_cd_hunk, decompress_cd_hunk, deflate_compress,
    deflate_decompress, tag_to_bytes,
};
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
        compress_cd_hunk(data, lzma_compress, deflate_compress)
    }
}

#[derive(Debug, Clone)]
pub struct CdlzDecompressor;

impl ChdDecompressor for CdlzDecompressor {
    fn tag_bytes(&self) -> [u8; 4] {
        tag_to_bytes("cdlz")
    }

    fn decompress(&self, compressed: &[u8], output_len: usize) -> ChdResult<Vec<u8>> {
        decompress_cd_hunk(compressed, output_len, lzma_decompress, deflate_decompress)
    }
}
