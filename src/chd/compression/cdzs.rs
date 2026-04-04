use crate::chd::compression::{
    ChdCompressor, ChdDecompressor, compress_cd_hunk, decompress_cd_hunk, tag_to_bytes,
};
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

#[derive(Debug, Clone)]
pub struct CdZsDecompressor;

impl ChdDecompressor for CdZsDecompressor {
    fn tag_bytes(&self) -> [u8; 4] {
        tag_to_bytes("cdzs")
    }

    fn decompress(&self, compressed: &[u8], output_len: usize) -> ChdResult<Vec<u8>> {
        decompress_cd_hunk(
            compressed,
            output_len,
            |data, _expected_len| Ok(zstd::decode_all(data)?),
            |data, _expected_len| Ok(zstd::decode_all(data)?),
        )
    }
}
