//! CSO (CISO v1) and ZSO support: block-compressed PSP/PS2 ISO
//! containers, the maxcso equivalent.
//!
//! Target matrix: real PSP hardware (CFW) and PPSSPP read CSO v1;
//! Open PS2 Loader (>= 1.2) on real PS2 reads ZSO. CSO v2 was never
//! adopted (PPSSPP rejects it) and DAX is legacy; both are out of
//! scope.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use log::info;

use crate::cd::IO_BUFFER_SIZE;
use crate::util::{BYTES_PER_MB, ProgressReporter, await_with_progress};

pub mod compression;
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
    if tokio::fs::metadata(&output_path).await.is_ok() && !opts.force {
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

    let input_owned = input_path.clone();
    let output_owned = output_path.clone();
    let format = opts.format;
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = tokio::task::spawn_blocking(move || -> CsoResult<()> {
        writer::write_cso_blocking(
            &input_owned,
            &output_owned,
            format,
            block_size,
            index_shift,
            &bytes_done_bg,
        )
    });

    await_with_progress(progress, &bytes_done, handle).await?;

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

/// Compress every `.iso` directly inside `input_dir` (top level only,
/// matching the other batch commands). Outputs land next to their
/// inputs with the extension replaced by the format's.
pub async fn compress_to_cso_batch(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &std::path::Path,
    opts: CsoCompressOptions,
) -> CsoResult<()> {
    let is_iso = |path: &std::path::Path| {
        path.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("iso"))
    };

    let mut count: u64 = 0;
    let mut scan = tokio::fs::read_dir(input_dir).await?;
    while let Ok(Some(entry)) = scan.next_entry().await {
        if is_iso(&entry.path()) {
            count += 1;
        }
    }
    if count == 0 {
        log::warn!("No .iso inputs found in {}", input_dir.display());
        return Ok(());
    }

    total_progress.start(count, &format!("Compressing {count} images..."));

    let mut entries = tokio::fs::read_dir(input_dir).await?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !is_iso(&path) {
            continue;
        }
        let output = path.with_extension(opts.format.extension());
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
    if tokio::fs::metadata(&output_path).await.is_ok() && !force {
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

    let input_owned = input_path.clone();
    let output_owned = output_path.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = tokio::task::spawn_blocking(move || -> CsoResult<()> {
        use crate::util::worker_pool::{Pool, parallelism};

        let handle = reader::open_cso_sync(&input_owned)?;
        let out_file = std::fs::File::create(&output_owned)?;
        let mut writer = std::io::BufWriter::with_capacity(IO_BUFFER_SIZE, out_file);

        let workers = reader::make_cso_extract_workers(parallelism(), handle.format, &handle.file);
        let pool: Pool<reader::CsoExtractWork, reader::CsoExtractedOut, CsoError> =
            Pool::spawn(workers);
        let result = reader::extract_blocks(&pool, &handle, &mut writer, &bytes_done_bg);
        pool.shutdown();
        result?;

        use std::io::Write as _;
        writer.flush()?;
        Ok(())
    });

    await_with_progress(progress, &bytes_done, handle).await?;

    info!("Decompressed: {:.2} MB ISO from {:?}", total_mb, input_path);
    Ok(())
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

    #[tokio::test]
    async fn zso_round_trips() {
        round_trip(CsoFormat::Zso, 16 * 2048, None).await;
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
            writer::write_cso_blocking(&iso, &packed, format, 2048, 2, &bytes_done).unwrap();

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
