use crate::chd::error::ChdResult;
use lzma_sdk_sys::*;
use std::io;
use std::ptr;

const LZMA_LEVEL: i32 = 8;

/// Upper bound for LZMA compressed output size.
fn lzma_max_output_size(input_len: usize) -> usize {
    input_len + input_len / 3 + 128
}

/// Configure LZMA encoder properties matching chdman's configure_properties.
/// Uses hunk_bytes as reduceSize (matching chdman which passes hunkbytes, not base data length).
/// Encoder and decoder props can differ per level, but `LzmaEncProps_Normalize`
/// guarantees the decoder's fixed level-[`LZMA_LEVEL`] dictSize (reduceSize-capped)
/// is always >= the encoder's dictSize for any level within the 1 MB hunk cap,
/// so decode stays correct.
fn configure_props(hunk_bytes: usize, level: i32) -> CLzmaEncProps {
    unsafe {
        let mut props = CLzmaEncProps::default();
        LzmaEncProps_Init(&mut props);
        props.level = level;
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

/// A reusable LZMA encoder that creates the encoder handle once and reuses it
/// via `LzmaEnc_MemEncode`, matching chdman's approach of persistent codec state.
pub(crate) struct LzmaEncoder {
    handle: CLzmaEncHandle,
    alloc: Allocator,
}

// SAFETY: The LZMA encoder handle is only accessed by one thread at a time
// (protected by Mutex in CdCodecSet).
unsafe impl Send for LzmaEncoder {}

impl LzmaEncoder {
    pub fn new(hunk_bytes: usize, level: i32) -> io::Result<Self> {
        let alloc = Allocator::default();
        let handle = unsafe { LzmaEnc_Create(alloc.as_ref()) };
        if handle.is_null() {
            return Err(io::Error::other("Failed to create LZMA encoder"));
        }
        let props = configure_props(hunk_bytes, level);
        unsafe {
            let res = LzmaEnc_SetProps(handle, &props);
            if res != SZ_OK as i32 {
                LzmaEnc_Destroy(handle, alloc.as_ref(), alloc.as_ref());
                return Err(io::Error::other("Failed to set LZMA encoder props"));
            }
        }
        Ok(Self { handle, alloc })
    }

    pub fn compress(&self, data: &[u8]) -> ChdResult<Vec<u8>> {
        let max_out = lzma_max_output_size(data.len());
        let mut compressed = vec![0u8; max_out];
        let mut compressed_size = max_out as SizeT;
        let res = unsafe {
            LzmaEnc_MemEncode(
                self.handle,
                compressed.as_mut_ptr(),
                &mut compressed_size,
                data.as_ptr(),
                data.len() as SizeT,
                0, // writeEndMark = false
                ptr::null(),
                self.alloc.as_ref(),
                self.alloc.as_ref(),
            )
        };
        if res != SZ_OK as i32 {
            return Err(io::Error::other(format!("LZMA encode failed with code {res}")).into());
        }
        compressed.truncate(compressed_size as usize);
        Ok(compressed)
    }
}

impl Drop for LzmaEncoder {
    fn drop(&mut self) {
        unsafe {
            LzmaEnc_Destroy(self.handle, self.alloc.as_ref(), self.alloc.as_ref());
        }
    }
}

/// Persistent LZMA decoder that keeps the probability table and
/// dictionary buffer alive across calls, mirroring the way
/// [`LzmaEncoder`] reuses its encoder handle. One instance lives
/// for the lifetime of a decompress worker thread so every hunk
/// skips the `LzmaDec_Allocate` + dictionary allocation.
pub(crate) struct LzmaDecoder {
    handle: CLzmaDec,
    alloc: Allocator,
}

// SAFETY: The decoder handle is only accessed by one thread at a
// time (owned by a [`crate::util::worker_pool::Worker`]).
unsafe impl Send for LzmaDecoder {}

impl LzmaDecoder {
    pub fn new(hunk_bytes: usize) -> io::Result<Self> {
        let props = configure_props(hunk_bytes, LZMA_LEVEL);
        Self::with_props(&encode_props(&props))
    }

    /// Build a decoder from raw 5-byte LZMA properties as found in
    /// container metadata (WIA stores them verbatim in its
    /// `compressor_data` field).
    pub fn with_props(props_encoded: &[u8]) -> io::Result<Self> {
        if props_encoded.len() != LZMA_PROPS_SIZE as usize {
            return Err(io::Error::other(format!(
                "LZMA props must be {LZMA_PROPS_SIZE} bytes, got {}",
                props_encoded.len()
            )));
        }
        let alloc = Allocator::default();
        // CLzmaDec::default() zeros the struct, matching the
        // `LzmaDec_Construct` macro in `LzmaDec.h`. That's the
        // required pre-state before `LzmaDec_Allocate`.
        let mut handle = CLzmaDec::default();
        let res = unsafe {
            LzmaDec_Allocate(
                &mut handle,
                props_encoded.as_ptr(),
                LZMA_PROPS_SIZE,
                alloc.as_ref(),
            )
        };
        if res != SZ_OK as i32 {
            return Err(io::Error::other(format!(
                "LzmaDec_Allocate failed with code {res}"
            )));
        }
        Ok(Self { handle, alloc })
    }

    pub fn decompress(&mut self, src: &[u8], expected_len: usize) -> ChdResult<Vec<u8>> {
        // `LzmaDec_Init` resets the decoder state but keeps the
        // allocated dictionary + probability table, so per-call
        // cost is a ~1 µs struct wipe instead of a fresh
        // allocation.
        unsafe { LzmaDec_Init(&mut self.handle) };

        let mut dest = vec![0u8; expected_len];
        let mut dest_len = expected_len as SizeT;
        let mut src_len = src.len() as SizeT;
        let mut status = ELzmaStatus::LZMA_STATUS_NOT_SPECIFIED;

        let res = unsafe {
            LzmaDec_DecodeToBuf(
                &mut self.handle,
                dest.as_mut_ptr(),
                &mut dest_len,
                src.as_ptr(),
                &mut src_len,
                ELzmaFinishMode::LZMA_FINISH_END,
                &mut status,
            )
        };

        if res != SZ_OK as i32 {
            return Err(
                io::Error::other(format!("LzmaDec_DecodeToBuf failed with code {res}")).into(),
            );
        }
        dest.truncate(dest_len as usize);
        Ok(dest)
    }
}

impl Drop for LzmaDecoder {
    fn drop(&mut self) {
        unsafe {
            LzmaDec_Free(&mut self.handle, self.alloc.as_ref());
        }
    }
}
