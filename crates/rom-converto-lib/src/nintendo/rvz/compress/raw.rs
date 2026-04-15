//! Raw-region encoder.
//!
//! Handles the non-partition portions of a Wii disc and the entire
//! GameCube body. Chunks flow through the same persistent worker
//! pool as the partition encoder ([`super::partition`]); see
//! [`crate::nintendo::rvz::worker_pool::Pool`] for the pool shape.
//! Each worker's `zstd::bulk::Compressor` is allocated once per
//! thread for the lifetime of the region.
//!
//! The per-chunk math here mirrors Dolphin's
//! `Source/Core/DiscIO/WIABlob.cpp` raw-data branch:
//!
//! * `effective_start = region_offset - (region_offset % BLOCK_TOTAL_SIZE)`
//!   so chunks are indexed from a sector-aligned absolute position.
//! * `bytes_to_read = min(chunk_size, data_offset + data_size - bytes_read)`
//!   so the final chunk of a region is shorter than `chunk_size`
//!   when the region doesn't end on a chunk boundary.
//! * Chunks whose bytes are all zero become an all-zero sentinel
//!   group entry (`data_off4 = 0, data_size = 0`) with no on-disk
//!   footprint.

use super::{CompressedKind, WriteMsg, push_compressed_chunk_via_channel, write_msg_drain_loop};
use crate::nintendo::rvl::constants::WII_SECTOR_SIZE_U64;
use crate::nintendo::rvz::error::{RvzError, RvzResult};
use crate::nintendo::rvz::format::RvzGroup;
use crate::util::worker_pool::{Pool, Worker, drive, parallelism};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Result of compressing one raw-region chunk. `this_size` is the
/// clipped disc range the chunk covers (used for progress
/// accounting); the actual bytes on disk are in `kind`.
pub(super) struct CompressedChunk {
    pub kind: CompressedKind,
    pub this_size: u64,
    pub rvz_packed_size: u32,
}

/// Raw-region compression work item: one chunk's worth of input
/// bytes plus its disc position. The worker compresses and returns
/// a [`CompressedChunk`] tagged with the effective clipped size.
pub(super) struct RawWork {
    data: Vec<u8>,
    this_size: u64,
    data_offset: u64,
}

/// Per-thread raw-region encoder state. The persistent
/// `zstd::bulk::Compressor` is allocated once per thread for the
/// lifetime of the [`Pool`], mirroring Dolphin's
/// `MultithreadedCompressor` pattern.
pub(super) struct RawCompressWorker {
    compressor: zstd::bulk::Compressor<'static>,
}

impl Worker<RawWork, CompressedChunk, RvzError> for RawCompressWorker {
    fn process(&mut self, work: RawWork) -> RvzResult<CompressedChunk> {
        compress_one_chunk_with(
            &mut self.compressor,
            work.data,
            work.this_size,
            work.data_offset,
        )
    }
}

pub(super) fn make_raw_compress_workers(
    n_threads: usize,
    compression_level: i32,
) -> RvzResult<Vec<RawCompressWorker>> {
    (0..n_threads)
        .map(|_| -> RvzResult<RawCompressWorker> {
            Ok(RawCompressWorker {
                compressor: zstd::bulk::Compressor::new(compression_level)
                    .map_err(|e| RvzError::Custom(format!("zstd init: {e}")))?,
            })
        })
        .collect()
}

