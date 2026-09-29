//! NCA -> NCZ. Single-threaded reference path used by tests and as the
//! correctness oracle for the parallel block-mode compressor.

use std::io::{Seek, SeekFrom, Write};

use crate::nintendo::nx::constants::{
    DEFAULT_BLOCK_SIZE_EXP, DEFAULT_ZSTD_LEVEL, ENC_AES_CTR, ENC_AES_CTR_EX,
    ENC_AES_CTR_EX_SKIP_LAYER_HASH, ENC_AES_CTR_SKIP_LAYER_HASH, ENC_NONE, MAX_BLOCK_SIZE_EXP,
    MAX_ZSTD_LEVEL, MIN_BLOCK_SIZE_EXP, MIN_ZSTD_LEVEL, NCA_PREFIX_SIZE,
};
use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::models::nca::initial_ctr_for_offset;
use crate::nintendo::nx::ncz::LARGE_BLOCK_STREAM_CHUNK;
use crate::nintendo::nx::ncz::compress_worker::{NczBlockWork, spawn_ncz_pool};
use crate::nintendo::nx::ncz::header::{
    NczBlockInfo, NczSectionEntry, write_nczblock, write_nczsectn,
};
use crate::nintendo::nx::walker::NcaWalker;
use crate::util::ProgressReporter;
use crate::util::worker_pool::{drive, parallelism};

/// NCZ payload layout: one continuous zstd stream, or fixed-size
/// blocks that can be decompressed independently.
#[derive(Debug, Clone, Copy)]
pub enum NczMode {
    Solid,
    Block { size_exp: u8 },
}

impl Default for NczMode {
    fn default() -> Self {
        NczMode::Block {
            size_exp: DEFAULT_BLOCK_SIZE_EXP,
        }
    }
}

/// Options controlling NCA -> NCZ compression: block layout and zstd level.
#[derive(Debug, Clone, Copy)]
pub struct NcaToNczOptions {
    pub mode: NczMode,
    pub level: i32,
}

impl Default for NcaToNczOptions {
    fn default() -> Self {
        Self {
            mode: NczMode::Solid,
            level: DEFAULT_ZSTD_LEVEL,
        }
    }
}

impl NcaToNczOptions {
    fn validate(self) -> NxResult<Self> {
        if !(MIN_ZSTD_LEVEL..=MAX_ZSTD_LEVEL).contains(&self.level) {
            return Err(NxError::InvalidCompressionLevel {
                level: self.level,
                min: MIN_ZSTD_LEVEL,
                max: MAX_ZSTD_LEVEL,
            });
        }
        if let NczMode::Block { size_exp } = self.mode
            && !(MIN_BLOCK_SIZE_EXP..=MAX_BLOCK_SIZE_EXP).contains(&size_exp)
        {
            return Err(NxError::BlockSizeOutOfRange(size_exp));
        }
        Ok(self)
    }
}

/// Writes `walker`'s NCA payload out as an NCZ container: the raw
/// prefix, the section table, then the payload compressed per
/// `opts.mode` (solid single-stream or independently decompressable blocks).
///
/// # Errors
///
/// Returns an error if `opts` fails validation, if reading from
/// `walker` or writing to `out` fails, or if zstd compression fails.
pub fn nca_to_ncz<W: Write + Seek>(
    walker: &NcaWalker,
    out: &mut W,
    opts: NcaToNczOptions,
    progress: &dyn ProgressReporter,
) -> NxResult<()> {
    let opts = opts.validate()?;

    let nca_offset = walker.nca_offset();
    let nca_size = walker.nca_size();
    let prefix_size = usize::try_from(nca_size.min(NCA_PREFIX_SIZE as u64))
        .map_err(|_| NxError::IncompleteSection)?;

    let mut prefix = vec![0u8; prefix_size];
    walker.read_exact_at(&mut prefix, nca_offset)?;
    out.write_all(&prefix)?;
    progress.inc(prefix_size as u64);

    let entries = build_section_entries(walker)?;
    write_nczsectn(out, &entries)?;

    let payload_size = nca_size.saturating_sub(NCA_PREFIX_SIZE as u64);
    if payload_size == 0 {
        return Ok(());
    }

    match opts.mode {
        NczMode::Solid => write_solid(walker, out, payload_size, opts.level, progress),
        NczMode::Block { size_exp } => {
            let size_exp = if payload_size >= (1u64 << 32) {
                // The u32 compressed-size table cannot represent a 4 GiB raw block.
                size_exp.min(31)
            } else {
                size_exp
            };
            write_block(walker, out, payload_size, size_exp, opts.level, progress)
        }
    }
}

