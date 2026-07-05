//! CSO (CISO v1) and ZSO support: block-compressed PSP/PS2 ISO
//! containers, the maxcso equivalent.
//!
//! Target matrix: real PSP hardware (CFW) and PPSSPP read CSO v1;
//! Open PS2 Loader (>= 1.2) on real PS2 reads ZSO. CSO v2 was never
//! adopted (PPSSPP rejects it) and stays unsupported. DAX (legacy PSP)
//! is accepted as a decode-only input.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use log::info;

use crate::cd::IO_BUFFER_SIZE;
use crate::util::hash::{FileDigests, HashAlgo};
use crate::util::{BYTES_PER_MB, CancelToken, ProgressReporter, await_with_progress_cancel};

pub mod compression;
pub(crate) mod dax;
pub mod error;
pub mod info;
pub mod models;
pub(crate) mod reader;
pub mod verify;
pub(crate) mod writer;

pub use error::{CsoError, CsoResult};
pub use info::CsoInfo;
pub use models::CsoFormat;
pub use verify::verify_cso;

use models::{pick_block_size, pick_index_shift, valid_block_size};

/// See [`crate::util::scratch_output_path`]: sibling temp path so an
/// interrupted write never lands on the final name.
fn scratch_output_path(output: &std::path::Path) -> PathBuf {
    let mut name = output.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    output.with_file_name(name)
}

#[derive(Debug, Clone)]
pub struct CsoCompressOptions {
    pub format: CsoFormat,
    /// Block size override; the default is 2048, or 16384 for inputs
    /// of 2 GiB and beyond, matching maxcso.
    pub block_size: Option<u32>,
    pub force: bool,
}

impl Default for CsoCompressOptions {
    fn default() -> Self {
        Self {
            format: CsoFormat::Cso,
            block_size: None,
            force: false,
        }
    }
}

/// Compress an ISO into a CSO or ZSO container.
pub async fn compress_to_cso(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    opts: CsoCompressOptions,
) -> CsoResult<()> {
    compress_to_cso_cancellable(progress, input_path, output_path, opts, CancelToken::new()).await
}

/// Compress an ISO into a CSO or ZSO container, observing `cancel` at
/// every block boundary. On cancel the partial output is removed and a
/// pre-existing overwrite target is left untouched (the writer targets a
/// sibling temp file that is renamed into place only on success).
pub async fn compress_to_cso_cancellable(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    opts: CsoCompressOptions,
    cancel: CancelToken,
) -> CsoResult<()> {
    let preexisting = tokio::fs::metadata(&output_path).await.is_ok();
    if preexisting && !opts.force {
        return Err(CsoError::OutputAlreadyExists);
    }

    let input_size = tokio::fs::metadata(&input_path).await?.len();
    let block_size = opts
        .block_size
        .unwrap_or_else(|| pick_block_size(input_size));
    if !valid_block_size(block_size) {
        return Err(CsoError::InvalidBlockSize(block_size));
    }
    let index_shift = pick_index_shift(input_size, block_size);

    let total_mb = input_size as f64 / BYTES_PER_MB;
    progress.start(
        input_size,
        &format!(
            "Compressing to {} (~{:.2} MB)",
            opts.format.name(),
            total_mb
        ),
    );

    let write_path = scratch_output_path(&output_path);
    let input_owned = input_path.clone();
    let write_owned = write_path.clone();
    let format = opts.format;
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = tokio::task::spawn_blocking(move || -> CsoResult<()> {
        writer::write_cso_blocking(
            &input_owned,
            &write_owned,
            format,
            block_size,
            index_shift,
            &bytes_done_bg,
            &cancel_bg,
        )
    });

    let cleanup = {
        let write_path = write_path.clone();
        move || -> CsoError {
            let _ = std::fs::remove_file(&write_path);
            CsoError::Cancelled
        }
    };
    if let Err(err) =
        await_with_progress_cancel(progress, &bytes_done, handle, &cancel, cleanup).await
    {
        let _ = tokio::fs::remove_file(&write_path).await;
        return Err(err);
    }

    tokio::fs::rename(&write_path, &output_path).await?;

    let out_size = tokio::fs::metadata(&output_path).await?.len();
    info!(
        "Original: {:.2} MB, {}: {:.2} MB ({:.1}% compression ratio)",
        total_mb,
        format.name(),
        out_size as f64 / BYTES_PER_MB,
        (out_size as f64 / input_size as f64) * 100.0
    );
    Ok(())
}