/// Encode one raw region by sharding its chunks across the
/// caller-provided persistent worker pool. [`drive`] handles
/// submit/recv/reorder so this function only supplies the
/// per-chunk `produce` closure (disc read + sector clip) and
/// `consume` closure (write compressed chunk + append group
/// entry). The pool itself is shared across all raw regions in
/// one compress invocation so each compress run only pays the
/// pool-spawn cost once.
#[allow(clippy::too_many_arguments)]
pub(super) fn parallel_encode_raw_region(
    pool: &Pool<RawWork, CompressedChunk, RvzError>,
    reader: &mut BufReader<std::fs::File>,
    writer: &mut BufWriter<std::fs::File>,
    writer_pos: &mut u64,
    region_offset: u64,
    region_size: u64,
    iso_size: u64,
    chunk_size: u32,
    groups: &mut Vec<RvzGroup>,
    bytes_done: &Arc<AtomicU64>,
) -> RvzResult<()> {
    let chunk_size_u64 = chunk_size as u64;

    // Port of Dolphin's `data_offset -= data_offset % BLOCK_TOTAL_SIZE`
    // from `WIABlob.cpp`: align the effective read-start DOWN to a
    // `VolumeWii::BLOCK_TOTAL_SIZE` boundary so chunks are indexed
    // from an absolute-disc-position the decoder can reproduce.
    // For GameCube's first region (`region_offset = 0x80`),
    // effective start becomes 0 and chunk 0 contains disc bytes
    // 0..chunk_size (including the disc header, which is also
    // stored in `wia_disc_t.dhead`; the decoder skips the first
    // `region_offset` bytes of chunk 0).
    let effective_start = region_offset - (region_offset % WII_SECTOR_SIZE_U64);
    let effective_size = region_size + (region_offset - effective_start);
    let total_chunks = effective_size.div_ceil(chunk_size_u64);
    let region_end_abs = region_offset + region_size;

    reader.seek(SeekFrom::Start(effective_start))?;

    // In-flight cap so the dispatcher doesn't enqueue the entire
    // disc while workers are still chewing on earlier chunks. Two
    // units of work per thread keeps all workers busy without
    // wasting memory.
    let max_in_flight = parallelism() * 2;

    // Overlap writes with dispatch via a dedicated writer
    // thread. The dispatcher does read, submit, recv, and
    // forward-via-channel; the writer thread drains the
    // channel and calls `writer.write_all`. Without this
    // split, the dispatcher stalls on per-chunk writes
    // between reads. Reads stay on the dispatcher thread:
    // both a dedicated reader thread and an mmap'd input
    // measured worse on Windows than plain
    // `BufReader::read_exact`.
    let mut local_writer_pos = *writer_pos;
    let (write_tx, write_rx) = std::sync::mpsc::sync_channel::<WriteMsg>(max_in_flight * 2);

    let scope_result: RvzResult<()> = std::thread::scope(|s| {
        let writer_slot: &mut BufWriter<std::fs::File> = writer;
        let writer_handle =
            s.spawn(move || -> RvzResult<()> { write_msg_drain_loop(writer_slot, write_rx) });

        let drive_result = drive(
            pool,
            total_chunks,
            max_in_flight,
            // produce: read the next chunk from the ISO in
            // submission order. Port of Dolphin's
            // `bytes_to_read = min(chunk_size, data_offset +
            // data_size - bytes_read)`.
            //
            // The input buffer is allocated uninitialised
            // rather than zero-filled via `vec![0u8; n]`.
            // `alloc_zeroed` pays for a 128 KiB `memset` on
            // every chunk even though `reader.read_exact`
            // immediately overwrites the bytes. The only
            // reason we'd need the buffer zeroed is the final
            // chunk of a region, which may be short; in that
            // case we zero just the tail past `bytes_to_read`.
            |chunk_idx| -> RvzResult<RawWork> {
                let chunk_abs_start = effective_start + chunk_idx * chunk_size_u64;
                let effective_remaining =
                    (effective_start + effective_size).saturating_sub(chunk_abs_start);
                let bytes_to_compress = effective_remaining.min(chunk_size_u64) as usize;
                let bytes_available_on_disc =
                    iso_size.saturating_sub(chunk_abs_start).min(chunk_size_u64) as usize;
                let bytes_to_read = bytes_to_compress.min(bytes_available_on_disc);

                let mut data = Vec::with_capacity(bytes_to_compress);
                // SAFETY: `Vec::with_capacity(n)` gives us at
                // least `n` bytes of uninitialised memory.
                // `u8` has no drop glue and no validity
                // invariants beyond "initialised before read",
                // so extending `len` past uninit bytes is
                // sound as long as no reader observes them
                // before we write. We then overwrite
                // `[..bytes_to_read]` via `read_exact` (which
                // only writes its destination;
                // `BufReader<File>` never reads uninit bytes)
                // and initialise `[bytes_to_read..bytes_to_compress]`
                // with zeros on the short-tail path. On the
                // error path the `Vec` is dropped without any
                // byte ever being read, which is fine for
                // `u8` (no drop). Clippy's lint is overly
                // conservative for this element type.
                #[allow(clippy::uninit_vec)]
                unsafe {
                    data.set_len(bytes_to_compress);
                }
                if bytes_to_read > 0 {
                    reader.read_exact(&mut data[..bytes_to_read])?;
                }
                if bytes_to_read < bytes_to_compress {
                    data[bytes_to_read..].fill(0);
                }

                let chunk_abs_end = chunk_abs_start + bytes_to_compress as u64;
                let clip_start = chunk_abs_start.max(region_offset);
                let clip_end = chunk_abs_end.min(region_end_abs);
                let this_size = clip_end.saturating_sub(clip_start);
                Ok(RawWork {
                    data,
                    this_size,
                    data_offset: chunk_abs_start,
                })
            },
            // consume: forward the compressed chunk to the
            // writer thread and update `groups` + the tracked
            // writer position on the dispatcher side. The heavy
            // `writer.write_all` memcpy happens on the writer
            // thread in parallel with the next produce call.
            |_seq, chunk| -> RvzResult<()> {
                let this_size = chunk.this_size;
                push_compressed_chunk_via_channel(
                    &write_tx,
                    &mut local_writer_pos,
                    groups,
                    chunk.kind,
                    chunk.rvz_packed_size,
                )?;
                bytes_done.fetch_add(this_size, Ordering::Relaxed);
                Ok(())
            },
        );

        drop(write_tx);
        let writer_result = writer_handle
            .join()
            .unwrap_or_else(|_| Err(RvzError::Custom("writer thread panicked".into())));
        drive_result?;
        writer_result
    });

    *writer_pos = local_writer_pos;
    scope_result
}

