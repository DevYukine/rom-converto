//! Worker-pool CD hunk compressor.
//!
//! Drives a persistent worker pool for CDLZ/CDZL/CDFL codec trials,
//! overlaps writes with read + dispatch via a dedicated writer
//! thread inside `std::thread::scope`, and tracks the writer
//! position manually so the hot loop never calls
//! `stream_position()` (which flushes `BufWriter` and defeats
//! buffering).
//!
//! The shape mirrors the RVZ raw-region encoder. One worker owns
//! one [`CdCodecSet`] (persistent LZMA encoder + deflate contexts)
//! for the lifetime of the compress call.

use crate::cd::FRAME_SIZE;
use crate::chd::compression::dvd::DvdCodecSet;
use crate::chd::compression::{CdCodecSet, ChdCompression};
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::map::{MapEntry, crc16_ccitt};
use crate::util::CancelToken;
use crate::util::worker_pool::{Pool, Worker, drive, parallelism};
use sha1::{Digest, Sha1};
use std::io::{BufReader, BufWriter, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// One hunk worth of input bytes, already interleaved as
/// `[sector0 || zero_subcode0 || sector1 || zero_subcode1 || ...]`
/// with zero padding on the final partial hunk. Ready to hand to a
/// `CdCodecSet::compress_hunk` call without any further fixup.
pub(super) struct ChdCompressWork {
    pub hunk: Vec<u8>,
}

/// Compressed output plus the codec slot the best-of trial picked
/// and a CRC-16 over the raw hunk (matches chdman's
/// `hunk_write_compressed` input).
pub(super) struct ChdCompressedOut {
    pub compressed: Vec<u8>,
    pub compression: u8,
    pub crc16: u16,
}

/// Per-thread CHD compress worker. Owns one persistent
/// [`CdCodecSet`] so LZMA probability tables and deflate state
/// allocate exactly once per thread.
pub(super) struct ChdCompressWorker {
    codecs: CdCodecSet,
}

impl ChdCompressWorker {
    pub fn new(hunk_bytes: usize) -> ChdResult<Self> {
        Ok(Self {
            codecs: CdCodecSet::new(hunk_bytes)?,
        })
    }
}

impl Worker<ChdCompressWork, ChdCompressedOut, ChdError> for ChdCompressWorker {
    fn process(&mut self, work: ChdCompressWork) -> ChdResult<ChdCompressedOut> {
        let crc16 = crc16_ccitt(&work.hunk);
        let (compressed, compression) = match self.codecs.compress_hunk(&work.hunk) {
            Ok((data, codec_type)) => (data, codec_type),
            Err(_) => (work.hunk, ChdCompression::None as u8),
        };
        Ok(ChdCompressedOut {
            compressed,
            compression,
            crc16,
        })
    }
}

pub(super) fn make_chd_compress_workers(
    n: usize,
    hunk_bytes: usize,
) -> ChdResult<Vec<ChdCompressWorker>> {
    (0..n).map(|_| ChdCompressWorker::new(hunk_bytes)).collect()
}

/// DVD twin of [`ChdCompressWorker`]: same work/output shape, raw
/// codecs instead of the CD frame-split set.
pub(super) struct ChdDvdCompressWorker {
    codecs: DvdCodecSet,
}

impl Worker<ChdCompressWork, ChdCompressedOut, ChdError> for ChdDvdCompressWorker {
    fn process(&mut self, work: ChdCompressWork) -> ChdResult<ChdCompressedOut> {
        let crc16 = crc16_ccitt(&work.hunk);
        let (compressed, compression) = match self.codecs.compress_hunk(&work.hunk) {
            Ok((data, codec_type)) => (data, codec_type),
            Err(_) => (work.hunk, ChdCompression::None as u8),
        };
        Ok(ChdCompressedOut {
            compressed,
            compression,
            crc16,
        })
    }
}

pub(super) fn make_chd_dvd_compress_workers(
    n: usize,
    hunk_bytes: usize,
    allow_zstd: bool,
) -> ChdResult<Vec<ChdDvdCompressWorker>> {
    (0..n)
        .map(|_| {
            Ok(ChdDvdCompressWorker {
                codecs: DvdCodecSet::new(hunk_bytes, allow_zstd)?,
            })
        })
        .collect()
}

/// Drive the full compress pipeline:
///
/// * **Reader (dispatcher thread)**: sequential `BufReader` over
///   the bin file. Produces one interleaved hunk per `drive` call,
///   updates the running `raw_sha1` with the full frame bytes in
///   hunk order.
/// * **Workers (pool threads)**: receive hunks, trial every codec
///   via `CdCodecSet::compress_hunk`, return the smallest output.
/// * **Writer (dedicated thread)**: drains a bounded channel and
///   calls `write_all` on the output `BufWriter` so writes overlap
///   with reads and compresses.
///
/// `total_sectors` includes any track padding frames; only the first
/// `data_sectors` are read from the source (`sector_data_size` bytes
/// each, 2352 for raw bin tracks, 2048 for MODE1/2048 ISO data). The
/// padding frames stay zero but are still hashed: chdman includes
/// them in the raw SHA-1.
///
/// `writer_pos` is the file position **before** the next
/// compressed hunk would land. The caller owns it and passes it
/// through; this function updates it in place.
#[allow(clippy::too_many_arguments)]
pub(super) fn compress_hunks(
    pool: &Pool<ChdCompressWork, ChdCompressedOut, ChdError>,
    bin_reader: &mut BufReader<std::fs::File>,
    writer: &mut BufWriter<std::fs::File>,
    writer_pos: &mut u64,
    map_entries: &mut Vec<MapEntry>,
    raw_sha1: &mut Sha1,
    total_sectors: u32,
    data_sectors: u32,
    sector_data_size: usize,
    hunk_bytes: usize,
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
) -> ChdResult<()> {
    let frames_per_hunk = hunk_bytes / FRAME_SIZE;
    let total_hunks = total_sectors.div_ceil(frames_per_hunk as u32) as u64;

    run_pipeline(
        pool,
        writer,
        writer_pos,
        map_entries,
        total_hunks,
        // produce: zero padding on the short final hunk comes for
        // free from the `vec![0; hunk_bytes]` allocation.
        |chunk_idx| -> ChdResult<ChdCompressWork> {
            if cancel.is_cancelled() {
                return Err(ChdError::Cancelled);
            }
            let first_sector = (chunk_idx as u32) * frames_per_hunk as u32;
            let sectors_in_hunk = frames_per_hunk.min((total_sectors - first_sector) as usize);
            let read_sectors =
                (data_sectors.saturating_sub(first_sector) as usize).min(sectors_in_hunk);
            let read_bytes = read_sectors * sector_data_size;

            let mut sector_buf = vec![0u8; read_bytes];
            bin_reader.read_exact(&mut sector_buf)?;

            let mut hunk = vec![0u8; hunk_bytes];
            for s in 0..read_sectors {
                let src = s * sector_data_size;
                let dst = s * FRAME_SIZE;
                hunk[dst..dst + sector_data_size]
                    .copy_from_slice(&sector_buf[src..src + sector_data_size]);
            }
            for s in 0..sectors_in_hunk {
                let dst = s * FRAME_SIZE;
                raw_sha1.update(&hunk[dst..dst + FRAME_SIZE]);
            }
            bytes_done.fetch_add(read_bytes as u64, Ordering::Relaxed);
            Ok(ChdCompressWork { hunk })
        },
    )
}

/// DVD produce path: flat 2048-byte sectors, no interleave, no
/// subcode. The raw SHA-1 covers exactly `logical_bytes`; the zero
/// padding of the final partial hunk is compressed but never hashed,
/// matching chdman.
#[allow(clippy::too_many_arguments)]
pub(super) fn compress_hunks_dvd(
    pool: &Pool<ChdCompressWork, ChdCompressedOut, ChdError>,
    iso_reader: &mut BufReader<std::fs::File>,
    writer: &mut BufWriter<std::fs::File>,
    writer_pos: &mut u64,
    map_entries: &mut Vec<MapEntry>,
    raw_sha1: &mut Sha1,
    logical_bytes: u64,
    hunk_bytes: usize,
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
) -> ChdResult<()> {
    let total_hunks = logical_bytes.div_ceil(hunk_bytes as u64);

    run_pipeline(
        pool,
        writer,
        writer_pos,
        map_entries,
        total_hunks,
        |chunk_idx| -> ChdResult<ChdCompressWork> {
            if cancel.is_cancelled() {
                return Err(ChdError::Cancelled);
            }
            let offset = chunk_idx * hunk_bytes as u64;
            let take = ((logical_bytes - offset) as usize).min(hunk_bytes);

            let mut hunk = vec![0u8; hunk_bytes];
            iso_reader.read_exact(&mut hunk[..take])?;
            raw_sha1.update(&hunk[..take]);
            bytes_done.fetch_add(take as u64, Ordering::Relaxed);
            Ok(ChdCompressWork { hunk })
        },
    )
}

/// Shared compress scaffold: `drive` the pool with the mode-specific
/// `produce` closure while a dedicated writer thread drains a bounded
/// channel, so reads, codec trials, and writes overlap. The consume
/// side is mode-independent: append a map entry, forward bytes,
/// advance the writer position.
fn run_pipeline<F>(
    pool: &Pool<ChdCompressWork, ChdCompressedOut, ChdError>,
    writer: &mut BufWriter<std::fs::File>,
    writer_pos: &mut u64,
    map_entries: &mut Vec<MapEntry>,
    total_hunks: u64,
    produce: F,
) -> ChdResult<()>
where
    F: FnMut(u64) -> ChdResult<ChdCompressWork>,
{
    let max_in_flight = parallelism() * 2;
    let mut local_writer_pos = *writer_pos;
    let (write_tx, write_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(max_in_flight * 2);

    let scope_result: ChdResult<()> = std::thread::scope(|s| {
        let writer_slot: &mut BufWriter<std::fs::File> = writer;
        let writer_handle = s.spawn(move || -> ChdResult<()> {
            while let Ok(bytes) = write_rx.recv() {
                writer_slot.write_all(&bytes)?;
            }
            Ok(())
        });

        let drive_result = drive(
            pool,
            total_hunks,
            max_in_flight,
            produce,
            |_seq, out: ChdCompressedOut| -> ChdResult<()> {
                let offset = local_writer_pos;
                let length = out.compressed.len() as u32;
                map_entries.push(MapEntry {
                    compression: out.compression,
                    length,
                    offset,
                    crc16: out.crc16,
                });
                write_tx
                    .send(out.compressed)
                    .map_err(|_| ChdError::WorkerPoolClosed)?;
                local_writer_pos += length as u64;
                Ok(())
            },
        );

        drop(write_tx);
        let writer_result = writer_handle
            .join()
            .unwrap_or_else(|_| Err(ChdError::WorkerPoolPanic));
        drive_result?;
        writer_result
    });

    *writer_pos = local_writer_pos;
    scope_result
}
