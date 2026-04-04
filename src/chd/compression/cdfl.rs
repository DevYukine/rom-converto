use crate::chd::compression::flac::{CD_SAMPLE_RATE, Endian, encode_flac_samples, samples_from_bytes};
use crate::chd::compression::{ChdCompressor, compress_cd_hunk, deflate_compress, tag_to_bytes};
use crate::chd::error::{ChdError, ChdResult};

const CD_CHANNELS: usize = 2;
const BYTES_PER_SAMPLE: usize = 2;
const BYTES_PER_STEREO_SAMPLE: usize = CD_CHANNELS * BYTES_PER_SAMPLE;

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
                if base.len() % BYTES_PER_STEREO_SAMPLE != 0 {
                    return Err(ChdError::InvalidHunkSize);
                }
                let samples = samples_from_bytes(base, Endian::Big);
                let samples_per_channel = samples.len() / CD_CHANNELS;
                encode_flac_samples(&samples, CD_CHANNELS, CD_SAMPLE_RATE, samples_per_channel)
            },
            deflate_compress,
        )
    }
}
