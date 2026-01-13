use crate::cd::{FRAME_SIZE, SECTOR_SIZE, SUBCODE_SIZE};
use crate::chd::compression::{ChdCompressor, tag_to_bytes};
use crate::chd::error::{ChdError, ChdResult};

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
        compress_cd_hunk_zstd(data)
    }
}

fn compress_cd_hunk_zstd(data: &[u8]) -> ChdResult<Vec<u8>> {
    if data.len() % FRAME_SIZE != 0 {
        return Err(ChdError::InvalidHunkSize);
    }

    let frames = data.len() / FRAME_SIZE;
    let complen_bytes = if data.len() < 65536 { 2 } else { 3 };
    let ecc_bytes = (frames + 7) / 8;
    let header_bytes = ecc_bytes + complen_bytes;

    let mut base = Vec::with_capacity(frames * SECTOR_SIZE);
    let mut subcode = Vec::with_capacity(frames * SUBCODE_SIZE);

    for frame in 0..frames {
        let start = frame * FRAME_SIZE;
        base.extend_from_slice(&data[start..start + SECTOR_SIZE]);
        subcode.extend_from_slice(&data[start + SECTOR_SIZE..start + FRAME_SIZE]);
    }

    let base_compressed = zstd::encode_all(base.as_slice(), 0)?;
    let subcode_compressed = zstd::encode_all(subcode.as_slice(), 0)?;

    let mut output =
        Vec::with_capacity(header_bytes + base_compressed.len() + subcode_compressed.len());
    output.resize(header_bytes, 0);

    if complen_bytes == 2 {
        write_u16_be(
            &mut output[ecc_bytes..ecc_bytes + 2],
            base_compressed.len() as u16,
        );
    } else {
        write_u24_be(
            &mut output[ecc_bytes..ecc_bytes + 3],
            base_compressed.len() as u32,
        );
    }

    output.extend_from_slice(&base_compressed);
    output.extend_from_slice(&subcode_compressed);
    Ok(output)
}

fn write_u16_be(buf: &mut [u8], value: u16) {
    buf[0] = (value >> 8) as u8;
    buf[1] = value as u8;
}

fn write_u24_be(buf: &mut [u8], value: u32) {
    let value = value & 0x00ff_ffff;
    buf[0] = (value >> 16) as u8;
    buf[1] = (value >> 8) as u8;
    buf[2] = value as u8;
}
