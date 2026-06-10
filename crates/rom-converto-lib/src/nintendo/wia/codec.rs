//! Per-worker WIA group decoding: codec dispatch plus the exception
//! list framing rules.
//!
//! Framing (Dolphin `WIABlob.cpp` / `WiaAndRvz.md`):
//! * None and Purge store exception lists uncompressed before the
//!   payload, padded to a 4-byte boundary after the last list.
//! * Bzip2, LZMA, and LZMA2 compress the exception lists together
//!   with the payload; no padding.
//! * Purge payloads are sparse `wia_segment_t` runs followed by a
//!   SHA-1 over the exception list bytes and the segment structs.
//! * Decompressed group data may be shorter than expected; the
//!   missing tail is zeroes (the writer trims trailing zeroes).

use sha1::{Digest, Sha1};

use super::error::{WiaError, WiaResult};
use super::format::{
    WIA_COMPR_BZIP2, WIA_COMPR_LZMA, WIA_COMPR_LZMA2, WIA_COMPR_NONE, WIA_COMPR_PURGE,
};
use crate::nintendo::rvl::partition::HashException;

/// Upper bound for one serialised exception list: u16 count plus
/// Dolphin's documented 3328-entry cap.
const MAX_EXCEPTION_LIST_BYTES: usize = 2 + 3328 * 22;

pub(crate) enum WiaCodec {
    None,
    Purge,
    Bzip2,
    Lzma(RawLzmaDec),
    Lzma2(RawLzma2Dec),
}

impl WiaCodec {
    pub(crate) fn new(compression: u32, props: &[u8]) -> WiaResult<Self> {
        match compression {
            WIA_COMPR_NONE => Ok(Self::None),
            WIA_COMPR_PURGE => Ok(Self::Purge),
            WIA_COMPR_BZIP2 => Ok(Self::Bzip2),
            WIA_COMPR_LZMA => Ok(Self::Lzma(RawLzmaDec::new(props)?)),
            WIA_COMPR_LZMA2 => Ok(Self::Lzma2(RawLzma2Dec::new(props)?)),
            other => Err(WiaError::UnsupportedCompression(other)),
        }
    }

    fn uncompressed_framing(&self) -> bool {
        matches!(self, Self::None | Self::Purge)
    }

    /// Decode one group's stored bytes into its exception lists and
    /// its payload, zero-extended to `payload_expected`.
    pub(crate) fn decode_group(
        &mut self,
        stored: &[u8],
        n_lists: usize,
        payload_expected: usize,
    ) -> WiaResult<(Vec<Vec<HashException>>, Vec<u8>)> {
        if self.uncompressed_framing() {
            let (lists, raw_exception_bytes, rest) = parse_exception_lists(stored, n_lists, true)?;
            let mut payload = match self {
                Self::None => rest.to_vec(),
                Self::Purge => purge_decode(rest, raw_exception_bytes, payload_expected)?,
                _ => unreachable!("uncompressed framing is None or Purge"),
            };
            zero_extend(&mut payload, payload_expected);
            Ok((lists, payload))
        } else {
            let max_out = payload_expected + n_lists * MAX_EXCEPTION_LIST_BYTES;
            let decompressed = match self {
                Self::Bzip2 => bzip2_decode(stored, max_out)?,
                Self::Lzma(dec) => dec.decode(stored, max_out)?,
                Self::Lzma2(dec) => dec.decode(stored, max_out)?,
                _ => unreachable!("compressed framing"),
            };
            let (lists, _, rest) = parse_exception_lists(&decompressed, n_lists, false)?;
            let mut payload = rest.to_vec();
            zero_extend(&mut payload, payload_expected);
            Ok((lists, payload))
        }
    }

    /// Decode a metadata table (raw-data or group table): same codec,
    /// no exception framing.
    pub(crate) fn decode_table(&mut self, stored: &[u8], expected: usize) -> WiaResult<Vec<u8>> {
        let (_, payload) = self.decode_group(stored, 0, expected)?;
        Ok(payload)
    }
}

fn zero_extend(payload: &mut Vec<u8>, expected: usize) {
    if payload.len() < expected {
        payload.resize(expected, 0);
    } else {
        payload.truncate(expected);
    }
}

