//! Random-access `NcaInput` adapter over an NCZ file.
//!
//! `NcaWalker` only ever issues positional reads, so this can present
//! the decompressed + re-encrypted NCA byte stream as a virtual file
//! and walk an NCZ without first materializing it to disk.
//!
//! Block-mode NCZ decodes on demand: small blocks are decoded whole
//! and cached (at most one at a time), large blocks are streamed
//! through a zstd decoder that discards unread bytes into a small
//! fixed buffer and only ever fills the caller's requested range, so
//! reading a few bytes out of a multi-GiB block never materializes
//! the whole thing. Raw-stored blocks are read directly from disk at
//! the exact requested sub-range.
//!
//! Solid-mode (single zstd frame) cannot be block-random-accessed, so
//! it is served lazily through a forward-resumable decoder cursor:
//! decoding advances only as far as the furthest byte any caller has
//! requested (never eagerly, so a small compressed blob can't be used
//! as a decompression bomb and a truncated frame is only detected once
//! the missing tail is actually consumed). Backward reads restart the
//! decoder and discard through a reusable bounded buffer. Decoder
//! failure is sticky so a later read never silently re-attempts a
//! broken stream. The logical length comes from the already-decrypted
//! NCA header's own `content_size` field (the verbatim prefix is
//! available before any payload byte is touched), not from trusting
//! the zstd frame's own pledged size or draining it to find out.
//!
//! Re-encryption applies the stored per-section CTR keystream so the
//! bytes match what `NcaWalker` would see on a real encrypted NCA,
//! including unaligned reads that don't start on a 16-byte CTR
//! boundary. XTS sections are out of scope; the Control NCA path that
//! drives this adapter only touches CTR / NONE sections.

use std::fs::File;
use std::io::{BufReader, Read};
use std::sync::{Arc, Mutex};

use aes::Aes128;
use aes::cipher::{KeyIvInit, StreamCipher};
use ctr::Ctr128BE;

use crate::nintendo::nx::constants::{
    ENC_AES_CTR, ENC_AES_CTR_EX, ENC_AES_CTR_EX_SKIP_LAYER_HASH, ENC_AES_CTR_SKIP_LAYER_HASH,
    ENC_NONE, NCA_HEADER_SIZE, NCA_PREFIX_SIZE,
};
use crate::nintendo::nx::crypto::aes_xts::decrypt_nca_header;
use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::models::nca::NcaHeader;
use crate::nintendo::nx::ncz::header::{NczSectionEntry, read_headers};
use crate::nintendo::nx::walker::NcaInput;
use crate::util::extent_end;
use crate::util::positional_reader::PositionalReader;
use crate::util::pread::file_read_exact_at;

type AesCtr = Ctr128BE<Aes128>;
type SolidDecoder = zstd::stream::read::Decoder<'static, BufReader<PositionalReader>>;

/// Block (or solid-frame) size above which this reader stream-decodes
/// through a small buffer instead of decoding the whole block into a
/// cached `Vec`. Comfortably above every default block exponent
/// (2^20 = 1 MiB) and any real Control NCA, so ordinary reads never
/// take the slower path; only unusually large explicit block
/// exponents (>=24, i.e. >=16 MiB) or solid frames (real multi-GB
/// Program NCAs) do.
const LARGE_PAYLOAD_THRESHOLD: u64 = 8 * 1024 * 1024;
/// Buffered-reader capacity and discard/fill chunk size used by the
/// streamed decode paths, so large transfers cost one syscall per
/// megabyte rather than one per few KiB.
const STREAM_CHUNK: usize = 1024 * 1024;

fn buffered_decoder(source: PositionalReader) -> NxResult<SolidDecoder> {
    let buffered = BufReader::with_capacity(STREAM_CHUNK, source);
    SolidDecoder::with_buffer(buffered)
        .map_err(|e| NxError::ZstdError(format!("zstd decoder init: {e}")))
}

enum NczPayload {
    Block(BlockPayload),
    Solid(SolidPayload),
}

struct BlockPayload {
    file: Arc<File>,
    block_size: u64,
    block_offsets: Vec<u64>,
    block_sizes: Vec<u32>,
    decompressed_payload_size: u64,
    /// Most recently decoded block; small blocks retain their bytes,
    /// while large blocks retain a decoder cursor for forward reads.
    cache: Mutex<Option<BlockCache>>,
}
enum BlockCache {
    Decoded(usize, Vec<u8>),
    Cursor(usize, SolidDecoder, u64, Vec<u8>),
}

/// Solid-frame payload: either fully decoded in memory up front
/// (bounded by the NCA-header-validated logical size, matching the
/// original fast path for small payloads) or served lazily through a
/// resumable decoder cursor for payloads above [`LARGE_PAYLOAD_THRESHOLD`].
enum SolidPayload {
    Memory(Vec<u8>),
    Cursor(SolidCursor),
}

struct SolidCursor {
    file: Arc<File>,
    payload_abs_start: u64,
    payload_compressed_size: u64,
    /// Logical (decompressed) length from the decrypted NCA header.
    total_len: u64,
    state: Mutex<SolidState>,
}

struct SolidState {
    decoder: Option<SolidDecoder>,
    position: u64,
    failed: bool,
    /// Reused for skipped bytes rather than allocated per read.
    scratch: Vec<u8>,
}

