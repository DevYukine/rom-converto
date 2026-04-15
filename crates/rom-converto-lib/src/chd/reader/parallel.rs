//! Parallel CHD hunk decompressor.
//!
//! Drives a worker pool via [`crate::util::worker_pool::drive`] so
//! N workers decode compressed hunks concurrently. Each worker
//! holds a [`CdDecoderSet`] (persistent LZMA + deflate state) and
//! shares an `Arc<std::fs::File>` for positional reads, so the
//! dispatcher thread never holds a read cursor and multiple
//! workers never fight over a single `BufReader`.
//!
//! The consume closure gathers each decoded hunk's sector-only
//! bytes (subcodes stripped) into one buffer and ships them to a
//! dedicated writer thread inside `std::thread::scope`, same
//! shape as the compress path.

use crate::cd::{FRAME_SIZE, SECTOR_SIZE};
use crate::chd::compression::CdDecoderSet;
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::map::{
    COMPRESSION_NONE, COMPRESSION_PARENT, COMPRESSION_SELF, MapEntry, crc16_ccitt,
};
use crate::util::pread::file_read_exact_at;
use crate::util::worker_pool::{Pool, Worker, drive, parallelism};
use sha1::{Digest, Sha1};
use std::io::{BufWriter, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Per-hunk work item. Holds the already-resolved map entry so the
/// worker never has to walk a self-reference chain.
pub(crate) struct ChdExtractWork {
    pub entry: MapEntry,
}

/// Decoded full hunk bytes (sector + subcode interleaved,
/// `hunk_bytes` total). Extract and verify each shape this into
/// their own output.
pub(crate) struct ChdExtractedOut {
    pub hunk: Vec<u8>,
}

/// Per-thread decompress worker. Owns the shared file handle + a
/// reusable [`CdDecoderSet`] so LZMA probability tables and
/// deflate state allocate exactly once per thread.
pub(crate) struct ChdExtractWorker {
    decoders: CdDecoderSet,
    file: Arc<std::fs::File>,
    hunk_bytes: usize,
}

impl ChdExtractWorker {
    pub fn new(file: Arc<std::fs::File>, hunk_bytes: usize) -> ChdResult<Self> {
        Ok(Self {
            decoders: CdDecoderSet::new(hunk_bytes)?,
            file,
            hunk_bytes,
        })
    }
}

impl Worker<ChdExtractWork, ChdExtractedOut, ChdError> for ChdExtractWorker {
    fn process(&mut self, work: ChdExtractWork) -> ChdResult<ChdExtractedOut> {
        let hunk_bytes = self.hunk_bytes;
        let entry = work.entry;

        let hunk = match entry.compression {
            0 => {
                let mut compressed = vec![0u8; entry.length as usize];
                file_read_exact_at(&self.file, &mut compressed, entry.offset)?;
                self.decoders.decompress_cdlz(&compressed, hunk_bytes)?
            }
            1 => {
                let mut compressed = vec![0u8; entry.length as usize];
                file_read_exact_at(&self.file, &mut compressed, entry.offset)?;
                self.decoders.decompress_cdzl(&compressed, hunk_bytes)?
            }
            2 => {
                let mut compressed = vec![0u8; entry.length as usize];
                file_read_exact_at(&self.file, &mut compressed, entry.offset)?;
                self.decoders.decompress_cdfl(&compressed, hunk_bytes)?
            }
            COMPRESSION_NONE => {
                let mut data = vec![0u8; hunk_bytes];
                file_read_exact_at(&self.file, &mut data, entry.offset)?;
                data
            }
            _ => {
                return Err(ChdError::UnknownCompressionCodec([
                    entry.compression,
                    0,
                    0,
                    0,
                ]));
            }
        };

        if hunk.len() != hunk_bytes {
            return Err(ChdError::DecompressionSizeMismatch {
                expected: hunk_bytes,
                actual: hunk.len(),
            });
        }

        let computed_crc = crc16_ccitt(&hunk);
        if computed_crc != entry.crc16 {
            return Err(ChdError::HunkCrcMismatch {
                hunk: 0,
                expected: entry.crc16,
                actual: computed_crc,
            });
        }

        Ok(ChdExtractedOut { hunk })
    }
}

pub(crate) fn make_chd_extract_workers(
    n: usize,
    file: &Arc<std::fs::File>,
    hunk_bytes: usize,
) -> ChdResult<Vec<ChdExtractWorker>> {
    (0..n)
        .map(|_| ChdExtractWorker::new(file.clone(), hunk_bytes))
        .collect()
}

/// Resolve any `COMPRESSION_SELF` entry into its target by chasing
/// the reference chain. SELF only points at earlier hunks, so the
/// chain terminates. PARENT is not supported yet.
fn resolve_entry(map: &[MapEntry], hunk_index: u32) -> ChdResult<MapEntry> {
    let mut idx = hunk_index as usize;
    let mut guard = 0usize;
    loop {
        if guard > map.len() {
            return Err(ChdError::MapDecompressionError);
        }
        let entry = map[idx];
        if entry.compression == COMPRESSION_SELF {
            idx = entry.offset as usize;
            guard += 1;
            continue;
        }
        if entry.compression == COMPRESSION_PARENT {
            return Err(ChdError::ParentChdNotSupported);
        }
        return Ok(entry);
    }
}

/// Drive the parallel extract pipeline: pool of decompressors
/// reading a shared file via positional reads, reorder-buffered
/// drive, dedicated writer thread for the output bin.
#[allow(clippy::too_many_arguments)]
pub(crate) fn parallel_extract_hunks(
    pool: &Pool<ChdExtractWork, ChdExtractedOut, ChdError>,
    map: &[MapEntry],
    writer: &mut BufWriter<std::fs::File>,
    hunk_bytes: usize,
    total_frames: u32,
    bytes_done: &Arc<AtomicU64>,
) -> ChdResult<()> {
    let frames_per_hunk = hunk_bytes / FRAME_SIZE;
    let hunk_count = map.len() as u64;
    let max_in_flight = parallelism() * 2;

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
            hunk_count,
            max_in_flight,
            |chunk_idx| -> ChdResult<ChdExtractWork> {
                let entry = resolve_entry(map, chunk_idx as u32)?;
                Ok(ChdExtractWork { entry })
            },
            |seq, out| -> ChdResult<()> {
                // Gather sector-only bytes from the interleaved
                // hunk, dropping the 96-byte subcode after each
                // frame. `chdman extractcd` writes sector-only
                // bins, so the output contains exactly
                // `total_frames * SECTOR_SIZE` bytes.
                let first_sector = (seq as u32) * frames_per_hunk as u32;
                let frames_in_hunk = frames_per_hunk.min((total_frames - first_sector) as usize);
                let mut sectors = Vec::with_capacity(frames_in_hunk * SECTOR_SIZE);
                for frame in 0..frames_in_hunk {
                    let off = frame * FRAME_SIZE;
                    sectors.extend_from_slice(&out.hunk[off..off + SECTOR_SIZE]);
                }
                let len = sectors.len() as u64;
                write_tx
                    .send(sectors)
                    .map_err(|_| ChdError::WorkerPoolClosed)?;
                bytes_done.fetch_add(len, Ordering::Relaxed);
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

    scope_result
}

/// Verify-side variant: same pool shape but instead of writing
/// decoded bytes to a file, hash them in order with a single
/// rolling `Sha1`. The dispatcher's consume closure runs on the
/// main thread because SHA-1 isn't parallelisable across hunks;
/// the worker pool still does every decompression in parallel.
///
/// Only the first `logical_bytes` bytes of the decoded hunks
/// are folded into the hash, matching the existing serial verify
/// path and chdman's raw SHA-1 coverage rule.
pub(crate) fn parallel_verify_hunks(
    pool: &Pool<ChdExtractWork, ChdExtractedOut, ChdError>,
    map: &[MapEntry],
    raw_sha1: &mut Sha1,
    hunk_bytes: usize,
    logical_bytes: u64,
    bytes_done: &Arc<AtomicU64>,
) -> ChdResult<()> {
    let hunk_count = map.len() as u64;
    let max_in_flight = parallelism() * 2;
    let hunk_bytes_u64 = hunk_bytes as u64;

    let mut bytes_remaining = logical_bytes;

    drive(
        pool,
        hunk_count,
        max_in_flight,
        |chunk_idx| -> ChdResult<ChdExtractWork> {
            let entry = resolve_entry(map, chunk_idx as u32)?;
            Ok(ChdExtractWork { entry })
        },
        |_seq, out| -> ChdResult<()> {
            // Hash the full interleaved hunk, capped at
            // `logical_bytes` so the final partial hunk's zero
            // padding isn't folded in. Matches chdman's
            // `do_verify` and the existing serial verify path.
            let take = bytes_remaining.min(hunk_bytes_u64) as usize;
            raw_sha1.update(&out.hunk[..take]);
            bytes_remaining = bytes_remaining.saturating_sub(hunk_bytes_u64);
            bytes_done.fetch_add(take as u64, Ordering::Relaxed);
            Ok(())
        },
    )
}
