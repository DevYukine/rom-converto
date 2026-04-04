use crate::cd::CD_HUNK_BYTES;
use crate::chd::compression::{ChdCompressor, tag_to_bytes};
use crate::chd::error::ChdResult;
use lzma_sdk_sys::*;
use std::io;
use std::ptr;

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

const LZMA_LEVEL: i32 = 8;

/// Configure LZMA encoder properties matching chdman's configure_properties.
/// Uses hunk_bytes as reduceSize (matching chdman which passes hunkbytes, not base data length).
fn configure_props(hunk_bytes: usize) -> CLzmaEncProps {
    unsafe {
        let mut props = CLzmaEncProps::default();
        LzmaEncProps_Init(&mut props);
        props.level = LZMA_LEVEL;
        props.reduceSize = hunk_bytes as u64;
        LzmaEncProps_Normalize(&mut props);
        props
    }
}

/// Encode the 5-byte LZMA properties from configured props.
fn encode_props(props: &CLzmaEncProps) -> [u8; LZMA_PROPS_SIZE as usize] {
    let dict_size = unsafe { LzmaEncProps_GetDictSize(props) };
    let byte0 = ((props.pb * 5 + props.lp) * 9 + props.lc) as u8;
    let mut out = [0u8; LZMA_PROPS_SIZE as usize];
    out[0] = byte0;
    out[1..5].copy_from_slice(&dict_size.to_le_bytes());
    out
}

pub(crate) fn lzma_compress(data: &[u8]) -> ChdResult<Vec<u8>> {
    let props = configure_props(CD_HUNK_BYTES as usize);
    let alloc = Allocator::default();

    // Output buffer: compressed data can't be much larger than input
    let max_out = data.len() + data.len() / 3 + 128;
    let mut compressed = vec![0u8; max_out];
    let mut compressed_size = max_out as SizeT;
    let mut props_encoded = [0u8; LZMA_PROPS_SIZE as usize];
    let mut props_size = LZMA_PROPS_SIZE as SizeT;

    let res = unsafe {
        LzmaEncode(
            compressed.as_mut_ptr(),
            &mut compressed_size,
            data.as_ptr(),
            data.len() as SizeT,
            &props,
            props_encoded.as_mut_ptr(),
            &mut props_size,
            0, // writeEndMark = false
            ptr::null(),
            alloc.as_ref(),
            alloc.as_ref(),
        )
    };

    if res != SZ_OK as i32 {
        return Err(io::Error::other(
            format!("LZMA encode failed with code {res}"),
        )
        .into());
    }

    compressed.truncate(compressed_size as usize);
    Ok(compressed)
}

pub(crate) fn lzma_decompress(data: &[u8], expected_len: usize) -> ChdResult<Vec<u8>> {
    // Reconstruct the same props that were used during compression (hunk_bytes, not data length)
    let props = configure_props(CD_HUNK_BYTES as usize);
    let props_encoded = encode_props(&props);
    let alloc = Allocator::default();

    let mut dest = vec![0u8; expected_len];
    let mut dest_len = expected_len as SizeT;
    let mut src_len = data.len() as SizeT;
    let mut status = ELzmaStatus::LZMA_STATUS_NOT_SPECIFIED;

    let res = unsafe {
        LzmaDecode(
            dest.as_mut_ptr(),
            &mut dest_len,
            data.as_ptr(),
            &mut src_len,
            props_encoded.as_ptr(),
            LZMA_PROPS_SIZE,
            ELzmaFinishMode::LZMA_FINISH_END,
            &mut status,
            alloc.as_ref(),
        )
    };

    if res != SZ_OK as i32 {
        return Err(io::Error::other(
            format!("LZMA decode failed with code {res}"),
        )
        .into());
    }

    dest.truncate(dest_len as usize);
    Ok(dest)
}
