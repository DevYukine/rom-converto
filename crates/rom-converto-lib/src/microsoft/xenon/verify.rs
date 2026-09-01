//! Verify a ZArchive (`.zar`): footer/section validation happens on
//! open, then the stored SHA-256 digest is re-checked and every block
//! is decoded through the pool to prove the compressed data is intact.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::microsoft::zar::format::ZarError;
use crate::microsoft::zar::{ZarReader, decompress_block};
use crate::util::CancelToken;
use crate::util::worker_pool::{Pool, Worker, drive, parallelism};

use super::error::{XenonError, XenonResult};
use super::extract::logical_size;

/// Outcome of a verify run, mirroring [`crate::nintendo::rvz::verify::RvzStructuralVerify`]'s
/// shape at a scale that fits a single-hash, single-tree container.
#[derive(Debug, Clone, Copy)]
pub struct ZarVerifyResult {
    pub blocks: u64,
    pub logical_bytes: u64,
    pub hash_ok: bool,
}

impl ZarVerifyResult {
    /// True if the archive's stored integrity hash matched.
    pub fn ok(&self) -> bool {
        self.hash_ok
    }
}

struct BlockDecodeWorker;

impl Worker<(Vec<u8>, bool), Vec<u8>, XenonError> for BlockDecodeWorker {
    fn process(&mut self, (payload, stored_raw): (Vec<u8>, bool)) -> XenonResult<Vec<u8>> {
        Ok(decompress_block(&payload, stored_raw)?)
    }
}

pub fn verify_blocking(
    input: &Path,
    bytes_done: &AtomicU64,
    cancel: &CancelToken,
) -> XenonResult<ZarVerifyResult> {
    let mut reader = ZarReader::open(std::fs::File::open(input)?)?;
    let logical_bytes = logical_size(input)?;

    let hash_ok = match reader.verify_integrity(cancel) {
        Ok(()) => true,
        Err(ZarError::HashMismatch) => false,
        Err(ZarError::Cancelled) => return Err(XenonError::Cancelled),
        Err(e) => return Err(e.into()),
    };

    let block_count = reader.block_count();
    let n_threads = parallelism();
    let workers: Vec<BlockDecodeWorker> = (0..n_threads).map(|_| BlockDecodeWorker).collect();
    let pool: Pool<(Vec<u8>, bool), Vec<u8>, XenonError> = Pool::spawn(workers);

    // Blocks are always the full 65536 bytes, but the trailing block
    // may include padding past the last file's logical size; clamp so
    // the reported progress never overshoots `logical_bytes`.
    let mut logical_done = 0u64;
    let result = drive(
        &pool,
        block_count,
        n_threads * 2,
        |seq| {
            if cancel.is_cancelled() {
                return Err(XenonError::Cancelled);
            }
            Ok(reader.read_block_raw(seq)?)
        },
        |_seq, block| {
            if cancel.is_cancelled() {
                return Err(XenonError::Cancelled);
            }
            let take = (block.len() as u64).min(logical_bytes - logical_done);
            logical_done += take;
            bytes_done.fetch_add(take, Ordering::Relaxed);
            Ok(())
        },
    );
    pool.shutdown();
    result?;

    Ok(ZarVerifyResult {
        blocks: block_count,
        logical_bytes,
        hash_ok,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::microsoft::zar::ZarWriter;

    #[test]
    fn verify_fails_after_a_flipped_payload_byte() {
        // Incompressible data is stored raw, so flipping one byte still
        // decodes cleanly (it's not a zstd frame) and only the SHA-256
        // check should notice.
        let mut state: u64 = 0x1234_5678_9ABC_DEF0;
        let data: Vec<u8> = (0..crate::microsoft::zar::COMPRESSED_BLOCK_SIZE)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 33) as u8
            })
            .collect();

        let mut buf = Vec::new();
        let mut writer = ZarWriter::new(&mut buf, 1).unwrap();
        writer.start_file("noise.bin").unwrap();
        writer.append_data(&data).unwrap();
        writer.finish().unwrap();
        buf[64] ^= 0x01; // corrupt a payload byte, well before the footer

        let work = tempfile::tempdir().unwrap();
        let zar_path = work.path().join("archive.zar");
        std::fs::write(&zar_path, &buf).unwrap();

        let bytes_done = AtomicU64::new(0);
        let cancel = CancelToken::new();
        let result = verify_blocking(&zar_path, &bytes_done, &cancel).unwrap();
        assert!(!result.hash_ok);
        assert!(!result.ok());
        assert_eq!(result.blocks, 1);
    }
}
