//! CSO/ZSO integrity verification.
//!
//! Neither format embeds a checksum, so the standard pass is purely
//! structural: header sanity, monotonic in-bounds index offsets, and
//! a file size that matches the end-of-file sentinel. The full pass
//! additionally decodes every block on the worker pool, which catches
//! payload corruption (broken deflate/LZ4 streams, short blocks) that
//! the index cannot see.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use log::info;

use crate::cso::error::{CsoError, CsoResult};
use crate::cso::models::CISO_INDEX_UNCOMPRESSED;
use crate::cso::reader::{CsoSyncHandle, block_spec, make_cso_extract_workers, open_cso_sync};
use crate::util::{BYTES_PER_MB, CancelToken, ProgressReporter, await_with_progress_cancel};

pub async fn verify_cso(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    full: bool,
) -> CsoResult<()> {
    verify_cso_cancellable(progress, input_path, full, CancelToken::new()).await
}

pub async fn verify_cso_cancellable(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    full: bool,
    cancel: CancelToken,
) -> CsoResult<()> {
    if cancel.is_cancelled() {
        return Err(CsoError::Cancelled);
    }
    let peek_path = input_path.clone();
    let peek_cancel = cancel.clone();
    let (uncompressed_size, format) =
        tokio::task::spawn_blocking(move || -> CsoResult<(u64, crate::cso::CsoFormat)> {
            let handle = open_cso_sync(&peek_path)?;
            verify_structure(&handle, &peek_cancel)?;
            Ok((handle.header.uncompressed_size, handle.format))
        })
        .await??;
    info!("Index structure OK");

    if !full {
        return Ok(());
    }

    let total_mb = uncompressed_size as f64 / BYTES_PER_MB;
    progress.start(
        uncompressed_size,
        &format!("Verifying {} blocks (~{:.2} MB)", format.name(), total_mb),
    );

    let input_owned = input_path.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();
    let cancel_bg = cancel.clone();

    let handle = tokio::task::spawn_blocking(move || -> CsoResult<()> {
        use crate::util::worker_pool::{Pool, drive, parallelism};

        let handle = open_cso_sync(&input_owned)?;
        let workers = make_cso_extract_workers(parallelism(), handle.format, &handle.file);
        let pool = Pool::spawn(workers);

        let result = drive(
            &pool,
            handle.header.block_count(),
            parallelism() * 2,
            |block| {
                if cancel_bg.is_cancelled() {
                    return Err(CsoError::Cancelled);
                }
                Ok(crate::cso::reader::CsoExtractWork {
                    spec: block_spec(&handle, block)?,
                    block,
                })
            },
            |_seq, out: crate::cso::reader::CsoExtractedOut| {
                if cancel_bg.is_cancelled() {
                    return Err(CsoError::Cancelled);
                }
                bytes_done_bg
                    .fetch_add(out.bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            },
        );
        pool.shutdown();
        result
    });

    await_with_progress_cancel(progress, &bytes_done, handle, &cancel, || {
        CsoError::Cancelled
    })
    .await?;
    info!("All blocks decoded successfully");
    Ok(())
}

/// Index sanity: offsets monotonic and in bounds, sentinel equal to
/// the file size, no raw bit on the sentinel.
fn verify_structure(handle: &CsoSyncHandle, cancel: &CancelToken) -> CsoResult<()> {
    if handle.dax.is_some() {
        // DAX carries no end-of-file sentinel; validate each frame's span.
        for block in 0..handle.header.block_count() {
            if cancel.is_cancelled() {
                return Err(CsoError::Cancelled);
            }
            block_spec(handle, block)?;
        }
        return Ok(());
    }

    let blocks = handle.header.block_count();
    let shift = handle.header.index_shift;

    let mut prev = 0u64;
    for (i, &entry) in handle.index.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(CsoError::Cancelled);
        }
        let offset = ((entry & !CISO_INDEX_UNCOMPRESSED) as u64) << shift;
        if offset < prev {
            return Err(CsoError::CorruptIndex(format!(
                "offset of block {i} goes backwards"
            )));
        }
        prev = offset;
    }

    for block in 0..blocks {
        if cancel.is_cancelled() {
            return Err(CsoError::Cancelled);
        }
        block_spec(handle, block)?;
    }

    let sentinel = handle.index[blocks as usize];
    if sentinel & CISO_INDEX_UNCOMPRESSED != 0 {
        return Err(CsoError::CorruptIndex(
            "end-of-file sentinel carries the raw-block bit".into(),
        ));
    }
    let end = ((sentinel & !CISO_INDEX_UNCOMPRESSED) as u64) << shift;
    if end != handle.file_size {
        return Err(CsoError::CorruptIndex(format!(
            "file is {} bytes but the index ends at {end}",
            handle.file_size
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cso::models::CsoFormat;
    use crate::cso::writer::write_cso_blocking;
    use crate::util::{CancelToken, NoProgress};

    fn make_cso(dir: &std::path::Path, data: &[u8]) -> PathBuf {
        let iso = dir.join("game.iso");
        std::fs::write(&iso, data).unwrap();
        let packed = dir.join("game.cso");
        let bytes_done = Arc::new(AtomicU64::new(0));
        write_cso_blocking(
            &iso,
            &packed,
            CsoFormat::Cso,
            2048,
            0,
            &bytes_done,
            &CancelToken::new(),
        )
        .unwrap();
        packed
    }

    fn payload() -> Vec<u8> {
        (0..8 * 2048usize).map(|i| (i / 7) as u8).collect()
    }

    #[tokio::test]
    async fn intact_file_passes_both_passes() {
        let dir = tempfile::tempdir().unwrap();
        let packed = make_cso(dir.path(), &payload());
        verify_cso(&NoProgress, packed.clone(), false)
            .await
            .unwrap();
        verify_cso(&NoProgress, packed, true).await.unwrap();
    }

    #[tokio::test]
    async fn truncation_fails_the_structural_pass() {
        let dir = tempfile::tempdir().unwrap();
        let packed = make_cso(dir.path(), &payload());
        let bytes = std::fs::read(&packed).unwrap();
        std::fs::write(&packed, &bytes[..bytes.len() - 3]).unwrap();
        assert!(verify_cso(&NoProgress, packed, false).await.is_err());
    }

    #[tokio::test]
    async fn payload_corruption_fails_only_the_full_pass() {
        let dir = tempfile::tempdir().unwrap();
        let packed = make_cso(dir.path(), &payload());
        let mut bytes = std::fs::read(&packed).unwrap();
        // Past the header and index, inside compressed block data.
        let data_start = 0x18 + (8 + 1) * 4;
        let mid = data_start + (bytes.len() - data_start) / 2;
        bytes[mid] ^= 0xFF;
        std::fs::write(&packed, &bytes).unwrap();

        verify_cso(&NoProgress, packed.clone(), false)
            .await
            .unwrap();
        assert!(verify_cso(&NoProgress, packed, true).await.is_err());
    }
}