impl SolidCursor {
    fn read_at(&self, dest: &mut [u8], payload_off: u64) -> NxResult<usize> {
        let mut state = self
            .state
            .lock()
            .expect("solid payload mutex should not be poisoned");
        if state.failed {
            return Err(NxError::NczSolidDecodeFailed);
        }
        if payload_off < state.position {
            let source = PositionalReader::new(
                self.file.clone(),
                self.payload_abs_start,
                self.payload_compressed_size,
            );
            let decoder = match buffered_decoder(source) {
                Ok(decoder) => decoder,
                Err(error) => {
                    state.failed = true;
                    return Err(error);
                }
            };
            state.decoder = Some(decoder);
            state.position = 0;
        }
        let want_end = payload_off
            .saturating_add(dest.len() as u64)
            .min(self.total_len);
        while state.position < payload_off {
            let to_read = (payload_off - state.position).min(state.scratch.len() as u64) as usize;
            let result = {
                let SolidState {
                    decoder, scratch, ..
                } = &mut *state;
                match decoder.as_mut() {
                    Some(decoder) => decoder.read(&mut scratch[..to_read]),
                    None => break,
                }
            };
            let n = match result {
                Ok(n) => n,
                Err(e) => {
                    state.failed = true;
                    return Err(NxError::ZstdError(format!("solid zstd decode: {e}")));
                }
            };
            if n == 0 {
                state.decoder = None;
                break;
            }
            state.position += n as u64;
        }
        if state.position < payload_off || state.decoder.is_none() {
            return Ok(0);
        }
        let take = usize::try_from(want_end.saturating_sub(payload_off))
            .map_err(|_| NxError::IncompleteSection)?;
        let mut written = 0;
        while written < take {
            let result = match state.decoder.as_mut() {
                Some(decoder) => decoder.read(&mut dest[written..take]),
                None => break,
            };
            let n = match result {
                Ok(n) => n,
                Err(e) => {
                    state.failed = true;
                    return Err(NxError::ZstdError(format!("solid zstd decode: {e}")));
                }
            };
            if n == 0 {
                state.decoder = None;
                break;
            }
            written += n;
            state.position += n as u64;
        }
        Ok(written)
    }
}

/// Decodes up to the header-declared size. Short frames expose their
/// actual output length; longer frames are truncated at that size.
fn decode_solid_bounded(
    file: &Arc<File>,
    payload_abs_start: u64,
    payload_compressed_size: u64,
    total_len: u64,
) -> NxResult<Vec<u8>> {
    let source = PositionalReader::new(file.clone(), payload_abs_start, payload_compressed_size);
    let mut decoder = buffered_decoder(source)?;
    let mut out = Vec::with_capacity(total_len as usize);
    let mut scratch = vec![0u8; STREAM_CHUNK];
    while (out.len() as u64) < total_len {
        let want = (total_len - out.len() as u64).min(scratch.len() as u64) as usize;
        let n = decoder
            .read(&mut scratch[..want])
            .map_err(|e| NxError::ZstdError(format!("solid zstd decode: {e}")))?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&scratch[..n]);
    }
    Ok(out)
}

/// Random-access reader over an NCZ file: caches the raw prefix bytes
/// and the parsed section table, and demand-decodes the payload for
/// on-demand block decompression / solid cursor reads plus re-encryption
/// on read.
pub struct NczReader {
    prefix: Box<[u8; NCA_PREFIX_SIZE]>,
    sections: Vec<NczSectionEntry>,
    payload: NczPayload,
}

