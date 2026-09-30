//! Raw-region decompressor.
//!
//! Decodes the non-partition chunks of a disc. Workers own a
//! persistent `zstd::bulk::Decompressor` and read compressed chunk
//! bytes from a shared `Arc<File>` via
//! [`crate::util::pread::file_read_exact_at`]. Positional reads let N workers
//! satisfy chunks concurrently without contending for a single
//! seek cursor. See the parent module docs for the overall
//! pipeline shape.
//!
//! The per-chunk math mirrors the encoder's raw path (see
//! [`super::super::compress::raw`]): chunks are indexed from
//! `effective_start = region_offset - (region_offset %
//! WII_SECTOR_SIZE)`, the last chunk of a region may be shorter
//! than `chunk_size`, and all-zero sentinel groups (`data_size =
//! 0`) are synthesised from zeros without issuing I/O.
//!
use super::sink::{DiscSink, UsageFilter};
use crate::nintendo::disc::rvz::error::{RvzError, RvzResult};
use crate::nintendo::disc::rvz::format::{RvzGroup, WiaRawData};
use crate::nintendo::rvl::constants::WII_SECTOR_SIZE_U64;
use crate::util::positional_reader::PositionalReader;
use crate::util::pread::file_read_exact_at;
use crate::util::worker_pool::{Budget, Pool, Worker, drive, parallelism};
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Per-chunk work item. Built on the dispatcher thread. Computing
/// the write range needs `region.raw_data_off` plus the ISO file
/// size, which are cheap to capture but live on the main thread.
#[derive(Clone)]
pub(crate) struct RawDecompressWork {
    // File offset + compressed size of the chunk in the RVZ file.
    // A `data_size` of 0 is the format's all-zero sentinel; the
    // worker synthesises zeros instead of issuing I/O.
    pub(crate) data_off: u64,
    pub(crate) data_size: u32,
    pub(crate) is_compressed: bool,
    pub(crate) rvz_packed_size: u32,
    // Fully decompressed chunk length in bytes. The worker sizes
    // its output buffer to this; the last chunk of a region may be
    // smaller because the encoder only compressed the remaining
    // bytes.
    pub(crate) chunk_bytes: usize,
    // `pack_decode`'s `data_offset` parameter: absolute disc
    // position of the chunk's first byte.
    pub(crate) chunk_abs_start: u64,
    pub(crate) write_start: u64,
    pub(crate) write_len: usize,
    pub(crate) chunk_slice_offset: usize,
}

pub(crate) struct RawDecompressOut {
    pub(crate) decoded: Box<[u8]>,
    pub(crate) write_start: u64,
    pub(crate) write_len: usize,
    pub(crate) chunk_slice_offset: usize,
}

pub(crate) struct RawDecompressWorker {
    decompressor: zstd::bulk::Decompressor<'static>,
    file: Arc<std::fs::File>,
    scratch_in: Vec<u8>,
    scratch_out: Vec<u8>,
}

/// The descriptor bounds a group with stored bytes must satisfy. Shared
/// by the bulk worker, the region streaming paths and the random-access
/// reader so all three verdicts agree: the stored bytes fit zstd's
/// worst case for the chunk (a genuine group decompresses to at most
/// the chunk plus one packing record per 0x8000-byte block, each record
/// costing 76 bytes, and the stored form is bounded by zstd's
/// worst-case expansion of that), a packed group's declared record
/// stream fits the same worst case, and a stored packed group is
/// exactly its record stream. Non-packed groups are held to the stored
/// bound only.
pub(crate) fn check_group_bounds(work: &RawDecompressWork) -> RvzResult<()> {
    let stage1_cap =
        work.chunk_bytes + 76 * work.chunk_bytes.div_ceil(WII_SECTOR_SIZE_U64 as usize);
    let stored_cap = if work.is_compressed {
        zstd::zstd_safe::compress_bound(stage1_cap)
    } else {
        stage1_cap
    };
    if u64::from(work.data_size) > stored_cap as u64 {
        return Err(RvzError::Custom(format!(
            "group stores {} bytes, more than the {}-byte bound for a {}-byte chunk",
            work.data_size, stored_cap, work.chunk_bytes
        )));
    }
    if work.rvz_packed_size != 0 && work.rvz_packed_size as usize > stage1_cap {
        return Err(RvzError::DecompressedSizeMismatch {
            expected: stage1_cap as u64,
            actual: u64::from(work.rvz_packed_size),
        });
    }
    if !work.is_compressed && work.rvz_packed_size != 0 && work.data_size != work.rvz_packed_size {
        return Err(RvzError::Custom(format!(
            "stored packed chunk holds {} bytes, but rvz_packed_size declares {}",
            work.data_size, work.rvz_packed_size
        )));
    }
    Ok(())
}

