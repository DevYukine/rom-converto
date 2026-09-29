//! Worker-pool Z3DS frame decompressor.
//!
//! Uses the seek table at the tail of the compressed payload to
//! schedule frame decode in parallel: N workers each hold an
//! `Arc<std::fs::File>` and a persistent `zstd::bulk::Decompressor`,
//! positional-read their assigned frame via `file_read_exact_at`,
//! decompress into a scratch `Vec<u8>`, and ship the bytes to a
//! dedicated writer thread inside `std::thread::scope`. `drive()`'s
//! reorder buffer guarantees the writer sees frames in strict
//! sequence, so the output file is identical to a single-threaded
//! pass.
//!
//! Peak working memory is bounded by `max_in_flight * max_frame_size`
//! instead of `compressed_size + uncompressed_size`, which drops the
//! 631 MB CIA case from ~1.3 GB peak to ~256 MB peak and keeps a
//! multi-GB input from exhausting RAM regardless of file size.

use crate::nintendo::ctr::z3ds::error::{Z3dsError, Z3dsResult};
use crate::nintendo::ctr::z3ds::seekable::{FrameEntry, parse_seek_table, read_seek_table_footer};
use crate::util::CancelToken;
use crate::util::Cancelled;
use crate::util::extent_end;
use crate::util::hash::{FileDigests, HashAlgo, MultiHasher};
use crate::util::positional_reader::PositionalReader;
use crate::util::pread::file_read_exact_at;
use crate::util::worker_pool::{Pool, PoolChannelClosed, Worker, drive};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// One frame worth of work: where to read the compressed bytes from
/// in the shared file, and how many uncompressed bytes the worker
/// is expected to produce.
pub(crate) struct Z3dsDecompressWork {
    pub(crate) file_offset: u64,
    pub(crate) compressed_size: u32,
    pub(crate) uncompressed_size: u32,
}

/// Decoded frame bytes, sized exactly to the frame's declared uncompressed size.
pub(super) struct Z3dsDecompressedFrame {
    pub bytes: Vec<u8>,
}

/// Per-thread Z3DS decompress worker. Owns the shared file handle
/// and a persistent `zstd::bulk::Decompressor` so the zstd DCtx
/// (dictionary tables, window buffer) is allocated exactly once per
/// thread.
pub(super) struct Z3dsDecompressWorker {
    file: Arc<std::fs::File>,
    decoder: zstd::bulk::Decompressor<'static>,
}

impl Z3dsDecompressWorker {
    pub fn new(file: Arc<std::fs::File>) -> Z3dsResult<Self> {
        let decoder = zstd::bulk::Decompressor::new()?;
        Ok(Self { file, decoder })
    }
}

impl Worker<Z3dsDecompressWork, Z3dsDecompressedFrame, Z3dsError> for Z3dsDecompressWorker {
    fn process(&mut self, work: Z3dsDecompressWork) -> Z3dsResult<Z3dsDecompressedFrame> {
        let mut compressed = vec![0u8; work.compressed_size as usize];
        file_read_exact_at(&self.file, &mut compressed, work.file_offset)?;
        // `decompress` sizes the output to `capacity`; pass the exact
        // declared uncompressed size as the cap so allocation happens once
        // and libzstd writes straight in.
        let bytes = self
            .decoder
            .decompress(&compressed, work.uncompressed_size as usize)?;
        Ok(Z3dsDecompressedFrame { bytes })
    }
}

pub(super) fn make_z3ds_decompress_workers(
    n: usize,
    file: &Arc<std::fs::File>,
) -> Z3dsResult<Vec<Z3dsDecompressWorker>> {
    (0..n)
        .map(|_| Z3dsDecompressWorker::new(file.clone()))
        .collect()
}