impl NczReader {
    /// Opens `file` as an NCZ container starting at
    /// `nca_offset_in_container`. Reads the prefix and headers; for
    /// block mode it only records block offsets/sizes for later
    /// on-demand decompression, while solid mode sets up a lazy
    /// decoder cursor whose logical length comes from the decrypted
    /// NCA header's own `content_size` field. Opening never touches
    /// payload bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if `ncz_total_size` is smaller than the NCA
    /// prefix, the headers are malformed, or the NCA header is invalid.
    pub fn open(
        file: Arc<File>,
        nca_offset_in_container: u64,
        ncz_total_size: u64,
        header_key: &[u8; 32],
    ) -> NxResult<Self> {
        if ncz_total_size < NCA_PREFIX_SIZE as u64 {
            return Err(NxError::IncompleteSection);
        }

        let mut prefix = Box::new([0u8; NCA_PREFIX_SIZE]);
        file_read_exact_at(&file, prefix.as_mut_slice(), nca_offset_in_container)?;

        let mut header_reader = PositionalReader::new(
            file.clone(),
            nca_offset_in_container + NCA_PREFIX_SIZE as u64,
            ncz_total_size - NCA_PREFIX_SIZE as u64,
        );
        let parsed = read_headers(&mut header_reader)?;
        let payload_abs_start =
            nca_offset_in_container + NCA_PREFIX_SIZE as u64 + parsed.payload_offset;
        let payload_container_bound = ncz_total_size
            .checked_add(nca_offset_in_container)
            .ok_or(NxError::IncompleteSection)?;

        let payload = match parsed.block {
            Some(block_info) => {
                let block_size = block_info.block_size_bytes();
                let mut block_offsets = Vec::with_capacity(block_info.compressed_block_sizes.len());
                let mut cursor = payload_abs_start;
                for &csz in &block_info.compressed_block_sizes {
                    block_offsets.push(cursor);
                    cursor = extent_end(cursor, u64::from(csz), payload_container_bound)
                        .ok_or(NxError::IncompleteSection)?;
                }
                NczPayload::Block(BlockPayload {
                    file: file.clone(),
                    block_size,
                    block_offsets,
                    block_sizes: block_info.compressed_block_sizes,
                    decompressed_payload_size: block_info.decompressed_size as u64,
                    cache: Mutex::new(None),
                })
            }
            None => {
                let payload_compressed_size =
                    ncz_total_size.saturating_sub(payload_abs_start - nca_offset_in_container);
                if payload_compressed_size == 0 {
                    return Err(NxError::IncompleteSection);
                }
                // Logical length comes from the NCA's own decrypted
                // header, not the zstd frame: a malicious/corrupt
                // frame pledge (or draining an unpledged frame to find
                // out) could otherwise be used as a decompression
                // bomb.
                let mut header_copy = [0u8; NCA_HEADER_SIZE];
                header_copy.copy_from_slice(&prefix[..NCA_HEADER_SIZE]);
                decrypt_nca_header(&mut header_copy, header_key)?;
                let header = NcaHeader::parse(&header_copy)?;
                let total_len = header
                    .content_size
                    .checked_sub(NCA_PREFIX_SIZE as u64)
                    .ok_or(NxError::IncompleteSection)?;

                if total_len <= LARGE_PAYLOAD_THRESHOLD {
                    // The NCA header bounds eager decoding: EOF before
                    // that bound returns available bytes; excess output
                    // is never read.
                    let decoded = decode_solid_bounded(
                        &file,
                        payload_abs_start,
                        payload_compressed_size,
                        total_len,
                    )?;
                    NczPayload::Solid(SolidPayload::Memory(decoded))
                } else {
                    let source = PositionalReader::new(
                        file.clone(),
                        payload_abs_start,
                        payload_compressed_size,
                    );
                    let decoder = buffered_decoder(source)?;
                    NczPayload::Solid(SolidPayload::Cursor(SolidCursor {
                        file: file.clone(),
                        payload_abs_start,
                        payload_compressed_size,
                        total_len,
                        state: Mutex::new(SolidState {
                            decoder: Some(decoder),
                            position: 0,
                            failed: false,
                            scratch: vec![0u8; STREAM_CHUNK],
                        }),
                    }))
                }
            }
        };

        Ok(Self {
            prefix,
            sections: parsed.sections,
            payload,
        })
    }

    /// Returns the total decompressed NCA size: the fixed prefix plus
    /// the decompressed payload size.
    pub fn decompressed_nca_size(&self) -> u64 {
        let payload_size = match &self.payload {
            NczPayload::Block(b) => b.decompressed_payload_size,
            NczPayload::Solid(SolidPayload::Memory(v)) => v.len() as u64,
            NczPayload::Solid(SolidPayload::Cursor(cursor)) => {
                let state = cursor
                    .state
                    .lock()
                    .expect("solid payload mutex should not be poisoned");
                if state.decoder.is_none() {
                    state.position
                } else {
                    cursor.total_len
                }
            }
        };
        NCA_PREFIX_SIZE as u64 + payload_size
    }

    fn copy_payload(&self, dest: &mut [u8], payload_off: u64) -> NxResult<usize> {
        match &self.payload {
            NczPayload::Solid(SolidPayload::Memory(v)) => {
                let off = usize::try_from(payload_off).map_err(|_| NxError::IncompleteSection)?;
                let take = (v.len() - off).min(dest.len());
                dest[..take].copy_from_slice(&v[off..off + take]);
                Ok(take)
            }
            NczPayload::Solid(SolidPayload::Cursor(cursor)) => cursor.read_at(dest, payload_off),
            NczPayload::Block(b) => b.read_at(dest, payload_off),
        }
    }
}