fn write_solid<W: Write + Seek>(
    walker: &NcaWalker,
    out: &mut W,
    payload_size: u64,
    level: i32,
    progress: &dyn ProgressReporter,
) -> NxResult<()> {
    let workers = crate::util::worker_pool::parallelism().min(u32::MAX as usize) as u32;
    let mut encoder = zstd::stream::write::Encoder::new(out, level)
        .map_err(|e| NxError::ZstdError(format!("zstd encoder init: {e}")))?;
    // Match nsz's `ZstdCompressionParameters.from_level(level,
    // threads=N)` solid pipeline. With `zstdmt`, libzstd splits the
    // input into jobs, compresses them on N worker threads, and
    // serializes the output. This both saturates more cores AND
    // bumps the effective window/job sizing zstd uses, which on
    // multi-GB program NCAs trims a handful of percent off the output
    // compared to single-threaded `from_level` defaults.
    encoder
        .set_parameter(zstd::stream::raw::CParameter::NbWorkers(workers))
        .map_err(|e| NxError::ZstdError(format!("zstd NbWorkers: {e}")))?;
    // Long-distance matching gives the encoder a 27-bit (128 MiB)
    // window into past data. Without it, the level-18 default window
    // is 23 bits (8 MiB), which can never see far enough to dedupe
    // the multi-GB redundancy in real NCA RomFS payloads.
    encoder
        .set_parameter(zstd::stream::raw::CParameter::EnableLongDistanceMatching(
            true,
        ))
        .map_err(|e| NxError::ZstdError(format!("zstd EnableLDM: {e}")))?;
    let mut scratch = vec![0u8; LARGE_BLOCK_STREAM_CHUNK];
    stream_plain_range(
        walker,
        0,
        payload_size,
        &mut scratch,
        |chunk| -> NxResult<()> {
            encoder
                .write_all(chunk)
                .map_err(|e| NxError::ZstdError(format!("zstd write: {e}")))?;
            progress.inc(chunk.len() as u64);
            Ok(())
        },
    )?;
    encoder
        .finish()
        .map_err(|e| NxError::ZstdError(format!("zstd finish: {e}")))?;
    Ok(())
}

pub(super) fn write_block<W: Write + Seek>(
    walker: &NcaWalker,
    out: &mut W,
    payload_size: u64,
    size_exp: u8,
    level: i32,
    progress: &dyn ProgressReporter,
) -> NxResult<()> {
    let block_size_u64 = 1u64 << size_exp;
    let num_blocks_u64 = payload_size.div_ceil(block_size_u64);
    let Ok(block_size) = usize::try_from(block_size_u64) else {
        return write_block_large(walker, out, payload_size, size_exp, level, progress);
    };
    let Ok(num_blocks) = usize::try_from(num_blocks_u64) else {
        return write_block_large(walker, out, payload_size, size_exp, level, progress);
    };

    // Each in-flight job holds its plaintext input and (until the
    // reorder buffer consumes it) its compressed/raw output; workers
    // hold a persistent zstd context plus a `compress_bound`-sized
    // scratch buffer. Only fall back to the bounded sequential
    // streaming path when even one in-memory block's worth of that
    // doesn't fit the shared budget (huge block exponents) or there are
    // no blocks at all; ordinary exponents (including the default) admit
    // full host parallelism, identical to the original unconditional pool path.
    let codec_bytes_per_worker = crate::util::worker_pool::zstd_cctx_estimate(level, block_size)
        + zstd::zstd_safe::compress_bound(block_size);
    let queued_bytes_per_job = block_size.saturating_mul(2);
    let Some(admission) = crate::util::worker_pool::Budget {
        codec_per_worker: codec_bytes_per_worker,
        per_job: queued_bytes_per_job,
        writer_slot: 0,
        fixed: 0,
    }
    .admit(parallelism(), num_blocks as u64) else {
        return write_block_large(walker, out, payload_size, size_exp, level, progress);
    };
    let n_threads = admission.workers;
    let max_in_flight = admission.max_in_flight;
    let pool = spawn_ncz_pool(level, block_size, n_threads)?;

    // nsz has emitted version 2 / type 1 since the format's first
    // commit; readers that validate these bytes expect them.
    let header_start = out.stream_position()?;
    write_nczblock(out, &placeholder_block_info(payload_size, size_exp)?)?;

    let mut sizes = vec![0u32; num_blocks];
    let mut producer = PlaintextBlockProducer::new(walker, payload_size, block_size);

    let drive_result = drive(
        &pool,
        num_blocks as u64,
        max_in_flight,
        |_seq| -> NxResult<NczBlockWork> { producer.next_block(progress) },
        |seq, out_block| -> NxResult<()> {
            sizes[seq as usize] = out_block.bytes.len() as u32;
            out.write_all(&out_block.bytes)?;
            Ok(())
        },
    );
    pool.shutdown();
    drive_result?;
    finalize_block_header(out, header_start, payload_size, size_exp, sizes)
}