/// Plans one decompress invocation from the seek table at the end of the
/// payload, returning one work item per compressed frame in submission order.
pub(crate) fn plan_decompress_work<R: Read + Seek>(
    mut reader: R,
    payload_offset: u64,
    compressed_size: u64,
) -> Z3dsResult<Vec<Z3dsDecompressWork>> {
    let file_len = reader.seek(SeekFrom::End(0))?;
    let payload_end = extent_end(payload_offset, compressed_size, file_len).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "compressed payload overruns input file",
        )
    })?;
    let footer_offset = payload_end.checked_sub(9).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "compressed payload too small for seek-table footer",
        )
    })?;
    let mut footer = [0u8; 9];
    reader.seek(SeekFrom::Start(footer_offset))?;
    reader.read_exact(&mut footer)?;
    let (_num_frames, skippable_total) = read_seek_table_footer(&footer)?;

    if skippable_total > compressed_size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "seek table larger than compressed payload",
        )
        .into());
    }
    let frame_start = payload_end.checked_sub(skippable_total).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "seek table extent underflow",
        )
    })?;
    if extent_end(frame_start, skippable_total, file_len).is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "seek table overruns input file",
        )
        .into());
    }
    let table_size = usize::try_from(skippable_total).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "seek table does not fit in memory address space",
        )
    })?;
    let mut frame_bytes = vec![0u8; table_size];
    reader.seek(SeekFrom::Start(frame_start))?;
    reader.read_exact(&mut frame_bytes)?;
    let entries: Vec<FrameEntry> = parse_seek_table(&frame_bytes)?;

    let mut work = Vec::with_capacity(entries.len());
    let mut cursor = payload_offset;
    let mut frames_bytes_sum = 0u64;
    for e in &entries {
        let frame_end =
            extent_end(cursor, u64::from(e.compressed_size), frame_start).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "seek-table frame overruns its payload",
                )
            })?;
        work.push(Z3dsDecompressWork {
            file_offset: cursor,
            compressed_size: e.compressed_size,
            uncompressed_size: e.decompressed_size,
        });
        cursor = frame_end;
        frames_bytes_sum = frames_bytes_sum
            .checked_add(u64::from(e.compressed_size))
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "frame sizes overflow")
            })?;
    }
    let expected = frames_bytes_sum
        .checked_add(skippable_total)
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "payload sizes overflow")
        })?;
    if expected != compressed_size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "seek table disagrees with header: sum of frames ({frames_bytes_sum}) + \
                 seek table ({skippable_total}) != compressed_size ({compressed_size})"
            ),
        )
        .into());
    }
    Ok(work)
}

pub(super) fn decompression_admission(
    work_items: &[Z3dsDecompressWork],
    requested_workers: usize,
) -> Option<crate::util::worker_pool::Admission> {
    let max_uncompressed = work_items
        .iter()
        .map(|w| w.uncompressed_size)
        .max()
        .unwrap_or(0) as usize;
    let max_compressed = work_items
        .iter()
        .map(|w| w.compressed_size)
        .max()
        .unwrap_or(0) as usize;
    let frame_bytes = max_compressed.saturating_add(max_uncompressed);
    crate::util::worker_pool::Budget {
        codec_per_worker: crate::util::worker_pool::zstd_dctx_estimate(),
        per_job: frame_bytes,
        writer_slot: max_uncompressed,
        fixed: 0,
    }
    .admit(requested_workers, work_items.len() as u64)
}

const STREAM_CHUNK_SIZE: usize = 4 * 1024 * 1024;

pub(super) fn stream_decompress_frames(
    file: &Arc<std::fs::File>,
    work_items: &[Z3dsDecompressWork],
    mut on_chunk: impl FnMut(&[u8]) -> Z3dsResult<()>,
) -> Z3dsResult<u64> {
    let mut total = 0u64;
    let mut output = vec![0u8; STREAM_CHUNK_SIZE];
    for work in work_items {
        let source = PositionalReader::new(
            file.clone(),
            work.file_offset,
            u64::from(work.compressed_size),
        );
        let mut decoder = zstd::stream::read::Decoder::new(source)?;
        let mut frame_total = 0u64;
        loop {
            let n = decoder.read(&mut output)?;
            if n == 0 {
                break;
            }
            frame_total += n as u64;
            if frame_total > u64::from(work.uncompressed_size) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "zstd frame exceeds declared decompressed size",
                )
                .into());
            }
            on_chunk(&output[..n])?;
        }
        // A short frame is passed through like the pooled bulk path does;
        // the digest path compares the total against the header.
        total += frame_total;
    }
    Ok(total)
}

