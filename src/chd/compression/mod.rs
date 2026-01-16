use crate::cd::{FRAME_SIZE, SECTOR_SIZE, SUBCODE_SIZE};
use crate::chd::error::{ChdError, ChdResult};
use flate2::Compression;
use flate2::write::DeflateEncoder;
use std::fmt::Debug;
use std::io::Write;

pub mod cdfl;
pub mod cdlz;
pub mod cdzl;
pub mod cdzs;
pub mod flac;
pub mod lzma;
pub mod zlib;
pub mod zstd;

// Convert tag to FourCC bytes
pub const fn tag_to_bytes(tag: &str) -> [u8; 4] {
    let bytes = tag.as_bytes();
    assert!(bytes.len() == 4, "tag must be exactly 4 bytes");
    [bytes[0], bytes[1], bytes[2], bytes[3]]
}

// IMPORTANT: These values map to positions in the header, not codec IDs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChdCompression {
    Codec0 = 0, // First codec in header
    Codec1 = 1, // Second codec in header
    Codec2 = 2, // Third codec in header
    Codec3 = 3, // Fourth codec in header
    None = 4,   // Uncompressed
    Self_ = 5,  // Same as another hunk
    Parent = 6, // From parent CHD
}

pub trait ChdCompressor: Debug {
    fn name(&self) -> &'static str;
    fn tag_bytes(&self) -> [u8; 4];
    fn compress(&self, data: &[u8]) -> ChdResult<Vec<u8>>;
}

pub(crate) fn compress_cd_hunk<F1, F2>(
    data: &[u8],
    base_compress: F1,
    subcode_compress: F2,
) -> ChdResult<Vec<u8>>
where
    F1: FnOnce(&[u8]) -> ChdResult<Vec<u8>>,
    F2: FnOnce(&[u8]) -> ChdResult<Vec<u8>>,
{
    let (frames, base, subcode) = split_cd_frames(data)?;
    let (header_bytes, ecc_bytes, complen_bytes) = cd_header_sizes(data.len(), frames);

    let base_compressed = base_compress(&base)?;
    let subcode_compressed = subcode_compress(&subcode)?;

    let mut output = vec![0u8; header_bytes];
    write_cd_header(&mut output, ecc_bytes, base_compressed.len(), complen_bytes);
    output.extend_from_slice(&base_compressed);
    output.extend_from_slice(&subcode_compressed);
    Ok(output)
}

pub(crate) fn deflate_compress(data: &[u8]) -> ChdResult<Vec<u8>> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}

fn split_cd_frames(data: &[u8]) -> ChdResult<(usize, Vec<u8>, Vec<u8>)> {
    if data.len() % FRAME_SIZE != 0 {
        return Err(ChdError::InvalidHunkSize);
    }

    let frames = data.len() / FRAME_SIZE;
    let mut base = Vec::with_capacity(frames * SECTOR_SIZE);
    let mut subcode = Vec::with_capacity(frames * SUBCODE_SIZE);

    for frame in 0..frames {
        let start = frame * FRAME_SIZE;
        base.extend_from_slice(&data[start..start + SECTOR_SIZE]);
        subcode.extend_from_slice(&data[start + SECTOR_SIZE..start + FRAME_SIZE]);
    }

    Ok((frames, base, subcode))
}

fn cd_header_sizes(data_len: usize, frames: usize) -> (usize, usize, usize) {
    let complen_bytes = if data_len < 65536 { 2 } else { 3 };
    let ecc_bytes = (frames + 7) / 8;
    let header_bytes = ecc_bytes + complen_bytes;
    (header_bytes, ecc_bytes, complen_bytes)
}

fn write_cd_header(buf: &mut [u8], ecc_bytes: usize, base_len: usize, complen_bytes: usize) {
    if complen_bytes == 2 {
        write_u16_be(&mut buf[ecc_bytes..ecc_bytes + 2], base_len as u16);
    } else {
        write_u24_be(&mut buf[ecc_bytes..ecc_bytes + 3], base_len as u32);
    }
}

fn write_u16_be(buf: &mut [u8], value: u16) {
    buf.copy_from_slice(&value.to_be_bytes());
}

fn write_u24_be(buf: &mut [u8], value: u32) {
    let bytes = value.to_be_bytes();
    buf.copy_from_slice(&bytes[1..]);
}
