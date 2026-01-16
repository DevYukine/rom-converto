use crate::chd::compression::flac::encode_flac_samples;
use crate::chd::compression::{ChdCompressor, compress_cd_hunk, deflate_compress, tag_to_bytes};
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
        compress_cd_hunk(
            data,
            |base| {
                let samples = samples_from_be_bytes(base);
                let samples_per_channel = samples.len() / 2;
                encode_flac_samples(&samples, 2, 44_100, samples_per_channel)
            },
            |subcode| deflate_compress(subcode),
        )
    }
}

fn samples_from_be_bytes(data: &[u8]) -> Vec<i32> {
    let mut samples = Vec::with_capacity(data.len() / 2);
    for chunk in data.chunks_exact(2) {
        let value = i16::from_be_bytes([chunk[0], chunk[1]]);
        samples.push(value as i32);
    }
    samples
}
