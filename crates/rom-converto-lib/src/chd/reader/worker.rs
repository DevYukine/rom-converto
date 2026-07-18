//! Worker-pool CHD hunk decompressor.
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
use crate::chd::compression::dvd::DvdDecoderSet;
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::map::{
    COMPRESSION_NONE, COMPRESSION_PARENT, COMPRESSION_SELF, MapEntry, crc16_ccitt,
};
use crate::chd::swap_audio_sector;
use crate::util::CancelToken;
use crate::util::hash::MultiHasher;
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
/// reusable [`CdDecoderSet`] whose slots are resolved from the
/// header compressor tags, so codec state allocates exactly once
/// per thread and any chdman codec combination decodes correctly.
pub(crate) struct ChdExtractWorker {
    decoders: CdDecoderSet,
    file: Arc<std::fs::File>,
    hunk_bytes: usize,
}

impl ChdExtractWorker {
    pub fn new(
        file: Arc<std::fs::File>,
        hunk_bytes: usize,
        compressors: [[u8; 4]; 4],
    ) -> ChdResult<Self> {
        Ok(Self {
            decoders: CdDecoderSet::new(compressors, hunk_bytes)?,
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
            slot @ 0..=3 => {
                let mut compressed = vec![0u8; entry.length as usize];
                file_read_exact_at(&self.file, &mut compressed, entry.offset)?;
                self.decoders.decompress(slot, &compressed, hunk_bytes)?
            }
            COMPRESSION_NONE => {
                let mut data = vec![0u8; hunk_bytes];
                file_read_exact_at(&self.file, &mut data, entry.offset)?;
                data
            }
            other => {
                return Err(ChdError::UnknownCompressionCodec([other, 0, 0, 0]));
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
    compressors: [[u8; 4]; 4],
) -> ChdResult<Vec<ChdExtractWorker>> {
    (0..n)
        .map(|_| ChdExtractWorker::new(file.clone(), hunk_bytes, compressors))
        .collect()
}

/// DVD twin of [`ChdExtractWorker`]: hunks are flat sector data and
/// the codec for each map slot comes from the header's compressor
/// tags instead of the fixed CD set.
pub(crate) struct ChdDvdExtractWorker {
    decoders: DvdDecoderSet,
    file: Arc<std::fs::File>,
    hunk_bytes: usize,
}

impl Worker<ChdExtractWork, ChdExtractedOut, ChdError> for ChdDvdExtractWorker {
    fn process(&mut self, work: ChdExtractWork) -> ChdResult<ChdExtractedOut> {
        let hunk_bytes = self.hunk_bytes;
        let entry = work.entry;

        let hunk = match entry.compression {
            slot @ 0..=3 => {
                let mut compressed = vec![0u8; entry.length as usize];
                file_read_exact_at(&self.file, &mut compressed, entry.offset)?;
                self.decoders.decompress(slot, &compressed, hunk_bytes)?
            }
            COMPRESSION_NONE => {
                let mut data = vec![0u8; hunk_bytes];
                file_read_exact_at(&self.file, &mut data, entry.offset)?;
                data
            }
            other => {
                return Err(ChdError::UnknownCompressionCodec([other, 0, 0, 0]));
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

pub(crate) fn make_chd_dvd_extract_workers(
    n: usize,
    file: &Arc<std::fs::File>,
    hunk_bytes: usize,
    compressors: [[u8; 4]; 4],
) -> ChdResult<Vec<ChdDvdExtractWorker>> {
    (0..n)
        .map(|_| {
            Ok(ChdDvdExtractWorker {
                decoders: DvdDecoderSet::new(compressors, hunk_bytes)?,
                file: file.clone(),
                hunk_bytes,
            })
        })
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

/// Drive the extract pipeline: pool of decompressors reading a
/// shared file via positional reads, reorder-buffered drive,
/// dedicated writer thread for the output bin.
pub(crate) fn extract_hunks(
    pool: &Pool<ChdExtractWork, ChdExtractedOut, ChdError>,
    map: &[MapEntry],
    writer: &mut BufWriter<std::fs::File>,
    hunk_bytes: usize,
    frame_sizes: &[usize],
    frame_audio: &[bool],
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
) -> ChdResult<()> {
    let frames_per_hunk = hunk_bytes / FRAME_SIZE;
    let total_frames = frame_sizes.len();

    run_extract_pipeline(pool, map, writer, bytes_done, cancel, |seq, mut out| {
        // Gather payload bytes from the interleaved hunk, dropping
        // the subcode and any tail past each track's datasize.
        // `chdman extractcd` writes datasize-wide bins; track padding
        // frames past the CHT2 frame counts are dropped entirely.
        let first_frame = seq as usize * frames_per_hunk;
        let frames_in_hunk = frames_per_hunk.min(total_frames.saturating_sub(first_frame));
        let mut sectors = Vec::with_capacity(frames_in_hunk * SECTOR_SIZE);
        for frame in 0..frames_in_hunk {
            let off = frame * FRAME_SIZE;
            let size = frame_sizes[first_frame + frame];
            // Audio frames are stored big-endian; swap back to the
            // little-endian samples the extracted bin carries.
            if frame_audio[first_frame + frame] {
                swap_audio_sector(&mut out.hunk[off..off + size]);
            }
            sectors.extend_from_slice(&out.hunk[off..off + size]);
        }
        Ok(sectors)
    })
}

/// DVD extract: hunks are already flat sector data, so each hunk is
/// written as-is, with the final one truncated to `logical_bytes`.
pub(crate) fn extract_hunks_dvd(
    pool: &Pool<ChdExtractWork, ChdExtractedOut, ChdError>,
    map: &[MapEntry],
    writer: &mut BufWriter<std::fs::File>,
    hunk_bytes: usize,
    logical_bytes: u64,
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
) -> ChdResult<()> {
    run_extract_pipeline(pool, map, writer, bytes_done, cancel, |seq, out| {
        let offset = seq * hunk_bytes as u64;
        let take = ((logical_bytes - offset.min(logical_bytes)) as usize).min(hunk_bytes);
        let mut hunk = out.hunk;
        hunk.truncate(take);
        Ok(hunk)
    })
}

/// Shared extract scaffold; `shape` turns one decoded hunk into the
/// bytes that belong in the output stream.
fn run_extract_pipeline<F>(
    pool: &Pool<ChdExtractWork, ChdExtractedOut, ChdError>,
    map: &[MapEntry],
    writer: &mut BufWriter<std::fs::File>,
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
    mut shape: F,
) -> ChdResult<()>
where
    F: FnMut(u64, ChdExtractedOut) -> ChdResult<Vec<u8>>,
{
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
                if cancel.is_cancelled() {
                    return Err(ChdError::Cancelled);
                }
                let entry = resolve_entry(map, chunk_idx as u32)?;
                Ok(ChdExtractWork { entry })
            },
            |seq, out| -> ChdResult<()> {
                let bytes = shape(seq, out)?;
                let len = bytes.len() as u64;
                write_tx
                    .send(bytes)
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
/// are folded into the hash, matching chdman's raw SHA-1
/// coverage rule.
pub(crate) fn verify_hunks(
    pool: &Pool<ChdExtractWork, ChdExtractedOut, ChdError>,
    map: &[MapEntry],
    raw_sha1: &mut Sha1,
    hunk_bytes: usize,
    logical_bytes: u64,
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
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
            if cancel.is_cancelled() {
                return Err(ChdError::Cancelled);
            }
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

/// Digest-side variant of [`extract_hunks`]: the pool decodes every
/// hunk in parallel and the ordered consume closure feeds each
/// frame's payload slice into its track hasher (`hashers[track]`)
/// and into the whole-image hasher. Shaping matches `extract_hunks`
/// byte-for-byte (same per-frame slice widths, same drop of padding
/// frames past the CHT2 counts), so the per-track digests reproduce
/// the bins `chdman extractcd` would write and `whole` reproduces the
/// single concatenated bin.
///
/// Routing is per FRAME via `frame_track`, never by byte offset into
/// the shaped output: track datasizes vary (2048/2336/2352) and one
/// hunk can straddle a track boundary, so only the frame index is a
/// reliable key.
#[allow(clippy::too_many_arguments)]
pub(crate) fn digest_hunks_per_track(
    pool: &Pool<ChdExtractWork, ChdExtractedOut, ChdError>,
    map: &[MapEntry],
    hunk_bytes: usize,
    frame_sizes: &[usize],
    frame_track: &[usize],
    frame_audio: &[bool],
    hashers: &mut [MultiHasher],
    whole: &mut MultiHasher,
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
) -> ChdResult<()> {
    let hunk_count = map.len() as u64;
    let max_in_flight = parallelism() * 2;
    let frames_per_hunk = hunk_bytes / FRAME_SIZE;
    let total_frames = frame_sizes.len();

    drive(
        pool,
        hunk_count,
        max_in_flight,
        |chunk_idx| -> ChdResult<ChdExtractWork> {
            if cancel.is_cancelled() {
                return Err(ChdError::Cancelled);
            }
            let entry = resolve_entry(map, chunk_idx as u32)?;
            Ok(ChdExtractWork { entry })
        },
        |seq, mut out| -> ChdResult<()> {
            let first_frame = seq as usize * frames_per_hunk;
            let frames_in_hunk = frames_per_hunk.min(total_frames.saturating_sub(first_frame));
            let mut folded = 0u64;
            for frame in 0..frames_in_hunk {
                let idx = first_frame + frame;
                let off = frame * FRAME_SIZE;
                let size = frame_sizes[idx];
                // Match the extracted bin: audio frames are stored
                // big-endian and swapped back on the way out.
                if frame_audio[idx] {
                    swap_audio_sector(&mut out.hunk[off..off + size]);
                }
                let slice = &out.hunk[off..off + size];
                hashers[frame_track[idx]].update(slice);
                whole.update(slice);
                folded += slice.len() as u64;
            }
            bytes_done.fetch_add(folded, Ordering::Relaxed);
            Ok(())
        },
    )
}

/// DVD-mode digest fold: decode every hunk in order and feed the
/// decoded bytes (capped at `logical_bytes` on the final partial
/// hunk) into `whole`. Same coverage rule as [`extract_hunks_dvd`]
/// and [`verify_hunks`], but the output is a multi-algorithm digest
/// of the flat ISO instead of a written file or a lone SHA-1.
pub(crate) fn digest_hunks_dvd(
    pool: &Pool<ChdExtractWork, ChdExtractedOut, ChdError>,
    map: &[MapEntry],
    hunk_bytes: usize,
    logical_bytes: u64,
    whole: &mut MultiHasher,
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
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
            if cancel.is_cancelled() {
                return Err(ChdError::Cancelled);
            }
            let entry = resolve_entry(map, chunk_idx as u32)?;
            Ok(ChdExtractWork { entry })
        },
        |_seq, out| -> ChdResult<()> {
            let take = bytes_remaining.min(hunk_bytes_u64) as usize;
            whole.update(&out.hunk[..take]);
            bytes_remaining = bytes_remaining.saturating_sub(hunk_bytes_u64);
            bytes_done.fetch_add(take as u64, Ordering::Relaxed);
            Ok(())
        },
    )
}