impl Worker<RawDecompressWork, RawDecompressOut, RvzError> for RawDecompressWorker {
    fn process(&mut self, work: RawDecompressWork) -> RvzResult<RawDecompressOut> {
        // data_size == 0 is the format's all-zero sentinel; synthesise
        // chunk_bytes zeros without I/O. The real chunk_slice_offset is
        // kept: the first chunk of an unaligned region (e.g. a region
        // starting at raw_data_off 0x80) covers padding before the
        // region start, and both the sequential consumer and the
        // random-access reader slice the decoded chunk at
        // chunk_slice_offset, so a write_len-sized buffer with a zeroed
        // offset would stall every read inside such a chunk.
        if work.data_size == 0 {
            let zeros = vec![0u8; work.chunk_bytes].into_boxed_slice();
            return Ok(RawDecompressOut {
                decoded: zeros,
                write_start: work.write_start,
                write_len: work.write_len,
                chunk_slice_offset: work.chunk_slice_offset,
            });
        }

        check_group_bounds(&work)?;

        self.scratch_in.resize(work.data_size as usize, 0);
        file_read_exact_at(&self.file, &mut self.scratch_in, work.data_off)?;

        let stage1_len: usize = if work.is_compressed {
            // Stage 1 is the zstd-decompressed byte stream: the plain
            // chunk, or the RVZ packing record stream when the chunk is
            // packed. A corrupt `rvz_packed_size` must not silently
            // truncate the zstd destination, so size from the declared
            // stream (already bounded by `check_group_bounds`).
            let target = if work.rvz_packed_size != 0 {
                (work.rvz_packed_size as usize).max(work.chunk_bytes)
            } else {
                work.chunk_bytes
            };
            if self.scratch_out.len() < target {
                self.scratch_out.resize(target, 0);
            }
            // Decode into exactly `target`: reused scratch from a larger
            // chunk must not let an oversized frame silently succeed.
            self.decompressor
                .decompress_to_buffer(&self.scratch_in, &mut self.scratch_out[..target])?
        } else {
            if self.scratch_out.len() < self.scratch_in.len() {
                self.scratch_out.resize(self.scratch_in.len(), 0);
            }
            self.scratch_out[..self.scratch_in.len()].copy_from_slice(&self.scratch_in);
            self.scratch_in.len()
        };

        let decoded: Box<[u8]> = if work.rvz_packed_size != 0 {
            let mut packed_out = vec![0u8; work.chunk_bytes];
            let (packed_len, input_len) = crate::nintendo::disc::rvz::packing::pack_decode_reader(
                &mut std::io::Cursor::new(&self.scratch_out[..stage1_len]),
                work.chunk_abs_start,
                &mut packed_out,
            )?;
            // The record stream is exactly `rvz_packed_size` bytes: the
            // streaming path cuts the decoder there. A bulk decode that
            // consumes a different count sees records the streaming
            // path never would, so the two paths must reject it alike.
            if input_len != work.rvz_packed_size as usize {
                return Err(RvzError::Custom(format!(
                    "packed record stream consumed {} bytes, but rvz_packed_size declares {}",
                    input_len, work.rvz_packed_size
                )));
            }
            packed_out.truncate(packed_len);
            packed_out.into_boxed_slice()
        } else {
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
    (0..n_threads).map(|_| make_one_raw_worker(file)).collect()
}

pub(crate) fn make_one_raw_worker(file: &Arc<std::fs::File>) -> RvzResult<RawDecompressWorker> {
    Ok(RawDecompressWorker {
        decompressor: zstd::bulk::Decompressor::new()
            .map_err(|e| RvzError::Custom(format!("zstd dctx init: {e}")))?,
        file: Arc::clone(file),
        scratch_in: Vec::new(),
        scratch_out: Vec::new(),
    })
}

/// Work item for chunk `i` of `region`, where `group` is
/// `groups[region.group_index + i]`. Returns `None` when the chunk
/// holds no writable bytes (padding before `raw_data_off` or past
/// `iso_file_size`). The per-chunk math mirrors the encoder's raw path
/// exactly (effective_start alignment, per-chunk clip, last-chunk
/// trim), so the decoded output is byte-identical to the sequential
/// decoder.
fn raw_chunk_work_i(
    region: &WiaRawData,
    group: &RvzGroup,
    i: u32,
    chunk_size: u64,
    iso_file_size: u64,
) -> Option<RawDecompressWork> {
    let effective_start = region.raw_data_off - (region.raw_data_off % WII_SECTOR_SIZE_U64);
    let region_end = region.raw_data_off + region.raw_data_size;
    let chunk_abs_start = effective_start + i as u64 * chunk_size;
    let chunk_abs_end = (chunk_abs_start + chunk_size).min(region_end);
    let write_start = chunk_abs_start.max(region.raw_data_off);
    let write_end = chunk_abs_end.min(region_end).min(iso_file_size);
    if write_start >= write_end {
        return None;
    }
    let chunk_slice_offset = (write_start - chunk_abs_start) as usize;
    let write_len = (write_end - write_start) as usize;
    Some(RawDecompressWork {
        data_off: (group.data_off4 as u64) << 2,
        data_size: group.compressed_size(),
        is_compressed: group.is_compressed(),
        rvz_packed_size: group.rvz_packed_size,
        chunk_bytes: (chunk_abs_end - chunk_abs_start) as usize,
        chunk_abs_start,
        write_start,
        write_len,
        chunk_slice_offset,
    })
}

/// Build the work-item list for one raw-data region.
pub(crate) fn build_raw_region_work_items(
    region: &WiaRawData,
    groups: &[RvzGroup],
    chunk_size: u64,
    iso_file_size: u64,
    filter: Option<&UsageFilter>,
) -> Vec<RawDecompressWork> {
    let mut items = Vec::with_capacity(region.n_groups as usize);
    for i in 0..region.n_groups {
        let group = &groups[(region.group_index + i) as usize];
        let Some(work) = raw_chunk_work_i(region, group, i, chunk_size, iso_file_size) else {
            continue;
        };
        // When scrubbing for WBFS, a chunk landing only in unused blocks
        // is never read or decompressed.
        if let Some(filter) = filter
            && !filter.keeps(work.write_start, work.write_len as u64)
        {
            continue;
        }
        items.push(work);
    }
    items
}

fn stream_raw_chunk(
    work: &RawDecompressWork,
    file: &std::fs::File,
    sink: &mut dyn DiscSink,
    bytes_done: &AtomicU64,
    buffer: &mut [u8],
) -> RvzResult<()> {
    // The same descriptor bounds the bulk worker applies to every chunk
    // with stored data, so a group it rejects never streams here.
    if work.data_size != 0 {
        check_group_bounds(work)?;
    }
    let stored = PositionalReader::new(file, work.data_off, u64::from(work.data_size));
    let mut source: Box<dyn Read> = if work.is_compressed && work.data_size != 0 {
        Box::new(zstd::stream::read::Decoder::new(stored)?)
    } else {
        Box::new(stored)
    };
    let slice_end = work.chunk_slice_offset + work.write_len;
    let mut decoded = 0usize;
    loop {
        // A read may overshoot `slice_end` (the zstd decoder returns as
        // much as fits the request), so exit on `>=`; an exact `==` would
        // loop on and issue a zero-length read.
        if decoded >= slice_end {
            break;
        }
        let count = if work.data_size == 0 {
            (work.chunk_bytes - decoded).min(buffer.len())
        } else {
            let read_len = (work.chunk_bytes - decoded).min(buffer.len());
            source.read(&mut buffer[..read_len])?
        };
        if count == 0 {
            break;
        }
        if work.data_size == 0 {
            buffer[..count].fill(0);
        }
        let end = decoded + count;
        let write_start = decoded.max(work.chunk_slice_offset);
        let write_end = end.min(slice_end);
        if write_start < write_end {
            let offset = work.write_start + (write_start - work.chunk_slice_offset) as u64;
            sink.write_at(offset, &buffer[write_start - decoded..write_end - decoded])?;
            bytes_done.fetch_add((write_end - write_start) as u64, Ordering::Relaxed);
        }
        decoded = end;
    }
    if decoded < slice_end {
        return Err(RvzError::DecompressedSizeMismatch {
            expected: slice_end as u64,
            actual: decoded as u64,
        });
    }
    // The requested slice is complete, but the chunk window may not be
    // (a sliced work item stops reading early): drain the compressed frame
    // in buffer-sized reads up to `chunk_bytes` (discarding, like the bulk
    // worker's fixed-size scratch), then probe one extra byte, so a frame
    // decoding past `chunk_bytes` is rejected instead of silently ignored.
    // Plain stored groups tolerate trailing bytes.
    if work.is_compressed && work.data_size != 0 {
        while decoded < work.chunk_bytes {
            let read_len = (work.chunk_bytes - decoded).min(buffer.len());
            let count = source.read(&mut buffer[..read_len])?;
            if count == 0 {
                break;
            }
            decoded += count;
        }
        if decoded == work.chunk_bytes {
            let mut extra = [0u8; 1];
            let count = source.read(&mut extra)?;
            if count != 0 {
                return Err(RvzError::DecompressedSizeMismatch {
                    expected: work.chunk_bytes as u64,
                    actual: decoded as u64 + count as u64,
                });
            }
        }
    }
    Ok(())
}

/// Stream one packed chunk through the bounded window: decode windows
/// through a resumable [`PackedDecoder`], discard output before
/// `chunk_slice_offset`, and write the rest. Keeps memory bounded even
/// when a raw-only RVZ declares an uncapped `chunk_size`.
fn stream_packed_chunk(
    work: &RawDecompressWork,
    file: &std::fs::File,
    sink: &mut dyn DiscSink,
    bytes_done: &AtomicU64,
    buffer: &mut [u8],
) -> RvzResult<()> {
    let stored = PositionalReader::new(file, work.data_off, u64::from(work.data_size));
    // The same descriptor bounds the bulk worker applies, so a chunk
    // rejected there never decodes here.
    check_group_bounds(work)?;
    let mut source: Box<dyn Read> = if work.is_compressed {
        Box::new(zstd::stream::read::Decoder::new(stored)?)
    } else {
        Box::new(std::io::BufReader::with_capacity(1024 * 1024, stored))
    };
    // Cut the record walk at the declared stream length so a packed
    // chunk never reads records past `rvz_packed_size`.
    let mut limited = (&mut *source).take(u64::from(work.rvz_packed_size));
    let mut decoder = crate::nintendo::disc::rvz::packing::PackedDecoder::new(
        &mut limited,
        work.chunk_abs_start,
        work.chunk_bytes,
    );
    let slice_end = work.chunk_slice_offset + work.write_len;
    let mut decoded = 0usize;
    while decoded < slice_end {
        let read_len = (work.chunk_bytes - decoded).min(buffer.len());
        let count = decoder.read(&mut buffer[..read_len])?;
        if count == 0 {
            break;
        }
        let end = decoded + count;
        let write_start = decoded.max(work.chunk_slice_offset);
        let write_end = end.min(slice_end);
        if write_start < write_end {
            let offset = work.write_start + (write_start - work.chunk_slice_offset) as u64;
            sink.write_at(offset, &buffer[write_start - decoded..write_end - decoded])?;
            bytes_done.fetch_add((write_end - write_start) as u64, Ordering::Relaxed);
        }
        decoded = end;
    }
    if decoded < slice_end {
        return Err(RvzError::DecompressedSizeMismatch {
            expected: slice_end as u64,
            actual: decoded as u64,
        });
    }
    // Drain the remaining records so a truncated trailing record is
    // rejected even though the requested window is already complete.
    let mut drained: [u8; 0] = [];
    decoder.read(&mut drained)?;
    // The record stream must fill the declaration exactly: a stream
    // that runs short disagrees with the bulk path's consumed-count
    // check.
    if limited.limit() != 0 {
        return Err(RvzError::Custom(format!(
            "packed record stream ends {} bytes short of the declared {}",
            limited.limit(),
            work.rvz_packed_size
        )));
    }
    // Probe one byte past the declared stream end: the zstd frame or
    // the stored bytes must end exactly at `rvz_packed_size`, matching
    // the bulk path's consumed-count check. Re-arming the cut by one
    // byte keeps the probe bounded.
    limited.set_limit(1);
    let mut probe = [0u8; 1];
    let extra = limited.read(&mut probe)?;
    if extra != 0 {
        return Err(RvzError::Custom(format!(
            "packed record stream continues past the declared {} bytes",
            work.rvz_packed_size
        )));
    }
    Ok(())
}

/// Build the single work item covering the chunk that contains disc
/// offset `pos`, plus its group-table index (the decode cache key).
/// Read-side counterpart of [`build_raw_region_work_items`] so one
/// random-access read doesn't build every chunk of the region.
pub(crate) fn raw_chunk_work_at(
    region: &WiaRawData,
    groups: &[RvzGroup],
    chunk_size: u64,
    iso_file_size: u64,
    pos: u64,
) -> Option<(u32, RawDecompressWork)> {
    let effective_start = region.raw_data_off - (region.raw_data_off % WII_SECTOR_SIZE_U64);
    if pos < effective_start {
        return None;
    }
    // `pos < iso_file_size` and `iso_file_size <= MAX_PLAUSIBLE_ISO_SIZE`,
    // so the division always fits in u32 for legal chunk sizes.
    let i = ((pos - effective_start) / chunk_size) as u32;
    if i >= region.n_groups {
        return None;
    }
    let group = &groups[(region.group_index + i) as usize];
    raw_chunk_work_i(region, group, i, chunk_size, iso_file_size)
        .map(|work| (region.group_index + i, work))
}

/// Inputs for one [`decompress_raw_region`] call: the region to
/// decode plus the shared file, usage filter, and progress counter.
pub(super) struct RawRegionDecode<'a> {
    pub region: &'a WiaRawData,
    pub groups: &'a [RvzGroup],
    pub chunk_size: u64,
    pub iso_file_size: u64,
    pub file: &'a Arc<std::fs::File>,
    pub usage: Option<&'a UsageFilter<'a>>,
    pub bytes_done: &'a Arc<AtomicU64>,
}

/// Worker-pool raw-region decoder. Spawns a worker [`Pool`] seeded
/// with persistent `zstd::bulk::Decompressor`s, builds the full
/// work list for the region, and pumps it via [`drive`]. Output is
/// written in submission order by the consume closure so
/// `writer_pos` tracking stays valid.
pub(super) fn decompress_raw_region(
    args: RawRegionDecode<'_>,
    sink: &mut dyn DiscSink,
) -> RvzResult<()> {
    let RawRegionDecode {
        region,
        groups,
        chunk_size,
        iso_file_size,
        file,
        usage,
        bytes_done,
    } = args;
    let items = build_raw_region_work_items(region, groups, chunk_size, iso_file_size, usage);
    if items.is_empty() {
        return Ok(());
    }

    let chunk_bytes = usize::try_from(chunk_size).unwrap_or(usize::MAX);
    // Scratch budget per worker: the stored-input bound plus the packed
    // bulk decompress bound (two chunks) plus the decompressor context.
    let codec_bytes = zstd::zstd_safe::compress_bound(chunk_bytes)
        .saturating_add(chunk_bytes.saturating_mul(2))
        .saturating_add(crate::util::worker_pool::zstd_dctx_estimate());
    let Some(admission) = Budget {
        codec_per_worker: codec_bytes,
        per_job: chunk_bytes,
        writer_slot: 0,
        fixed: 0,
    }
    .admit(parallelism(), items.len() as u64) else {
        let mut buffer = vec![0; 4 * 1024 * 1024];
        for work in items {
            if work.data_size != 0 && work.rvz_packed_size != 0 {
                stream_packed_chunk(&work, file, sink, bytes_done, &mut buffer)?;
            } else {
                stream_raw_chunk(&work, file, sink, bytes_done, &mut buffer)?;
            }
        }
        return Ok(());
    };
    let n_threads = admission.workers;
    let max_in_flight = admission.max_in_flight;
    let workers = make_raw_decompress_workers(n_threads, file)?;
    let pool: Pool<RawDecompressWork, RawDecompressOut, RvzError> = Pool::spawn(workers);

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
            sink.write_at(out.write_start, slice)?;
            bytes_done.fetch_add(out.write_len as u64, Ordering::Relaxed);
            Ok(())
        },
    );

    pool.shutdown();
    result
}
#[cfg(test)]
mod tests {
    use super::*;

