//! Parallel 64 KiB block compressor driving a persistent worker pool.
//!
//! Shape mirrors `z3ds::compress_parallel`: one persistent
//! [`zstd::bulk::Compressor`] per thread, fixed 64 KiB work items,
//! and [`crate::util::worker_pool::drive`] to reorder results back
//! into submission order before writing them to the output. A
//! dedicated writer thread inside [`std::thread::scope`] drains a
//! bounded channel and calls `write_all` on the output so disk I/O
//! runs concurrently with compression.

use std::io::Write;
use std::sync::mpsc::sync_channel;

use sha2::{Digest, Sha256};

use crate::nintendo::wup::constants::{
    COMPRESSED_BLOCK_SIZE, ENTRIES_PER_OFFSET_RECORD, ZARCHIVE_DEFAULT_ZSTD_LEVEL,
};
use crate::nintendo::wup::error::{WupError, WupResult};
use crate::nintendo::wup::models::CompressionOffsetRecord;
use crate::util::ProgressReporter;
use crate::util::worker_pool::{Pool, Worker, drive, parallelism};

const PROGRESS_INC_BATCH_BLOCKS: u64 = 256;

/// One 64 KiB uncompressed block ready to hand to a worker.
pub struct ZArchiveCompressWork {
    pub uncompressed: Vec<u8>,
}

/// Compressed result for one block. `is_raw` is set when the
/// compressor didn't manage to shrink the input, in which case
/// `bytes` is the raw 64 KiB block and the offset record stores the
/// incompressible-block sentinel (length `0xFFFF` after the `-1`
/// encoding).
pub struct ZArchiveCompressedBlock {
    pub bytes: Vec<u8>,
    pub is_raw: bool,
}

/// Per-thread worker: one persistent `zstd::bulk::Compressor` plus a
/// reusable scratch buffer sized to `compress_bound(64 KiB)` so the
/// hot loop never allocates for the destination buffer.
pub struct ZArchiveCompressWorker {
    compressor: zstd::bulk::Compressor<'static>,
    scratch: Vec<u8>,
}

impl ZArchiveCompressWorker {
    pub fn new(level: i32) -> WupResult<Self> {
        let effective_level = if level == 0 {
            ZARCHIVE_DEFAULT_ZSTD_LEVEL
        } else {
            level
        };
        let compressor = zstd::bulk::Compressor::new(effective_level)?;
        let bound = zstd::zstd_safe::compress_bound(COMPRESSED_BLOCK_SIZE);
        Ok(Self {
            compressor,
            scratch: vec![0u8; bound],
        })
    }
}

impl Worker<ZArchiveCompressWork, ZArchiveCompressedBlock, WupError> for ZArchiveCompressWorker {
    fn process(&mut self, work: ZArchiveCompressWork) -> WupResult<ZArchiveCompressedBlock> {
        debug_assert_eq!(work.uncompressed.len(), COMPRESSED_BLOCK_SIZE);
        let output_size = self
            .compressor
            .compress_to_buffer(&work.uncompressed, &mut self.scratch)?;
        if output_size >= COMPRESSED_BLOCK_SIZE {
            // Incompressible: ship the raw block and let the
            // consumer mark the slot as a full-length sentinel.
            Ok(ZArchiveCompressedBlock {
                bytes: work.uncompressed,
                is_raw: true,
            })
        } else {
            Ok(ZArchiveCompressedBlock {
                bytes: self.scratch[..output_size].to_vec(),
                is_raw: false,
            })
        }
    }
}

/// Build `n` compressor workers, all at the same zstd level.
pub fn make_workers(n: usize, level: i32) -> WupResult<Vec<ZArchiveCompressWorker>> {
    (0..n).map(|_| ZArchiveCompressWorker::new(level)).collect()
}

/// Spawn the worker pool [`compress_titles`] shares across its whole
/// invocation. Callers own the pool and must call
/// [`Pool::shutdown`] after the last `finalize`.
pub fn spawn_zarchive_pool(
    level: i32,
) -> WupResult<Pool<ZArchiveCompressWork, ZArchiveCompressedBlock, WupError>> {
    let workers = make_workers(parallelism(), level)?;
    Ok(Pool::spawn(workers))
}