/// Sequential, bounded-memory block-mode compressor used when
/// `write_block`'s admission check finds that even one in-memory
/// block doesn't fit the shared worker-pool budget. It compresses
/// directly into `out`, then rewrites blocks that do not compress.
fn write_block_large<W: Write + Seek>(
    walker: &NcaWalker,
    out: &mut W,
    payload_size: u64,
    size_exp: u8,
    level: i32,
    progress: &dyn ProgressReporter,
) -> NxResult<()> {
    let header_start = out.stream_position()?;
    write_nczblock(out, &placeholder_block_info(payload_size, size_exp)?)?;
    let sizes = compress_blocks_into(walker, out, payload_size, size_exp, level, progress)?;
    finalize_block_header(out, header_start, payload_size, size_exp, sizes)
}

fn placeholder_block_info(payload_size: u64, size_exp: u8) -> NxResult<NczBlockInfo> {
    let block_size = 1u64 << size_exp;
    let num_blocks = usize::try_from(payload_size.div_ceil(block_size))
        .map_err(|_| NxError::IncompleteSection)?;
    Ok(NczBlockInfo {
        version: 2,
        kind: 1,
        block_size_exp: size_exp,
        decompressed_size: payload_size as i64,
        compressed_block_sizes: vec![0u32; num_blocks],
    })
}

fn finalize_block_header<W: Write + Seek>(
    out: &mut W,
    header_start: u64,
    payload_size: u64,
    size_exp: u8,
    sizes: Vec<u32>,
) -> NxResult<()> {
    let payload_end = out.stream_position()?;
    out.seek(SeekFrom::Start(header_start))?;
    write_nczblock(
        out,
        &NczBlockInfo {
            version: 2,
            kind: 1,
            block_size_exp: size_exp,
            decompressed_size: payload_size as i64,
            compressed_block_sizes: sizes,
        },
    )?;
    out.seek(SeekFrom::Start(payload_end))?;
    Ok(())
}

/// Writes each block's compressed (or raw-fallback) bytes to `sink`
/// in order without materializing a whole block in memory.
fn compress_blocks_into<S: Write + Seek>(
    walker: &NcaWalker,
    sink: &mut S,
    payload_size: u64,
    size_exp: u8,
    level: i32,
    progress: &dyn ProgressReporter,
) -> NxResult<Vec<u32>> {
    let block_size = 1u64 << size_exp;
    let num_blocks = usize::try_from(payload_size.div_ceil(block_size))
        .map_err(|_| NxError::IncompleteSection)?;
    let mut sizes = Vec::with_capacity(num_blocks);
    let mut scratch = vec![0u8; LARGE_BLOCK_STREAM_CHUNK];

    for i in 0..num_blocks {
        let block_start = (i as u64) * block_size;
        let block_len = block_size.min(payload_size - block_start);
        let output_start = sink.stream_position()?;
        let mut capped = CappedWriter::new(&mut *sink, block_len);
        let compression_result = (|| -> NxResult<()> {
            let mut encoder = zstd::stream::write::Encoder::new(&mut capped, level)
                .map_err(|e| NxError::ZstdError(format!("zstd encoder init: {e}")))?;
            encoder
                .set_pledged_src_size(Some(block_len))
                .map_err(|e| NxError::ZstdError(format!("zstd pledged size: {e}")))?;
            stream_plain_range(walker, block_start, block_len, &mut scratch, |chunk| {
                encoder
                    .write_all(chunk)
                    .map_err(|e| NxError::ZstdError(format!("zstd write: {e}")))?;
                Ok(())
            })?;
            encoder
                .finish()
                .map_err(|e| NxError::ZstdError(format!("zstd finish: {e}")))?;
            Ok(())
        })();
        let incompressible = capped.exceeded;
        if let Err(error) = compression_result
            && !incompressible
        {
            return Err(error);
        }
        if incompressible {
            sink.seek(SeekFrom::Start(output_start))?;
            sizes.push(u32::try_from(block_len).map_err(|_| NxError::IncompleteSection)?);
            stream_plain_range(walker, block_start, block_len, &mut scratch, |chunk| {
                sink.write_all(chunk)?;
                Ok(())
            })?;
        } else {
            let compressed_len = sink
                .stream_position()?
                .checked_sub(output_start)
                .ok_or(NxError::IncompleteSection)?;
            sizes.push(u32::try_from(compressed_len).map_err(|_| NxError::IncompleteSection)?);
        }
        progress.inc(block_len);
    }
    Ok(sizes)
}