    struct VecSink(Vec<u8>);

    impl DiscSink for VecSink {
        fn write_at(&mut self, offset: u64, data: &[u8]) -> RvzResult<()> {
            let start = offset as usize;
            let end = start + data.len();
            self.0.resize(self.0.len().max(end), 0);
            self.0[start..end].copy_from_slice(data);
            Ok(())
        }
    }

    #[test]
    fn streaming_raw_chunks_match_worker_output() {
        let cases = [
            (
                "compressed",
                (0..32).map(|i| (i * 7) as u8).collect::<Vec<_>>(),
                true,
                0,
                32,
                32,
            ),
            ("stored", vec![0x5a; 32], false, 0, 32, 32),
            ("zero-sentinel", vec![0; 32], false, 0, 32, 0),
            (
                "clipped-front",
                (0..32).map(|i| i as u8).collect(),
                false,
                7,
                15,
                32,
            ),
            ("short-last", vec![0x31; 19], false, 0, 19, 19),
            (
                "compressed-clipped",
                (0..32).map(|i| (i * 7) as u8).collect::<Vec<_>>(),
                true,
                7,
                15,
                32,
            ),
        ];

        for (name, decoded, compressed, slice_offset, write_len, stored_size) in cases {
            let stored = if stored_size == 0 {
                Vec::new()
            } else if compressed {
                zstd::bulk::compress(&decoded, 0).unwrap()
            } else {
                decoded.clone()
            };
            let dir = tempfile::tempdir().unwrap();
            let file = write_stored(&dir, name, &stored);

            let work = RawDecompressWork {
                data_off: 0,
                data_size: stored.len() as u32,
                is_compressed: compressed,
                rvz_packed_size: 0,
                chunk_bytes: decoded.len(),
                chunk_abs_start: 0,
                write_start: 7,
                write_len,
                chunk_slice_offset: slice_offset,
            };
            let mut worker = make_one_raw_worker(&file).unwrap();
            let expected = worker.process(work.clone()).unwrap();
            let expected_slice = &expected.decoded
                [expected.chunk_slice_offset..expected.chunk_slice_offset + expected.write_len];
            let mut expected_sink = VecSink(Vec::new());
            expected_sink
                .write_at(expected.write_start, expected_slice)
                .unwrap();

            let mut actual_sink = VecSink(Vec::new());
            stream_raw_chunk(
                &work,
                &file,
                &mut actual_sink,
                &AtomicU64::new(0),
                &mut vec![0; 4 * 1024 * 1024],
            )
            .unwrap();
            assert_eq!(actual_sink.0, expected_sink.0, "{name}");
        }
    }