impl BlockPayload {
    fn read_at(&self, dest: &mut [u8], payload_off: u64) -> NxResult<usize> {
        let block_idx = usize::try_from(payload_off / self.block_size)
            .map_err(|_| NxError::IncompleteSection)?;
        let in_block = payload_off % self.block_size;
        let offset = *self
            .block_offsets
            .get(block_idx)
            .ok_or(NxError::IncompleteSection)?;
        let csz = usize::try_from(
            *self
                .block_sizes
                .get(block_idx)
                .ok_or(NxError::IncompleteSection)?,
        )
        .map_err(|_| NxError::IncompleteSection)?;

        let is_last = block_idx + 1 == self.block_sizes.len();
        let logical_size = if is_last {
            self.decompressed_payload_size
                .checked_sub((block_idx as u64) * self.block_size)
                .ok_or(NxError::IncompleteSection)?
        } else {
            self.block_size
        };
        if in_block >= logical_size {
            return Err(NxError::IncompleteSection);
        }
        let take = usize::try_from((logical_size - in_block).min(dest.len() as u64))
            .map_err(|_| NxError::IncompleteSection)?;
        let reaches_block_end = in_block + take as u64 == logical_size;

        if csz as u64 == logical_size {
            file_read_exact_at(&self.file, &mut dest[..take], offset + in_block)?;
            return Ok(take);
        }

        if logical_size <= LARGE_PAYLOAD_THRESHOLD {
            let logical_size =
                usize::try_from(logical_size).map_err(|_| NxError::IncompleteSection)?;
            let in_block = usize::try_from(in_block).map_err(|_| NxError::IncompleteSection)?;
            let mut cache = self
                .cache
                .lock()
                .expect("block cache mutex should not be poisoned");
            if let Some(BlockCache::Decoded(idx, decoded)) = &*cache
                && *idx == block_idx
            {
                if in_block >= decoded.len() {
                    return Ok(0);
                }
                let actual_take = take.min(decoded.len() - in_block);
                dest[..actual_take].copy_from_slice(&decoded[in_block..in_block + actual_take]);
                return Ok(actual_take);
            }
            let source = PositionalReader::new(self.file.clone(), offset, csz as u64);
            let mut decoder = buffered_decoder(source)?;
            let mut decoded = vec![0u8; logical_size];
            let mut filled = 0usize;
            while filled < decoded.len() {
                let n = decoder.read(&mut decoded[filled..]).map_err(|e| {
                    NxError::ZstdError(format!("decompress block {block_idx}: {e}"))
                })?;
                if n == 0 {
                    break;
                }
                filled += n;
            }
            decoded.truncate(filled);
            drain_decoder(&mut decoder, block_idx)?;
            if in_block >= decoded.len() {
                *cache = Some(BlockCache::Decoded(block_idx, decoded));
                return Ok(0);
            }
            let actual_take = take.min(decoded.len() - in_block);
            dest[..actual_take].copy_from_slice(&decoded[in_block..in_block + actual_take]);
            *cache = Some(BlockCache::Decoded(block_idx, decoded));
            return Ok(actual_take);
        }

        let mut cache = self
            .cache
            .lock()
            .expect("block cache mutex should not be poisoned");
        let (mut decoder, mut position, mut scratch) = match cache.take() {
            Some(BlockCache::Cursor(idx, decoder, position, scratch))
                if idx == block_idx && position <= in_block =>
            {
                (decoder, position, scratch)
            }
            _ => {
                let source = PositionalReader::new(self.file.clone(), offset, csz as u64);
                (buffered_decoder(source)?, 0, vec![0u8; STREAM_CHUNK])
            }
        };
        while position < in_block {
            let scratch_len = usize::try_from((in_block - position).min(scratch.len() as u64))
                .map_err(|_| NxError::IncompleteSection)?;
            let n = decoder
                .read(&mut scratch[..scratch_len])
                .map_err(|e| NxError::ZstdError(format!("decompress block {block_idx}: {e}")))?;
            if n == 0 {
                return Err(NxError::IncompleteSection);
            }
            position += n as u64;
        }
        let mut written = 0usize;
        while written < take {
            let n = decoder
                .read(&mut dest[written..take])
                .map_err(|e| NxError::ZstdError(format!("decompress block {block_idx}: {e}")))?;
            if n == 0 {
                break;
            }
            written += n;
        }
        position += written as u64;
        if reaches_block_end {
            drain_decoder(&mut decoder, block_idx)?;
        }
        *cache = Some(BlockCache::Cursor(block_idx, decoder, position, scratch));
        Ok(written)
    }
}

/// Drains the remainder of a block frame to validate it while ignoring
/// output beyond the NCA-declared logical block size.
fn drain_decoder(decoder: &mut SolidDecoder, block_idx: usize) -> NxResult<()> {
    let mut scratch = [0u8; 16 * 1024];
    loop {
        let n = decoder
            .read(&mut scratch)
            .map_err(|e| NxError::ZstdError(format!("decompress block {block_idx}: {e}")))?;
        if n == 0 {
            return Ok(());
        }
    }
}

impl NcaInput for NczReader {
    fn read_exact_at(&self, buf: &mut [u8], abs: u64) -> NxResult<()> {
        let total = self.decompressed_nca_size();
        if abs.saturating_add(buf.len() as u64) > total {
            return Err(NxError::IncompleteSection);
        }

        let mut written = 0usize;
        while written < buf.len() {
            let here = abs + written as u64;
            if here < NCA_PREFIX_SIZE as u64 {
                let take =
                    (NCA_PREFIX_SIZE as u64 - here).min((buf.len() - written) as u64) as usize;
                buf[written..written + take]
                    .copy_from_slice(&self.prefix[here as usize..here as usize + take]);
                written += take;
                continue;
            }

            let payload_off = here - NCA_PREFIX_SIZE as u64;
            let take = self.copy_payload(&mut buf[written..], payload_off)?;
            if take == 0 {
                return Err(NxError::IncompleteSection);
            }
            reencrypt_in_buf(&mut buf[written..written + take], here, &self.sections)?;
            written += take;
        }
        Ok(())
    }
}

