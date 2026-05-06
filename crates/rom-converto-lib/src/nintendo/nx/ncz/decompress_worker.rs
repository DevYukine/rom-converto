//! Parallel block decompression worker. Each worker runs an
//! independent zstd decode on a block-mode NCZ chunk; the driver
//! reorders results in submission order before feeding them into
//! `ReencryptWriter`.

use crate::nintendo::nx::error::{NxError, NxResult};
use crate::util::worker_pool::{Pool, Worker, parallelism};

pub struct NczDecompressWork {
    pub compressed: Vec<u8>,
    /// Logical (plaintext) block size. Equal to `compressed.len()`
    /// means the producer stored this block raw, so the worker just
    /// hands the input back unmodified.
    pub logical_size: usize,
    pub raw: bool,
}

pub struct NczDecompressedBlock {
    pub bytes: Vec<u8>,
}

pub struct NczDecompressWorker;

impl Worker<NczDecompressWork, NczDecompressedBlock, NxError> for NczDecompressWorker {
    fn process(&mut self, work: NczDecompressWork) -> NxResult<NczDecompressedBlock> {
        if work.raw {
            return Ok(NczDecompressedBlock {
                bytes: work.compressed,
            });
        }
        let mut out = vec![0u8; work.logical_size];
        let n = zstd::bulk::decompress_to_buffer(&work.compressed, &mut out)
            .map_err(|e| NxError::ZstdError(format!("zstd decode block: {e}")))?;
        if n != work.logical_size {
            return Err(NxError::ZstdError(format!(
                "zstd block decoded {n} bytes, expected {}",
                work.logical_size
            )));
        }
        Ok(NczDecompressedBlock { bytes: out })
    }
}

pub fn spawn_ncz_decompress_pool(
    n_threads: usize,
) -> Pool<NczDecompressWork, NczDecompressedBlock, NxError> {
    let workers = (0..n_threads).map(|_| NczDecompressWorker).collect();
    Pool::spawn(workers)
}

pub fn default_thread_count() -> usize {
    parallelism()
}
