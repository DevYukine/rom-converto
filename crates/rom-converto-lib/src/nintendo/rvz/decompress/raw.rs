//! Raw-region decompressor.
//!
//! Decodes the non-partition chunks of a disc. Workers own a
//! persistent `zstd::bulk::Decompressor` and read compressed chunk
//! bytes from a shared `Arc<File>` via
//! [`super::file_read_exact_at`]. Positional reads let N workers
//! satisfy chunks concurrently without contending for a single
//! seek cursor. See the parent module docs for the overall
//! pipeline shape.
//!
//! The per-chunk math mirrors the encoder's raw path (see
//! [`super::super::compress::raw`]): chunks are indexed from
//! `effective_start = region_offset - (region_offset %
//! BLOCK_TOTAL_SIZE)`, the last chunk of a region may be shorter
//! than `chunk_size`, and all-zero sentinel groups (`data_size =
//! 0`) are synthesised from zeros without issuing I/O.
//!
use crate::nintendo::rvl::constants::WII_SECTOR_SIZE_U64;
use crate::nintendo::rvz::error::{RvzError, RvzResult};
use crate::nintendo::rvz::format::{RvzGroup, WiaRawData};
use crate::util::pread::file_read_exact_at;
use crate::util::worker_pool::{Pool, Worker, drive, parallelism};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Per-chunk work item. Built on the dispatcher thread. Computing
/// the write range needs `region.raw_data_off` plus the ISO file
/// size, which are cheap to capture but live on the main thread.
#[derive(Clone)]
struct RawDecompressWork {
    // File offset + compressed size of the chunk in the RVZ file.
    // A `data_size` of 0 is Dolphin's all-zero sentinel; the
    // worker synthesises zeros instead of issuing I/O.
    data_off: u64,
    data_size: u32,
    is_compressed: bool,
    rvz_packed_size: u32,
    // Fully decompressed chunk length in bytes. The worker sizes
    // its output buffer to this; the last chunk of a region may be
    // smaller because the encoder only compressed the remaining
    // bytes.
    chunk_bytes: usize,
    // `pack_decode`'s `data_offset` parameter: absolute disc
    // position of the chunk's first byte.
    chunk_abs_start: u64,
    // Output disc range + slice offset within the decoded chunk.
    // Written sequentially in submission order by the consume
    // closure.
    write_start: u64,
    write_len: usize,
    chunk_slice_offset: usize,
}

struct RawDecompressOut {
    // Owned decoded slice ready to write. For the zero-sentinel
    // case this is a fresh zero buffer; otherwise it's an owned
    // copy of the worker's decoded bytes so the worker's scratch
    // can be reused for the next item without racing the consumer.
    decoded: Box<[u8]>,
    write_start: u64,
    write_len: usize,
    chunk_slice_offset: usize,
}

/// Per-thread raw-region decompressor state. Holds a persistent
/// `zstd::bulk::Decompressor` and a pair of `Vec<u8>` scratch
/// buffers reused across every chunk. Same trick the encoder
/// pool uses with `Compressor`, avoids one `ZSTD_createDCtx` per
/// chunk.
struct RawDecompressWorker {
    decompressor: zstd::bulk::Decompressor<'static>,
    file: Arc<std::fs::File>,
    scratch_in: Vec<u8>,
    scratch_out: Vec<u8>,
}

impl Worker<RawDecompressWork, RawDecompressOut, RvzError> for RawDecompressWorker {
    fn process(&mut self, work: RawDecompressWork) -> RvzResult<RawDecompressOut> {
        // All-zero sentinel: no I/O, no zstd, no pack_decode.
        // Hand back a zero slice of the requested write length.
        // Matches the sequential decoder's fast path.
        if work.data_size == 0 {
            let zeros = vec![0u8; work.write_len].into_boxed_slice();
            return Ok(RawDecompressOut {
                decoded: zeros,
                write_start: work.write_start,
                write_len: work.write_len,
                chunk_slice_offset: 0,
            });
        }

        // Read the compressed chunk via positional I/O so multiple
        // workers can safely share one `Arc<File>` without seek
        // contention. Grows `scratch_in` up to the largest
        // compressed chunk seen so far; never shrinks.
        self.scratch_in.resize(work.data_size as usize, 0);
        file_read_exact_at(&self.file, &mut self.scratch_in, work.data_off)?;

        // Stage 1: zstd decompress (if compressed) or verbatim
        // copy into the scratch output buffer.
        let stage1_len: usize = if work.is_compressed {
            if self.scratch_out.len() < work.chunk_bytes {
                self.scratch_out.resize(work.chunk_bytes, 0);
            }
            self.decompressor
                .decompress_to_buffer(&self.scratch_in, &mut self.scratch_out)?
        } else {
            if self.scratch_out.len() < self.scratch_in.len() {
                self.scratch_out.resize(self.scratch_in.len(), 0);
            }
            self.scratch_out[..self.scratch_in.len()].copy_from_slice(&self.scratch_in);
            self.scratch_in.len()
        };

        // Stage 2: RVZ unpack (if packed). `pack_decode` allocates
        // a fresh Vec; we move it into the returned Box so the
        // worker's `scratch_out` can be reused immediately.
        let decoded: Box<[u8]> = if work.rvz_packed_size != 0 {
            let unpacked = crate::nintendo::rvz::packing::pack_decode(
                &self.scratch_out[..stage1_len],
                work.chunk_abs_start,
            )?;
            unpacked.into_boxed_slice()
        } else {
            // Copy the stage-1 bytes into an owned buffer. One
            // allocation per chunk, cheaper than sharing `Arc`
            // around a scratch buffer and fighting interior
            // mutability across workers.
            self.scratch_out[..stage1_len].to_vec().into_boxed_slice()
        };

        if decoded.len() < work.chunk_slice_offset + work.write_len {
            return Err(RvzError::DecompressedSizeMismatch {
                expected: (work.chunk_slice_offset + work.write_len) as u64,
                actual: decoded.len() as u64,
            });
        }

        Ok(RawDecompressOut {
            decoded,
            write_start: work.write_start,
            write_len: work.write_len,
            chunk_slice_offset: work.chunk_slice_offset,
        })
    }
}

