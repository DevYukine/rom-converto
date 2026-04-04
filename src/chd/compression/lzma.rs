use crate::chd::compression::{ChdCompressor, tag_to_bytes};
use crate::chd::error::ChdResult;
use liblzma::read::XzEncoder;
use liblzma::stream::{LzmaOptions, Stream};
use std::io::{self, Read};

#[derive(Debug, Clone)]
pub struct LzmaCompressor;

impl ChdCompressor for LzmaCompressor {
    fn name(&self) -> &'static str {
        "LZMA Compressor"
    }

    fn tag_bytes(&self) -> [u8; 4] {
        tag_to_bytes("lzma")
    }

    fn compress(&self, data: &[u8]) -> ChdResult<Vec<u8>> {
        lzma_compress(data)
    }
}

const LZMA_LEVEL: u32 = 7;
const MIN_DICT_SIZE: u32 = 1 << 16;
const MAX_DICT_SIZE: u32 = 1 << 24;
const LZMA_PROPS_BYTES: usize = 5;
const LZMA_UNCOMPRESSED_SIZE_BYTES: usize = 8;
const LZMA_ALONE_HEADER_BYTES: usize = LZMA_PROPS_BYTES + LZMA_UNCOMPRESSED_SIZE_BYTES;

pub(crate) fn lzma_compress(data: &[u8]) -> ChdResult<Vec<u8>> {
    let mut options = LzmaOptions::new_preset(LZMA_LEVEL).map_err(io::Error::from)?;
    options.dict_size(lzma_dict_size(data.len()));

    let stream = Stream::new_lzma_encoder(&options).map_err(io::Error::from)?;
    let mut encoder = XzEncoder::new_stream(data, stream);
    let mut encoded = Vec::new();
    encoder.read_to_end(&mut encoded)?;

    if encoded.len() < LZMA_ALONE_HEADER_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "LZMA output too small").into());
    }

    let mut output = Vec::with_capacity(encoded.len() - LZMA_UNCOMPRESSED_SIZE_BYTES);
    output.extend_from_slice(&encoded[..LZMA_PROPS_BYTES]);
    output.extend_from_slice(&encoded[LZMA_ALONE_HEADER_BYTES..]);
    Ok(output)
}

fn lzma_dict_size(input_len: usize) -> u32 {
    let mut size = input_len as u64;
    if size < MIN_DICT_SIZE as u64 {
        size = MIN_DICT_SIZE as u64;
    }
    size = size.next_power_of_two();
    if size > MAX_DICT_SIZE as u64 {
        size = MAX_DICT_SIZE as u64;
    }
    size as u32
}