fn reencrypt_in_buf(buf: &mut [u8], start_abs: u64, sections: &[NczSectionEntry]) -> NxResult<()> {
    let mut covered = 0usize;
    while covered < buf.len() {
        let here = start_abs + covered as u64;
        let section = sections.iter().find(|s| {
            let so = s.offset as u64;
            let se = so.saturating_add(s.size as u64);
            s.size > 0 && here >= so && here < se
        });
        let Some(section) = section else {
            covered += 1;
            continue;
        };
        let section_end = (section.offset as u64).saturating_add(section.size as u64);
        let span = usize::try_from((section_end - here).min((buf.len() - covered) as u64))
            .map_err(|_| NxError::IncompleteSection)?;
        match section.crypto_type as u8 {
            ENC_NONE => {}
            ENC_AES_CTR
            | ENC_AES_CTR_EX
            | ENC_AES_CTR_SKIP_LAYER_HASH
            | ENC_AES_CTR_EX_SKIP_LAYER_HASH => {
                let mut counter = section.crypto_counter;
                let block = here / 16;
                counter[8..16].copy_from_slice(&block.to_be_bytes());
                let mut cipher = AesCtr::new_from_slices(&section.crypto_key, &counter)
                    .map_err(|e| NxError::AesError(format!("Ctr128BE init: {e}")))?;
                // The keystream is block-aligned but `here` may not
                // be: discard the leading `here % 16` keystream bytes
                // so the cipher is positioned exactly at `here`
                // rather than at the start of its 16-byte CTR block.
                let skip = (here % 16) as usize;
                let mut discarded = [0u8; 16];
                cipher.apply_keystream(&mut discarded[..skip]);
                cipher.apply_keystream(&mut buf[covered..covered + span]);
            }
            other => return Err(NxError::UnsupportedEncryption(other)),
        }
        covered += span;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::nx::constants::{NCZBLOCK_MAGIC, NCZSECTN_MAGIC};
    use byteorder::{LE, WriteBytesExt};
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn write_sections<W: Write>(bytes: &mut W, sections: &[NczSectionEntry]) {
        bytes.write_all(&NCZSECTN_MAGIC).unwrap();
        bytes.write_i64::<LE>(sections.len() as i64).unwrap();
        for s in sections {
            bytes.write_i64::<LE>(s.offset).unwrap();
            bytes.write_i64::<LE>(s.size).unwrap();
            bytes.write_i64::<LE>(s.crypto_type).unwrap();
            bytes.write_i64::<LE>(0).unwrap();
            bytes.write_all(&s.crypto_key).unwrap();
            bytes.write_all(&s.crypto_counter).unwrap();
        }
    }

    fn build_minimal_ncz_with_one_block(
        prefix_byte: u8,
        plaintext_block: &[u8],
        sections: &[NczSectionEntry],
    ) -> Vec<u8> {
        let mut bytes = vec![prefix_byte; NCA_PREFIX_SIZE];
        write_sections(&mut bytes, sections);
        let block_size_exp = 14u8;
        let block_size = 1usize << block_size_exp;
        assert!(plaintext_block.len() <= block_size);

        bytes.write_all(&NCZBLOCK_MAGIC).unwrap();
        bytes.write_u8(1).unwrap();
        bytes.write_u8(0).unwrap();
        bytes.write_u8(0).unwrap();
        bytes.write_u8(block_size_exp).unwrap();
        bytes.write_u32::<LE>(1).unwrap();
        bytes.write_i64::<LE>(plaintext_block.len() as i64).unwrap();
        bytes.write_u32::<LE>(plaintext_block.len() as u32).unwrap();

        bytes.extend_from_slice(plaintext_block);
        bytes
    }

    /// Builds a two-block NCZ (block size 0x4000) so tests can exercise
    /// section-crossing and backward reads across a block boundary.
    fn build_two_block_ncz(
        prefix_byte: u8,
        block0: &[u8],
        block1: &[u8],
        sections: &[NczSectionEntry],
        level: i32,
    ) -> Vec<u8> {
        let block_size_exp = 14u8;
        let block_size = 1usize << block_size_exp;
        assert_eq!(block0.len(), block_size);
        assert!(!block1.is_empty() && block1.len() <= block_size);

        let mut bytes = vec![prefix_byte; NCA_PREFIX_SIZE];
        write_sections(&mut bytes, sections);

        let c0 = zstd::stream::encode_all(std::io::Cursor::new(block0), level).unwrap();
        let c1 = zstd::stream::encode_all(std::io::Cursor::new(block1), level).unwrap();

        bytes.write_all(&NCZBLOCK_MAGIC).unwrap();
        bytes.write_u8(1).unwrap();
        bytes.write_u8(0).unwrap();
        bytes.write_u8(0).unwrap();
        bytes.write_u8(block_size_exp).unwrap();
        bytes.write_u32::<LE>(2).unwrap();
        bytes
            .write_i64::<LE>((block0.len() + block1.len()) as i64)
            .unwrap();
        bytes.write_u32::<LE>(c0.len() as u32).unwrap();
        bytes.write_u32::<LE>(c1.len() as u32).unwrap();

        bytes.extend_from_slice(&c0);
        bytes.extend_from_slice(&c1);
        bytes
    }

    fn open_reader(bytes: &[u8]) -> NczReader {
        static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "rom-converto-ncz-test-{}-{}",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut tmp = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        tmp.write_all(bytes).unwrap();
        tmp.flush().unwrap();
        drop(tmp);
        let file = Arc::new(File::open(&path).unwrap());
        std::fs::remove_file(path).unwrap();
        let len = bytes.len() as u64;
        NczReader::open(
            file,
            0,
            len,
            &crate::nintendo::nx::test_fixtures::TEST_HEADER_KEY,
        )
        .unwrap()
    }

    /// Builds a real encrypted NCA header (magic + `content_size`)
    /// padded to the fixed prefix size, so solid-mode tests exercise
    /// the actual NCA-header size authority rather than a synthetic
    /// pledge.
    fn build_encrypted_prefix(content_size: u64) -> [u8; NCA_PREFIX_SIZE] {
        use crate::nintendo::nx::constants::NCA3_MAGIC;
        use crate::nintendo::nx::test_fixtures::TEST_HEADER_KEY;
        let mut header = [0u8; NCA_HEADER_SIZE];
        header[0x200..0x204].copy_from_slice(&NCA3_MAGIC);
        header[0x208..0x210].copy_from_slice(&content_size.to_le_bytes());
        crate::nintendo::nx::crypto::aes_xts::encrypt_nca_header(&mut header, &TEST_HEADER_KEY)
            .unwrap();
        let mut prefix = [0u8; NCA_PREFIX_SIZE];
        prefix[..NCA_HEADER_SIZE].copy_from_slice(&header);
        prefix
    }

    fn build_solid_ncz(payload: &[u8], sections: &[NczSectionEntry]) -> Vec<u8> {
        build_solid_ncz_with_declared_size(payload, payload.len(), sections)
    }

    fn build_solid_ncz_with_declared_size(
        payload: &[u8],
        declared_payload_len: usize,
        sections: &[NczSectionEntry],
    ) -> Vec<u8> {
        let mut bytes =
            build_encrypted_prefix(NCA_PREFIX_SIZE as u64 + declared_payload_len as u64).to_vec();
        write_sections(&mut bytes, sections);
        let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
        encoder.write_all(payload).unwrap();
        bytes.extend_from_slice(&encoder.finish().unwrap());
        bytes
    }

    #[test]
    fn enc_none_section_passes_through() {
        let plaintext: Vec<u8> = (0..0x200).map(|i| (i & 0xFF) as u8).collect();
        let section = NczSectionEntry {
            offset: NCA_PREFIX_SIZE as i64,
            size: 0x200,
            crypto_type: ENC_NONE as i64,
            crypto_key: [0; 16],
            crypto_counter: [0; 16],
        };
        let ncz = build_minimal_ncz_with_one_block(0xAA, &plaintext, &[section]);
        let reader = open_reader(&ncz);
        let mut got = vec![0u8; 0x200];
        reader
            .read_exact_at(&mut got, NCA_PREFIX_SIZE as u64)
            .unwrap();
        assert_eq!(got, plaintext);
    }

    #[test]
    fn ctr_section_reencrypts_to_expected_ciphertext() {
        let key = [0x55u8; 16];
        let counter = [0u8; 16];
        let plaintext: Vec<u8> = (0..0x100).map(|i| (i ^ 0x3C) as u8).collect();
        let section = NczSectionEntry {
            offset: NCA_PREFIX_SIZE as i64,
            size: 0x100,
            crypto_type: ENC_AES_CTR as i64,
            crypto_key: key,
            crypto_counter: counter,
        };
        let ncz = build_minimal_ncz_with_one_block(0xAA, &plaintext, &[section]);
        let reader = open_reader(&ncz);
        let mut got = vec![0u8; 0x100];
        reader
            .read_exact_at(&mut got, NCA_PREFIX_SIZE as u64)
            .unwrap();

        let mut expected = plaintext.clone();
        let mut counter_filled = counter;
        let block = (NCA_PREFIX_SIZE as u64) / 16;
        counter_filled[8..16].copy_from_slice(&block.to_be_bytes());
        let mut cipher = AesCtr::new_from_slices(&key, &counter_filled).unwrap();
        cipher.apply_keystream(&mut expected);
        assert_eq!(got, expected);
    }

    #[test]
    fn ctr_unaligned_offset_matches_full_keystream_slice() {
        // Regression: re-encryption used to always start the CTR
        // keystream at the 16-byte block boundary, corrupting any
        // read that doesn't begin exactly on one.
        let key = [0x77u8; 16];
        let counter = [0u8; 16];
        let plaintext: Vec<u8> = (0..0x40).map(|i| (i ^ 0x11) as u8).collect();
        let section = NczSectionEntry {
            offset: NCA_PREFIX_SIZE as i64,
            size: 0x40,
            crypto_type: ENC_AES_CTR as i64,
            crypto_key: key,
            crypto_counter: counter,
        };
        let ncz = build_minimal_ncz_with_one_block(0xAA, &plaintext, &[section]);
        let reader = open_reader(&ncz);

        // Compute the expected full-range ciphertext once, then check
        // that unaligned sub-reads return the matching slice.
        let mut expected = plaintext.clone();
        let mut counter_filled = counter;
        let block = (NCA_PREFIX_SIZE as u64) / 16;
        counter_filled[8..16].copy_from_slice(&block.to_be_bytes());
        let mut cipher = AesCtr::new_from_slices(&key, &counter_filled).unwrap();
        cipher.apply_keystream(&mut expected);

        for start in [1usize, 15, 17, 33] {
            let len = expected.len() - start;
            let mut got = vec![0u8; len];
            reader
                .read_exact_at(&mut got, NCA_PREFIX_SIZE as u64 + start as u64)
                .unwrap();
            assert_eq!(got, expected[start..], "mismatch at start={start}");
        }
    }

    #[test]
    fn nca_prefix_passes_through() {
        let section = NczSectionEntry {
            offset: NCA_PREFIX_SIZE as i64,
            size: 0x10,
            crypto_type: ENC_NONE as i64,
            crypto_key: [0; 16],
            crypto_counter: [0; 16],
        };
        let ncz = build_minimal_ncz_with_one_block(0xCD, &[0xEE; 0x10], &[section]);
        let reader = open_reader(&ncz);
        let mut got = vec![0u8; 0x40];
        reader.read_exact_at(&mut got, 0).unwrap();
        assert!(got.iter().all(|b| *b == 0xCD));
    }

    #[test]
    fn block_read_crosses_section_gap_and_boundary() {
        // Two sections with a gap between them, spanning the block-0
        // / block-1 boundary (block size 0x4000). Bytes not covered
        // by any section must pass through unmodified.
        let block_size = 0x4000usize;
        let block0: Vec<u8> = (0..block_size).map(|i| (i & 0xFF) as u8).collect();
        let block1: Vec<u8> = (0..0x100).map(|i| ((i * 3) & 0xFF) as u8).collect();
        let sec_a = NczSectionEntry {
            offset: NCA_PREFIX_SIZE as i64,
            size: 0x100,
            crypto_type: ENC_AES_CTR as i64,
            crypto_key: [0x11; 16],
            crypto_counter: [0u8; 16],
        };
        // Gap of 0x100 bytes (unmodified) then a second CTR section
        // that starts inside block 0 and ends inside block 1.
        let sec_b = NczSectionEntry {
            offset: NCA_PREFIX_SIZE as i64 + 0x200,
            size: (block_size as i64 - 0x200) + 0x80,
            crypto_type: ENC_AES_CTR as i64,
            crypto_key: [0x22; 16],
            crypto_counter: [0u8; 16],
        };
        let ncz = build_two_block_ncz(0xAA, &block0, &block1, &[sec_a, sec_b], 3);
        let reader = open_reader(&ncz);

        let total = block0.len() + block1.len();
        let mut got = vec![0u8; total];
        reader
            .read_exact_at(&mut got, NCA_PREFIX_SIZE as u64)
            .unwrap();

        // Gap bytes [0x100, 0x200) are untouched.
        assert_eq!(&got[0x100..0x200], &block0[0x100..0x200]);
        // Section bytes must differ from plaintext (they were
        // re-encrypted), proving both sections were applied across
        // the block boundary.
        assert_ne!(&got[..0x100], &block0[..0x100]);
        assert_ne!(&got[0x200..block_size], &block0[0x200..]);
        assert_ne!(&got[block_size..block_size + 0x80], &block1[..0x80]);
    }

    #[test]
    fn block_backward_read_matches_forward_read() {
        let block_size = 0x4000usize;
        let block0: Vec<u8> = (0..block_size).map(|i| (i & 0xFF) as u8).collect();
        let block1: Vec<u8> = (0..0x200).map(|i| ((i * 7) & 0xFF) as u8).collect();
        let ncz = build_two_block_ncz(0xAA, &block0, &block1, &[], 3);
        let reader = open_reader(&ncz);

        let mut forward = vec![0u8; block0.len() + block1.len()];
        reader
            .read_exact_at(&mut forward, NCA_PREFIX_SIZE as u64)
            .unwrap();

        // Re-read block 0's tail after having already decoded past it
        // into block 1 (a "backward" access relative to the forward
        // scan above).
        let mut backward = vec![0u8; 0x100];
        reader
            .read_exact_at(&mut backward, NCA_PREFIX_SIZE as u64 + 0x3F00)
            .unwrap();
        assert_eq!(backward, forward[0x3F00..0x4000]);
    }

    #[test]
    fn large_block_streams_without_materializing_whole_block() {
        // The logical size exceeds LARGE_PAYLOAD_THRESHOLD so the
        // streaming path is selected even though the nominal block
        // exponent is larger.
        let large_exp = 24u8; // 16 MiB nominal block size
        let payload_len = usize::try_from(LARGE_PAYLOAD_THRESHOLD + 1).unwrap();
        let payload: Vec<u8> = (0..payload_len).map(|i| (i & 0xFF) as u8).collect();

        let mut bytes = vec![0xAAu8; NCA_PREFIX_SIZE];
        write_sections(&mut bytes, &[]);
        let compressed =
            zstd::stream::encode_all(std::io::Cursor::new(payload.as_slice()), 3).unwrap();
        // Exercise the streaming path with a logical size above the
        // threshold, not merely a large nominal exponent.
        assert!(payload.len() as u64 > LARGE_PAYLOAD_THRESHOLD);
        assert_ne!(compressed.len(), payload.len());

        bytes.write_all(&NCZBLOCK_MAGIC).unwrap();
        bytes.write_u8(1).unwrap();
        bytes.write_u8(0).unwrap();
        bytes.write_u8(0).unwrap();
        bytes.write_u8(large_exp).unwrap();
        bytes.write_u32::<LE>(1).unwrap();
        bytes.write_i64::<LE>(payload.len() as i64).unwrap();
        bytes.write_u32::<LE>(compressed.len() as u32).unwrap();
        bytes.extend_from_slice(&compressed);

        let reader = open_reader(&bytes);
        let mut got = vec![0u8; 0x20];
        reader
            .read_exact_at(&mut got, NCA_PREFIX_SIZE as u64 + 0x800)
            .unwrap();
        assert_eq!(got, payload[0x800..0x820]);
    }

    #[test]
    fn large_block_cursor_cache_handles_forward_backward_and_tail_reads() {
        let block_size = 1usize << 24;
        let block0: Vec<u8> = (0..block_size).map(|i| (i & 0xFF) as u8).collect();
        let block1: Vec<u8> = (0..0x800).map(|i| ((i * 7) & 0xFF) as u8).collect();
        assert!(block0.len() as u64 > LARGE_PAYLOAD_THRESHOLD);

        let compressed0 =
            zstd::stream::encode_all(std::io::Cursor::new(block0.as_slice()), 3).unwrap();
        let compressed1 =
            zstd::stream::encode_all(std::io::Cursor::new(block1.as_slice()), 3).unwrap();
        let mut bytes = vec![0xAAu8; NCA_PREFIX_SIZE];
        write_sections(&mut bytes, &[]);
        bytes.write_all(&NCZBLOCK_MAGIC).unwrap();
        bytes.write_u8(1).unwrap();
        bytes.write_u8(0).unwrap();
        bytes.write_u8(0).unwrap();
        bytes.write_u8(24).unwrap();
        bytes.write_u32::<LE>(2).unwrap();
        bytes
            .write_i64::<LE>((block0.len() + block1.len()) as i64)
            .unwrap();
        bytes.write_u32::<LE>(compressed0.len() as u32).unwrap();
        bytes.write_u32::<LE>(compressed1.len() as u32).unwrap();
        bytes.extend_from_slice(&compressed0);
        bytes.extend_from_slice(&compressed1);
        let reader = open_reader(&bytes);

        let mut a = [0u8; 0x40];
        reader
            .read_exact_at(&mut a, NCA_PREFIX_SIZE as u64 + 0x800)
            .unwrap();
        assert_eq!(a, block0[0x800..0x840]);

        let mut b = [0u8; 0x40];
        reader
            .read_exact_at(&mut b, NCA_PREFIX_SIZE as u64 + 0x10000)
            .unwrap();
        assert_eq!(b, block0[0x10000..0x10040]);
        {
            let cache = match &reader.payload {
                NczPayload::Block(block) => block.cache.lock().unwrap(),
                _ => unreachable!(),
            };
            assert!(matches!(
                &*cache,
                Some(BlockCache::Cursor(0, _, 0x10040, _))
            ));
        }

        let mut c = [0u8; 0x40];
        reader
            .read_exact_at(&mut c, NCA_PREFIX_SIZE as u64 + 0x100)
            .unwrap();
        assert_eq!(c, block0[0x100..0x140]);
        {
            let cache = match &reader.payload {
                NczPayload::Block(block) => block.cache.lock().unwrap(),
                _ => unreachable!(),
            };
            assert!(matches!(&*cache, Some(BlockCache::Cursor(0, _, 0x140, _))));
        }

        let end = block0.len() + block1.len();
        let mut tail = [0u8; 0x20];
        let tail_len = tail.len();
        reader
            .read_exact_at(
                &mut tail,
                NCA_PREFIX_SIZE as u64 + end as u64 - tail_len as u64,
            )
            .unwrap();
        assert_eq!(tail, block1[block1.len() - tail_len..]);

        let mut after_tail = [0u8; 0x20];
        reader
            .read_exact_at(&mut after_tail, NCA_PREFIX_SIZE as u64 + 0x180)
            .unwrap();
        assert_eq!(after_tail, block0[0x180..0x1A0]);
    }

    #[test]
    fn solid_cursor_resumes_forward_and_restarts_backward() {
        let payload_len = usize::try_from(LARGE_PAYLOAD_THRESHOLD + 0x4000).unwrap();
        let payload: Vec<u8> = (0..payload_len).map(|i| (i & 0xFF) as u8).collect();
        let ncz = build_solid_ncz(&payload, &[]);
        let reader = open_reader(&ncz);
        assert_eq!(
            reader.decompressed_nca_size(),
            NCA_PREFIX_SIZE as u64 + payload.len() as u64
        );

        let mut got = vec![0u8; 0x100];
        reader
            .read_exact_at(&mut got, NCA_PREFIX_SIZE as u64 + 0x8000)
            .unwrap();
        assert_eq!(got, payload[0x8000..0x8100]);
        let mut forward = vec![0u8; 0x80];
        reader
            .read_exact_at(&mut forward, NCA_PREFIX_SIZE as u64 + 0x8100)
            .unwrap();
        assert_eq!(forward, payload[0x8100..0x8180]);

        // Backward reads restart the decoder and must return the same
        // bytes as the original forward pass.
        let mut back = vec![0u8; 0x40];
        reader
            .read_exact_at(&mut back, NCA_PREFIX_SIZE as u64 + 0x10)
            .unwrap();
        assert_eq!(back, payload[0x10..0x50]);
    }

    #[test]
    fn solid_short_frame_yields_only_available_bytes() {
        let payload_len = (LARGE_PAYLOAD_THRESHOLD + 0x4000) as usize;
        let payload: Vec<u8> = (0..payload_len).map(|i| (i & 0xFF) as u8).collect();
        let ncz = build_solid_ncz_with_declared_size(&payload, payload_len + 0x1000, &[]);
        let reader = open_reader(&ncz);

        let mut prefix = vec![0u8; 0x100];
        reader
            .read_exact_at(&mut prefix, NCA_PREFIX_SIZE as u64)
            .unwrap();
        assert_eq!(prefix, payload[..0x100]);

        let mut beyond_eof = vec![0u8; payload_len + 1];
        assert!(
            reader
                .read_exact_at(&mut beyond_eof, NCA_PREFIX_SIZE as u64)
                .is_err()
        );
        assert_eq!(
            reader.decompressed_nca_size(),
            NCA_PREFIX_SIZE as u64 + payload.len() as u64
        );
    }

    #[test]
    fn solid_long_frame_is_truncated_to_header_size() {
        let payload: Vec<u8> = (0..0x1000).map(|i| (i & 0xFF) as u8).collect();
        let ncz = build_solid_ncz_with_declared_size(&payload, 0x800, &[]);
        let reader = open_reader(&ncz);
        assert_eq!(
            reader.decompressed_nca_size(),
            NCA_PREFIX_SIZE as u64 + 0x800
        );
        let mut got = vec![0u8; 0x800];
        reader
            .read_exact_at(&mut got, NCA_PREFIX_SIZE as u64)
            .unwrap();
        assert_eq!(got, payload[..0x800]);
    }
}