/// Compress every block in `blocks` through a persistent worker
/// pool, reorder results back into submission order, and stream them
/// to `output` through a dedicated writer thread. `hasher`,
/// `bytes_written`, and `offset_records` are updated in strict block
/// order on the dispatch thread.
///
/// The writer thread decouples disk I/O from the consume loop. If
/// `write_all` blocks on an OS write-cache flush, the dispatcher
/// keeps draining worker results and reporting progress; the channel
/// absorbs up to `2 * max_in_flight` blocks of backlog.
pub(super) fn parallel_compress_blocks<W: Write + Send>(
    pool: &Pool<ZArchiveCompressWork, ZArchiveCompressedBlock, WupError>,
    blocks: Vec<Vec<u8>>,
    output: &mut W,
    hasher: &mut Sha256,
    bytes_written: &mut u64,
    offset_records: &mut Vec<CompressionOffsetRecord>,
    progress: Option<&dyn ProgressReporter>,
) -> WupResult<()> {
    if blocks.is_empty() {
        return Ok(());
    }
    let n_workers = parallelism();

    let total = blocks.len() as u64;
    // Two items in flight per worker keeps the pipeline saturated
    // without blowing up RAM: each in-flight item is one 64 KiB
    // block of raw input plus at most 64 KiB of compressed output,
    // so peak usage is `n_workers * 2 * 128 KiB`.
    let max_in_flight = (n_workers * 2).max(4);
    let (write_tx, write_rx) = sync_channel::<Vec<u8>>(max_in_flight * 2);

    let mut blocks_iter = blocks.into_iter();
    let mut progress_batch_blocks: u64 = 0;

    let scope_result: WupResult<()> = std::thread::scope(|s| {
        let output_slot: &mut W = output;
        let writer_handle = s.spawn(move || -> WupResult<()> {
            while let Ok(bytes) = write_rx.recv() {
                output_slot.write_all(&bytes).map_err(WupError::from)?;
            }
            Ok(())
        });

        let drive_result = drive(
            pool,
            total,
            max_in_flight,
            |_seq| -> WupResult<ZArchiveCompressWork> {
                let uncompressed = blocks_iter
                    .next()
                    .expect("drive called produce past total count");
                Ok(ZArchiveCompressWork { uncompressed })
            },
            |seq, block: ZArchiveCompressedBlock| -> WupResult<()> {
                let compressed_write_offset = *bytes_written;
                hasher.update(&block.bytes);
                *bytes_written += block.bytes.len() as u64;
                let emitted_size = if block.is_raw {
                    COMPRESSED_BLOCK_SIZE
                } else {
                    block.bytes.len()
                };
                let slot = (seq as usize) % ENTRIES_PER_OFFSET_RECORD;
                if slot == 0 {
                    let mut record = CompressionOffsetRecord::new(compressed_write_offset);
                    record.set_block_size(0, emitted_size);
                    offset_records.push(record);
                } else {
                    offset_records
                        .last_mut()
                        .expect("offset record must exist once the first block has been consumed")
                        .set_block_size(slot, emitted_size);
                }
                if let Some(reporter) = progress {
                    progress_batch_blocks += 1;
                    if progress_batch_blocks >= PROGRESS_INC_BATCH_BLOCKS {
                        reporter.inc(progress_batch_blocks * COMPRESSED_BLOCK_SIZE as u64);
                        progress_batch_blocks = 0;
                    }
                }
                write_tx
                    .send(block.bytes)
                    .map_err(|_| WupError::WorkerPoolClosed)?;
                Ok(())
            },
        );

        drop(write_tx);
        let writer_result = writer_handle
            .join()
            .unwrap_or_else(|_| Err(WupError::WorkerPoolClosed));
        drive_result?;
        writer_result
    });

    scope_result?;

    if let Some(reporter) = progress
        && progress_batch_blocks > 0
    {
        reporter.inc(progress_batch_blocks * COMPRESSED_BLOCK_SIZE as u64);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(blocks: Vec<Vec<u8>>, level: i32) -> (Vec<u8>, Vec<CompressionOffsetRecord>, [u8; 32]) {
        let mut output = Vec::new();
        let mut hasher = Sha256::new();
        let mut bytes_written = 0u64;
        let mut records = Vec::new();
        let pool = spawn_zarchive_pool(level).unwrap();
        parallel_compress_blocks(
            &pool,
            blocks,
            &mut output,
            &mut hasher,
            &mut bytes_written,
            &mut records,
            None,
        )
        .unwrap();
        pool.shutdown();
        let hash: [u8; 32] = hasher.finalize().into();
        assert_eq!(output.len() as u64, bytes_written);
        (output, records, hash)
    }

    fn make_block(fill: u8) -> Vec<u8> {
        vec![fill; COMPRESSED_BLOCK_SIZE]
    }

    fn make_random_block(seed: u64) -> Vec<u8> {
        // Simple linear-congruential RNG so blocks are reproducible
        // but also mostly incompressible for the raw-fallback path.
        let mut s = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut out = vec![0u8; COMPRESSED_BLOCK_SIZE];
        for b in &mut out {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *b = (s >> 33) as u8;
        }
        out
    }

    #[test]
    fn empty_input_produces_no_records() {
        let (output, records, _) = run(Vec::new(), 6);
        assert!(output.is_empty());
        assert!(records.is_empty());
    }

    #[test]
    fn one_compressible_block_produces_one_record_one_block() {
        let (output, records, _) = run(vec![make_block(0)], 6);
        assert_eq!(records.len(), 1);
        assert!(
            output.len() < COMPRESSED_BLOCK_SIZE,
            "all-zero block must be compressible"
        );
        assert_eq!(records[0].base_offset, 0);
        assert_eq!(records[0].block_size(0), output.len());
    }

    #[test]
    fn incompressible_block_stored_raw() {
        let block = make_random_block(42);
        let (output, records, _) = run(vec![block.clone()], 6);
        assert_eq!(output.len(), COMPRESSED_BLOCK_SIZE);
        assert_eq!(
            output, block,
            "incompressible block must be stored verbatim"
        );
        assert_eq!(records[0].block_size(0), COMPRESSED_BLOCK_SIZE);
    }

    #[test]
    fn two_blocks_produce_sequential_offsets() {
        let (output, records, _) = run(vec![make_block(0), make_block(1)], 6);
        assert_eq!(records.len(), 1);
        // Block 0 starts at offset 0.
        assert_eq!(records[0].base_offset, 0);
        // Block 1's compressed bytes immediately follow block 0's.
        let size0 = records[0].block_size(0);
        let size1 = records[0].block_size(1);
        assert_eq!(size0 + size1, output.len());
    }

    #[test]
    fn seventeen_blocks_produce_two_records() {
        let blocks: Vec<_> = (0..17).map(|i| make_block(i as u8)).collect();
        let (_output, records, _) = run(blocks, 6);
        assert_eq!(
            records.len(),
            2,
            "17 blocks must roll over into a second offset record"
        );
        // base_offset of the second record points past the first
        // 16 blocks' worth of compressed bytes.
        assert!(records[1].base_offset > 0);
        let first_record_bytes: usize = (0..16).map(|i| records[0].block_size(i)).sum();
        assert_eq!(records[1].base_offset as usize, first_record_bytes);
    }

    #[test]
    fn order_is_preserved_even_though_workers_finish_out_of_order() {
        // Mix compressible and incompressible blocks so they take
        // different amounts of time to complete. The drive helper
        // must still hand them to the consume closure in
        // submission order, which means the offset records table
        // is byte-for-byte reproducible across runs.
        let blocks: Vec<_> = (0..10u64)
            .map(|i| {
                if i.is_multiple_of(2) {
                    make_block(0)
                } else {
                    make_random_block(i)
                }
            })
            .collect();
        let (output_a, records_a, hash_a) = run(blocks.clone(), 6);
        let (output_b, records_b, hash_b) = run(blocks, 6);
        assert_eq!(output_a, output_b, "parallel output must be deterministic");
        assert_eq!(records_a, records_b);
        assert_eq!(hash_a, hash_b);
    }

    #[test]
    fn hasher_sees_every_output_byte() {
        let blocks: Vec<_> = (0..5u64).map(|i| make_block(i as u8)).collect();
        let (output, _, hash) = run(blocks, 6);
        let mut reference = Sha256::new();
        reference.update(&output);
        let reference_hash: [u8; 32] = reference.finalize().into();
        assert_eq!(hash, reference_hash);
    }
}
