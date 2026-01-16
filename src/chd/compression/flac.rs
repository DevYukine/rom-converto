use crate::chd::compression::{ChdCompressor, tag_to_bytes};
use crate::chd::error::{ChdError, ChdResult};
use anyhow::anyhow;
use flacenc::bitsink::ByteSink;
use flacenc::component::BitRepr;
use flacenc::config;
use flacenc::error::Verify;
use flacenc::source::MemSource;
use std::io;

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
        if data.len() % 2 != 0 {
            return Err(ChdError::InvalidHunkSize);
        }

        let le_samples = samples_from_bytes(data, Endian::Little);
        let be_samples = samples_from_bytes(data, Endian::Big);
        let samples_per_channel = data.len() / 2;
        let block_size = flac_block_size(samples_per_channel);

        let le_flac = encode_flac_samples(&le_samples, 1, 44_100, block_size)?;
        let be_flac = encode_flac_samples(&be_samples, 1, 44_100, block_size)?;

        let (endian_flag, mut compressed) = if le_flac.len() <= be_flac.len() {
            (0u8, le_flac)
        } else {
            (1u8, be_flac)
        };

        let mut output = Vec::with_capacity(compressed.len() + 1);
        output.push(endian_flag);
        output.append(&mut compressed);
        Ok(output)
    }
}

pub fn encode_flac_samples(
    samples: &[i32],
    channels: usize,
    sample_rate: usize,
    block_size: usize,
) -> ChdResult<Vec<u8>> {
    if channels == 0 || samples.len() % channels != 0 {
        return Err(ChdError::InvalidHunkSize);
    }

    let mut config = config::Encoder::default();
    config.block_size = flac_block_size(block_size);
    let config = config
        .into_verified()
        .map_err(|(_, err)| io::Error::new(io::ErrorKind::InvalidInput, err))?;

    let source = MemSource::from_samples(samples, channels, 16, sample_rate);
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|_| anyhow!("Could not encode to flac with flixed block size"))?;

    let mut sink = ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;
    Ok(sink.into_inner())
}

fn flac_block_size(samples_per_channel: usize) -> usize {
    let mut block_size = samples_per_channel;
    if block_size < flacenc::constant::MIN_BLOCK_SIZE {
        block_size = flacenc::constant::MIN_BLOCK_SIZE;
    }
    if block_size > flacenc::constant::MAX_BLOCK_SIZE {
        block_size = flacenc::constant::MAX_BLOCK_SIZE;
    }
    block_size
}

enum Endian {
    Little,
    Big,
}

fn samples_from_bytes(data: &[u8], endian: Endian) -> Vec<i32> {
    let mut samples = Vec::with_capacity(data.len() / 2);
    for chunk in data.chunks_exact(2) {
        let value = match endian {
            Endian::Little => i16::from_le_bytes([chunk[0], chunk[1]]),
            Endian::Big => i16::from_be_bytes([chunk[0], chunk[1]]),
        };
        samples.push(value as i32);
    }
    samples
}