struct CappedWriter<'a, W> {
    inner: &'a mut W,
    limit: u64,
    written: u64,
    exceeded: bool,
}

impl<'a, W> CappedWriter<'a, W> {
    fn new(inner: &'a mut W, limit: u64) -> Self {
        Self {
            inner,
            limit,
            written: 0,
            exceeded: false,
        }
    }
}

impl<W: Write> Write for CappedWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let remaining = self.limit - self.written;
        if buf.len() as u64 >= remaining {
            if remaining <= 1 {
                self.exceeded = true;
                return Err(std::io::Error::other("compressed block reached raw size"));
            }
            let written = self.inner.write(&buf[..(remaining - 1) as usize])?;
            self.written += written as u64;
            return Ok(written);
        }
        let written = self.inner.write(buf)?;
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

struct PlaintextBlockProducer<'a> {
    walker: &'a NcaWalker,
    block_size: usize,
    payload_size: u64,
    cursor: u64,
}

impl<'a> PlaintextBlockProducer<'a> {
    fn new(walker: &'a NcaWalker, payload_size: u64, block_size: usize) -> Self {
        Self {
            walker,
            block_size,
            payload_size,
            cursor: 0,
        }
    }

    // The driver only calls this on the dispatcher thread, so the read
    // state is owned here and never duplicated across threads. Allocates one Vec<u8> per
    // block handed to the worker pool; reuse would require the worker to send
    // the buffer back, which complicates the channel for a tiny win.
    fn next_block(&mut self, progress: &dyn ProgressReporter) -> NxResult<NczBlockWork> {
        let take = (self.block_size as u64).min(self.payload_size - self.cursor) as usize;
        let mut buf = vec![0u8; take];
        let abs = self.walker.nca_offset()
            + crate::nintendo::nx::constants::NCA_PREFIX_SIZE as u64
            + self.cursor;
        read_plain_range(self.walker, abs, &mut buf)?;
        self.cursor += take as u64;
        progress.inc(take as u64);
        Ok(NczBlockWork { plaintext: buf })
    }
}

fn stream_plain_range<F: FnMut(&[u8]) -> NxResult<()>>(
    walker: &NcaWalker,
    start: u64,
    len: u64,
    scratch: &mut [u8],
    mut sink: F,
) -> NxResult<()> {
    let payload_start_in_nca = NCA_PREFIX_SIZE as u64;
    let mut written = 0u64;
    while written < len {
        let take = (scratch.len() as u64).min(len - written) as usize;
        let abs_offset = walker.nca_offset() + payload_start_in_nca + start + written;
        read_plain_range(walker, abs_offset, &mut scratch[..take])?;
        sink(&scratch[..take])?;
        written += take as u64;
    }
    Ok(())
}

