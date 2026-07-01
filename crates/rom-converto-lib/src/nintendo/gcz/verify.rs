//! GCZ integrity verification: check every block's stored Adler-32.
//!
//! The checksums cover the bytes exactly as stored, so the pass needs
//! no inflation: one sequential read of the data section with the
//! checksum math spread over the worker pool. Stricter than Dolphin,
//! which logs a mismatch and keeps serving data; here a mismatch is a
//! hard error, matching nod.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::error::{GczError, GczResult};
use super::format::adler32;
use super::reader::GczLayout;
use crate::util::CancelToken;
use crate::util::worker_pool::{Pool, Worker, drive, parallelism};

struct HashCheckWork {
    block: u64,
    stored: Vec<u8>,
    stored_hash: u32,
}

struct HashCheckWorker;

impl Worker<HashCheckWork, u64, GczError> for HashCheckWorker {
    fn process(&mut self, work: HashCheckWork) -> GczResult<u64> {
        let computed = adler32(&work.stored);
        if computed != work.stored_hash {
            return Err(GczError::BlockHashMismatch {
                block: work.block,
                stored: work.stored_hash,
                computed,
            });
        }
        Ok(work.stored.len() as u64)
    }
}

/// Verify all block checksums. `bytes_done` advances by stored bytes
/// up to the header's `compressed_data_size`.
pub fn verify_gcz_blocking(
    path: &Path,
    bytes_done: Arc<AtomicU64>,
    cancel: CancelToken,
) -> GczResult<()> {
    let mut f = File::open(path)?;
    let layout = GczLayout::parse(&mut f)?;
    let total_blocks = layout.header.num_blocks as u64;

    let workers: Vec<HashCheckWorker> = (0..parallelism()).map(|_| HashCheckWorker).collect();
    let pool: Pool<HashCheckWork, u64, GczError> = Pool::spawn(workers);

    let result = drive(
        &pool,
        total_blocks,
        parallelism() * 2,
        |i| {
            if cancel.is_cancelled() {
                return Err(GczError::Cancelled);
            }
            let (off, len, _) = layout.stored_extent(i)?;
            let mut stored = vec![0u8; len as usize];
            use std::io::{Read, Seek, SeekFrom};
            f.seek(SeekFrom::Start(off))?;
            f.read_exact(&mut stored)?;
            Ok(HashCheckWork {
                block: i,
                stored,
                stored_hash: layout.stored_hash(i),
            })
        },
        |_seq, stored_len| {
            bytes_done.fetch_add(stored_len, Ordering::Relaxed);
            Ok(())
        },
    );
    pool.shutdown();
    result
}

/// Verification work total for progress reporting.
pub fn verify_total(path: &Path) -> GczResult<u64> {
    let mut f = File::open(path)?;
    Ok(GczLayout::parse(&mut f)?.header.compressed_data_size)
}
