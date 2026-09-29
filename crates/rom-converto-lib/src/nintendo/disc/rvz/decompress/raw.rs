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
//! BLOCK_TOTAL_SIZE)`, the last chunk of a region may be shorter
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

impl RawDecompressWorker {
    /// Decode a packed chunk into the chunk-sized `out`, reusing the
    /// worker's persistent zstd decompressor and scratch for the common
    /// case, and streaming from the file when the stored size sits above
    /// the codec bound or the packed stream exceeds two chunks.
    fn decode_packed_stored(&mut self, work: &RawDecompressWork, out: &mut [u8]) -> RvzResult<()> {
        let packed_len = work.rvz_packed_size as usize;
        let max_stored = if work.is_compressed {
            zstd::zstd_safe::compress_bound(work.chunk_bytes)
        } else {
            work.chunk_bytes
        };
        let decoded_len = if work.data_size as usize <= max_stored
            && packed_len <= 2 * work.chunk_bytes
        {
            self.scratch_in.resize(work.data_size as usize, 0);
            file_read_exact_at(&self.file, &mut self.scratch_in, work.data_off)?;
            // Stored frames decode straight from scratch_in; only zstd
            // frames need the scratch_out staging pass.
            let packed: &[u8] = if work.is_compressed {
                // The packed stream can overshoot the chunk by its
                // per-record headers, so decompress into the larger of
                // the two bounds like develop's chunk-sized scratch.
                let output_size = packed_len.max(work.chunk_bytes);
                if self.scratch_out.len() < output_size {
                    self.scratch_out.resize(output_size, 0);
                }
                let stage1_len = self
                    .decompressor
                    .decompress_to_buffer(&self.scratch_in, &mut self.scratch_out[..output_size])?;
                &self.scratch_out[..stage1_len]
            } else {
                &self.scratch_in
            };
            let mut packed_cursor = std::io::Cursor::new(packed);
            crate::nintendo::disc::rvz::packing::pack_decode_reader(
                &mut packed_cursor,
                work.chunk_abs_start,
                out,
            )?
            .0
        } else {
            // Rare oversized packed chunk: stream from the file instead
            // of buffering the stored bytes.
            let stored =
                PositionalReader::new(&*self.file, work.data_off, u64::from(work.data_size));
            if work.is_compressed {
                let mut decoder = zstd::stream::read::Decoder::new(stored)?;
                crate::nintendo::disc::rvz::packing::pack_decode_reader(
                    &mut decoder,
                    work.chunk_abs_start,
                    out,
                )?
                .0
            } else {
                let mut stored = std::io::BufReader::with_capacity(1024 * 1024, stored);
                crate::nintendo::disc::rvz::packing::pack_decode_reader(
                    &mut stored,
                    work.chunk_abs_start,
                    out,
                )?
                .0
            }
        };
        let required = work.chunk_slice_offset + work.write_len;
        if decoded_len < required {
            return Err(RvzError::DecompressedSizeMismatch {
                expected: required as u64,
                actual: decoded_len as u64,
            });
        }
        Ok(())
    }
}

