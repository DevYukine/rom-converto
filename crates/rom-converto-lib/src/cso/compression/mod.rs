//! Per-block codecs: raw DEFLATE for CSO, LZ4 block format for ZSO.
//!
//! Both formats compress each block independently. CSO blocks are
//! raw deflate (no zlib wrapper, windowBits -15 in maxcso and
//! PPSSPP); ZSO blocks are the raw LZ4 block format with no frame,
//! compressed with LZ4HC like maxcso for the best ratio. Any valid
//! LZ4 block decodes on the consumer side (OPL, ARK-4), so the HC
//! level is a ratio knob, not a compatibility one.

use std::io;

use crate::chd::compression::{deflate_decompress_with, deflate_with_reset};
use crate::cso::error::{CsoError, CsoResult};
use crate::cso::models::CsoFormat;

const LZ4_HC_LEVEL: u32 = 12;

/// Persistent per-worker block compressor.
pub(crate) enum BlockCompressor {
    Deflate(Box<flate2::Compress>),
    Lz4,
}

impl BlockCompressor {
    pub fn new(format: CsoFormat) -> Self {
        match format {
            CsoFormat::Cso => Self::Deflate(Box::new(flate2::Compress::new(
                flate2::Compression::best(),
                false,
            ))),
            CsoFormat::Zso => Self::Lz4,
            CsoFormat::Dax => unreachable!("DAX is decode-only"),
        }
    }

    pub fn compress(&mut self, block: &[u8]) -> CsoResult<Vec<u8>> {
        match self {
            Self::Deflate(deflate) => deflate_with_reset(deflate, block).map_err(chd_to_cso),
            Self::Lz4 => Ok(lz4::block::compress(
                block,
                Some(lz4::block::CompressionMode::HIGHCOMPRESSION(
                    LZ4_HC_LEVEL as i32,
                )),
                false,
            )?),
        }
    }
}

/// Persistent per-worker block decompressor.
pub(crate) enum BlockDecompressor {
    Deflate(Box<flate2::Decompress>),
    Lz4,
    Dax(Box<flate2::Decompress>),
}

impl BlockDecompressor {
    pub fn new(format: CsoFormat) -> Self {
        match format {
            CsoFormat::Cso => Self::Deflate(Box::new(flate2::Decompress::new(false))),
            CsoFormat::Zso => Self::Lz4,
            CsoFormat::Dax => Self::Dax(Box::new(flate2::Decompress::new(true))),
        }
    }

    pub fn decompress(&mut self, data: &[u8], expected_len: usize) -> CsoResult<Vec<u8>> {
        match self {
            Self::Deflate(inflate) => {
                deflate_decompress_with(inflate, data, expected_len).map_err(chd_to_cso)
            }
            Self::Lz4 => lz4_decompress_partial(data, expected_len),
            Self::Dax(inflate) => dax_inflate(inflate, data, expected_len),
        }
    }
}

/// DAX frames are zlib streams (compress2, windowBits 15). Some
/// third-party writers emit raw deflate instead, so when the zlib
/// inflate fails, retry the same bytes as raw deflate before erroring.
fn dax_inflate(
    inflate: &mut flate2::Decompress,
    src: &[u8],
    expected_len: usize,
) -> CsoResult<Vec<u8>> {
    match inflate_once(inflate, src, expected_len, true) {
        Ok(out) => Ok(out),
        Err(_) => inflate_once(inflate, src, expected_len, false),
    }
}

fn inflate_once(
    inflate: &mut flate2::Decompress,
    src: &[u8],
    expected_len: usize,
    zlib_header: bool,
) -> CsoResult<Vec<u8>> {
    inflate.reset(zlib_header);
    let mut out = vec![0u8; expected_len];
    let before = inflate.total_out();
    let status = inflate
        .decompress(src, &mut out, flate2::FlushDecompress::Finish)
        .map_err(|e| CsoError::IoError(io::Error::other(format!("DAX inflate error: {e}"))))?;
    match status {
        flate2::Status::StreamEnd | flate2::Status::Ok => {}
        flate2::Status::BufError => {
            return Err(CsoError::IoError(io::Error::other(
                "DAX inflate buffer error",
            )));
        }
    }
    let written = (inflate.total_out() - before) as usize;
    out.truncate(written);
    Ok(out)
}

// The lz4 crate only binds LZ4_decompress_safe, which rejects input
// with trailing bytes. ZSO spans may carry alignment padding after
// the stream (index_shift > 0), so decode with the partial variant
// that stops once the target size is reached, the same call maxcso
// and OPL use. The symbol comes from the liblz4 the lz4 crate links.
unsafe extern "C" {
    fn LZ4_decompress_safe_partial(
        src: *const std::ffi::c_char,
        dst: *mut std::ffi::c_char,
        src_size: std::ffi::c_int,
        target_output_size: std::ffi::c_int,
        dst_capacity: std::ffi::c_int,
    ) -> std::ffi::c_int;
}

fn lz4_decompress_partial(data: &[u8], expected_len: usize) -> CsoResult<Vec<u8>> {
    let mut out = vec![0u8; expected_len];
    let written = unsafe {
        LZ4_decompress_safe_partial(
            data.as_ptr() as *const std::ffi::c_char,
            out.as_mut_ptr() as *mut std::ffi::c_char,
            data.len() as std::ffi::c_int,
            expected_len as std::ffi::c_int,
            expected_len as std::ffi::c_int,
        )
    };
    if written < 0 || written as usize != expected_len {
        return Err(CsoError::IoError(io::Error::other(format!(
            "LZ4 block decode failed (result {written}, expected {expected_len})"
        ))));
    }
    Ok(out)
}

/// The deflate helpers are shared with the CHD module and speak its
/// error type; only the io variant can actually occur here.
fn chd_to_cso(err: crate::chd::error::ChdError) -> CsoError {
    match err {
        crate::chd::error::ChdError::IoError(e) => CsoError::IoError(e),
        other => CsoError::IoError(io::Error::other(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> Vec<u8> {
        (0..2048usize).map(|i| (i / 32) as u8).collect()
    }

    #[test]
    fn deflate_block_round_trips() {
        let data = payload();
        let mut c = BlockCompressor::new(CsoFormat::Cso);
        let mut d = BlockDecompressor::new(CsoFormat::Cso);
        let compressed = c.compress(&data).unwrap();
        assert!(compressed.len() < data.len());
        assert_eq!(d.decompress(&compressed, data.len()).unwrap(), data);
    }

    #[test]
    fn lz4_block_round_trips() {
        let data = payload();
        let mut c = BlockCompressor::new(CsoFormat::Zso);
        let mut d = BlockDecompressor::new(CsoFormat::Zso);
        let compressed = c.compress(&data).unwrap();
        assert!(compressed.len() < data.len());
        assert_eq!(d.decompress(&compressed, data.len()).unwrap(), data);
    }

    #[test]
    fn deflate_block_is_raw_deflate() {
        // A zlib wrapper would start with 0x78; raw deflate streams
        // for this payload start with a block header instead. Decode
        // with a raw inflater to prove there is no wrapper.
        let data = payload();
        let mut c = BlockCompressor::new(CsoFormat::Cso);
        let compressed = c.compress(&data).unwrap();
        let mut inflater = flate2::Decompress::new(false);
        let mut out = vec![0u8; data.len()];
        inflater
            .decompress(&compressed, &mut out, flate2::FlushDecompress::Finish)
            .unwrap();
        assert_eq!(out, data);
    }
}