/// Compress every `.iso` under `input_dir`, descending into subdirectories
/// up to `max_depth` (`None` for unlimited). Outputs land next to their
/// inputs with the extension replaced by the format's, or mirror the source
/// tree under `output_dir` when one is given.
pub async fn compress_to_cso_batch(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &std::path::Path,
    opts: CsoCompressOptions,
    output_dir: Option<&std::path::Path>,
    max_depth: Option<usize>,
) -> CsoResult<()> {
    let images = crate::util::fs::collect_files_with_exts(input_dir, &["iso"], max_depth)?;
    if images.is_empty() {
        log::warn!("No .iso inputs found in {}", input_dir.display());
        return Ok(());
    }

    if let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }

    total_progress.start(
        images.len() as u64,
        &format!("Compressing {} images", images.len()),
    );

    for path in images {
        let output = crate::util::place_in_dir_mirrored(
            &path.with_extension(opts.format.extension()),
            input_dir,
            output_dir,
        );
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Err(err) = compress_to_cso(progress, path.clone(), output, opts.clone()).await {
            log::warn!("Failed to compress {}: {err}", path.display());
        }
        total_progress.inc(1);
    }

    total_progress.finish();
    Ok(())
}

/// Restore the original ISO from a CSO or ZSO container.
pub async fn decompress_from_cso(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    force: bool,
) -> CsoResult<()> {
    decompress_from_cso_cancellable(progress, input_path, output_path, force, CancelToken::new())
        .await
}

/// Restore the original ISO from a CSO or ZSO container, observing
/// `cancel` at every block boundary. Cleanup and overwrite guarantees
/// match [`compress_to_cso_cancellable`].
pub async fn decompress_from_cso_cancellable(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    force: bool,
    cancel: CancelToken,
) -> CsoResult<()> {
    let preexisting = tokio::fs::metadata(&output_path).await.is_ok();
    if preexisting && !force {
        return Err(CsoError::OutputAlreadyExists);
    }

    let peek_path = input_path.clone();
    let (uncompressed_size, format) =
        tokio::task::spawn_blocking(move || -> CsoResult<(u64, CsoFormat)> {
            let handle = reader::open_cso_sync(&peek_path)?;
            Ok((handle.header.uncompressed_size, handle.format))
        })
        .await??;

    let total_mb = uncompressed_size as f64 / BYTES_PER_MB;
    progress.start(
        uncompressed_size,
        &format!(
            "Decompressing {} to ISO (~{:.2} MB)",
            format.name(),
            total_mb
        ),
    );

    let write_path = scratch_output_path(&output_path);
    let input_owned = input_path.clone();
    let write_owned = write_path.clone();
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = tokio::task::spawn_blocking(move || -> CsoResult<()> {
        use crate::util::worker_pool::{Pool, parallelism};

        let handle = reader::open_cso_sync(&input_owned)?;
        let out_file = std::fs::File::create(&write_owned)?;
        let mut writer = std::io::BufWriter::with_capacity(IO_BUFFER_SIZE, out_file);

        let workers = reader::make_cso_extract_workers(parallelism(), handle.format, &handle.file);
        let pool: Pool<reader::CsoExtractWork, reader::CsoExtractedOut, CsoError> =
            Pool::spawn(workers);
        let result =
            reader::extract_blocks(&pool, &handle, &mut writer, &bytes_done_bg, &cancel_bg);
        pool.shutdown();
        result?;

        use std::io::Write as _;
        writer.flush()?;
        Ok(())
    });

    let cleanup = {
        let write_path = write_path.clone();
        move || -> CsoError {
            let _ = std::fs::remove_file(&write_path);
            CsoError::Cancelled
        }
    };
    if let Err(err) =
        await_with_progress_cancel(progress, &bytes_done, handle, &cancel, cleanup).await
    {
        let _ = tokio::fs::remove_file(&write_path).await;
        return Err(err);
    }

    tokio::fs::rename(&write_path, &output_path).await?;

    info!(
        "Decompressed: {:.2} MB ISO from {}",
        total_mb,
        input_path.display()
    );
    Ok(())
}

