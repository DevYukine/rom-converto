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

use crate::cd::{FRAME_SIZE, SECTOR_SIZE};
use crate::chd::compression::dvd::DvdCodecSet;
use crate::chd::compression::{CdCodecSet, ChdCodec, ChdCompression, avhuff};
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::map::{COMPRESSION_SELF, MapEntry, crc16_ccitt};
use crate::chd::models::SHA1_BYTES;
use crate::chd::swap_audio_sector;
use crate::laserdisc::avi::{AviFile, LdParams};
use crate::laserdisc::vbi::{VBI_PACKED_BYTES, vbi_metadata_pack, vbi_parse_all};
use crate::util::CancelToken;
use crate::util::worker_pool::{Pool, Worker, drive, parallelism};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::io::{BufReader, BufWriter, Read, Seek, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// YUY16 words hold luma in the high byte, which is the plane VBI codes
/// are read from.
const VBI_LUMA_SHIFT: u32 = 8;

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
    /// SHA-1 over the raw hunk, set only by paths that dedup
    /// identical hunks into `COMPRESSION_SELF` map entries.
    pub sha1: Option<[u8; SHA1_BYTES]>,
}

/// Per-thread CHD compress worker. Owns one persistent
/// [`CdCodecSet`] so LZMA probability tables and deflate state
/// allocate exactly once per thread.
pub(super) struct ChdCompressWorker {
    codecs: CdCodecSet,
}

impl ChdCompressWorker {
    pub fn new(hunk_bytes: usize, codecs: Vec<ChdCodec>, level: Option<i32>) -> ChdResult<Self> {
        Ok(Self {
            codecs: CdCodecSet::new(hunk_bytes, codecs, level)?,
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
            sha1: None,
        })
    }
}

pub(super) fn make_chd_compress_workers(
    n: usize,
    hunk_bytes: usize,
    codecs: &[ChdCodec],
    level: Option<i32>,
) -> ChdResult<Vec<ChdCompressWorker>> {
    (0..n)
        .map(|_| ChdCompressWorker::new(hunk_bytes, codecs.to_vec(), level))
        .collect()
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
            sha1: None,
        })
    }
}

pub(super) fn make_chd_dvd_compress_workers(
    n: usize,
    hunk_bytes: usize,
    codecs: &[ChdCodec],
    level: Option<i32>,
) -> ChdResult<Vec<ChdDvdCompressWorker>> {
    (0..n)
        .map(|_| {
            Ok(ChdDvdCompressWorker {
                codecs: DvdCodecSet::new(hunk_bytes, codecs.to_vec(), level)?,
            })
        })
        .collect()
}

/// Laserdisc worker: one video field per hunk, `avhu` only. chdman
/// refuses to store an uncompressed A/V hunk (the reader rejects them),
/// so a failed or oversized encode is fatal rather than a fallback.
pub(super) struct ChdLdCompressWorker;

impl Worker<ChdCompressWork, ChdCompressedOut, ChdError> for ChdLdCompressWorker {
    fn process(&mut self, work: ChdCompressWork) -> ChdResult<ChdCompressedOut> {
        let crc16 = crc16_ccitt(&work.hunk);
        let sha1: [u8; SHA1_BYTES] = Sha1::digest(&work.hunk).into();
        let compressed = avhuff::encode(&work.hunk)?;
        if compressed.len() >= work.hunk.len() {
            return Err(
                std::io::Error::other("avhuff frame did not compress below the hunk size").into(),
            );
        }
        Ok(ChdCompressedOut {
            compressed,
            compression: ChdCompression::Codec0 as u8,
            crc16,
            sha1: Some(sha1),
        })
    }
}

/// Output side of a compress run: the file being written, the
/// position the next hunk lands at, and the map entries built so far.
pub(super) struct HunkWriteState<'a> {
    pub writer: &'a mut BufWriter<std::fs::File>,
    pub writer_pos: &'a mut u64,
    pub map_entries: &'a mut Vec<MapEntry>,
}