fn read_plain_range(walker: &NcaWalker, abs_offset: u64, buf: &mut [u8]) -> NxResult<()> {
    walker.read_exact_at(buf, abs_offset)?;
    if buf.is_empty() {
        return Ok(());
    }
    let nca_off_start = abs_offset - walker.nca_offset();

    let mut covered = 0usize;
    while covered < buf.len() {
        let here_nca = nca_off_start + covered as u64;
        let section = walker.sections.iter().find(|s| {
            let section_nca_offset = s.raw_offset - walker.nca_offset();
            here_nca >= section_nca_offset && here_nca < section_nca_offset + s.raw_size
        });
        let Some(section) = section else {
            covered += 1;
            continue;
        };
        let section_nca_offset = section.raw_offset - walker.nca_offset();
        let section_end = section_nca_offset + section.raw_size;
        let until = section_end.min(nca_off_start + buf.len() as u64);
        let span = (until - here_nca) as usize;

        match section.encryption_type {
            ENC_NONE => {
                covered += span;
            }
            ENC_AES_CTR
            | ENC_AES_CTR_EX
            | ENC_AES_CTR_SKIP_LAYER_HASH
            | ENC_AES_CTR_EX_SKIP_LAYER_HASH => {
                let in_section_offset = here_nca - section_nca_offset;
                let aligned_in = in_section_offset & !0xF;
                let head_skip = (in_section_offset - aligned_in) as usize;
                let aligned_len = (span + head_skip + 0xF) & !0xF;

                let mut tmp = vec![0u8; aligned_len];
                walker.read_exact_at(
                    &mut tmp,
                    walker.nca_offset() + section_nca_offset + aligned_in,
                )?;
                let counter_offset_in_nca = section_nca_offset + aligned_in;
                let fs = crate::nintendo::nx::models::nca::FsHeader {
                    section_ctr_low: section.section_ctr_low,
                    section_ctr_high: section.section_ctr_high,
                    ..Default::default()
                };
                let ctr = initial_ctr_for_offset(&fs, counter_offset_in_nca);
                crate::nintendo::nx::crypto::aes_ctr::apply_ctr(&section.key, &ctr, &mut tmp)?;
                buf[covered..covered + span].copy_from_slice(&tmp[head_skip..head_skip + span]);
                covered += span;
            }
            other => return Err(NxError::UnsupportedEncryption(other)),
        }
    }
    Ok(())
}

fn build_section_entries(walker: &NcaWalker) -> NxResult<Vec<NczSectionEntry>> {
    let mut out = Vec::with_capacity(walker.sections.len());
    for s in &walker.sections {
        let section_nca_offset = (s.raw_offset - walker.nca_offset()) as i64;
        // nsz stores bytes 0..8 of crypto_counter as the FsHeader's
        // section_ctr reversed, and bytes 8..16 as zeros (the
        // decompressor fills in `position_in_nca / 16` BE on the fly).
        // Match that convention so other tools can read the resulting NSZ.
        let mut crypto_counter = [0u8; 16];
        crypto_counter[0..4].copy_from_slice(&s.section_ctr_high.to_be_bytes());
        crypto_counter[4..8].copy_from_slice(&s.section_ctr_low.to_be_bytes());
        out.push(NczSectionEntry {
            offset: section_nca_offset,
            size: s.raw_size as i64,
            crypto_type: s.encryption_type as i64,
            crypto_key: s.key,
            crypto_counter,
        });
    }
    // Sort by offset ascending. nsz's `__getDecompressedNczSize`
    // accumulates `0x4000 + sum(section.size)` after inserting a
    // synthetic "fake section" for the gap before `sections[0]`,
    // and that math only matches the actual NCA size when entries
    // are ordered by offset. Real NCAs sometimes lay out sections
    // in non-monotonic fs_entry slots (such as slot 0 holding the
    // highest-offset section), so this sorts here.
    out.sort_by_key(|e| e.offset);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::nx::test_fixtures::{build_synthetic_nca, synthetic_keyset};
    use crate::util::NoProgress;
    use std::fs::File;
    use std::io::Cursor;
    use std::sync::Arc;
    use tempfile::NamedTempFile;

    #[test]
    fn large_block_compression_matches_pool_with_raw_fallback() {
        let block_size = 1 << 14;
        let mut plaintext = vec![0x5A; block_size];
        let mut state = 0x1234_5678u32;
        plaintext.extend((0..block_size).map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        }));
        let nca_bytes = build_synthetic_nca(&plaintext);
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(&nca_bytes).unwrap();
        tmp.flush().unwrap();
        let walker = NcaWalker::open(
            Arc::new(File::open(tmp.path()).unwrap()),
            0,
            nca_bytes.len() as u64,
            &synthetic_keyset(),
        )
        .unwrap();

        for level in [3, DEFAULT_ZSTD_LEVEL] {
            let mut pool_output = Cursor::new(Vec::new());
            write_block(
                &walker,
                &mut pool_output,
                plaintext.len() as u64,
                14,
                level,
                &NoProgress,
            )
            .unwrap();
            let mut large_output = Cursor::new(Vec::new());
            write_block_large(
                &walker,
                &mut large_output,
                plaintext.len() as u64,
                14,
                level,
                &NoProgress,
            )
            .unwrap();

            assert_eq!(large_output.into_inner(), pool_output.into_inner());
        }
    }
}