/// Digest a CSO/ZSO's decoded ISO content in a single streaming pass,
/// no temp files, following [`decompress_from_cso_cancellable`]'s
/// open/pool/drive shape but folding decoded blocks into the hashers
/// instead of a writer. The returned `size_bytes` is the uncompressed
/// ISO size.
///
/// Synchronous: intended to run inside the caller's `spawn_blocking`.
/// Progress is relayed through the shared `bytes_done` counter.
pub fn digest_cso_inner(
    path: &std::path::Path,
    algos: &[HashAlgo],
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
) -> CsoResult<FileDigests> {
    use crate::util::worker_pool::{Pool, parallelism};

    let handle = reader::open_cso_sync(path)?;
    let workers = reader::make_cso_extract_workers(parallelism(), handle.format, &handle.file);
    let pool: Pool<reader::CsoExtractWork, reader::CsoExtractedOut, CsoError> =
        Pool::spawn(workers);
    let result = reader::hash_blocks(&handle, &pool, algos, bytes_done, cancel);
    pool.shutdown();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::NoProgress;

    fn mixed_payload(len: usize) -> Vec<u8> {
        let mut data = vec![0u8; len];
        let mut state = 0xFEED_F00D_DEAD_BEEFu64;
        for (i, b) in data.iter_mut().enumerate() {
            if (i / 4096).is_multiple_of(2) {
                *b = (i / 53) as u8;
            } else {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *b = state as u8;
            }
        }
        data
    }

    async fn round_trip(format: CsoFormat, len: usize, block_size: Option<u32>) {
        let dir = tempfile::tempdir().unwrap();
        let data = mixed_payload(len);
        let iso = dir.path().join("game.iso");
        std::fs::write(&iso, &data).unwrap();

        let packed = dir.path().join(format!("game.{}", format.extension()));
        compress_to_cso(
            &NoProgress,
            iso,
            packed.clone(),
            CsoCompressOptions {
                format,
                block_size,
                force: false,
            },
        )
        .await
        .unwrap();

        let restored_path = dir.path().join("restored.iso");
        decompress_from_cso(&NoProgress, packed, restored_path.clone(), false)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&restored_path).unwrap(), data);
    }

    #[tokio::test]
    async fn cso_round_trips_with_raw_and_compressed_blocks() {
        round_trip(CsoFormat::Cso, 11 * 2048, None).await;
    }

    /// digest_cso_inner must equal a plain hash of the decompressed ISO
    /// for both formats.
    #[tokio::test]
    async fn digest_cso_inner_matches_decompressed_hash() {
        use crate::util::hash::{HashAlgo, hash_file};
        for format in [CsoFormat::Cso, CsoFormat::Zso] {
            let dir = tempfile::tempdir().unwrap();
            let data = mixed_payload(17 * 2048);
            let iso = dir.path().join("game.iso");
            std::fs::write(&iso, &data).unwrap();

            let packed = dir.path().join(format!("game.{}", format.extension()));
            compress_to_cso(
                &NoProgress,
                iso.clone(),
                packed.clone(),
                CsoCompressOptions {
                    format,
                    block_size: None,
                    force: false,
                },
            )
            .await
            .unwrap();

            let algos = [HashAlgo::Crc32, HashAlgo::Sha1, HashAlgo::Sha256];
            let bytes_done = Arc::new(AtomicU64::new(0));
            let inner =
                digest_cso_inner(&packed, &algos, &bytes_done, &CancelToken::new()).unwrap();
            let direct = hash_file(&iso, &algos, &NoProgress).unwrap();
            assert_eq!(inner, direct, "format {format:?}");
        }
    }

    #[tokio::test]
    async fn zso_round_trips() {
        round_trip(CsoFormat::Zso, 16 * 2048, None).await;
    }

    /// Build a DAX container in memory. `nc_areas` are (first_frame,
    /// frame_count) runs stored raw; when `raw_deflate_frame` is set,
    /// that frame is written as bare deflate (no zlib wrapper) to
    /// exercise the reader's fallback.
    fn build_dax(
        data: &[u8],
        nc_areas: &[(u32, u32)],
        version: u32,
        raw_deflate_frame: Option<usize>,
    ) -> Vec<u8> {
        use flate2::{Compress, Compression, FlushCompress, Status};
        use std::collections::HashSet;

        const FRAME: usize = 0x2000;
        let nframes = data.len().div_ceil(FRAME);
        let raw: HashSet<usize> = nc_areas
            .iter()
            .flat_map(|(f, c)| (*f as usize)..(*f as usize + *c as usize))
            .collect();

        let deflate = |chunk: &[u8], zlib: bool| -> Vec<u8> {
            let mut c = Compress::new(Compression::best(), zlib);
            let mut out = vec![0u8; chunk.len() + chunk.len() / 100 + 64];
            let status = c.compress(chunk, &mut out, FlushCompress::Finish).unwrap();
            assert_eq!(status, Status::StreamEnd);
            out.truncate(c.total_out() as usize);
            out
        };

        let mut frames: Vec<Vec<u8>> = Vec::with_capacity(nframes);
        for i in 0..nframes {
            let chunk = &data[i * FRAME..((i + 1) * FRAME).min(data.len())];
            if raw.contains(&i) {
                frames.push(chunk.to_vec());
            } else if raw_deflate_frame == Some(i) {
                frames.push(deflate(chunk, false));
            } else {
                frames.push(deflate(chunk, true));
            }
        }

        let table_size =
            nframes * 4 + nframes * 2 + if version >= 1 { nc_areas.len() * 8 } else { 0 };
        let mut file = Vec::new();
        file.extend_from_slice(&models::DAX_MAGIC);
        file.extend_from_slice(&(data.len() as u32).to_le_bytes());
        file.extend_from_slice(&version.to_le_bytes());
        file.extend_from_slice(&(nc_areas.len() as u32).to_le_bytes());
        file.extend_from_slice(&[0u8; 16]);

        let mut cursor = (0x20 + table_size) as u32;
        for f in &frames {
            file.extend_from_slice(&cursor.to_le_bytes());
            cursor += f.len() as u32;
        }
        for f in &frames {
            file.extend_from_slice(&(f.len() as u16).to_le_bytes());
        }
        if version >= 1 {
            for (first, count) in nc_areas {
                file.extend_from_slice(&first.to_le_bytes());
                file.extend_from_slice(&count.to_le_bytes());
            }
        }
        for f in &frames {
            file.extend_from_slice(f);
        }
        file
    }

    #[tokio::test]
    async fn dax_round_trips_with_raw_and_compressed_frames() {
        let dir = tempfile::tempdir().unwrap();
        // 2 full frames plus a short tail; frame 1 stored raw via an NC area.
        let data = mixed_payload(2 * 0x2000 + 0x1000);
        let packed = dir.path().join("game.dax");
        std::fs::write(&packed, build_dax(&data, &[(1, 1)], 1, None)).unwrap();

        let restored = dir.path().join("restored.iso");
        decompress_from_cso(&NoProgress, packed, restored.clone(), false)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&restored).unwrap(), data);
    }

    #[tokio::test]
    async fn dax_digest_matches_decompressed_hash() {
        use crate::util::hash::{HashAlgo, hash_file};
        let dir = tempfile::tempdir().unwrap();
        let data = mixed_payload(5 * 0x2000 + 0x800);
        let iso = dir.path().join("game.iso");
        std::fs::write(&iso, &data).unwrap();
        let packed = dir.path().join("game.dax");
        std::fs::write(&packed, build_dax(&data, &[(2, 2)], 1, None)).unwrap();

        let algos = [HashAlgo::Crc32, HashAlgo::Sha1, HashAlgo::Sha256];
        let bytes_done = Arc::new(AtomicU64::new(0));
        let inner = digest_cso_inner(&packed, &algos, &bytes_done, &CancelToken::new()).unwrap();
        let direct = hash_file(&iso, &algos, &NoProgress).unwrap();
        assert_eq!(inner, direct);
    }

    #[tokio::test]
    async fn dax_raw_deflate_frame_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let data = mixed_payload(3 * 0x2000);
        let packed = dir.path().join("game.dax");
        // Frame 0 has no zlib wrapper; the reader must retry it as raw
        // deflate after the zlib inflate rejects the missing header.
        std::fs::write(&packed, build_dax(&data, &[], 1, Some(0))).unwrap();

        let restored = dir.path().join("restored.iso");
        decompress_from_cso(&NoProgress, packed, restored.clone(), false)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&restored).unwrap(), data);
    }

    #[tokio::test]
    async fn dax_truncated_header_and_table_error_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let data = mixed_payload(3 * 0x2000);
        let full = build_dax(&data, &[(1, 1)], 1, None);

        let short_header = dir.path().join("head.dax");
        std::fs::write(&short_header, &full[..0x1C]).unwrap();
        let out = dir.path().join("head.iso");
        assert!(
            decompress_from_cso(&NoProgress, short_header, out, false)
                .await
                .is_err()
        );

        let short_table = dir.path().join("table.dax");
        std::fs::write(&short_table, &full[..0x24]).unwrap();
        let out2 = dir.path().join("table.iso");
        assert!(
            decompress_from_cso(&NoProgress, short_table, out2, false)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn short_last_block_round_trips_with_large_blocks() {
        // 16 KiB blocks over a 2048-aligned but not 16K-aligned
        // input leaves a short final block.
        round_trip(CsoFormat::Cso, 5 * 16384 + 3 * 2048, Some(16384)).await;
        round_trip(CsoFormat::Zso, 5 * 16384 + 3 * 2048, Some(16384)).await;
    }

    #[tokio::test]
    async fn nonzero_index_shift_round_trips() {
        // Force shift 2 to exercise offset packing + alignment
        // padding without a multi-GiB fixture; the reader follows
        // whatever shift the header declares.
        for format in [CsoFormat::Cso, CsoFormat::Zso] {
            let dir = tempfile::tempdir().unwrap();
            let data = mixed_payload(9 * 2048);
            let iso = dir.path().join("game.iso");
            std::fs::write(&iso, &data).unwrap();
            let packed = dir.path().join("game.cso");

            let bytes_done = Arc::new(AtomicU64::new(0));
            writer::write_cso_blocking(
                &iso,
                &packed,
                format,
                2048,
                2,
                &bytes_done,
                &CancelToken::new(),
            )
            .unwrap();

            let handle = reader::open_cso_sync(&packed).unwrap();
            assert_eq!(handle.header.index_shift, 2);
            for &entry in &handle.index {
                let offset = ((entry & !models::CISO_INDEX_UNCOMPRESSED) as u64) << 2;
                assert!(offset.is_multiple_of(4));
            }

            let restored_path = dir.path().join("restored.iso");
            decompress_from_cso(&NoProgress, packed, restored_path.clone(), false)
                .await
                .unwrap();
            assert_eq!(std::fs::read(&restored_path).unwrap(), data);
        }
    }

    #[tokio::test]
    async fn rejects_existing_output_and_bad_block_size() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        std::fs::write(&iso, vec![0u8; 4096]).unwrap();

        let exists = dir.path().join("exists.cso");
        std::fs::write(&exists, b"x").unwrap();
        assert!(matches!(
            compress_to_cso(
                &NoProgress,
                iso.clone(),
                exists,
                CsoCompressOptions::default()
            )
            .await,
            Err(CsoError::OutputAlreadyExists)
        ));

        let out = dir.path().join("out.cso");
        assert!(matches!(
            compress_to_cso(
                &NoProgress,
                iso,
                out,
                CsoCompressOptions {
                    block_size: Some(3000),
                    ..Default::default()
                }
            )
            .await,
            Err(CsoError::InvalidBlockSize(3000))
        ));
    }

    fn large_iso(dir: &std::path::Path) -> PathBuf {
        let iso = dir.join("game.iso");
        std::fs::write(&iso, mixed_payload(4096 * 2048)).unwrap();
        iso
    }

    #[tokio::test]
    async fn cancel_before_start_leaves_no_output() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        std::fs::write(&iso, mixed_payload(11 * 2048)).unwrap();
        let out = dir.path().join("game.cso");

        let token = CancelToken::new();
        token.cancel();
        let result = compress_to_cso_cancellable(
            &NoProgress,
            iso,
            out.clone(),
            CsoCompressOptions::default(),
            token,
        )
        .await;

        assert!(matches!(result, Err(CsoError::Cancelled)));
        assert!(!out.exists(), "no partial output");
        assert!(!scratch_output_path(&out).exists(), "no leftover temp");
    }

    #[tokio::test]
    async fn cancel_mid_stream_leaves_no_output() {
        let dir = tempfile::tempdir().unwrap();
        let iso = large_iso(dir.path());
        let out = dir.path().join("game.cso");

        let token = CancelToken::new();
        let token2 = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            token2.cancel();
        });

        let result = compress_to_cso_cancellable(
            &NoProgress,
            iso,
            out.clone(),
            CsoCompressOptions::default(),
            token,
        )
        .await;

        // Mid-stream timing can occasionally complete before the cancel
        // fires; accept either a clean cancel (no output) or a completed
        // run, but never a leftover temp file.
        match result {
            Err(CsoError::Cancelled) => {
                assert!(!out.exists(), "no partial output after mid-stream cancel");
            }
            Ok(()) => assert!(out.exists()),
            other => panic!("unexpected result: {other:?}"),
        }
        assert!(!scratch_output_path(&out).exists(), "no leftover temp");
    }

    #[tokio::test]
    async fn cancel_after_completion_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        std::fs::write(&iso, mixed_payload(11 * 2048)).unwrap();
        let out = dir.path().join("game.cso");

        let token = CancelToken::new();
        compress_to_cso_cancellable(
            &NoProgress,
            iso,
            out.clone(),
            CsoCompressOptions::default(),
            token.clone(),
        )
        .await
        .unwrap();
        token.cancel();
        assert!(out.exists(), "output survives a post-completion cancel");
    }

    #[tokio::test]
    async fn force_overwrite_preexisting_survives_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let iso = large_iso(dir.path());
        let out = dir.path().join("game.cso");
        let original = b"do not destroy me".to_vec();
        std::fs::write(&out, &original).unwrap();

        let token = CancelToken::new();
        let token2 = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            token2.cancel();
        });

        let result = compress_to_cso_cancellable(
            &NoProgress,
            iso,
            out.clone(),
            CsoCompressOptions {
                force: true,
                ..Default::default()
            },
            token,
        )
        .await;

        // Mid-stream timing can occasionally complete before the cancel
        // fires; on a clean cancel the pre-existing target stays intact,
        // while a completed run replaces it with a valid CSO. A leftover
        // temp file is never acceptable.
        match result {
            Err(CsoError::Cancelled) => {
                assert_eq!(
                    std::fs::read(&out).unwrap(),
                    original,
                    "pre-existing overwrite target must be untouched on cancel"
                );
            }
            Ok(()) => {
                let bytes = std::fs::read(&out).unwrap();
                assert_ne!(bytes, original, "a completed run must replace the target");
                assert_eq!(&bytes[..4], b"CISO", "output must be a valid CSO");
            }
            other => panic!("unexpected result: {other:?}"),
        }
        assert!(!scratch_output_path(&out).exists(), "no leftover temp");
    }

    /// Cross-checks against real maxcso; set ROMCONVERTO_MAXCSO to
    /// the binary path to enable.
    #[tokio::test]
    async fn maxcso_parity() {
        let Some(maxcso) = std::env::var_os("ROMCONVERTO_MAXCSO") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let data = mixed_payload(64 * 2048);
        let iso = dir.path().join("game.iso");
        std::fs::write(&iso, &data).unwrap();

        let their_cso = dir.path().join("their.cso");
        let status = std::process::Command::new(&maxcso)
            .arg(&iso)
            .arg("-o")
            .arg(&their_cso)
            .status()
            .expect("run maxcso");
        assert!(status.success(), "maxcso compress failed");
        let restored = dir.path().join("restored.iso");
        decompress_from_cso(&NoProgress, their_cso, restored.clone(), false)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&restored).unwrap(), data);

        let our_cso = dir.path().join("our.cso");
        compress_to_cso(
            &NoProgress,
            iso,
            our_cso.clone(),
            CsoCompressOptions::default(),
        )
        .await
        .unwrap();
        let their_restored = dir.path().join("their_restored.iso");
        let status = std::process::Command::new(&maxcso)
            .arg("--decompress")
            .arg(&our_cso)
            .arg("-o")
            .arg(&their_restored)
            .status()
            .expect("run maxcso --decompress");
        assert!(status.success(), "maxcso rejected our CSO");
        assert_eq!(std::fs::read(&their_restored).unwrap(), data);
    }
}