/// Input side shared by the CD and DVD compress paths.
pub(super) struct HunkCompressArgs<'a> {
    pub reader: &'a mut BufReader<std::fs::File>,
    pub raw_sha1: &'a mut Sha1,
    pub hunk_bytes: usize,
    pub bytes_done: &'a Arc<AtomicU64>,
    pub cancel: &'a CancelToken,
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
/// `frame_data[i]` marks the frames read from the source
/// (`sector_data_size` bytes each, 2352 for raw bin tracks, 2048 for
/// MODE1/2048 ISO data); the interleaved per-track padding frames
/// stay zero but are still hashed: chdman includes them in the raw
/// SHA-1.
///
/// `state.writer_pos` is the file position **before** the next
/// compressed hunk would land. The caller owns it and passes it
/// through; this function updates it in place.
pub(super) fn compress_hunks(
    pool: &Pool<ChdCompressWork, ChdCompressedOut, ChdError>,
    state: HunkWriteState<'_>,
    args: HunkCompressArgs<'_>,
    frame_data: &[bool],
    sector_data_size: usize,
    cd_audio_frames: &[bool],
) -> ChdResult<()> {
    let HunkCompressArgs {
        reader,
        raw_sha1,
        hunk_bytes,
        bytes_done,
        cancel,
    } = args;
    let frames_per_hunk = hunk_bytes / FRAME_SIZE;
    let total_sectors = frame_data.len();
    let total_hunks = total_sectors.div_ceil(frames_per_hunk) as u64;

    run_pipeline(
        pool,
        state,
        total_hunks,
        // produce: zero padding on the short final hunk comes for
        // free from the `vec![0; hunk_bytes]` allocation.
        |chunk_idx| -> ChdResult<ChdCompressWork> {
            if cancel.is_cancelled() {
                return Err(ChdError::Cancelled);
            }
            let first_sector = chunk_idx as usize * frames_per_hunk;
            let sectors_in_hunk = frames_per_hunk.min(total_sectors - first_sector);

            let mut hunk = vec![0u8; hunk_bytes];
            let mut read_bytes = 0usize;
            for s in 0..sectors_in_hunk {
                if frame_data[first_sector + s] {
                    let dst = s * FRAME_SIZE;
                    reader.read_exact(&mut hunk[dst..dst + sector_data_size])?;
                    read_bytes += sector_data_size;
                }
            }
            // Byte-swap 16-bit samples of audio-track sectors before
            // hashing or compressing, so the stored data and raw SHA-1
            // match chdman (which swaps audio on ingest).
            for s in 0..sectors_in_hunk {
                if cd_audio_frames
                    .get(first_sector + s)
                    .copied()
                    .unwrap_or(false)
                {
                    let dst = s * FRAME_SIZE;
                    swap_audio_sector(&mut hunk[dst..dst + SECTOR_SIZE]);
                }
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
pub(super) fn compress_hunks_dvd(
    pool: &Pool<ChdCompressWork, ChdCompressedOut, ChdError>,
    state: HunkWriteState<'_>,
    args: HunkCompressArgs<'_>,
    logical_bytes: u64,
) -> ChdResult<()> {
    let HunkCompressArgs {
        reader,
        raw_sha1,
        hunk_bytes,
        bytes_done,
        cancel,
    } = args;
    let total_hunks = logical_bytes.div_ceil(hunk_bytes as u64);

    run_pipeline(
        pool,
        state,
        total_hunks,
        |chunk_idx| -> ChdResult<ChdCompressWork> {
            if cancel.is_cancelled() {
                return Err(ChdError::Cancelled);
            }
            let offset = chunk_idx * hunk_bytes as u64;
            let take = ((logical_bytes - offset) as usize).min(hunk_bytes);

            let mut hunk = vec![0u8; hunk_bytes];
            reader.read_exact(&mut hunk[..take])?;
            raw_sha1.update(&hunk[..take]);
            bytes_done.fetch_add(take as u64, Ordering::Relaxed);
            Ok(ChdCompressWork { hunk })
        },
    )
}

/// Input side of the laserdisc compress path: the AVI, the geometry it
/// resolved to, and the VBI blob the dispatcher fills in field order
/// (empty when the field height is neither NTSC nor PAL).
pub(super) struct LdCompressArgs<'a, R> {
    pub avi: &'a mut AviFile<R>,
    pub params: &'a LdParams,
    pub raw_sha1: &'a mut Sha1,
    pub vbi: &'a mut [u8],
    pub bytes_done: &'a Arc<AtomicU64>,
    pub cancel: &'a CancelToken,
}

/// Audio window of output field `effframe` as `(first_sample, samples)`.
/// The ceiling division runs on the absolute field index, which is what
/// spreads a non-integral samples-per-field count without drift.
pub(super) fn ld_audio_window(params: &LdParams, effframe: u32) -> (u64, u32) {
    let at = |field: u64| {
        (u64::from(params.rate) * field * 1_000_000).div_ceil(u64::from(params.fps_times_1million))
    };
    let first = at(u64::from(effframe));
    (first, (at(u64::from(effframe) + 1) - first) as u32)
}

fn avi_err(err: anyhow::Error) -> ChdError {
    std::io::Error::other(err).into()
}

/// Laserdisc produce path: hunk *n* is output field *n*. Interlaced
/// input takes both fields from one AVI frame, so consecutive fields
/// share a single decode; a field is the frame's rows from `n % 2`
/// stepping by the interlace factor. VBI is parsed on that same field
/// and packed by the dispatcher, since the blob is indexed by field.
pub(super) fn compress_hunks_ld<R: Read + Seek>(
    pool: &Pool<ChdCompressWork, ChdCompressedOut, ChdError>,
    state: HunkWriteState<'_>,
    args: LdCompressArgs<'_, R>,
) -> ChdResult<()> {
    let LdCompressArgs {
        avi,
        params,
        raw_sha1,
        vbi,
        bytes_done,
        cancel,
    } = args;

    let width_u16 =
        u16::try_from(params.width).map_err(|_| avi_err(anyhow::anyhow!("frame width > 65535")))?;
    let height_u16 = u16::try_from(params.height)
        .map_err(|_| avi_err(anyhow::anyhow!("field height > 65535")))?;
    let interlace_factor = if params.interlaced { 2 } else { 1 };
    let width = params.width as usize;
    let height = params.height as usize;
    let hunk_bytes = params.bytes_per_frame as usize;
    let pack_vbi = !vbi.is_empty();

    let mut frame = vec![0u16; width * height * interlace_factor];
    let mut cached_frame: Option<u32> = None;
    let mut field = vec![0u16; width * height];
    let mut audio = vec![Vec::<i16>::new(); params.channels as usize];

    run_pipeline(
        pool,
        state,
        u64::from(params.frame_count),
        |chunk_idx| -> ChdResult<ChdCompressWork> {
            if cancel.is_cancelled() {
                return Err(ChdError::Cancelled);
            }
            let effframe = chunk_idx as u32;
            let source_frame = effframe / interlace_factor as u32;
            if cached_frame != Some(source_frame) {
                avi.read_video_frame(source_frame, &mut frame)
                    .map_err(avi_err)?;
                cached_frame = Some(source_frame);
            }
            let first_row = effframe as usize % interlace_factor;
            for (row, dest) in field.chunks_exact_mut(width).enumerate() {
                let src = (first_row + row * interlace_factor) * width;
                dest.copy_from_slice(&frame[src..src + width]);
            }

            let (first_sample, samples) = ld_audio_window(params, effframe);
            for (channel, buf) in audio.iter_mut().enumerate() {
                buf.clear();
                buf.resize(samples as usize, 0);
                avi.read_sound_samples(channel as u32, first_sample, samples, buf)
                    .map_err(avi_err)?;
            }

            if pack_vbi {
                let metadata = vbi_parse_all(&field, width, width, VBI_LUMA_SHIFT);
                let start = effframe as usize * VBI_PACKED_BYTES;
                let record = vbi
                    .get_mut(start..start + VBI_PACKED_BYTES)
                    .ok_or_else(|| avi_err(anyhow::anyhow!("VBI blob is short of one record")))?;
                vbi_metadata_pack(record, effframe, &metadata);
            }

            let channels: Vec<&[i16]> = audio.iter().map(Vec::as_slice).collect();
            let mut hunk = avhuff::assemble_raw_frame(width_u16, height_u16, &field, &channels)?;
            if hunk.len() > hunk_bytes {
                return Err(avi_err(anyhow::anyhow!(
                    "assembled frame is larger than the hunk size"
                )));
            }
            hunk.resize(hunk_bytes, 0);
            raw_sha1.update(&hunk);
            bytes_done.fetch_add(hunk_bytes as u64, Ordering::Relaxed);
            Ok(ChdCompressWork { hunk })
        },
    )
}

/// Shared compress scaffold: `drive` the pool with the mode-specific
/// `produce` closure while a dedicated writer thread drains a bounded
/// channel, so reads, codec trials, and writes overlap. The consume
/// side is mode-independent: append a map entry, forward bytes,
/// advance the writer position.
///
/// Outputs carrying a raw-hunk SHA-1 additionally go through chdman's
/// self-map dedup: a hunk whose (crc16, sha1) was already written
/// becomes a `COMPRESSION_SELF` back-reference and stores no bytes.
fn run_pipeline<F>(
    pool: &Pool<ChdCompressWork, ChdCompressedOut, ChdError>,
    state: HunkWriteState<'_>,
    total_hunks: u64,
    produce: F,
) -> ChdResult<()>
where
    F: FnMut(u64) -> ChdResult<ChdCompressWork>,
{
    let HunkWriteState {
        writer,
        writer_pos,
        map_entries,
    } = state;
    let max_in_flight = parallelism() * 2;
    let mut local_writer_pos = *writer_pos;
    let mut written_hunks: HashMap<(u16, [u8; SHA1_BYTES]), u64> = HashMap::new();
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
            |seq, out: ChdCompressedOut| -> ChdResult<()> {
                if let Some(sha1) = out.sha1 {
                    match written_hunks.entry((out.crc16, sha1)) {
                        Entry::Occupied(first) => {
                            map_entries.push(MapEntry {
                                compression: COMPRESSION_SELF,
                                length: 0,
                                offset: *first.get(),
                                crc16: 0,
                            });
                            return Ok(());
                        }
                        Entry::Vacant(slot) => {
                            slot.insert(seq);
                        }
                    }
                }
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
