//! Streaming ZArchive sink and the pool-backed pipeline it feeds.
//!
//! Shape mirrors `z3ds::compress_parallel` at a higher level: a
//! driver thread runs [`crate::util::worker_pool::drive`] with
//! `produce` pulling blocks from a channel and `consume` forwarding
//! compressed bytes to a writer thread. The writer thread owns the
//! output file while the scope is live, updates the running SHA-256
//! hasher and the offset-records table, and returns everything back
//! to the main thread when the channel closes.
//!
//! Set up inside a [`std::thread::scope`]:
//!
//! ```text
//! compress_titles_blocking
//!   |
//!   + std::thread::scope
//!       + spawn_stream_pipeline(...) -> (block_tx, handle)
//!       | + driver thread  (runs drive(), pulls from block_rx,
//!       |                   submits to pool, forwards compressed
//!       |                   bytes to writer thread)
//!       | + writer thread  (drains write_rx, updates hasher,
//!       |                   tracks offset records, writes to inner)
//!       |
//!       + StreamingSink::new(block_tx)
//!       + for each title: compress_*_title(title, &mut sink, ...)
//!       + sink.flush_trailing() -> PathTree
//!       + handle.join() -> StreamResult { inner, hasher, ... }
//!       + write metadata sections to inner
//! ```
//!
//! After the scope exits the main thread has the inner writer back
//! plus all the state needed to emit the offset-records, name-table,
//! file-tree, and footer sections.

use std::io::Write;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::thread::{Scope, ScopedJoinHandle};

use sha2::{Digest, Sha256};

use crate::nintendo::wup::compress_parallel::{ZArchiveCompressWork, ZArchiveCompressedBlock};
use crate::nintendo::wup::constants::{COMPRESSED_BLOCK_SIZE, ENTRIES_PER_OFFSET_RECORD};
use crate::nintendo::wup::error::{WupError, WupResult};
use crate::nintendo::wup::models::CompressionOffsetRecord;
use crate::nintendo::wup::path_tree::PathTree;
use crate::nintendo::wup::zarchive_writer::ArchiveSink;
use crate::util::ProgressReporter;
use crate::util::worker_pool::{Pool, drive, parallelism};

/// How many completed blocks the writer batches before emitting one
/// progress inc. 32 blocks * 64 KiB = 2 MiB of uncompressed data per
/// inc: fine enough for a smooth bar while amortising the event-emit
/// overhead.
const WRITER_PROGRESS_BATCH_BLOCKS: u64 = 32;

/// State handed back to the main thread when the streaming pipeline
/// finishes. `inner` is the writer that started the pipeline, now
/// positioned just past the last compressed block; `hasher`,
/// `bytes_written`, and `offset_records` are exactly what
/// [`ZArchiveWriter`]'s finalize would have produced for the same
/// input.
pub struct StreamResult<W> {
    pub inner: W,
    pub hasher: Sha256,
    pub bytes_written: u64,
    pub offset_records: Vec<CompressionOffsetRecord>,
}

/// Handle onto the driver + writer threads for the streaming
/// pipeline. [`Self::join`] drains them in order and returns the
/// aggregated state. Call it only after the caller has dropped
/// every [`SyncSender<Vec<u8>>`] handed out by
/// [`spawn_stream_pipeline`] so the driver can exit.
pub struct StreamPipelineHandle<'scope, W: Write + Send + 'scope> {
    driver_handle: ScopedJoinHandle<'scope, WupResult<()>>,
    writer_handle: ScopedJoinHandle<'scope, WupResult<StreamResult<W>>>,
}

impl<'scope, W: Write + Send + 'scope> StreamPipelineHandle<'scope, W> {
    pub fn join(self) -> WupResult<StreamResult<W>> {
        self.driver_handle
            .join()
            .unwrap_or_else(|_| Err(WupError::WorkerPoolClosed))?;
        self.writer_handle
            .join()
            .unwrap_or_else(|_| Err(WupError::WorkerPoolClosed))
    }
}