fn make_raw_decompress_workers(
    n_threads: usize,
    file: &Arc<std::fs::File>,
) -> RvzResult<Vec<RawDecompressWorker>> {
    (0..n_threads)
        .map(|_| -> RvzResult<RawDecompressWorker> {
            Ok(RawDecompressWorker {
                decompressor: zstd::bulk::Decompressor::new()
                    .map_err(|e| RvzError::Custom(format!("zstd dctx init: {e}")))?,
                file: Arc::clone(file),
                scratch_in: Vec::new(),
                scratch_out: Vec::new(),
            })
        })
        .collect()
}

/// Build the work-item list for one raw-data region. Mirrors the
/// sequential loop's math exactly (effective_start alignment, per-
/// chunk clip, last-chunk trim) so the decoded output is byte-
/// identical to the pre-parallel path.
fn build_raw_region_work_items(
    region: &WiaRawData,
    groups: &[RvzGroup],
    chunk_size: u64,
    iso_file_size: u64,
) -> Vec<RawDecompressWork> {
    let effective_start = region.raw_data_off - (region.raw_data_off % WII_SECTOR_SIZE_U64);
    let region_end = region.raw_data_off + region.raw_data_size;
    let mut items = Vec::with_capacity(region.n_groups as usize);
    for i in 0..region.n_groups {
        let group = &groups[(region.group_index + i) as usize];
        let chunk_abs_start = effective_start + i as u64 * chunk_size;
        let chunk_abs_end = (chunk_abs_start + chunk_size).min(region_end);
        let write_start = chunk_abs_start.max(region.raw_data_off);
        let write_end = chunk_abs_end.min(region_end).min(iso_file_size);
        if write_start >= write_end {
            continue;
        }
        let chunk_slice_offset = (write_start - chunk_abs_start) as usize;
        let write_len = (write_end - write_start) as usize;
        let chunk_bytes = (chunk_abs_end - chunk_abs_start) as usize;
        items.push(RawDecompressWork {
            data_off: (group.data_off4 as u64) << 2,
            data_size: group.compressed_size(),
            is_compressed: group.is_compressed(),
            rvz_packed_size: group.rvz_packed_size,
            chunk_bytes,
            chunk_abs_start,
            write_start,
            write_len,
            chunk_slice_offset: if group.data_size == 0 {
                0
            } else {
                chunk_slice_offset
            },
        });
    }
    items
}

/// Parallel raw-region decoder. Spawns a worker [`Pool`] seeded
/// with persistent `zstd::bulk::Decompressor`s, builds the full
/// work list for the region, and pumps it via [`drive`]. Output is
/// written in submission order by the consume closure so
/// `writer_pos` tracking stays valid.
#[allow(clippy::too_many_arguments)]
pub(super) fn parallel_decompress_raw_region(
    region: &WiaRawData,
    groups: &[RvzGroup],
    chunk_size: u64,
    iso_file_size: u64,
    file: &Arc<std::fs::File>,
    writer: &mut BufWriter<std::fs::File>,
    writer_pos: &mut u64,
    bytes_done: &Arc<AtomicU64>,
) -> RvzResult<()> {
    let items = build_raw_region_work_items(region, groups, chunk_size, iso_file_size);
    if items.is_empty() {
        return Ok(());
    }

    let n_threads = parallelism();
    let workers = make_raw_decompress_workers(n_threads, file)?;
    let pool: Pool<RawDecompressWork, RawDecompressOut, RvzError> = Pool::spawn(workers);
    let max_in_flight = n_threads * 2;

    let total = items.len() as u64;
    let mut items_iter = items.into_iter();

    let result = drive(
        &pool,
        total,
        max_in_flight,
        |_seq| -> RvzResult<RawDecompressWork> {
            // `drive` calls produce in strict submission order, so
            // a single `Iterator::next` on the work-item vec
            // yields the right item without any per-call lookup.
            items_iter
                .next()
                .ok_or_else(|| RvzError::Custom("raw-region work iterator exhausted".into()))
        },
        |_seq, out| -> RvzResult<()> {
            let slice =
                &out.decoded[out.chunk_slice_offset..out.chunk_slice_offset + out.write_len];
            if out.write_start != *writer_pos {
                writer.seek(SeekFrom::Start(out.write_start))?;
                *writer_pos = out.write_start;
            }
            writer.write_all(slice)?;
            *writer_pos += out.write_len as u64;
            bytes_done.fetch_add(out.write_len as u64, Ordering::Relaxed);
            Ok(())
        },
    );

    pool.shutdown();
    result
}