/// Single-worker fallback for frames that exceed the memory target:
/// decodes each frame as a zstd stream and writes it in
/// `STREAM_CHUNK_SIZE` pieces, so no frame is ever held whole.
///
/// Returns the total number of bytes written.
pub(super) fn stream_decompress_to_writer(
    file: &Arc<std::fs::File>,
    work_items: &[Z3dsDecompressWork],
    writer: &mut BufWriter<std::fs::File>,
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
) -> Z3dsResult<u64> {
    stream_decompress_frames(file, work_items, |chunk| {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        writer.write_all(chunk)?;
        bytes_done.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        Ok(())
    })
}
/// Drives the worker-pool decompress pipeline, handing decoded frames to a
/// dedicated writer thread in strict frame order.
///
/// Returns the total number of bytes handed to the writer.
pub(super) fn decompress_frames(
    admission: crate::util::worker_pool::Admission,
    pool: &Pool<Z3dsDecompressWork, Z3dsDecompressedFrame, Z3dsError>,
    writer: &mut BufWriter<std::fs::File>,
    work_items: Vec<Z3dsDecompressWork>,
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
) -> Z3dsResult<u64> {
    let num_frames = work_items.len() as u64;
    if num_frames == 0 {
        return Ok(0);
    }

    let max_in_flight = admission.max_in_flight;
    let mut written = 0u64;
    crate::util::worker_pool::with_writer_thread(
        writer,
        admission.writer_capacity,
        Z3dsError::WorkerPoolPanic,
        |write_tx| {
            // Moved into the closure so `produce` hands items out without cloning.
            let mut work_iter = work_items.into_iter();
            drive(
                pool,
                num_frames,
                max_in_flight,
                |_seq| -> Z3dsResult<Z3dsDecompressWork> {
                    if cancel.is_cancelled() {
                        return Err(Cancelled.into());
                    }
                    work_iter.next().ok_or_else(|| {
                        Z3dsError::IoError(std::io::Error::other(
                            "work iterator exhausted before drive() finished",
                        ))
                    })
                },
                |_seq, out| -> Z3dsResult<()> {
                    let len = out.bytes.len() as u64;
                    written += len;
                    write_tx
                        .send(out.bytes)
                        .map_err(|_| Z3dsError::WorkerPoolClosed(PoolChannelClosed))?;
                    bytes_done.fetch_add(len, Ordering::Relaxed);
                    Ok(())
                },
            )
        },
    )?;
    Ok(written)
}

