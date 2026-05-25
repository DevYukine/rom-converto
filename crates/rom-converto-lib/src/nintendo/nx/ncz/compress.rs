//! NCA -> NCZ. Single-threaded reference path used by tests and as the
//! correctness oracle for the parallel block-mode compressor in Phase 4.

use std::io::{Seek, SeekFrom, Write};

use crate::nintendo::nx::constants::{
    DEFAULT_BLOCK_SIZE_EXP, DEFAULT_ZSTD_LEVEL, ENC_AES_CTR, ENC_AES_CTR_EX,
    ENC_AES_CTR_EX_SKIP_LAYER_HASH, ENC_AES_CTR_SKIP_LAYER_HASH, ENC_NONE, MAX_BLOCK_SIZE_EXP,
    MAX_ZSTD_LEVEL, MIN_BLOCK_SIZE_EXP, MIN_ZSTD_LEVEL, NCA_PREFIX_SIZE,
};
use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::models::nca::initial_ctr_for_offset;
use crate::nintendo::nx::ncz::compress_worker::{
    NczBlockWork, default_thread_count, spawn_ncz_pool,
};
use crate::nintendo::nx::ncz::header::{
    NczBlockInfo, NczSectionEntry, write_nczblock, write_nczsectn,
};
use crate::nintendo::nx::walker::NcaWalker;
use crate::util::ProgressReporter;
use crate::util::worker_pool::drive;

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

pub fn nca_to_ncz<W: Write + Seek>(
    walker: &NcaWalker,
    out: &mut W,
    opts: NcaToNczOptions,
    progress: &dyn ProgressReporter,
) -> NxResult<()> {
    let opts = opts.validate()?;

    let nca_offset = walker.nca_offset();
    let nca_size = walker.nca_size();
    let prefix_size = (nca_size as usize).min(NCA_PREFIX_SIZE);

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
    let mut encoder = zstd::stream::write::Encoder::new(out, level)
        .map_err(|e| NxError::ZstdError(format!("zstd encoder init: {e}")))?;
    // Match nsz's `ZstdCompressionParameters.from_level(level,
    // threads=N)` solid pipeline. With `zstdmt`, libzstd splits the
    // input into jobs, compresses them on N worker threads, and
    // serialises the output. This both saturates more cores AND
    // bumps the effective window/job sizing zstd uses, which on
    // multi-GB program NCAs trims a handful of percent off the output
    // compared to single-threaded `from_level` defaults.
    let workers = crate::util::worker_pool::parallelism().min(u32::MAX as usize) as u32;
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
    stream_plaintext(walker, payload_size, |chunk| -> NxResult<()> {
        encoder
            .write_all(chunk)
            .map_err(|e| NxError::ZstdError(format!("zstd write: {e}")))?;
        progress.inc(chunk.len() as u64);
        Ok(())
    })?;
    encoder
        .finish()
        .map_err(|e| NxError::ZstdError(format!("zstd finish: {e}")))?;
    Ok(())
}

fn write_block<W: Write + Seek>(
    walker: &NcaWalker,
    out: &mut W,
    payload_size: u64,
    size_exp: u8,
    level: i32,
    progress: &dyn ProgressReporter,
) -> NxResult<()> {
    let block_size = 1usize << size_exp;
    let num_blocks = (payload_size as usize).div_ceil(block_size);

    let n_threads = default_thread_count().min(num_blocks.max(1));
    let pool = spawn_ncz_pool(level, block_size, n_threads)?;

    let placeholder_info = NczBlockInfo {
        version: 1,
        kind: 0,
        block_size_exp: size_exp,
        decompressed_size: payload_size as i64,
        compressed_block_sizes: vec![0u32; num_blocks],
    };
    let header_start = out.stream_position()?;
    write_nczblock(out, &placeholder_info)?;

    let mut sizes = vec![0u32; num_blocks];
    let mut producer = PlaintextBlockProducer::new(walker, payload_size, block_size);

    let drive_result = drive(
        &pool,
        num_blocks as u64,
        n_threads * 2,
        |_seq| -> NxResult<NczBlockWork> { producer.next_block(progress) },
        |seq, out_block| -> NxResult<()> {
            sizes[seq as usize] = out_block.bytes.len() as u32;
            out.write_all(&out_block.bytes)?;
            Ok(())
        },
    );
    pool.shutdown();
    drive_result?;

    let payload_end = out.stream_position()?;
    let final_info = NczBlockInfo {
        version: 1,
        kind: 0,
        block_size_exp: size_exp,
        decompressed_size: payload_size as i64,
        compressed_block_sizes: sizes,
    };
    out.seek(SeekFrom::Start(header_start))?;
    write_nczblock(out, &final_info)?;
    out.seek(SeekFrom::Start(payload_end))?;
    Ok(())
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

    /// Pulls the next plaintext block out of the walker. The driver
    /// only calls this on the dispatcher thread, so we own the read
    /// state and never duplicate work across threads. Allocates one
    /// `Vec<u8>` per block (handed to the worker pool); reuse would
    /// require the worker to send the buffer back, which complicates
    /// the channel for a tiny win.
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

fn stream_plaintext<F: FnMut(&[u8]) -> NxResult<()>>(
    walker: &NcaWalker,
    payload_size: u64,
    mut sink: F,
) -> NxResult<()> {
    const CHUNK: usize = 4 * 1024 * 1024;
    let mut scratch = vec![0u8; CHUNK];
    let payload_start_in_nca = NCA_PREFIX_SIZE as u64;
    let mut written = 0u64;
    while written < payload_size {
        let take = (CHUNK as u64).min(payload_size - written) as usize;
        let abs_offset = walker.nca_offset() + payload_start_in_nca + written;
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
        // Match that convention so other tools can read our NSZ.
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
    // in non-monotonic fs_entry slots (e.g. slot 0 holding the
    // highest-offset section), so we sort here.
    out.sort_by_key(|e| e.offset);
    Ok(out)
}