/// Parse `n_lists` exception lists from the front of `data`. Returns
/// the lists, the raw bytes of the lists themselves (for the Purge
/// SHA-1, which covers them), and the remaining payload region.
/// Missing trailing bytes count as zeroes (empty lists), matching the
/// writer's trailing-zero trimming.
type ParsedLists<'a> = (Vec<Vec<HashException>>, &'a [u8], &'a [u8]);

fn parse_exception_lists(
    data: &[u8],
    n_lists: usize,
    align_to_4: bool,
) -> WiaResult<ParsedLists<'_>> {
    let mut lists = Vec::with_capacity(n_lists);
    let mut pos = 0usize;
    for _ in 0..n_lists {
        if pos + 2 > data.len() {
            lists.push(Vec::new());
            pos = data.len();
            continue;
        }
        let n = u16::from_be_bytes(data[pos..pos + 2].try_into().unwrap()) as usize;
        pos += 2;
        if pos + n * 22 > data.len() {
            return Err(WiaError::Decode(format!(
                "exception list declares {n} entries but the group data ends early"
            )));
        }
        let mut list = Vec::with_capacity(n);
        for i in 0..n {
            let e = &data[pos + i * 22..pos + (i + 1) * 22];
            list.push(HashException {
                offset: u16::from_be_bytes(e[..2].try_into().unwrap()),
                hash: e[2..22].try_into().unwrap(),
            });
        }
        pos += n * 22;
        lists.push(list);
    }
    let exception_bytes = &data[..pos];
    if align_to_4 {
        pos = ((pos + 3) & !3).min(data.len());
    }
    Ok((lists, exception_bytes, &data[pos..]))
}