    #[test]
    fn streamed_zero_sentinel_clears_reused_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_stored(&dir, "zero-sentinel.bin", &[0x5a; 8]);
        let mut sink = VecSink(Vec::new());
        let bytes_done = AtomicU64::new(0);
        let mut buffer = [0xa5; 8];
        let data = RawDecompressWork {
            data_off: 0,
            data_size: 8,
            is_compressed: false,
            rvz_packed_size: 0,
            chunk_bytes: 8,
            chunk_abs_start: 0,
            write_start: 0,
            write_len: 8,
            chunk_slice_offset: 0,
        };
        let sentinel = RawDecompressWork {
            data_off: 8,
            data_size: 0,
            is_compressed: false,
            rvz_packed_size: 0,
            chunk_bytes: 8,
            chunk_abs_start: 8,
            write_start: 8,
            write_len: 8,
            chunk_slice_offset: 0,
        };

        stream_raw_chunk(&data, &file, &mut sink, &bytes_done, &mut buffer).unwrap();
        stream_raw_chunk(&sentinel, &file, &mut sink, &bytes_done, &mut buffer).unwrap();

        assert_eq!(sink.0, [vec![0x5a; 8], vec![0; 8]].concat());
    }

    /// Packed record stream: verbatim 1000 bytes, LFG-seeded 200 bytes,
    /// verbatim 12 bytes. Decoded size 1212.
    fn packed_plain_random_tail_records() -> Vec<u8> {
        let mut records = Vec::new();
        records.extend_from_slice(&1000u32.to_be_bytes());
        records.extend_from_slice(&[0x41u8; 1000]);
        records.extend_from_slice(&(0x8000_00C8u32).to_be_bytes());
        records.extend_from_slice(&[0x7Bu8; 68]);
        records.extend_from_slice(&12u32.to_be_bytes());
        records.extend_from_slice(b"tail-payload");
        records
    }

    fn expected_plain_random_tail(chunk_abs_start: u64) -> Vec<u8> {
        let mut expected = vec![0x41u8; 1000];
        let mut lfg = crate::nintendo::disc::rvz::packing::LaggedFibonacci::init(&[0x7Bu8; 68]);
        lfg.forward_bytes(((chunk_abs_start + 1000) % 0x8000) as usize);
        let mut junk = [0u8; 200];
        lfg.fill(&mut junk);
        expected.extend_from_slice(&junk);
        expected.extend_from_slice(b"tail-payload");
        expected
    }

    fn write_stored(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> Arc<std::fs::File> {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        Arc::new(std::fs::File::open(&path).unwrap())
    }

    /// The stored-bytes bound fires before the read: a group descriptor
    /// claiming more stored bytes than zstd's worst case for the chunk
    /// must be rejected from the descriptor alone, without sizing the
    /// input scratch from it.
    #[test]
    fn oversized_stored_size_rejected_before_read() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_stored(&dir, "tiny.bin", &[0u8; 16]);
        // Mirror the worker's bound: the chunk plus one packing record
        // per 0x8000-byte block, run through zstd's worst case.
        let chunk_bytes = 64usize;
        let stage1_cap = chunk_bytes + 76 * chunk_bytes.div_ceil(0x8000);
        let stored_cap = zstd::zstd_safe::compress_bound(stage1_cap);
        let work = RawDecompressWork {
            data_off: 0,
            data_size: (stored_cap + 1) as u32,
            is_compressed: true,
            rvz_packed_size: 0,
            chunk_bytes,
            chunk_abs_start: 0,
            write_start: 0,
            write_len: 64,
            chunk_slice_offset: 0,
        };
        let mut worker = make_one_raw_worker(&file).unwrap();
        let err = match worker.process(work.clone()) {
            Ok(_) => panic!("oversized stored size unexpectedly accepted"),
            Err(e) => e,
        };
        assert!(
            matches!(&err, RvzError::Custom(m)
                if m.contains(&format!("group stores {} bytes", stored_cap + 1))
                    && m.contains(&format!("{}-byte bound", stored_cap))),
            "{err}"
        );
        assert!(
            worker.scratch_in.is_empty(),
            "the input scratch must not be sized from a rejected descriptor"
        );
    }

    /// The bulk path consumes exactly the declared record stream: a
    /// frame whose records run past `rvz_packed_size` is rejected here
    /// just as the streaming path rejects it at the cut, so the verdict
    /// does not depend on which path the worker budget picks.
    #[test]
    fn bulk_packed_decode_bounded_by_declared_stream() {
        let mut records = Vec::new();
        // One verbatim record that fills the 64-byte chunk window.
        records.extend_from_slice(&64u32.to_be_bytes());
        records.extend_from_slice(&[0x41u8; 64]);
        // A junk record the declaration cuts in half.
        records.extend_from_slice(&(0x8000_0044u32).to_be_bytes());
        records.extend_from_slice(&[0x7Bu8; 68]);
        let stored = zstd::bulk::compress(&records, 0).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = write_stored(&dir, "packed-overdeclare.bin", &stored);
        let work = RawDecompressWork {
            data_off: 0,
            data_size: stored.len() as u32,
            is_compressed: true,
            rvz_packed_size: 100,
            chunk_bytes: 4096,
            chunk_abs_start: 0,
            write_start: 0,
            write_len: 64,
            chunk_slice_offset: 0,
        };
        let mut worker = make_one_raw_worker(&file).unwrap();
        assert!(
            worker.process(work.clone()).is_err(),
            "an over-declared packed frame must not decode"
        );
        // The streaming path fails the same container at the cut.
        let mut sink = VecSink(Vec::new());
        let mut buffer = vec![0u8; 4096];
        assert!(
            stream_packed_chunk(&work, &file, &mut sink, &AtomicU64::new(0), &mut buffer).is_err()
        );
    }

    /// Every packed path enforces the same stream end: a zstd frame or
    /// stored byte run that continues past `rvz_packed_size` fails in
    /// the bulk worker and in the streaming path alike.
    #[test]
    fn packed_trailing_bytes_rejected_on_both_paths() {
        // Compressed: the frame carries a whole record past the
        // declaration, so the bulk path's consumed-count check fires.
        let mut records = Vec::new();
        records.extend_from_slice(&64u32.to_be_bytes());
        records.extend_from_slice(&[0x41u8; 64]);
        records.extend_from_slice(&10u32.to_be_bytes());
        records.extend_from_slice(&[0x22u8; 10]);
        let stored = zstd::bulk::compress(&records, 0).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let compressed = RawDecompressWork {
            data_off: 0,
            data_size: stored.len() as u32,
            is_compressed: true,
            rvz_packed_size: 68,
            chunk_bytes: 4096,
            chunk_abs_start: 0,
            write_start: 0,
            write_len: 64,
            chunk_slice_offset: 0,
        };
        let file = write_stored(&dir, "compressed-tail.bin", &stored);
        let mut worker = make_one_raw_worker(&file).unwrap();
        let err = match worker.process(compressed.clone()) {
            Ok(_) => panic!("over-declared packed frame unexpectedly accepted"),
            Err(e) => e,
        };
        assert!(
            matches!(&err, RvzError::Custom(m) if m.contains("consumed 82 bytes")
                && m.contains("declares 68")),
            "{err}"
        );
        // The streaming path fails the same container at the cut.
        let mut sink = VecSink(Vec::new());
        let mut buffer = vec![0u8; 4096];
        assert!(
            stream_packed_chunk(
                &compressed,
                &file,
                &mut sink,
                &AtomicU64::new(0),
                &mut buffer
            )
            .is_err()
        );

        // Stored: the byte run continues 4 bytes past the declaration.
        let mut stored_records = records.clone();
        stored_records.extend_from_slice(&[0u8; 4]);
        let file = write_stored(&dir, "stored-tail.bin", &stored_records);
        let stored_case = RawDecompressWork {
            data_off: 0,
            data_size: stored_records.len() as u32,
            is_compressed: false,
            rvz_packed_size: records.len() as u32,
            chunk_bytes: 64,
            chunk_abs_start: 0,
            write_start: 0,
            write_len: 64,
            chunk_slice_offset: 0,
        };
        let mut worker = make_one_raw_worker(&file).unwrap();
        let err = match worker.process(stored_case.clone()) {
            Ok(_) => panic!("stored trailing bytes unexpectedly accepted"),
            Err(e) => e,
        };
        assert!(
            matches!(&err, RvzError::Custom(m) if m.contains("stored packed chunk holds 86 bytes")
                && m.contains("declares 82")),
            "{err}"
        );
        let mut sink = VecSink(Vec::new());
        assert!(
            stream_packed_chunk(
                &stored_case,
                &file,
                &mut sink,
                &AtomicU64::new(0),
                &mut buffer
            )
            .is_err()
        );
    }

    /// Every packed path enforces the same stream end: a record stream
    /// shorter than `rvz_packed_size` fails in the bulk worker and in
    /// the streaming path alike, for compressed and stored groups.
    #[test]
    fn packed_short_stream_rejected_on_both_paths() {
        // Records that fill the 64-byte chunk window; the declaration
        // claims 4 bytes more than the streams hold.
        let mut records = Vec::new();
        records.extend_from_slice(&64u32.to_be_bytes());
        records.extend_from_slice(&[0x41u8; 64]);
        let compressed = zstd::bulk::compress(&records, 0).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let short_declared = (records.len() + 4) as u32;

        let compressed_case = RawDecompressWork {
            data_off: 0,
            data_size: compressed.len() as u32,
            is_compressed: true,
            rvz_packed_size: short_declared,
            chunk_bytes: 64,
            chunk_abs_start: 0,
            write_start: 0,
            write_len: 64,
            chunk_slice_offset: 0,
        };
        let file = write_stored(&dir, "compressed-short.bin", &compressed);
        let mut worker = make_one_raw_worker(&file).unwrap();
        let err = match worker.process(compressed_case.clone()) {
            Ok(_) => panic!("short compressed stream unexpectedly accepted"),
            Err(e) => e,
        };
        assert!(
            matches!(&err, RvzError::Custom(m) if m.contains("consumed 68 bytes")
                && m.contains("declares 72")),
            "{err}"
        );
        let mut sink = VecSink(Vec::new());
        let mut buffer = vec![0u8; 4096];
        let err = stream_packed_chunk(
            &compressed_case,
            &file,
            &mut sink,
            &AtomicU64::new(0),
            &mut buffer,
        )
        .unwrap_err();
        assert!(
            matches!(&err, RvzError::Custom(m) if m.contains("ends 4 bytes short of the declared 72")),
            "{err}"
        );

        let file = write_stored(&dir, "stored-short.bin", &records);
        let stored_case = RawDecompressWork {
            data_off: 0,
            data_size: records.len() as u32,
            is_compressed: false,
            rvz_packed_size: short_declared,
            chunk_bytes: 64,
            chunk_abs_start: 0,
            write_start: 0,
            write_len: 64,
            chunk_slice_offset: 0,
        };
        let mut worker = make_one_raw_worker(&file).unwrap();
        let err = match worker.process(stored_case.clone()) {
            Ok(_) => panic!("short stored stream unexpectedly accepted"),
            Err(e) => e,
        };
        assert!(
            matches!(&err, RvzError::Custom(m) if m.contains("stored packed chunk holds 68 bytes")
                && m.contains("declares 72")),
            "{err}"
        );
        let err = stream_packed_chunk(
            &stored_case,
            &file,
            &mut sink,
            &AtomicU64::new(0),
            &mut buffer,
        )
        .unwrap_err();
        assert!(
            matches!(&err, RvzError::Custom(m) if m.contains("stored packed chunk holds 68 bytes")
                && m.contains("declares 72")),
            "{err}"
        );
    }

    /// The streaming path applies the same descriptor bounds as the
    /// bulk worker: a stored packed chunk whose stored size exceeds the
    /// chunk's bound is rejected on both paths with the same message.
    #[test]
    fn stream_packed_bounds_match_bulk() {
        // A 64-byte plain record plus 20 zero-size records: 148 stored
        // and declared bytes, above the 140-byte bound for a 64-byte
        // chunk.
        let mut records = Vec::new();
        records.extend_from_slice(&64u32.to_be_bytes());
        records.extend_from_slice(&[0x41u8; 64]);
        records.extend_from_slice(&[0u8; 4 * 20]);
        let dir = tempfile::tempdir().unwrap();
        let file = write_stored(&dir, "packed-overbound-stored.bin", &records);
        let work = RawDecompressWork {
            data_off: 0,
            data_size: records.len() as u32,
            is_compressed: false,
            rvz_packed_size: records.len() as u32,
            chunk_bytes: 64,
            chunk_abs_start: 0,
            write_start: 0,
            write_len: 64,
            chunk_slice_offset: 0,
        };
        let mut worker = make_one_raw_worker(&file).unwrap();
        let bulk_err = match worker.process(work.clone()) {
            Ok(_) => panic!("over-bound stored chunk unexpectedly accepted"),
            Err(e) => e,
        };
        assert!(
            matches!(&bulk_err, RvzError::Custom(m) if m.contains("group stores 148 bytes")
                && m.contains("140-byte bound")),
            "{bulk_err}"
        );
        let mut sink = VecSink(Vec::new());
        let mut buffer = vec![0u8; 4096];
        let stream_err =
            stream_packed_chunk(&work, &file, &mut sink, &AtomicU64::new(0), &mut buffer)
                .unwrap_err();
        assert_eq!(bulk_err.to_string(), stream_err.to_string());
    }

    /// The region streaming path applies the same descriptor bounds as
    /// the bulk worker to a non-packed chunk: a stored group one byte
    /// above the bound is rejected on both paths with the same message.
    #[test]
    fn stream_raw_bounds_match_bulk() {
        let chunk_bytes = 64usize;
        let stage1_cap = chunk_bytes + 76 * chunk_bytes.div_ceil(0x8000);
        let stored = vec![0x33u8; stage1_cap + 1];
        let dir = tempfile::tempdir().unwrap();
        let file = write_stored(&dir, "raw-overbound-stored.bin", &stored);
        let work = RawDecompressWork {
            data_off: 0,
            data_size: stored.len() as u32,
            is_compressed: false,
            rvz_packed_size: 0,
            chunk_bytes,
            chunk_abs_start: 0,
            write_start: 0,
            write_len: 64,
            chunk_slice_offset: 0,
        };
        let mut worker = make_one_raw_worker(&file).unwrap();
        let bulk_err = match worker.process(work.clone()) {
            Ok(_) => panic!("over-bound stored chunk unexpectedly accepted"),
            Err(e) => e,
        };
        let mut sink = VecSink(Vec::new());
        let mut buffer = vec![0u8; 4096];
        let stream_err =
            stream_raw_chunk(&work, &file, &mut sink, &AtomicU64::new(0), &mut buffer).unwrap_err();
        assert!(
            matches!(&bulk_err, RvzError::Custom(m) if m.contains("group stores 141 bytes")),
            "{bulk_err}"
        );
        assert_eq!(bulk_err.to_string(), stream_err.to_string());
    }

    fn processed_slice(worker: &mut RawDecompressWorker, work: &RawDecompressWork) -> Vec<u8> {
        let out = worker.process(work.clone()).unwrap();
        out.decoded[out.chunk_slice_offset..out.chunk_slice_offset + out.write_len].to_vec()
    }

    /// Drives the production sequential dispatch (sentinel before packed)
    /// and returns the bytes the sink received, placed at `write_start`.
    fn streamed_sequential(
        work: &RawDecompressWork,
        file: &Arc<std::fs::File>,
        buffer: &mut [u8],
    ) -> Vec<u8> {
        let mut sink = VecSink(Vec::new());
        let bytes_done = AtomicU64::new(0);
        if work.data_size == 0 {
            stream_raw_chunk(work, file, &mut sink, &bytes_done, buffer).unwrap();
        } else if work.rvz_packed_size != 0 {
            stream_packed_chunk(work, file, &mut sink, &bytes_done, buffer).unwrap();
        } else {
            stream_raw_chunk(work, file, &mut sink, &bytes_done, buffer).unwrap();
        }
        assert_eq!(bytes_done.load(Ordering::Relaxed), work.write_len as u64);
        sink.0
    }

    #[test]
    fn sequential_packed_plain_and_random_matches_worker_output() {
        let records = packed_plain_random_tail_records();
        let dir = tempfile::tempdir().unwrap();
        let file = write_stored(&dir, "packed-plain-random.bin", &records);
        let work = RawDecompressWork {
            data_off: 0,
            data_size: records.len() as u32,
            is_compressed: false,
            rvz_packed_size: records.len() as u32,
            chunk_bytes: 1212,
            chunk_abs_start: 0x8000,
            write_start: 0x8000,
            write_len: 1212,
            chunk_slice_offset: 0,
        };
        let mut worker = make_one_raw_worker(&file).unwrap();
        let expected = processed_slice(&mut worker, &work);
        assert_eq!(expected, expected_plain_random_tail(0x8000));
        let mut buffer = vec![0u8; 4096];
        let streamed = streamed_sequential(&work, &file, &mut buffer);
        assert_eq!(&streamed[work.write_start as usize..], &expected[..]);
    }

    #[test]
    fn sequential_packed_clipped_front_matches_worker_output() {
        let records = packed_plain_random_tail_records();
        let dir = tempfile::tempdir().unwrap();
        let file = write_stored(&dir, "packed-clipped-front.bin", &records);
        let work = RawDecompressWork {
            data_off: 0,
            data_size: records.len() as u32,
            is_compressed: false,
            rvz_packed_size: records.len() as u32,
            chunk_bytes: 1212,
            chunk_abs_start: 0,
            write_start: 100,
            write_len: 1112,
            chunk_slice_offset: 100,
        };
        let mut worker = make_one_raw_worker(&file).unwrap();
        let expected = processed_slice(&mut worker, &work);
        assert_eq!(expected, expected_plain_random_tail(0)[100..].to_vec());
        let mut buffer = vec![0u8; 4096];
        let streamed = streamed_sequential(&work, &file, &mut buffer);
        assert_eq!(streamed[100..], expected[..]);
        assert!(streamed[..100].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn sequential_zero_sentinel_with_stale_packed_size_matches_worker_output() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_stored(&dir, "sentinel.bin", &[0x5au8; 8]);
        // data_size 0 is the all-zero sentinel; a stale rvz_packed_size
        // must not route it into the packed decoder.
        let work = RawDecompressWork {
            data_off: 0,
            data_size: 0,
            is_compressed: false,
            rvz_packed_size: 999,
            chunk_bytes: 8,
            chunk_abs_start: 0x4000,
            write_start: 0x4000,
            write_len: 8,
            chunk_slice_offset: 0,
        };
        let mut worker = make_one_raw_worker(&file).unwrap();
        let expected = processed_slice(&mut worker, &work);
        assert_eq!(expected, vec![0u8; 8]);
        let mut buffer = vec![0xa5u8; 4096];
        let streamed = streamed_sequential(&work, &file, &mut buffer);
        assert_eq!(&streamed[work.write_start as usize..], &expected[..]);
    }

    #[test]
    fn truncated_trailing_record_errors_on_worker_and_sequential_paths() {
        // The chunk window completes inside the plain+random records; the
        // trailing record is truncated, so only the post-window drain can
        // reject it on the sequential path.
        let mut records = Vec::new();
        records.extend_from_slice(&1000u32.to_be_bytes());
        records.extend_from_slice(&[0x41u8; 1000]);
        records.extend_from_slice(&(0x8000_00C8u32).to_be_bytes());
        records.extend_from_slice(&[0x7Bu8; 68]);
        records.extend_from_slice(&12u32.to_be_bytes());
        records.extend_from_slice(b"tail-pa");
        let dir = tempfile::tempdir().unwrap();
        let file = write_stored(&dir, "packed-truncated-tail.bin", &records);
        let work = RawDecompressWork {
            data_off: 0,
            data_size: records.len() as u32,
            is_compressed: false,
            rvz_packed_size: records.len() as u32,
            chunk_bytes: 1200,
            chunk_abs_start: 0,
            write_start: 0,
            write_len: 1200,
            chunk_slice_offset: 0,
        };
        let mut worker = make_one_raw_worker(&file).unwrap();
        assert!(worker.process(work.clone()).is_err());
        let mut sink = VecSink(Vec::new());
        let mut buffer = vec![0u8; 4096];
        assert!(
            stream_packed_chunk(&work, &file, &mut sink, &AtomicU64::new(0), &mut buffer).is_err()
        );
    }

    /// The packed record stream is bounded by the declared
    /// `rvz_packed_size`: a frame whose declared record stream ends
    /// mid-record must error instead of draining zero-size records past
    /// the declaration.
    #[test]
    fn packed_stream_drain_bounded_by_declared_packed_size() {
        let mut records = Vec::new();
        // One verbatim record that completes the 64-byte chunk window.
        records.extend_from_slice(&64u32.to_be_bytes());
        records.extend_from_slice(&[0x41u8; 64]);
        // Then eight zero-size records after the chunk window; the
        // declaration cuts the last one in half.
        records.extend_from_slice(&[0u8; 4 * 8]);
        let stored = zstd::bulk::compress(&records, 0).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = write_stored(&dir, "packed-overbound.bin", &stored);
        let work = RawDecompressWork {
            data_off: 0,
            data_size: stored.len() as u32,
            is_compressed: true,
            rvz_packed_size: records.len() as u32 - 2,
            chunk_bytes: 64,
            chunk_abs_start: 0,
            write_start: 0,
            write_len: 64,
            chunk_slice_offset: 0,
        };
        let mut sink = VecSink(Vec::new());
        let mut buffer = vec![0u8; 4096];
        let err = stream_packed_chunk(&work, &file, &mut sink, &AtomicU64::new(0), &mut buffer)
            .unwrap_err();
        assert!(
            matches!(&err, RvzError::Custom(m) if m.contains("truncated RVZ packing record header")),
            "{err}"
        );
    }

    /// A compressed chunk whose zstd frame decodes to `chunk_bytes + 1`:
    /// the streaming path probes one extra byte once the chunk window is
    /// complete and rejects the trailing output instead of ignoring it.
    #[test]
    fn streamed_chunk_decoding_past_chunk_bytes_errors() {
        let decoded = vec![0x77u8; 33];
        let stored = zstd::bulk::compress(&decoded, 0).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = write_stored(&dir, "over.bin", &stored);
        let work = RawDecompressWork {
            data_off: 0,
            data_size: stored.len() as u32,
            is_compressed: true,
            rvz_packed_size: 0,
            chunk_bytes: 32,
            chunk_abs_start: 0,
            write_start: 0,
            write_len: 32,
            chunk_slice_offset: 0,
        };
        let mut sink = VecSink(Vec::new());
        let mut buffer = vec![0u8; 4096];
        let err =
            stream_raw_chunk(&work, &file, &mut sink, &AtomicU64::new(0), &mut buffer).unwrap_err();
        assert!(
            matches!(
                err,
                RvzError::DecompressedSizeMismatch {
                    expected: 32,
                    actual: 33
                }
            ),
            "{err}"
        );
    }

    /// A sliced work item (write_len < chunk_bytes) over a frame decoding
    /// to `chunk_bytes + 1`: once the requested slice is satisfied, the
    /// stream path must keep draining up to `chunk_bytes` and probe one
    /// extra byte, rejecting the over-long frame like the bulk worker
    /// instead of stopping silently at the slice boundary.
    #[test]
    fn streamed_sliced_chunk_decoding_past_chunk_bytes_errors() {
        let decoded = vec![0x77u8; 33];
        let stored = zstd::bulk::compress(&decoded, 0).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = write_stored(&dir, "over-sliced.bin", &stored);
        let work = RawDecompressWork {
            data_off: 0,
            data_size: stored.len() as u32,
            is_compressed: true,
            rvz_packed_size: 0,
            chunk_bytes: 32,
            chunk_abs_start: 0,
            write_start: 0,
            write_len: 16,
            chunk_slice_offset: 0,
        };
        let mut worker = make_one_raw_worker(&file).unwrap();
        assert!(worker.process(work.clone()).is_err());

        let mut sink = VecSink(Vec::new());
        // A buffer smaller than the slice leaves the main loop at the slice
        // boundary, so the drain loop must iterate before the probe rejects.
        let mut buffer = vec![0u8; 8];
        let err =
            stream_raw_chunk(&work, &file, &mut sink, &AtomicU64::new(0), &mut buffer).unwrap_err();
        assert!(
            matches!(
                err,
                RvzError::DecompressedSizeMismatch {
                    expected: 32,
                    actual: 33
                }
            ),
            "{err}"
        );
    }

    /// A packed raw chunk whose stage-1 length (the RVZ packing record
    /// stream) exceeds `chunk_bytes`. Genuinely encoded data can't
    /// produce this: every recorded junk run is ≥ 2084 bytes (seed
    /// recovery needs the generator's full 521-word recurrence state)
    /// and saves more than its 76-byte record
    /// cost, so `pack_encode` output always fits `chunk_bytes`, but a
    /// corrupt `rvz_packed_size` or hand-crafted records can, so the
    /// stage-1 buffer must be sized from `rvz_packed_size` instead of
    /// silently truncating the zstd destination.
    #[test]
    fn packed_raw_chunk_with_verbatim_runs_decodes() {
        use crate::nintendo::disc::rvz::packing::LaggedFibonacci;

        // junk record (4 + 68) + verbatim record (4 + 27): stage 1 is
        // 103 bytes for a 100-byte chunk, within the worst-case cap of
        // 100 + 76 * 1.
        let seed = [0x5Au8; 68];
        let mut packed = Vec::new();
        packed.extend_from_slice(&(0x8000_0000u32 | 73).to_be_bytes());
        packed.extend_from_slice(&seed);
        packed.extend_from_slice(&27u32.to_be_bytes());
        packed.extend_from_slice(&[0xABu8; 27]);

        let mut expected = vec![0u8; 73];
        LaggedFibonacci::init(&seed).fill(&mut expected);
        expected.extend_from_slice(&[0xABu8; 27]);
        assert_eq!(expected.len(), 100);

        let compressed = zstd::bulk::compress(&packed, 3).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chunks.bin");
        std::fs::write(&path, &compressed).unwrap();
        let file = Arc::new(std::fs::File::open(&path).unwrap());

        let mut worker = make_one_raw_worker(&file).unwrap();
        let out = worker
            .process(RawDecompressWork {
                data_off: 0,
                data_size: compressed.len() as u32,
                is_compressed: true,
                rvz_packed_size: packed.len() as u32,
                chunk_bytes: 100,
                chunk_abs_start: 0,
                write_start: 0,
                write_len: 100,
                chunk_slice_offset: 0,
            })
            .unwrap();
        assert_eq!(&out.decoded[..100], &expected[..]);
    }

    /// `rvz_packed_size` beyond the worst-case expansion cap must read
    /// as `DecompressedSizeMismatch` before any buffer is sized.
    #[test]
    fn packed_raw_chunk_over_stage1_cap_errors() {
        let packed = vec![0u8; 16];
        let compressed = zstd::bulk::compress(&packed, 3).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chunks.bin");
        std::fs::write(&path, &compressed).unwrap();
        let file = Arc::new(std::fs::File::open(&path).unwrap());

        let mut worker = make_one_raw_worker(&file).unwrap();
        let err = match worker.process(RawDecompressWork {
            data_off: 0,
            data_size: compressed.len() as u32,
            is_compressed: true,
            rvz_packed_size: 177, // cap for chunk_bytes=100 is 176
            chunk_bytes: 100,
            chunk_abs_start: 0,
            write_start: 0,
            write_len: 100,
            chunk_slice_offset: 0,
        }) {
            Ok(_) => panic!("over-cap rvz_packed_size unexpectedly decoded"),
            Err(e) => e,
        };
        assert!(
            matches!(
                err,
                RvzError::DecompressedSizeMismatch {
                    expected: 176,
                    actual: 177
                }
            ),
            "{err}"
        );
    }
}
