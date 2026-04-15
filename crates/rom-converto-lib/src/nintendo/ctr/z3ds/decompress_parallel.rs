//! Parallel Z3DS frame decompressor.
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
use crate::util::pread::file_read_exact_at;
use crate::util::worker_pool::{Pool, Worker, drive, parallelism};
use std::io::{BufWriter, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// One frame worth of work: where to read the compressed bytes from
/// in the shared file, and how many uncompressed bytes the worker
/// is expected to produce.
pub(super) struct Z3dsDecompressWork {
    pub file_offset: u64,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
}

/// Decoded frame bytes. Sized exactly to the frame's declared
/// uncompressed size; the writer closure forwards this to a
/// `BufWriter<std::fs::File>` via a dedicated writer thread.
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
        // declared uncompressed size as the cap so we allocate once
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

/// Plan one decompress invocation: read the seek-table footer from
/// the end of the payload, read the full skippable frame, parse
/// entries, and build absolute `file_offset`s for each compressed
/// frame.
///
/// Returns the work items in submission order. The caller passes
/// them one by one through `drive()`.
pub(super) fn plan_decompress_work(
    file: &std::fs::File,
    payload_offset: u64,
    compressed_size: u64,
) -> Z3dsResult<Vec<Z3dsDecompressWork>> {
    // 1. Read the 9-byte footer at the very end of the payload.
    let footer_offset = payload_offset
        .checked_add(compressed_size)
        .and_then(|end| end.checked_sub(9))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "compressed payload too small for seek-table footer",
            )
        })?;
    let mut footer = [0u8; 9];
    file_read_exact_at(file, &mut footer, footer_offset)?;
    let (_num_frames, skippable_total) = read_seek_table_footer(&footer)?;

    // 2. Read the full skippable frame (header + entries + footer).
    let frame_start = payload_offset
        .checked_add(compressed_size)
        .and_then(|end| end.checked_sub(skippable_total))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "compressed payload too small for seek table",
            )
        })?;
    let mut frame_bytes = vec![0u8; skippable_total as usize];
    file_read_exact_at(file, &mut frame_bytes, frame_start)?;
    let entries: Vec<FrameEntry> = parse_seek_table(&frame_bytes)?;

    // 3. Build work items with cumulative offsets starting from the
    //    payload offset. Sanity-check that the sum of compressed
    //    frame sizes + seek-table size equals the declared
    //    compressed_size, so a corrupted file fails fast.
    let mut work = Vec::with_capacity(entries.len());
    let mut cursor = payload_offset;
    let mut frames_bytes_sum: u64 = 0;
    for e in &entries {
        work.push(Z3dsDecompressWork {
            file_offset: cursor,
            compressed_size: e.compressed_size,
            uncompressed_size: e.decompressed_size,
        });
        cursor = cursor.saturating_add(e.compressed_size as u64);
        frames_bytes_sum = frames_bytes_sum.saturating_add(e.compressed_size as u64);
    }
    let expected = frames_bytes_sum + skippable_total;
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

/// Pick a conservative `max_in_flight` cap that respects both the
/// thread count and the expected per-frame working set.
///
/// Decompress peak memory ≈ `max_in_flight * (compressed_size +
/// uncompressed_size)` per frame. For 32 MB CIA frames capping at 4
/// caps the working set at ~256 MB. For 256 KB frames
/// `parallelism() * 2` is typically 32-64 on modern CPUs which
/// stays well under 32 MB.
fn pick_max_in_flight(max_uncompressed: u32) -> usize {
    const LARGE_FRAME_CUTOFF: u32 = 4 * 1024 * 1024;
    if max_uncompressed >= LARGE_FRAME_CUTOFF {
        4
    } else {
        parallelism() * 2
    }
}