/// Purge payload: sparse `wia_segment_t {u32 offset, u32 size, data}`
/// runs over a zero base, with a trailing SHA-1 over the exception
/// list bytes plus all segment structs.
fn purge_decode(stream: &[u8], exception_bytes: &[u8], out_len: usize) -> WiaResult<Vec<u8>> {
    if stream.len() < 20 {
        return Err(WiaError::Decode("purge group shorter than its hash".into()));
    }
    let mut hasher = Sha1::new();
    hasher.update(exception_bytes);
    let mut out = vec![0u8; out_len];
    let mut pos = 0usize;
    while stream.len() - pos > 20 {
        if stream.len() - pos < 28 {
            return Err(WiaError::Decode("truncated purge segment header".into()));
        }
        let offset = u32::from_be_bytes(stream[pos..pos + 4].try_into().unwrap()) as usize;
        let size = u32::from_be_bytes(stream[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if stream.len() - pos - 8 < size {
            return Err(WiaError::Decode("truncated purge segment data".into()));
        }
        if offset + size > out.len() {
            return Err(WiaError::Decode(format!(
                "purge segment {offset:#X}+{size:#X} exceeds group size {out_len:#X}"
            )));
        }
        hasher.update(&stream[pos..pos + 8 + size]);
        out[offset..offset + size].copy_from_slice(&stream[pos + 8..pos + 8 + size]);
        pos += 8 + size;
    }
    let computed: [u8; 20] = hasher.finalize().into();
    if computed != stream[pos..pos + 20] {
        return Err(WiaError::HashChainMismatch("purge group"));
    }
    Ok(out)
}

fn bzip2_decode(stored: &[u8], max_out: usize) -> WiaResult<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    bzip2::read::BzDecoder::new(stored)
        .take(max_out as u64)
        .read_to_end(&mut out)
        .map_err(|e| WiaError::Decode(format!("bzip2: {e}")))?;
    Ok(out)
}

/// Persistent raw LZMA1 decoder fed by WIA's 5-byte `compr_data`
/// props. Streams with `LZMA_FINISH_ANY` until the input is consumed,
/// so end-marker and marker-less streams both decode.
pub(crate) struct RawLzmaDec {
    handle: lzma_sdk_sys::CLzmaDec,
    alloc: lzma_sdk_sys::Allocator,
}

unsafe impl Send for RawLzmaDec {}

impl RawLzmaDec {
    fn new(props: &[u8]) -> WiaResult<Self> {
        if props.len() < lzma_sdk_sys::LZMA_PROPS_SIZE as usize {
            return Err(WiaError::InvalidHeader(format!(
                "LZMA props must be {} bytes, got {}",
                lzma_sdk_sys::LZMA_PROPS_SIZE,
                props.len()
            )));
        }
        let alloc = lzma_sdk_sys::Allocator::default();
        let mut handle = lzma_sdk_sys::CLzmaDec::default();
        let res = unsafe {
            lzma_sdk_sys::LzmaDec_Allocate(
                &mut handle,
                props.as_ptr(),
                lzma_sdk_sys::LZMA_PROPS_SIZE,
                alloc.as_ref(),
            )
        };
        if res != lzma_sdk_sys::SZ_OK as i32 {
            return Err(WiaError::Decode(format!("LzmaDec_Allocate failed ({res})")));
        }
        Ok(Self { handle, alloc })
    }

    fn decode(&mut self, src: &[u8], max_out: usize) -> WiaResult<Vec<u8>> {
        unsafe { lzma_sdk_sys::LzmaDec_Init(&mut self.handle) };
        let mut out = vec![0u8; max_out];
        let mut out_pos = 0usize;
        let mut in_pos = 0usize;
        loop {
            let mut dest_len = (out.len() - out_pos) as lzma_sdk_sys::SizeT;
            let mut src_len = (src.len() - in_pos) as lzma_sdk_sys::SizeT;
            if dest_len == 0 {
                break;
            }
            let mut status = lzma_sdk_sys::ELzmaStatus::LZMA_STATUS_NOT_SPECIFIED;
            let res = unsafe {
                lzma_sdk_sys::LzmaDec_DecodeToBuf(
                    &mut self.handle,
                    out[out_pos..].as_mut_ptr(),
                    &mut dest_len,
                    src[in_pos..].as_ptr(),
                    &mut src_len,
                    lzma_sdk_sys::ELzmaFinishMode::LZMA_FINISH_ANY,
                    &mut status,
                )
            };
            if res != lzma_sdk_sys::SZ_OK as i32 {
                return Err(WiaError::Decode(format!("LZMA decode failed ({res})")));
            }
            out_pos += dest_len as usize;
            in_pos += src_len as usize;
            if status == lzma_sdk_sys::ELzmaStatus::LZMA_STATUS_FINISHED_WITH_MARK
                || (dest_len == 0 && src_len == 0)
            {
                break;
            }
        }
        out.truncate(out_pos);
        Ok(out)
    }
}

impl Drop for RawLzmaDec {
    fn drop(&mut self) {
        unsafe { lzma_sdk_sys::LzmaDec_Free(&mut self.handle, self.alloc.as_ref()) };
    }
}

/// Persistent raw LZMA2 decoder; WIA stores the single LZMA2 props
/// byte in `compr_data`.
pub(crate) struct RawLzma2Dec {
    handle: lzma_sdk_sys::CLzma2Dec,
    alloc: lzma_sdk_sys::Allocator,
}

unsafe impl Send for RawLzma2Dec {}

impl RawLzma2Dec {
    fn new(props: &[u8]) -> WiaResult<Self> {
        let prop = *props
            .first()
            .ok_or_else(|| WiaError::InvalidHeader("missing LZMA2 props byte".into()))?;
        let alloc = lzma_sdk_sys::Allocator::default();
        let mut handle = lzma_sdk_sys::CLzma2Dec::default();
        let res = unsafe { lzma_sdk_sys::Lzma2Dec_Allocate(&mut handle, prop, alloc.as_ref()) };
        if res != lzma_sdk_sys::SZ_OK as i32 {
            return Err(WiaError::Decode(format!(
                "Lzma2Dec_Allocate failed ({res})"
            )));
        }
        Ok(Self { handle, alloc })
    }

    fn decode(&mut self, src: &[u8], max_out: usize) -> WiaResult<Vec<u8>> {
        unsafe { lzma_sdk_sys::Lzma2Dec_Init(&mut self.handle) };
        let mut out = vec![0u8; max_out];
        let mut out_pos = 0usize;
        let mut in_pos = 0usize;
        loop {
            let mut dest_len = (out.len() - out_pos) as lzma_sdk_sys::SizeT;
            let mut src_len = (src.len() - in_pos) as lzma_sdk_sys::SizeT;
            if dest_len == 0 {
                break;
            }
            let mut status = lzma_sdk_sys::ELzmaStatus::LZMA_STATUS_NOT_SPECIFIED;
            let res = unsafe {
                lzma_sdk_sys::Lzma2Dec_DecodeToBuf(
                    &mut self.handle,
                    out[out_pos..].as_mut_ptr(),
                    &mut dest_len,
                    src[in_pos..].as_ptr(),
                    &mut src_len,
                    lzma_sdk_sys::ELzmaFinishMode::LZMA_FINISH_ANY,
                    &mut status,
                )
            };
            if res != lzma_sdk_sys::SZ_OK as i32 {
                return Err(WiaError::Decode(format!("LZMA2 decode failed ({res})")));
            }
            out_pos += dest_len as usize;
            in_pos += src_len as usize;
            if status == lzma_sdk_sys::ELzmaStatus::LZMA_STATUS_FINISHED_WITH_MARK
                || (dest_len == 0 && src_len == 0)
            {
                break;
            }
        }
        out.truncate(out_pos);
        Ok(out)
    }
}

impl Drop for RawLzma2Dec {
    fn drop(&mut self) {
        // The SDK's Lzma2Dec_Free macro expands to freeing the inner
        // LZMA1 state; bindgen does not export macros.
        unsafe { lzma_sdk_sys::LzmaDec_Free(&mut self.handle.decoder, self.alloc.as_ref()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lists_handles_zero_trimmed_tails() {
        // Two lists requested, data ends after the first: second is empty.
        let mut data = Vec::new();
        data.extend_from_slice(&1u16.to_be_bytes());
        data.extend_from_slice(&0x0400u16.to_be_bytes());
        data.extend_from_slice(&[0xAA; 20]);
        let (lists, _, rest) = parse_exception_lists(&data, 2, false).unwrap();
        assert_eq!(lists.len(), 2);
        assert_eq!(lists[0].len(), 1);
        assert_eq!(lists[0][0].offset, 0x0400);
        assert!(lists[1].is_empty());
        assert!(rest.is_empty());
    }

    #[test]
    fn parse_lists_aligns_after_all_lists() {
        // One list with one entry: 2 + 22 = 24 bytes, aligned to 24
        // (already multiple of 4); with zero entries: 2 -> pad to 4.
        let mut data = vec![0u8, 0u8];
        data.extend_from_slice(&[0xFF, 0xFF]); // padding then payload
        data.extend_from_slice(b"PAYL");
        let (lists, _, rest) = parse_exception_lists(&data, 1, true).unwrap();
        assert!(lists[0].is_empty());
        assert_eq!(rest, b"PAYL");
    }

    #[test]
    fn purge_round_trip_with_hash() {
        let exception_bytes = [0u8, 0u8];
        let mut payload = vec![0u8; 0x100];
        payload[0x40..0x44].copy_from_slice(b"DATA");

        let mut stream = Vec::new();
        stream.extend_from_slice(&0x40u32.to_be_bytes());
        stream.extend_from_slice(&4u32.to_be_bytes());
        stream.extend_from_slice(b"DATA");
        let mut hasher = Sha1::new();
        hasher.update(exception_bytes);
        hasher.update(&stream);
        let digest: [u8; 20] = hasher.finalize().into();
        stream.extend_from_slice(&digest);

        let out = purge_decode(&stream, &exception_bytes, 0x100).unwrap();
        assert_eq!(out, payload);

        let mut bad = stream.clone();
        let n = bad.len();
        bad[n - 1] ^= 1;
        assert!(purge_decode(&bad, &exception_bytes, 0x100).is_err());
    }

    #[test]
    fn lzma_round_trips_marker_less_stream() {
        // chd's encoder writes marker-less LZMA with known props.
        let original: Vec<u8> = (0u8..=255).cycle().take(50_000).collect();
        let (props, compressed) = crate::nintendo::wia::test_fixtures::lzma_encode(&original);
        let mut dec = RawLzmaDec::new(&props).unwrap();
        let out = dec.decode(&compressed, original.len()).unwrap();
        assert_eq!(out, original);
    }

    #[test]
    fn lzma2_round_trips() {
        let original: Vec<u8> = (0u8..=255).cycle().take(50_000).collect();
        let (prop, compressed) = crate::nintendo::wia::test_fixtures::lzma2_encode(&original);
        let mut dec = RawLzma2Dec::new(&[prop]).unwrap();
        let out = dec.decode(&compressed, original.len()).unwrap();
        assert_eq!(out, original);
    }
}
