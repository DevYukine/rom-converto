//! Parallel Z3DS frame compressor.
//!
//! Drives a persistent worker pool to compress every frame in
//! parallel, overlaps writes with read + dispatch via a dedicated
//! writer thread inside `std::thread::scope`, and writes the seek
//! table footer on the main thread once every frame has been drained
//! in order.
//!
//! Shape mirrors the CHD writer's `parallel_compress_hunks`. One
//! worker owns one persistent `zstd::bulk::Compressor` for the
//! lifetime of the compress call, so the zstd CCtx is allocated
//! exactly once per thread instead of once per frame.

use crate::nintendo::ctr::z3ds::error::{Z3dsError, Z3dsResult};
use crate::nintendo::ctr::z3ds::seekable::{FrameEntry, write_seek_table};
use crate::util::worker_pool::{Pool, Worker, drive, parallelism};
use std::io::{BufReader, BufWriter, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// One uncompressed frame ready to hand to a worker. The `Vec<u8>`
/// is sized exactly to the bytes read from the input (which matches
/// the configured max frame size except for the short final frame),
/// so the worker can compress it without any further bookkeeping.
pub(super) struct Z3dsCompressWork {
    pub uncompressed: Vec<u8>,
}

/// Compressed frame output. `uncompressed_size` is echoed back so
/// the driver can write the correct `decompressed_size` field in the
/// seek table without keeping a parallel bookkeeping vector.
pub(super) struct Z3dsCompressedFrame {
    pub compressed: Vec<u8>,
    pub uncompressed_size: u32,
}

/// Per-thread Z3DS compress worker. Owns one persistent
/// `zstd::bulk::Compressor` so the zstd CCtx (lookup tables, match
/// finder state, window buffer) is allocated exactly once per thread
/// instead of once per frame.
pub(super) struct Z3dsCompressWorker {
    compressor: zstd::bulk::Compressor<'static>,
}

impl Z3dsCompressWorker {
    pub fn new(level: i32) -> Z3dsResult<Self> {
        let compressor = zstd::bulk::Compressor::new(level)?;
        Ok(Self { compressor })
    }
}

impl Worker<Z3dsCompressWork, Z3dsCompressedFrame, Z3dsError> for Z3dsCompressWorker {
    fn process(&mut self, work: Z3dsCompressWork) -> Z3dsResult<Z3dsCompressedFrame> {
        let uncompressed_size = work.uncompressed.len() as u32;
        let compressed = self.compressor.compress(&work.uncompressed)?;
        Ok(Z3dsCompressedFrame {
            compressed,
            uncompressed_size,
        })
    }
}

pub(super) fn make_z3ds_compress_workers(
    n: usize,
    level: i32,
) -> Z3dsResult<Vec<Z3dsCompressWorker>> {
    (0..n).map(|_| Z3dsCompressWorker::new(level)).collect()
}

/// Pick a conservative `max_in_flight` cap that respects both the
/// thread count and the per-frame working set.
///
/// For 32 MB CIA frames, capping at 4 in-flight keeps the peak
/// working set around 4 x 32 MB = 128 MB (uncompressed queue) plus
/// a similar amount in flight through the workers and writer
/// channel, which stays well inside a reasonable RAM budget even on
/// 8 GB laptops. For 256 KB frames the cap is `parallelism() * 2`,
/// typically 32-64 on modern CPUs, which still fits inside 16 MB.
fn pick_max_in_flight(max_frame_size: usize) -> usize {
    const LARGE_FRAME_CUTOFF: usize = 4 * 1024 * 1024;
    if max_frame_size >= LARGE_FRAME_CUTOFF {
        4
    } else {
        parallelism() * 2
    }
}

/// Read the next frame worth of bytes from `reader` into a freshly
/// allocated `Vec`. Retries on `Interrupted` and handles the short
/// final frame. Returns `None` when the reader is already at EOF.
fn read_frame<R: Read>(reader: &mut R, max_frame_size: usize) -> Z3dsResult<Option<Vec<u8>>> {
    let mut buf = vec![0u8; max_frame_size];
    let mut filled = 0usize;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    if filled == 0 {
        return Ok(None);
    }
    buf.truncate(filled);
    Ok(Some(buf))
}

/// Drive the parallel Z3DS compress pipeline:
///
/// * **Reader (dispatcher thread)**: sequential `BufReader` over the
///   decrypted input. Produces one frame per `drive` call.
/// * **Workers (pool threads)**: receive frames, zstd-compress them
///   through a persistent `bulk::Compressor`, return the compressed
///   bytes plus the original uncompressed size.
/// * **Writer (dedicated thread)**: drains a bounded channel and
///   calls `write_all` on the output `BufWriter` so writes overlap
///   with reads and compresses.
/// * **Seek table**: accumulated on the main thread in the `consume`
///   closure (in strict order via `drive`), then written to the
///   `BufWriter` on the main thread after the writer thread has
///   been joined. Keeps the writer thread focused on one kind of
///   bytes and avoids interleaving frame writes with footer writes.
///
/// Returns the total number of bytes written to `writer` (frames +
/// seek table), which is the value that goes into the Z3DS header's
/// `compressed_size` field.
pub(super) fn parallel_encode_seekable(
    pool: &Pool<Z3dsCompressWork, Z3dsCompressedFrame, Z3dsError>,
    reader: &mut BufReader<std::fs::File>,
    writer: &mut BufWriter<std::fs::File>,
    max_frame_size: usize,
    uncompressed_size: u64,
    bytes_done: &Arc<AtomicU64>,
) -> Z3dsResult<u64> {
    let num_frames = if uncompressed_size == 0 {
        0
    } else {
        uncompressed_size.div_ceil(max_frame_size as u64)
    };
    let max_in_flight = pick_max_in_flight(max_frame_size);

    let mut entries: Vec<FrameEntry> = Vec::with_capacity(num_frames as usize);
    let mut frames_bytes: u64 = 0;

    let (write_tx, write_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(max_in_flight * 2);

    let scope_result: Z3dsResult<()> = std::thread::scope(|s| {
        let writer_slot: &mut BufWriter<std::fs::File> = writer;
        let writer_handle = s.spawn(move || -> Z3dsResult<()> {
            while let Ok(bytes) = write_rx.recv() {
                writer_slot.write_all(&bytes)?;
            }
            Ok(())
        });

        let drive_result = drive(
            pool,
            num_frames,
            max_in_flight,
            // produce: read the next frame from the sequential reader.
            |_seq| -> Z3dsResult<Z3dsCompressWork> {
                let uncompressed = read_frame(reader, max_frame_size)?.unwrap_or_default();
                Ok(Z3dsCompressWork { uncompressed })
            },
            // consume: append seek-table entry, forward bytes to the
            // writer thread, bump progress. Runs in strict seq order
            // so entries come out byte-identical to a sequential
            // pass.
            |_seq, out| -> Z3dsResult<()> {
                entries.push(FrameEntry {
                    compressed_size: out.compressed.len() as u32,
                    decompressed_size: out.uncompressed_size,
                });
                frames_bytes += out.compressed.len() as u64;
                bytes_done.fetch_add(out.uncompressed_size as u64, Ordering::Relaxed);
                write_tx
                    .send(out.compressed)
                    .map_err(|_| Z3dsError::WorkerPoolClosed)?;
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

    scope_result?;

    // Writer thread has exited and released its mutable borrow of
    // `writer`, so the main thread can append the seek table footer
    // directly.
    let footer_bytes = write_seek_table(writer, &entries)?;
    Ok(frames_bytes + footer_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::ctr::z3ds::seekable::{decode_seekable, encode_seekable_streaming};

    fn encode_parallel(input: &[u8], max_frame_size: usize, level: i32) -> Z3dsResult<Vec<u8>> {
        let tmp = tempfile::tempdir().unwrap();
        let in_path = tmp.path().join("in.bin");
        let out_path = tmp.path().join("out.bin");
        std::fs::write(&in_path, input).unwrap();

        let n_threads = parallelism();
        let workers = make_z3ds_compress_workers(n_threads, level)?;
        let pool: Pool<Z3dsCompressWork, Z3dsCompressedFrame, Z3dsError> = Pool::spawn(workers);

        let in_file = std::fs::File::open(&in_path)?;
        let mut reader = BufReader::with_capacity(4 * 1024 * 1024, in_file);
        let out_file = std::fs::File::create(&out_path)?;
        let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, out_file);

        let bytes_done = Arc::new(AtomicU64::new(0));
        parallel_encode_seekable(
            &pool,
            &mut reader,
            &mut writer,
            max_frame_size,
            input.len() as u64,
            &bytes_done,
        )?;
        writer.flush()?;
        drop(writer);

        pool.shutdown();
        Ok(std::fs::read(&out_path).unwrap())
    }

    /// Round-trip the parallel encoder output through `decode_seekable`
    /// to confirm the seek-table footer is well-formed and all frames
    /// decode back to the original bytes in order.
    ///
    /// Note: the parallel encoder uses `zstd::bulk::Compressor::compress`
    /// (the bulk API) for persistent-CCtx reuse, which produces slightly
    /// different frame-header flag bytes than the old sequential path's
    /// `zstd::encode_all` (streaming API). Both outputs are valid zstd
    /// and decode correctly via any zstd decoder; the byte-level layout
    /// of the frame header is the only difference.
    #[test]
    fn parallel_encode_roundtrips_through_decode_seekable() {
        let original: Vec<u8> = (0u8..=99).cycle().take(100_000).collect();
        let encoded = encode_parallel(&original, 8192, 0).unwrap();
        let decoded = decode_seekable(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    /// The parallel encoder's output must be decodable by the stock
    /// `zstd::stream::copy_decode` path the verifier uses, proving the
    /// bulk-API frame headers are compatible with libzstd's streaming
    /// decoder (not just our own `decode_seekable`).
    #[test]
    fn parallel_encode_decodes_via_zstd_streaming() {
        let original: Vec<u8> = (0u8..=255).cycle().take(200_000).collect();
        let encoded = encode_parallel(&original, 4096, 0).unwrap();

        let mut out = Vec::new();
        zstd::stream::copy_decode(&encoded[..], &mut out).unwrap();
        assert_eq!(original, out);
    }

    /// Exactly-boundary-aligned input: ensures the short-final-frame
    /// branch doesn't emit an empty work item when the last read hits
    /// EOF at a frame boundary.
    #[test]
    fn parallel_encode_exact_frame_boundary() {
        let original = vec![0xABu8; 16_384];
        let encoded = encode_parallel(&original, 4096, 0).unwrap();
        let decoded = decode_seekable(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    /// Deterministic across runs: same input + same worker count must
    /// produce the same bytes every call. Relevant because zstd at a
    /// fixed level is deterministic but the worker pool reorders work
    /// in flight; the `drive` reorder buffer must restore strict
    /// sequence before bytes hit the writer.
    #[test]
    fn parallel_encode_is_deterministic() {
        let original: Vec<u8> = (0u8..=199).cycle().take(80_000).collect();
        let a = encode_parallel(&original, 4096, 0).unwrap();
        let b = encode_parallel(&original, 4096, 0).unwrap();
        assert_eq!(a, b, "parallel encoder is not deterministic across runs");
    }

    /// Sanity check: the new bulk-API output must still compress
    /// roughly as well as the old streaming-API output (within 1 %)
    /// on highly compressible data. Guards against accidental level
    /// or parameter drift.
    #[test]
    fn parallel_encode_ratio_close_to_sequential() {
        let original: Vec<u8> = (0u8..=127).cycle().take(200_000).collect();

        let mut sequential = Vec::new();
        encode_seekable_streaming(
            &mut std::io::Cursor::new(&original),
            &mut sequential,
            4096,
            0,
            None,
        )
        .unwrap();

        let parallel = encode_parallel(&original, 4096, 0).unwrap();

        let delta = (parallel.len() as f64 - sequential.len() as f64).abs();
        let ratio = delta / sequential.len() as f64;
        assert!(
            ratio < 0.01,
            "parallel output size drifted >1 % from sequential: \
             sequential={} parallel={} delta={:.2}%",
            sequential.len(),
            parallel.len(),
            ratio * 100.0
        );
    }
}
