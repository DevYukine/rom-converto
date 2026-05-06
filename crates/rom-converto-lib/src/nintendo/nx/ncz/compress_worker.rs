//! Parallel block-mode NCZ compression using the shared worker pool.
//!
//! Each worker owns a persistent zstd context plus a `compress_bound`
//! sized scratch so the hot loop allocates nothing. Plaintext blocks
//! flow in, compressed blocks flow out, and `drive()`'s reorder buffer
//! reassembles them in submission order.

use crate::nintendo::nx::error::{NxError, NxResult};
use crate::util::worker_pool::{Pool, Worker, parallelism};

pub struct NczBlockWork {
    pub plaintext: Vec<u8>,
}

pub struct NczCompressedBlock {
    pub bytes: Vec<u8>,
    /// True when the compressed form was no smaller than the plaintext
    /// and the writer stored it raw. The on-disk size list records
    /// `bytes.len() == block_size` for these.
    pub raw: bool,
    pub original_len: usize,
}

pub struct NczCompressWorker {
    compressor: zstd::bulk::Compressor<'static>,
    scratch: Vec<u8>,
}

impl NczCompressWorker {
    pub fn new(level: i32, block_size: usize) -> NxResult<Self> {
        let compressor = zstd::bulk::Compressor::new(level)
            .map_err(|e| NxError::ZstdError(format!("zstd compressor init: {e}")))?;
        Ok(Self {
            compressor,
            scratch: vec![0u8; zstd::zstd_safe::compress_bound(block_size)],
        })
    }
}

impl Worker<NczBlockWork, NczCompressedBlock, NxError> for NczCompressWorker {
    fn process(&mut self, work: NczBlockWork) -> NxResult<NczCompressedBlock> {
        let plain = work.plaintext;
        let original_len = plain.len();
        let n = self
            .compressor
            .compress_to_buffer(&plain, &mut self.scratch)
            .map_err(|e| NxError::ZstdError(format!("zstd compress: {e}")))?;
        if n >= original_len {
            Ok(NczCompressedBlock {
                bytes: plain,
                raw: true,
                original_len,
            })
        } else {
            Ok(NczCompressedBlock {
                bytes: self.scratch[..n].to_vec(),
                raw: false,
                original_len,
            })
        }
    }
}

pub fn spawn_ncz_pool(
    level: i32,
    block_size: usize,
    n_threads: usize,
) -> NxResult<Pool<NczBlockWork, NczCompressedBlock, NxError>> {
    let mut workers = Vec::with_capacity(n_threads);
    for _ in 0..n_threads {
        workers.push(NczCompressWorker::new(level, block_size)?);
    }
    Ok(Pool::spawn(workers))
}

pub fn default_thread_count() -> usize {
    parallelism()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::worker_pool::drive;

    #[test]
    fn pool_matches_single_block_compression() {
        let block_size = 1usize << 14;
        let plain: Vec<u8> = (0..block_size).map(|i| (i & 0xFF) as u8).collect();

        let pool = spawn_ncz_pool(3, block_size, 4).unwrap();
        let mut got: Vec<Option<NczCompressedBlock>> = (0..8).map(|_| None).collect();
        let mut idx = 0usize;
        drive(
            &pool,
            8,
            8,
            |_seq| -> NxResult<NczBlockWork> {
                Ok(NczBlockWork {
                    plaintext: plain.clone(),
                })
            },
            |seq, out| -> NxResult<()> {
                got[seq as usize] = Some(out);
                idx += 1;
                Ok(())
            },
        )
        .unwrap();
        pool.shutdown();

        let mut single = zstd::bulk::Compressor::new(3).unwrap();
        let mut scratch = vec![0u8; zstd::zstd_safe::compress_bound(block_size)];
        let serial_n = single.compress_to_buffer(&plain, &mut scratch).unwrap();

        for block in got.into_iter().flatten() {
            if !block.raw {
                assert_eq!(block.bytes.as_slice(), &scratch[..serial_n]);
            } else {
                assert_eq!(block.bytes, plain);
            }
        }
    }
}
