use crate::chd::compression::{ChdCompressor, tag_to_bytes};
use crate::chd::error::ChdResult;
use liblzma::read::{XzDecoder, XzEncoder};
use liblzma::stream::{LzmaOptions, Stream};
use std::io::{self, Cursor, Read};

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

const LZMA_LEVEL: u32 = 8;
const LZMA_PROPS_BYTES: usize = 5;
const LZMA_UNCOMPRESSED_SIZE_BYTES: usize = 8;
const LZMA_ALONE_HEADER_BYTES: usize = LZMA_PROPS_BYTES + LZMA_UNCOMPRESSED_SIZE_BYTES;

/// Creates LZMA encoder options matching chdman's configure_properties.
fn lzma_options(data_len: usize) -> io::Result<LzmaOptions> {
    let mut options = LzmaOptions::new_preset(LZMA_LEVEL).map_err(io::Error::from)?;
    // Match chdman: reduceSize = hunkbytes, which limits dict to input size
    // liblzma doesn't have reduceSize directly, but we can set dict_size to match
    // what LzmaEncProps_Normalize would compute for the given data length
    // Match MAME's LzmaEncProps_Normalize: find smallest dict that fits data
    // MAME loops: for i in 11..=30: if reduceSize <= 2<<i or 3<<i, use that
    let dict_size = lzma_dict_size_for_reduce(data_len as u32);
    options.dict_size(dict_size);
    Ok(options)
}

/// Get the 5-byte LZMA properties for a given data length.
/// Used by the decompressor to reconstruct the encoder properties.
fn lzma_props_for_len(data_len: usize) -> io::Result<Vec<u8>> {
    let options = lzma_options(data_len)?;
    let stream = Stream::new_lzma_encoder(&options).map_err(io::Error::from)?;
    // Encode empty data to get the props from the LZMA Alone header
    let mut encoder = XzEncoder::new_stream(&[][..], stream);
    let mut header = Vec::new();
    encoder.read_to_end(&mut header)?;
    if header.len() < LZMA_PROPS_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Failed to get LZMA props"));
    }
    Ok(header[..LZMA_PROPS_BYTES].to_vec())
}

pub(crate) fn lzma_compress(data: &[u8]) -> ChdResult<Vec<u8>> {
    let options = lzma_options(data.len()).map_err(io::Error::from)?;

    let stream = Stream::new_lzma_encoder(&options).map_err(io::Error::from)?;
    let mut encoder = XzEncoder::new_stream(data, stream);
    let mut encoded = Vec::new();
    encoder.read_to_end(&mut encoded)?;

    if encoded.len() < LZMA_ALONE_HEADER_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "LZMA output too small").into());
    }

    // Strip the entire LZMA Alone header (props + uncompressed size) — just raw compressed data
    // chdman stores no props per-hunk; the decompressor reconstructs them from codec settings
    Ok(encoded[LZMA_ALONE_HEADER_BYTES..].to_vec())
}

/// Match MAME's LzmaEncProps_Normalize dict size selection.
/// For a given reduceSize, finds the smallest dict from the sequence 2<<i, 3<<i (i=11..30).
fn lzma_dict_size_for_reduce(reduce_size: u32) -> u32 {
    for i in 11..=30u32 {
        if reduce_size <= 2u32.wrapping_shl(i) {
            return 2u32.wrapping_shl(i);
        }
        if reduce_size <= 3u32.wrapping_shl(i) {
            return 3u32.wrapping_shl(i);
        }
    }
    reduce_size
}

pub(crate) fn lzma_decompress(data: &[u8], expected_len: usize) -> ChdResult<Vec<u8>> {
    // Reconstruct LZMA Alone header: props(5) + uncompressed_size(8) + compressed_data
    // Props are derived from the same settings used during compression
    let props = lzma_props_for_len(expected_len)?;

    let mut alone_data = Vec::with_capacity(LZMA_ALONE_HEADER_BYTES + data.len());
    alone_data.extend_from_slice(&props);
    alone_data.extend_from_slice(&[0xFF; LZMA_UNCOMPRESSED_SIZE_BYTES]);
    alone_data.extend_from_slice(data);

    let stream = Stream::new_lzma_decoder(u64::MAX).map_err(io::Error::from)?;
    let mut decoder = XzDecoder::new_stream(Cursor::new(alone_data), stream);
    let mut output = Vec::new();
    decoder.read_to_end(&mut output)?;
    Ok(output)
}
