//! Xbox 360 ("xenon") ISO -> XDVDFS -> ZArchive pipeline. Content
//! always lands at the archive root, matching what Xenia expects to
//! mount. [`convert_to_god`] takes the same ISOs the other way, into the
//! Games on Demand layout a console installs.

mod error;
mod extract;
mod god;
mod info;
mod pack;
mod verify;

#[cfg(test)]
pub(crate) mod test_fixtures;

pub use error::{XenonError, XenonResult};
pub use extract::XenonExtractSummary;
pub use god::{GodError, GodResult, GodSummary, convert_to_god, convert_to_god_cancellable};
pub use info::{ZarInfo, read_info};
pub use pack::{XenonPackSummary, total_input_bytes};
pub use verify::ZarVerifyResult;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use tokio::task;

use crate::util::{CancelToken, ProgressReporter, await_with_progress_cancel, scratch_output_path};

/// Pack `input` (an XDVDFS ISO file or an already-extracted game
/// directory) into a ZArchive at `output`.
pub async fn pack_zar(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
) -> XenonResult<()> {
    pack_zar_cancellable(input, output, progress, CancelToken::new()).await
}

/// Like [`pack_zar`] but observes `cancel` at chunk boundaries; on
/// cancel the partial archive is removed.
pub async fn pack_zar_cancellable(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> XenonResult<()> {
    let total = {
        let input = input.to_path_buf();
        task::spawn_blocking(move || pack::total_input_bytes(&input)).await??
    };
    progress.start(total, "Packing Xbox 360 ZArchive");

    let write_path = scratch_output_path(output)?;
    let input_owned = input.to_path_buf();
    let write_owned = write_path.to_path_buf();
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = task::spawn_blocking(move || -> XenonResult<pack::XenonPackSummary> {
        pack::pack_blocking(&input_owned, &write_owned, bytes_done_bg, &cancel_bg)
    });

    let cleanup = {
        let write_path = write_path.to_path_buf();
        move || -> XenonError {
            let _ = std::fs::remove_file(&write_path);
            XenonError::Cancelled
        }
    };
    let summary =
        match await_with_progress_cancel(progress, &bytes_done, handle, &cancel, cleanup).await {
            Ok(summary) => summary,
            Err(err) => {
                let _ = tokio::fs::remove_file(&write_path).await;
                return Err(err);
            }
        };
    crate::util::publish_temp(write_path, output, true)?;

    if !summary.has_default_xex {
        progress.warn("archive has no root-level default.xex; Xenia will refuse to mount it");
    }
    Ok(())
}

/// Extract every file in the ZArchive `input` into `output_dir`.
pub async fn extract_zar(
    input: &Path,
    output_dir: &Path,
    progress: &dyn ProgressReporter,
) -> XenonResult<()> {
    extract_zar_cancellable(input, output_dir, progress, CancelToken::new()).await
}

/// Like [`extract_zar`] but observes `cancel` at block boundaries.
/// Output is a directory rather than a single file, so unlike the pack
/// path there is no scratch/publish rename: files already extracted
/// stay on disk if cancelled.
pub async fn extract_zar_cancellable(
    input: &Path,
    output_dir: &Path,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> XenonResult<()> {
    let total = {
        let input = input.to_path_buf();
        task::spawn_blocking(move || extract::logical_size(&input)).await??
    };
    progress.start(total, "Extracting Xbox 360 ZArchive");

    let input_owned = input.to_path_buf();
    let output_owned = output_dir.to_path_buf();
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = task::spawn_blocking(move || -> XenonResult<extract::XenonExtractSummary> {
        extract::extract_blocking(&input_owned, &output_owned, &bytes_done_bg, &cancel_bg)
    });

    await_with_progress_cancel(progress, &bytes_done, handle, &cancel, || {
        XenonError::Cancelled
    })
    .await?;
    Ok(())
}

/// Verify a ZArchive: re-hash its stored digest and decode every block
/// to prove the compressed data is intact.
pub async fn verify_zar(
    input: &Path,
    progress: &dyn ProgressReporter,
) -> XenonResult<ZarVerifyResult> {
    verify_zar_cancellable(input, progress, CancelToken::new()).await
}

/// Like [`verify_zar`] but observes `cancel` at block boundaries.
pub async fn verify_zar_cancellable(
    input: &Path,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> XenonResult<ZarVerifyResult> {
    let input_owned = input.to_path_buf();
    let total = {
        let input = input_owned.clone();
        task::spawn_blocking(move || extract::logical_size(&input)).await??
    };
    progress.start(total, "Verifying Xbox 360 ZArchive");

    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = task::spawn_blocking(move || -> XenonResult<ZarVerifyResult> {
        verify::verify_blocking(&input_owned, &bytes_done_bg, &cancel_bg)
    });

    await_with_progress_cancel(progress, &bytes_done, handle, &cancel, || {
        XenonError::Cancelled
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Captures every `warn()` call; every other method is a no-op.
    struct WarnRecorder {
        warnings: Mutex<Vec<String>>,
    }

    impl WarnRecorder {
        fn new() -> Self {
            Self {
                warnings: Mutex::new(Vec::new()),
            }
        }
    }

    impl ProgressReporter for WarnRecorder {
        fn start(&self, _total: u64, _msg: &str) {}
        fn inc(&self, _delta: u64) {}
        fn finish(&self) {}
        fn warn(&self, message: &str) {
            self.warnings.lock().unwrap().push(message.to_string());
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn packing_without_default_xex_warns_but_succeeds() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("readme.txt"), b"no game here").unwrap();

        let work = tempfile::tempdir().unwrap();
        let output = work.path().join("archive.zar");
        let recorder = WarnRecorder::new();

        pack_zar(src.path(), &output, &recorder).await.unwrap();
        assert!(output.exists());

        let warnings = recorder.warnings.lock().unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("default.xex"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pack_extract_verify_and_info_round_trip() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("default.xex"), b"xex-bytes").unwrap();
        std::fs::create_dir(src.path().join("data")).unwrap();
        std::fs::write(src.path().join("data/save.bin"), b"save-bytes").unwrap();

        let work = tempfile::tempdir().unwrap();
        let zar_path = work.path().join("game.zar");
        let recorder = WarnRecorder::new();
        pack_zar(src.path(), &zar_path, &recorder).await.unwrap();
        assert!(recorder.warnings.lock().unwrap().is_empty());

        let info = read_info(&zar_path).unwrap();
        assert_eq!(info.file_count, 2);
        assert!(info.has_default_xex);

        let verify = verify_zar(&zar_path, &recorder).await.unwrap();
        assert!(verify.ok());
        assert_eq!(verify.logical_bytes, info.logical_size);

        let out_dir = work.path().join("out");
        extract_zar(&zar_path, &out_dir, &recorder).await.unwrap();
        assert_eq!(
            std::fs::read(out_dir.join("default.xex")).unwrap(),
            b"xex-bytes"
        );
        assert_eq!(
            std::fs::read(out_dir.join("data/save.bin")).unwrap(),
            b"save-bytes"
        );
    }
}