/// Per-chunk work item. Built on the dispatcher thread. Computing
/// the write range needs `region.raw_data_off` plus the ISO file
/// size, which are cheap to capture but live on the main thread.
#[derive(Clone)]
pub(crate) struct RawDecompressWork {
    // File offset + compressed size of the chunk in the RVZ file.
    // A `data_size` of 0 is Dolphin's all-zero sentinel; the
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

impl Worker<RawDecompressWork, RawDecompressOut, RvzError> for RawDecompressWorker {
    fn process(&mut self, work: RawDecompressWork) -> RvzResult<RawDecompressOut> {
        // data_size == 0 is Dolphin's all-zero sentinel; synthesise zeros without I/O.
        if work.data_size == 0 {
            let zeros = vec![0u8; work.write_len].into_boxed_slice();
            return Ok(RawDecompressOut {
                decoded: zeros,
                write_start: work.write_start,
                write_len: work.write_len,
                chunk_slice_offset: 0,
            });
        }

        let max_stored = if work.is_compressed {
            zstd::zstd_safe::compress_bound(work.chunk_bytes)
        } else {
            work.chunk_bytes
        };
        let fits = work.data_size as usize <= max_stored;
        let packed = work.rvz_packed_size != 0;

        let decoded = if packed {
            let mut decoded = vec![0u8; work.chunk_bytes].into_boxed_slice();
            self.decode_packed_stored(&work, &mut decoded)?;
            decoded
        } else if !fits {
            let mut decoded = vec![0u8; work.chunk_bytes];
            if work.is_compressed {
                let stored =
                    PositionalReader::new(&*self.file, work.data_off, u64::from(work.data_size));
                let mut decoder = zstd::stream::read::Decoder::new(stored)?;
                // Lenient like `stream_raw_chunk`: read up to `chunk_bytes` until
                // EOF; the length check below only needs `decoded >= required`,
                // matching develop's `decompress_to_buffer` (which may return
                // fewer bytes than the destination buffer's capacity).
                let mut n = 0usize;
                while n < decoded.len() {
                    let count = decoder.read(&mut decoded[n..])?;
                    if count == 0 {
                        break;
                    }
                    n += count;
                }
                decoded.truncate(n);
            } else {
                file_read_exact_at(&self.file, &mut decoded, work.data_off)?;
            }
            decoded.into_boxed_slice()
        } else {
            self.scratch_in.resize(work.data_size as usize, 0);
            file_read_exact_at(&self.file, &mut self.scratch_in, work.data_off)?;
            let stage1_len = if work.is_compressed {
                let output_size = work.chunk_bytes;
                if self.scratch_out.len() < output_size {
                    self.scratch_out.resize(output_size, 0);
                }
                self.decompressor
                    .decompress_to_buffer(&self.scratch_in, &mut self.scratch_out[..output_size])?
            } else {
                if self.scratch_out.len() < self.scratch_in.len() {
                    self.scratch_out.resize(self.scratch_in.len(), 0);
                }
                self.scratch_out[..self.scratch_in.len()].copy_from_slice(&self.scratch_in);
                self.scratch_in.len()
            };
            self.scratch_out[..stage1_len].to_vec().into_boxed_slice()
        };
        if !packed && decoded.len() < work.chunk_slice_offset + work.write_len {
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

/// Build the work-item list for one raw-data region. Mirrors the
/// sequential loop's math exactly (effective_start alignment, per-
/// chunk clip, last-chunk trim) so the decoded output is byte-
/// identical to the pre-parallel path.
pub(crate) fn build_raw_region_work_items(
    region: &WiaRawData,
    groups: &[RvzGroup],
    chunk_size: u64,
    iso_file_size: u64,
    filter: Option<&UsageFilter>,
) -> RvzResult<Vec<RawDecompressWork>> {
    let effective_start = region.raw_data_off - (region.raw_data_off % WII_SECTOR_SIZE_U64);
    let region_end = region.raw_data_off + region.raw_data_size;
    let mut items = Vec::with_capacity(region.n_groups as usize);
    for i in 0..region.n_groups {
        let Some(index) = region.group_index.checked_add(i) else {
            return Err(RvzError::Custom("group index past table".into()));
        };
        let Some(group) = groups.get(index as usize) else {
            return Err(RvzError::Custom("group index past table".into()));
        };
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
        // When scrubbing for WBFS, a chunk landing only in unused blocks
        // is never read or decompressed.
        if let Some(filter) = filter
            && !filter.keeps(write_start, write_len as u64)
        {
            continue;
        }
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
    Ok(items)
}

fn stream_raw_chunk(
    work: &RawDecompressWork,
    file: &std::fs::File,
    sink: &mut dyn DiscSink,
    bytes_done: &AtomicU64,
    buffer: &mut [u8],
) -> RvzResult<()> {
    let stored = PositionalReader::new(file, work.data_off, u64::from(work.data_size));
    let mut source: Box<dyn Read> = if work.is_compressed && work.data_size != 0 {
        Box::new(zstd::stream::read::Decoder::new(stored)?)
    } else {
        Box::new(stored)
    };
    let slice_end = work.chunk_slice_offset + work.write_len;
    let mut decoded = 0usize;
    loop {
        if decoded == slice_end {
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
    let source: Box<dyn Read> = if work.is_compressed {
        Box::new(zstd::stream::read::Decoder::new(stored)?)
    } else {
        Box::new(std::io::BufReader::with_capacity(1024 * 1024, stored))
    };
    let mut decoder =
        crate::nintendo::disc::rvz::packing::PackedDecoder::new(source, work.chunk_abs_start);
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
    Ok(())
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
    let items = build_raw_region_work_items(region, groups, chunk_size, iso_file_size, usage)?;
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
}