/// Check whether every byte in `data` is zero, using a
/// u64-chunked scan that LLVM reliably vectorises with SSE2/AVX2
/// on x86_64 and NEON on ARM64. On a warm cache this runs at
/// ~30-50 GB/s, an order of magnitude faster than the
/// byte-by-byte `iter().all(|&b| b == 0)` that it replaces. Over
/// a 4 GB Wii disc the saving adds up to the better part of a
/// second of pure scanning overhead.
///
/// `align_to::<u64>` handles the leading/trailing unaligned
/// tails for free; the middle `&[u64]` slice is what actually
/// gets vectorised. Interpreting bytes as `u64` is fine here
/// because we only compare against zero, which is invariant
/// under endian.
#[inline]
fn is_all_zero(data: &[u8]) -> bool {
    // SAFETY: `slice::align_to` is safe; the `unsafe` is a
    // historical marker on the stdlib API. The returned slices
    // together cover `data` exactly.
    let (prefix, middle, suffix) = unsafe { data.align_to::<u64>() };
    prefix.iter().all(|&b| b == 0)
        && middle.iter().all(|&w| w == 0)
        && suffix.iter().all(|&b| b == 0)
}

/// Compress one chunk using a persistent `Compressor` borrowed from
/// the worker pool. The zstd context is reused across calls, so the
/// expensive `ZSTD_createCCtx` happens once per worker thread rather
/// than once per chunk.
fn compress_one_chunk_with(
    compressor: &mut zstd::bulk::Compressor<'_>,
    data: Vec<u8>,
    this_size: u64,
    data_offset: u64,
) -> RvzResult<CompressedChunk> {
    if is_all_zero(&data) {
        return Ok(CompressedChunk {
            kind: CompressedKind::AllZero,
            this_size,
            rvz_packed_size: 0,
        });
    }
    // Try RVZ packing first. If the scanner finds LFG junk runs the
    // packed bytes replace the raw chunk going into zstd and the
    // decoder knows to invoke `pack_decode` via the non-zero
    // `rvz_packed_size`. Otherwise fall through to zstd on raw
    // bytes. Mirrors Dolphin's `RVZPack(data.data(), ...,
    // parameters.data_offset, ...)` on the raw-data branch in
    // `WIABlob.cpp`.
    let (bytes_for_zstd, rvz_packed_size) =
        match crate::nintendo::rvz::packing::pack_encode(&data, data_offset) {
            Some(packed) => {
                let pack_len = packed.len() as u32;
                (packed, pack_len)
            }
            None => (data, 0u32),
        };
    let compressed = compressor
        .compress(&bytes_for_zstd)
        .map_err(|e| RvzError::Custom(format!("zstd compress: {e}")))?;
    let kind = if compressed.len() < bytes_for_zstd.len() {
        CompressedKind::Compressed(compressed)
    } else {
        CompressedKind::Raw(bytes_for_zstd)
    };
    Ok(CompressedChunk {
        kind,
        this_size,
        rvz_packed_size,
    })
}