/// Drive the parallel Z3DS decompress pipeline:
///
/// * **Planner (main thread, before spawning)**: reads the seek
///   table footer and skippable frame via positional reads and
///   builds one `Z3dsDecompressWork` per frame with an absolute
///   `file_offset`.
/// * **Workers (pool threads)**: receive work items, positional-read
///   the compressed frame via the shared `Arc<std::fs::File>`,
///   decompress into a fresh `Vec<u8>` sized to the declared
///   uncompressed size through a persistent `zstd::bulk::Decompressor`.
/// * **Writer (dedicated thread)**: drains a bounded channel and
///   calls `write_all` on the output `BufWriter` in strict frame
///   order (the driver's `drive()` reorder buffer).
pub(super) fn parallel_decompress_frames(
    pool: &Pool<Z3dsDecompressWork, Z3dsDecompressedFrame, Z3dsError>,
    writer: &mut BufWriter<std::fs::File>,
    work_items: Vec<Z3dsDecompressWork>,
    bytes_done: &Arc<AtomicU64>,
) -> Z3dsResult<()> {
    let num_frames = work_items.len() as u64;
    if num_frames == 0 {
        return Ok(());
    }

    // Derive the in-flight cap from the largest declared frame so
    // the CIA case (32 MB frames) stays under its memory budget.
    let max_uncompressed = work_items
        .iter()
        .map(|w| w.uncompressed_size)
        .max()
        .unwrap_or(0);
    let max_in_flight = pick_max_in_flight(max_uncompressed);

    let (write_tx, write_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(max_in_flight * 2);

    let scope_result: Z3dsResult<()> = std::thread::scope(|s| {
        let writer_slot: &mut BufWriter<std::fs::File> = writer;
        let writer_handle = s.spawn(move || -> Z3dsResult<()> {
            while let Ok(bytes) = write_rx.recv() {
                writer_slot.write_all(&bytes)?;
            }
            Ok(())
        });

        // Move the work items into the closure so `produce` can
        // hand them out by sequence index without cloning.
        let mut work_iter = work_items.into_iter();
        let drive_result = drive(
            pool,
            num_frames,
            max_in_flight,
            |_seq| -> Z3dsResult<Z3dsDecompressWork> {
                work_iter.next().ok_or_else(|| {
                    Z3dsError::IoError(std::io::Error::other(
                        "work iterator exhausted before drive() finished",
                    ))
                })
            },
            |_seq, out| -> Z3dsResult<()> {
                let len = out.bytes.len() as u64;
                write_tx
                    .send(out.bytes)
                    .map_err(|_| Z3dsError::WorkerPoolClosed)?;
                bytes_done.fetch_add(len, Ordering::Relaxed);
                Ok(())
            },
        );

        drop(write_tx);
        let writer_result = writer_handle
            .join()
            .unwrap_or_else(|_| Err(Z3dsError::WorkerPoolPanic));
        drive_result?;
        writer_result
    });

    scope_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::ctr::z3ds::compress_parallel::{
        Z3dsCompressWork, Z3dsCompressedFrame, make_z3ds_compress_workers, parallel_encode_seekable,
    };
    use std::io::BufReader;

    fn write_z3ds_payload(input: &[u8], max_frame_size: usize, level: i32) -> Vec<u8> {
        // Write the raw Z3DS payload (frames + seek table, no
        // Z3dsHeader wrapper) to a temp file via the parallel
        // encoder, then read it back as a Vec for round-trip tests.
        let tmp = tempfile::tempdir().unwrap();
        let in_path = tmp.path().join("in.bin");
        let out_path = tmp.path().join("out.bin");
        std::fs::write(&in_path, input).unwrap();

        let n_threads = parallelism();
        let workers = make_z3ds_compress_workers(n_threads, level).unwrap();
        let pool: Pool<Z3dsCompressWork, Z3dsCompressedFrame, Z3dsError> = Pool::spawn(workers);

        let in_file = std::fs::File::open(&in_path).unwrap();
        let mut reader = BufReader::with_capacity(4 * 1024 * 1024, in_file);
        let out_file = std::fs::File::create(&out_path).unwrap();
        let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, out_file);

        let bytes_done = Arc::new(AtomicU64::new(0));
        parallel_encode_seekable(
            &pool,
            &mut reader,
            &mut writer,
            max_frame_size,
            input.len() as u64,
            &bytes_done,
        )
        .unwrap();
        writer.flush().unwrap();
        drop(writer);
        pool.shutdown();

        std::fs::read(&out_path).unwrap()
    }

    fn decompress_parallel_payload(payload: &[u8]) -> Vec<u8> {
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

        let work_items = plan_decompress_work(&in_file, 0, payload.len() as u64).unwrap();

        let n_threads = parallelism();
        let workers = make_z3ds_decompress_workers(n_threads, &in_file).unwrap();
        let pool: Pool<Z3dsDecompressWork, Z3dsDecompressedFrame, Z3dsError> = Pool::spawn(workers);

        let bytes_done = Arc::new(AtomicU64::new(0));
        parallel_decompress_frames(&pool, &mut writer, work_items, &bytes_done).unwrap();
        writer.flush().unwrap();
        drop(writer);
        pool.shutdown();

        std::fs::read(&out_path).unwrap()
    }

    #[test]
    fn parallel_decompress_roundtrips_multi_frame() {
        let original: Vec<u8> = (0u8..=255).cycle().take(200_000).collect();
        let payload = write_z3ds_payload(&original, 4096, 0);
        let decoded = decompress_parallel_payload(&payload);
        assert_eq!(original, decoded);
    }

    #[test]
    fn parallel_decompress_roundtrips_exact_frame_boundary() {
        let original = vec![0xABu8; 16_384];
        let payload = write_z3ds_payload(&original, 4096, 0);
        let decoded = decompress_parallel_payload(&payload);
        assert_eq!(original, decoded);
    }

    #[test]
    fn parallel_decompress_roundtrips_short_final_frame() {
        // 12,345 bytes with a 4,096-byte frame size = 3 full frames
        // plus a 57-byte final frame. Exercises the uneven tail.
        let original: Vec<u8> = (0u8..=99).cycle().take(12_345).collect();
        let payload = write_z3ds_payload(&original, 4096, 0);
        let decoded = decompress_parallel_payload(&payload);
        assert_eq!(original, decoded);
    }

    #[test]
    fn parallel_decompress_roundtrips_single_frame() {
        let original = b"small, fits in one frame".to_vec();
        let payload = write_z3ds_payload(&original, 1 << 20, 0);
        let decoded = decompress_parallel_payload(&payload);
        assert_eq!(original, decoded);
    }

    #[test]
    fn parallel_decompress_is_deterministic() {
        let original: Vec<u8> = (0u8..=199).cycle().take(80_000).collect();
        let payload = write_z3ds_payload(&original, 4096, 0);
        let a = decompress_parallel_payload(&payload);
        let b = decompress_parallel_payload(&payload);
        assert_eq!(a, b);
        assert_eq!(a, original);
    }

    /// Regression: a seek table produced by an external tool with
    /// the checksum flag set (descriptor bit 7 = XXH64 per entry,
    /// so each entry is 12 bytes instead of 8) must parse correctly
    /// and round-trip. Synthesises such a payload by hand from two
    /// zstd frames and a hand-rolled skippable frame, then runs it
    /// through the parallel decoder.
    #[test]
    fn parallel_decompress_handles_checksum_flag_seek_table() {
        // Two independent frames, chosen so the concatenation
        // round-trips cleanly through the parallel decoder's
        // per-frame pread path.
        let a = b"first frame, lorem ipsum dolor sit amet";
        let b = b"second frame, consectetur adipiscing elit";
        let original: Vec<u8> = a.iter().chain(b.iter()).copied().collect();

        let frame_a = zstd::bulk::Compressor::new(0).unwrap().compress(a).unwrap();
        let frame_b = zstd::bulk::Compressor::new(0).unwrap().compress(b).unwrap();

        // Hand-build the skippable frame with 12-byte entries (the
        // checksum slots are set to zero since we don't validate
        // them), descriptor = 0x80 (checksum flag set), SEEKABLE
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

        let decoded = decompress_parallel_payload(&payload);
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
}
