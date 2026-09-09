//! CHD integrity verification: re-decode every hunk, compare the raw
//! and overall SHA-1s against the header, and optionally rewrite them.

use crate::disc::chd::error::{ChdError, ChdResult};
use crate::disc::chd::models::{CHD_HEADER_RAW_SHA1_OFFSET, CHD_HEADER_SHA1_OFFSET, SHA1_BYTES};
use crate::disc::chd::reader::{ChdFlavor, SyncChdHandle};
use crate::disc::chd::writer::metadata::MetadataHash;
use crate::util::{CancelToken, ProgressReporter, await_with_progress_cancel};
use log::{debug, info};
use sha1::{Digest, Sha1};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::io::AsyncWriteExt;

use super::*;

/// Check every hunk of the CHD at `input_path` against its hashes,
/// rewriting the header SHA-1s when `fix` is set. Verify writes no
/// output, so cancellation only stops the read with
/// [`ChdError::Cancelled`].
pub async fn verify_chd(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    parent_path: Option<PathBuf>,
    fix: bool,
    cancel: CancelToken,
) -> ChdResult<()> {
    if parent_path.is_some() {
        return Err(ChdError::ParentChdNotSupported);
    }

    debug!("Opening CHD file for verification: {:?}", input_path);

    // Peek header + metadata hashes up front so the progress bar
    // can size itself and so the fix-path (rewrite header SHA1s)
    // has a metadata snapshot to rebuild the overall hash from.
    let input_for_peek = input_path.clone();
    let (handle, metadata_hashes) =
        tokio::task::spawn_blocking(move || -> ChdResult<(SyncChdHandle, Vec<MetadataHash>)> {
            let handle = crate::disc::chd::reader::open_chd_sync(&input_for_peek)?;
            let hashes: Vec<MetadataHash> = handle
                .metadata
                .iter()
                .filter(|m| m.flags & crate::disc::chd::models::CHD_METADATA_FLAG_HASHED != 0)
                .map(|m| MetadataHash {
                    tag: m.tag,
                    sha1: <[u8; SHA1_BYTES]>::from(Sha1::digest(&m.data)),
                })
                .collect();
            Ok((handle, hashes))
        })
        .await??;

    let logical_bytes = handle.header.logical_bytes;
    let expected_raw = handle.header.raw_sha1;
    let expected_overall = handle.header.sha1;
    progress.start(logical_bytes, "Verifying CHD integrity");

    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let task = tokio::task::spawn_blocking(move || -> ChdResult<[u8; SHA1_BYTES]> {
        use crate::disc::chd::reader::worker::{
            ChdExtractWork, ChdExtractedOut, make_chd_dvd_extract_workers,
            make_chd_extract_workers, verify_hunks,
        };
        use crate::util::worker_pool::{Pool, parallelism};

        let hunk_bytes = handle.header.hunk_bytes as usize;

        let n_threads = parallelism();
        let pool: Pool<ChdExtractWork, ChdExtractedOut, ChdError> =
            if handle.flavor() == ChdFlavor::Dvd {
                Pool::spawn(make_chd_dvd_extract_workers(
                    n_threads,
                    &handle.file,
                    hunk_bytes,
                    handle.header.compressors(),
                )?)
            } else {
                Pool::spawn(make_chd_extract_workers(
                    n_threads,
                    &handle.file,
                    hunk_bytes,
                    handle.header.compressors(),
                )?)
            };

        let mut raw_sha1_hasher = Sha1::new();
        let verify_result = verify_hunks(
            &pool,
            &handle.map,
            &mut raw_sha1_hasher,
            hunk_bytes,
            logical_bytes,
            &bytes_done_bg,
            &cancel_bg,
        );
        pool.shutdown();
        verify_result?;

        let computed: [u8; SHA1_BYTES] = raw_sha1_hasher.finalize().into();
        Ok(computed)
    });

    let computed_raw = await_with_progress_cancel(progress, &bytes_done, task, &cancel).await?;

    /// One header hash comparison. `Ok(true)` means the header was
    /// rewritten and the caller is done; a mismatch without `fix`
    /// aborts the verify.
    async fn check_sha1(
        label: &str,
        expected: [u8; SHA1_BYTES],
        computed: [u8; SHA1_BYTES],
        fix: bool,
        path: &std::path::Path,
        raw_sha1: [u8; SHA1_BYTES],
        metadata_hashes: &[MetadataHash],
    ) -> ChdResult<bool> {
        if expected == computed {
            return Ok(false);
        }
        info!(
            "{label} SHA1 mismatch: expected {}, got {}",
            hex::encode(expected),
            hex::encode(computed)
        );
        if !fix {
            return Err(ChdError::Sha1Mismatch {
                expected: hex::encode(expected),
                actual: hex::encode(computed),
            });
        }
        fix_sha1(path, raw_sha1, metadata_hashes).await?;
        info!("SHA1 updated to correct value in CHD file");
        Ok(true)
    }

    if check_sha1(
        "Raw",
        expected_raw,
        computed_raw,
        fix,
        &input_path,
        computed_raw,
        &metadata_hashes,
    )
    .await?
    {
        return Ok(());
    }
    info!("Raw SHA-1 verification passed");

    let computed_overall = compute_overall_sha1(computed_raw, &metadata_hashes);
    if check_sha1(
        "Overall",
        expected_overall,
        computed_overall,
        fix,
        &input_path,
        computed_raw,
        &metadata_hashes,
    )
    .await?
    {
        return Ok(());
    }

    info!(
        "Overall SHA-1 verification passed (SHA-1: {})",
        hex::encode(computed_overall)
    );

    Ok(())
}

async fn fix_sha1(
    path: &std::path::Path,
    raw_sha1: [u8; SHA1_BYTES],
    metadata_hashes: &[MetadataHash],
) -> ChdResult<()> {
    use tokio::io::AsyncSeekExt;

    let overall_sha1 = compute_overall_sha1(raw_sha1, metadata_hashes);

    let mut file = tokio::fs::OpenOptions::new().write(true).open(path).await?;

    file.seek(std::io::SeekFrom::Start(CHD_HEADER_RAW_SHA1_OFFSET))
        .await?;
    file.write_all(&raw_sha1).await?;

    file.seek(std::io::SeekFrom::Start(CHD_HEADER_SHA1_OFFSET))
        .await?;
    file.write_all(&overall_sha1).await?;

    file.flush().await?;

    Ok(())
}