/// Digest-side twin of [`decompress_frames`]: folds each decoded frame into a
/// [`MultiHasher`] instead of writing it. `drive`'s reorder buffer keeps strict
/// frame order, so the digest matches one taken over the decompressed output.
pub(super) fn digest_frames(
    admission: crate::util::worker_pool::Admission,
    pool: &Pool<Z3dsDecompressWork, Z3dsDecompressedFrame, Z3dsError>,
    work_items: Vec<Z3dsDecompressWork>,
    algos: &[HashAlgo],
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
) -> Z3dsResult<FileDigests> {
    let num_frames = work_items.len() as u64;
    let total_uncompressed: u64 = work_items.iter().map(|w| w.uncompressed_size as u64).sum();
    let mut hasher = MultiHasher::new(algos);
    if num_frames == 0 {
        return Ok(hasher.finalize(0));
    }

    let max_in_flight = admission.max_in_flight;
    let mut work_iter = work_items.into_iter();
    drive(
        pool,
        num_frames,
        max_in_flight,
        |_seq| -> Z3dsResult<Z3dsDecompressWork> {
            if cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            work_iter.next().ok_or_else(|| {
                Z3dsError::IoError(std::io::Error::other(
                    "work iterator exhausted before drive() finished",
                ))
            })
        },
        |_seq, out| -> Z3dsResult<()> {
            hasher.update(&out.bytes);
            bytes_done.fetch_add(out.bytes.len() as u64, Ordering::Relaxed);
            Ok(())
        },
    )?;

    Ok(hasher.finalize(total_uncompressed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::ctr::z3ds::compress_worker::{
        Z3dsCompressWork, Z3dsCompressedFrame, compression_admission, encode_seekable,
        make_z3ds_compress_workers,
    };
    use crate::util::worker_pool::parallelism;
    use std::io::BufReader;

    fn write_z3ds_payload(input: &[u8], max_frame_size: usize, level: i32) -> Vec<u8> {
        // Write the raw Z3DS payload (frames + seek table, no
        // Z3dsHeader wrapper) to a temp file via the pooled
        // encoder, then read it back as a Vec for round-trip tests.
        let tmp = tempfile::tempdir().unwrap();
        let in_path = tmp.path().join("in.bin");
        let out_path = tmp.path().join("out.bin");
        std::fs::write(&in_path, input).unwrap();

        let n_threads = parallelism();
        let num_frames = (input.len() as u64).div_ceil(max_frame_size as u64);
        let admission =
            compression_admission(level, max_frame_size, num_frames, n_threads).expect("admission");
        let workers = make_z3ds_compress_workers(admission.workers, level).unwrap();
        let pool: Pool<Z3dsCompressWork, Z3dsCompressedFrame, Z3dsError> = Pool::spawn(workers);

        let in_file = std::fs::File::open(&in_path).unwrap();
        let mut reader = BufReader::with_capacity(4 * 1024 * 1024, in_file);
        let out_file = std::fs::File::create(&out_path).unwrap();
        let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, out_file);

        let bytes_done = Arc::new(AtomicU64::new(0));
        encode_seekable(
            &pool,
            admission,
            &mut reader,
            &mut writer,
            max_frame_size,
            input.len() as u64,
            &bytes_done,
            &crate::util::CancelToken::new(),
        )
        .unwrap();
        writer.flush().unwrap();
        drop(writer);
        pool.shutdown();

        std::fs::read(&out_path).unwrap()
    }

    fn decompress_payload(payload: &[u8]) -> Vec<u8> {
        // Mirrors the production decompress_rom path without the
        // Z3DS header layer: write the raw payload, pread the seek
        // table, dispatch to the pool, collect output via a
        // BufWriter<File>, read back.
        let tmp = tempfile::tempdir().unwrap();
        let in_path = tmp.path().join("payload.bin");
        let out_path = tmp.path().join("out.bin");
        std::fs::write(&in_path, payload).unwrap();

        let in_file = Arc::new(std::fs::File::open(&in_path).unwrap());
        let out_file = std::fs::File::create(&out_path).unwrap();
        let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, out_file);

        let work_items = plan_decompress_work(&*in_file, 0, payload.len() as u64).unwrap();
        let admission = decompression_admission(&work_items, parallelism()).expect("admission");

        let n_threads = admission.workers;
        let workers = make_z3ds_decompress_workers(n_threads, &in_file).unwrap();
        let pool: Pool<Z3dsDecompressWork, Z3dsDecompressedFrame, Z3dsError> = Pool::spawn(workers);

        let bytes_done = Arc::new(AtomicU64::new(0));
        decompress_frames(
            admission,
            &pool,
            &mut writer,
            work_items,
            &bytes_done,
            &crate::util::CancelToken::new(),
        )
        .unwrap();
        writer.flush().unwrap();
        drop(writer);
        pool.shutdown();

        std::fs::read(&out_path).unwrap()
    }

    #[test]
    fn decompress_roundtrips_multi_frame() {
        let original: Vec<u8> = (0u8..=255).cycle().take(200_000).collect();
        let payload = write_z3ds_payload(&original, 4096, 0);
        let decoded = decompress_payload(&payload);
        assert_eq!(original, decoded);
    }

    #[test]
    fn decompress_roundtrips_exact_frame_boundary() {
        let original = vec![0xABu8; 16_384];
        let payload = write_z3ds_payload(&original, 4096, 0);
        let decoded = decompress_payload(&payload);
        assert_eq!(original, decoded);
    }

    #[test]
    fn decompress_roundtrips_short_final_frame() {
        // 12,345 bytes with a 4,096-byte frame size = 3 full frames
        // plus a 57-byte final frame. Exercises the uneven tail.
        let original: Vec<u8> = (0u8..=99).cycle().take(12_345).collect();
        let payload = write_z3ds_payload(&original, 4096, 0);
        let decoded = decompress_payload(&payload);
        assert_eq!(original, decoded);
    }

    #[test]
    fn decompress_roundtrips_single_frame() {
        let original = b"small, fits in one frame".to_vec();
        let payload = write_z3ds_payload(&original, 1 << 20, 0);
        let decoded = decompress_payload(&payload);
        assert_eq!(original, decoded);
    }

    #[test]
    fn decompress_is_deterministic() {
        let original: Vec<u8> = (0u8..=199).cycle().take(80_000).collect();
        let payload = write_z3ds_payload(&original, 4096, 0);
        let a = decompress_payload(&payload);
        let b = decompress_payload(&payload);
        assert_eq!(a, b);
        assert_eq!(a, original);
    }

    /// Regression: a seek table produced by an external tool with
    /// the checksum flag set (descriptor bit 7 = XXH64 per entry,
    /// so each entry is 12 bytes instead of 8) must parse correctly
    /// and round-trip. Synthesises such a payload by hand from two
    /// zstd frames and a hand-rolled skippable frame, then runs it
    /// through the pooled decoder.
    #[test]
    fn decompress_handles_checksum_flag_seek_table() {
        // Two independent frames, chosen so the concatenation
        // round-trips cleanly through the per-frame pread path.
        let a = b"first frame, lorem ipsum dolor sit amet";
        let b = b"second frame, consectetur adipiscing elit";
        let original: Vec<u8> = a.iter().chain(b.iter()).copied().collect();

        let frame_a = zstd::bulk::Compressor::new(0).unwrap().compress(a).unwrap();
        let frame_b = zstd::bulk::Compressor::new(0).unwrap().compress(b).unwrap();

        // Hand-build the skippable frame with 12-byte entries (the
        // checksum slots are set to zero since they are not validated
        // here), descriptor = 0x80 (checksum flag set), SEEKABLE
        // magic at the tail. Matches the layout produced by
        // external seekable-zstd encoders.
        let num_frames = 2u32;
        let entry_size = 12;
        let payload_size: u32 = num_frames * entry_size + 9;
        let mut skippable = Vec::new();
        skippable.extend_from_slice(&0x184D2A5Eu32.to_le_bytes()); // SKIPPABLE_MAGIC
        skippable.extend_from_slice(&payload_size.to_le_bytes());
        // entry 0
        skippable.extend_from_slice(&(frame_a.len() as u32).to_le_bytes());
        skippable.extend_from_slice(&(a.len() as u32).to_le_bytes());
        skippable.extend_from_slice(&0u32.to_le_bytes()); // checksum slot
        // entry 1
        skippable.extend_from_slice(&(frame_b.len() as u32).to_le_bytes());
        skippable.extend_from_slice(&(b.len() as u32).to_le_bytes());
        skippable.extend_from_slice(&0u32.to_le_bytes()); // checksum slot
        // footer
        skippable.extend_from_slice(&num_frames.to_le_bytes());
        skippable.push(0x80); // descriptor: checksum flag set
        skippable.extend_from_slice(&0x8F92EAB1u32.to_le_bytes()); // SEEKABLE_MAGIC

        let mut payload = Vec::new();
        payload.extend_from_slice(&frame_a);
        payload.extend_from_slice(&frame_b);
        payload.extend_from_slice(&skippable);

        let decoded = decompress_payload(&payload);
        assert_eq!(original, decoded);
    }

    #[test]
    fn plan_rejects_corrupted_compressed_size() {
        let original: Vec<u8> = (0u8..=99).cycle().take(10_000).collect();
        let payload = write_z3ds_payload(&original, 2048, 0);
        let tmp = tempfile::tempdir().unwrap();
        let in_path = tmp.path().join("payload.bin");
        std::fs::write(&in_path, &payload).unwrap();
        let in_file = std::fs::File::open(&in_path).unwrap();
        // Claim the payload is 32 bytes shorter than it really is,
        // which makes the seek-table self-check fail at plan time.
        let result = plan_decompress_work(&in_file, 0, payload.len() as u64 - 32);
        assert!(
            result.is_err(),
            "plan_decompress_work should reject a truncated compressed_size"
        );
    }

    #[test]
    fn plan_rejects_untrusted_frame_count_before_allocating_table() {
        let mut payload = vec![0u8; 9];
        payload[..4].copy_from_slice(&u32::MAX.to_le_bytes());
        payload[4] = 0;
        payload[5..].copy_from_slice(&0x8F92EAB1u32.to_le_bytes());
        let result = plan_decompress_work(std::io::Cursor::new(payload), 0, 9);
        assert!(result.is_err());
    }

    #[test]
    fn four_mib_frames_retain_requested_parallelism() {
        let work: Vec<_> = (0..8)
            .map(|_| Z3dsDecompressWork {
                file_offset: 0,
                compressed_size: 4 * 1024 * 1024,
                uncompressed_size: 4 * 1024 * 1024,
            })
            .collect();
        let admission = decompression_admission(&work, 8).expect("admission");
        assert!(admission.workers >= 8);
        assert!(admission.max_in_flight >= 8);
    }

    #[test]
    fn streaming_decoder_matches_bulk_path() {
        let original: Vec<u8> = (0u8..=255).cycle().take(256 * 1024).collect();
        let payload = write_z3ds_payload(&original, 64 * 1024, 0);
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("stream.z3ds");
        std::fs::write(&path, &payload).unwrap();
        let file = Arc::new(std::fs::File::open(path).unwrap());
        let work = plan_decompress_work(&*file, 0, payload.len() as u64).unwrap();
        let mut streamed = Vec::new();
        stream_decompress_frames(&file, &work, |chunk| {
            assert!(chunk.len() <= STREAM_CHUNK_SIZE);
            streamed.extend_from_slice(chunk);
            Ok(())
        })
        .unwrap();
        assert_eq!(streamed, decompress_payload(&payload));
        assert_eq!(streamed, original);
    }
}