/// Internal message between the driver thread and the writer thread.
/// `is_raw` distinguishes incompressible blocks (emitted at the full
/// 64 KiB length) from compressed blocks (emitted at whatever size
/// zstd produced).
struct WriteMsg {
    bytes: Vec<u8>,
    is_raw: bool,
}

/// Spawn the driver + writer threads inside `scope`. Returns the
/// sender the reader uses to push uncompressed 64 KiB blocks and a
/// handle that joins the pipeline at the end.
///
/// `total_blocks` must equal the exact number of blocks the reader
/// will end up sending (including the padded trailing block, if
/// any). `drive` submits exactly that many items to the pool and
/// blocks on `block_rx.recv` until each one arrives; submitting
/// fewer causes deadlock.
///
/// `progress`, when set, receives one `inc` per
/// [`WRITER_PROGRESS_BATCH_BLOCKS`] blocks committed to disk. The
/// reader is expected to stay silent on its own progress reporter
/// so the bar reflects only bytes that have cleared the pipeline,
/// keeping it smooth under back-pressure.
///
/// The pool is moved into the driver thread. It is shut down cleanly
/// when the driver returns (on either success or error), so workers
/// always get joined before the scope exits.
pub fn spawn_stream_pipeline<'scope, 'env, W>(
    scope: &'scope Scope<'scope, 'env>,
    pool: Pool<ZArchiveCompressWork, ZArchiveCompressedBlock, WupError>,
    total_blocks: u64,
    inner: W,
    progress: Option<&'scope (dyn ProgressReporter + Sync)>,
) -> (SyncSender<Vec<u8>>, StreamPipelineHandle<'scope, W>)
where
    W: Write + Send + 'scope,
{
    let n_workers = parallelism();
    let max_in_flight = (n_workers * 2).max(4);
    // Block-stream buffers sized generously: a 256-block backlog is
    // 16 MiB of uncompressed data between producer and workers, and
    // the same again between workers and the writer thread. Keeps
    // the pipeline saturated when either side hiccups, without
    // pinning noticeable RAM.
    let reader_slack = max_in_flight.max(256);
    let writer_slack = max_in_flight.max(256);

    let (block_tx, block_rx) = sync_channel::<Vec<u8>>(reader_slack);
    let (write_tx, write_rx) = sync_channel::<WriteMsg>(writer_slack);

    let writer_handle = scope.spawn(move || -> WupResult<StreamResult<W>> {
        let mut inner = inner;
        let mut hasher = Sha256::new();
        let mut bytes_written: u64 = 0;
        let mut offset_records: Vec<CompressionOffsetRecord> = Vec::new();
        let mut seq: u64 = 0;
        let mut progress_batch_blocks: u64 = 0;

        while let Ok(msg) = write_rx.recv() {
            let compressed_write_offset = bytes_written;
            inner.write_all(&msg.bytes).map_err(WupError::from)?;
            hasher.update(&msg.bytes);
            bytes_written += msg.bytes.len() as u64;
            let emitted_size = if msg.is_raw {
                COMPRESSED_BLOCK_SIZE
            } else {
                msg.bytes.len()
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
            seq += 1;

            if let Some(reporter) = progress {
                progress_batch_blocks += 1;
                if progress_batch_blocks >= WRITER_PROGRESS_BATCH_BLOCKS {
                    reporter.inc(progress_batch_blocks * COMPRESSED_BLOCK_SIZE as u64);
                    progress_batch_blocks = 0;
                }
            }
        }

        if let Some(reporter) = progress
            && progress_batch_blocks > 0
        {
            reporter.inc(progress_batch_blocks * COMPRESSED_BLOCK_SIZE as u64);
        }

        Ok(StreamResult {
            inner,
            hasher,
            bytes_written,
            offset_records,
        })
    });

    let driver_handle = scope.spawn(move || -> WupResult<()> {
        let drive_result = drive(
            &pool,
            total_blocks,
            max_in_flight,
            |_seq| -> WupResult<ZArchiveCompressWork> {
                block_rx
                    .recv()
                    .map(|uncompressed| ZArchiveCompressWork { uncompressed })
                    .map_err(|_| WupError::WorkerPoolClosed)
            },
            |_seq, out: ZArchiveCompressedBlock| -> WupResult<()> {
                write_tx
                    .send(WriteMsg {
                        bytes: out.bytes,
                        is_raw: out.is_raw,
                    })
                    .map_err(|_| WupError::WorkerPoolClosed)?;
                Ok(())
            },
        );

        drop(write_tx);
        pool.shutdown();
        drive_result
    });

    (
        block_tx,
        StreamPipelineHandle {
            driver_handle,
            writer_handle,
        },
    )
}

/// Reader-side sink: chunks `append_data` into 64 KiB blocks and
/// pushes them into the pipeline. Also owns the [`PathTree`] that
/// eventually becomes the file-tree section.
pub struct StreamingSink {
    block_tx: SyncSender<Vec<u8>>,
    write_buffer: Vec<u8>,
    tree: PathTree,
    current_file_path: Option<String>,
    current_input_offset: u64,
}

impl StreamingSink {
    pub fn new(block_tx: SyncSender<Vec<u8>>) -> Self {
        Self {
            block_tx,
            write_buffer: Vec::with_capacity(COMPRESSED_BLOCK_SIZE),
            tree: PathTree::new(),
            current_file_path: None,
            current_input_offset: 0,
        }
    }

    /// Pad the trailing partial buffer to a full 64 KiB block and
    /// push it, then drop the sender so the driver sees end-of-stream.
    /// Returns the accumulated [`PathTree`] the caller uses to emit
    /// the name-table and file-tree sections.
    pub fn flush_trailing(mut self) -> WupResult<PathTree> {
        if !self.write_buffer.is_empty() {
            let pad_len = COMPRESSED_BLOCK_SIZE - self.write_buffer.len();
            self.write_buffer.extend(std::iter::repeat_n(0u8, pad_len));
            let block = std::mem::replace(
                &mut self.write_buffer,
                Vec::with_capacity(COMPRESSED_BLOCK_SIZE),
            );
            self.block_tx
                .send(block)
                .map_err(|_| WupError::WorkerPoolClosed)?;
        }
        // Dropping self closes `block_tx`, telling the driver no more
        // blocks are coming.
        Ok(self.tree)
    }
}

impl ArchiveSink for StreamingSink {
    fn make_dir(&mut self, path: &str) -> WupResult<()> {
        self.tree.make_dir(path)
    }

    fn start_new_file(&mut self, path: &str) -> WupResult<()> {
        self.tree.add_file(path, self.current_input_offset)?;
        self.current_file_path = Some(path.to_string());
        Ok(())
    }

    fn append_data(&mut self, data: &[u8]) -> WupResult<()> {
        let total_data_size = data.len() as u64;
        let mut remaining = data;

        while !remaining.is_empty() {
            if self.write_buffer.is_empty() && remaining.len() >= COMPRESSED_BLOCK_SIZE {
                // Fast path: full block straight through without
                // buffering.
                let block = remaining[..COMPRESSED_BLOCK_SIZE].to_vec();
                self.block_tx
                    .send(block)
                    .map_err(|_| WupError::WorkerPoolClosed)?;
                remaining = &remaining[COMPRESSED_BLOCK_SIZE..];
                continue;
            }

            let free = COMPRESSED_BLOCK_SIZE - self.write_buffer.len();
            let take = remaining.len().min(free);
            self.write_buffer.extend_from_slice(&remaining[..take]);
            remaining = &remaining[take..];
            if self.write_buffer.len() == COMPRESSED_BLOCK_SIZE {
                let block = std::mem::replace(
                    &mut self.write_buffer,
                    Vec::with_capacity(COMPRESSED_BLOCK_SIZE),
                );
                self.block_tx
                    .send(block)
                    .map_err(|_| WupError::WorkerPoolClosed)?;
            }
        }

        if let Some(path) = self.current_file_path.clone()
            && let Some(node) = self.tree.get_mut(&path)
        {
            node.file_size += total_data_size;
        }
        self.current_input_offset += total_data_size;

        Ok(())
    }
}
