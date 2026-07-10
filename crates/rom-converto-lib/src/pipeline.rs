//! One-step CSO/ZSO <-> CHD conversion: chains the existing CSO decode
//! and CHD encode paths (or the reverse) through a temporary ISO,
//! removing the intermediate when the chain finishes, whether it
//! succeeds, fails, or is cancelled.
//!
//! Accepts CSO, ZSO, and DAX inputs; the CSO decode path detects the
//! container by magic and produces the temporary ISO the CHD writer
//! consumes.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::chd::{
    ChdDvdOptions, DiscMode, convert_disc_to_chd_cancellable, extract_from_chd_cancellable,
    is_dvd_mode_chd,
};
use crate::cso::{
    CsoCompressOptions, CsoFormat, compress_to_cso, compress_to_cso_cancellable,
    decompress_from_cso_cancellable,
};
use crate::cue::to_iso::cue_to_iso;
use crate::util::{CancelToken, ProgressReporter};

/// Sibling scratch ISO next to `output`, in the same spirit as
/// [`crate::util::scratch_output_path`] but suffixed `.iso.tmp` so it
/// never collides with either format's own `.tmp` scratch file.
fn temp_iso_path(output: &Path) -> PathBuf {
    let mut name = output.file_name().unwrap_or_default().to_os_string();
    name.push(".iso.tmp");
    output.with_file_name(name)
}

fn reject_unsupported_input(input: &Path) -> Result<()> {
    let ext_ok = input.extension().is_some_and(|e| {
        e.eq_ignore_ascii_case("cso")
            || e.eq_ignore_ascii_case("zso")
            || e.eq_ignore_ascii_case("dax")
    });
    if ext_ok {
        return Ok(());
    }
    bail!(
        "unsupported input format: {} (expected .cso, .zso, or .dax)",
        input.display()
    );
}

/// Compress a `.cso`/`.zso` straight to a CHD: decode to a temporary
/// ISO, then run the same disc-to-CHD writer a direct build would use
/// (so CD/DVD routing and any embedded tags match exactly), and
/// always remove the temporary ISO afterward.
pub async fn cso_to_chd_cancellable(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    mode: Option<DiscMode>,
    opts: ChdDvdOptions,
    cancel: CancelToken,
) -> Result<()> {
    reject_unsupported_input(&input_path)?;

    let temp_iso = temp_iso_path(&output_path);
    let result: Result<()> = async {
        decompress_from_cso_cancellable(
            progress,
            input_path.clone(),
            temp_iso.clone(),
            true,
            cancel.clone(),
        )
        .await?;
        convert_disc_to_chd_cancellable(
            progress,
            temp_iso.clone(),
            output_path.clone(),
            mode,
            opts.clone(),
            cancel.clone(),
        )
        .await?;
        Ok(())
    }
    .await;

    let _ = std::fs::remove_file(&temp_iso);
    result
}

/// Extract a CHD straight to a `.cso`/`.zso`: extract to a temporary
/// ISO, then compress it, always removing the temporary ISO
/// afterward. Only DVD-mode CHDs qualify; a CD-mode CHD has no flat
/// ISO for the CSO/ZSO writer to consume.
pub async fn chd_to_cso_cancellable(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    opts: CsoCompressOptions,
    cancel: CancelToken,
) -> Result<()> {
    if !is_dvd_mode_chd(input_path.clone()).await? {
        bail!(
            "{} is a CD-mode CHD (bin/cue layout); CSO/ZSO only hold flat DVD-mode ISOs \
             (PS2 DVD, PSP UMD)",
            input_path.display()
        );
    }

    let temp_iso = temp_iso_path(&output_path);
    let result: Result<()> = async {
        extract_from_chd_cancellable(
            progress,
            input_path.clone(),
            temp_iso.clone(),
            None,
            cancel.clone(),
        )
        .await?;
        compress_to_cso_cancellable(
            progress,
            temp_iso.clone(),
            output_path.clone(),
            opts.clone(),
            cancel.clone(),
        )
        .await?;
        Ok(())
    }
    .await;

    let _ = std::fs::remove_file(&temp_iso);
    result
}

/// Compress a `.cue`/`.bin` straight to a `.cso`/`.zso`: extract the data
/// track to a temporary ISO, then compress it, always removing the temporary
/// ISO afterward.
pub async fn cue_to_cso(
    progress: &dyn ProgressReporter,
    cue_path: PathBuf,
    output_path: PathBuf,
    format: CsoFormat,
    force: bool,
) -> Result<()> {
    let temp_iso = temp_iso_path(&output_path);
    let result: Result<()> = async {
        cue_to_iso(progress, cue_path.clone(), temp_iso.clone(), true).await?;
        compress_to_cso(
            progress,
            temp_iso.clone(),
            output_path.clone(),
            CsoCompressOptions {
                format,
                block_size: None,
                force,
            },
        )
        .await?;
        Ok(())
    }
    .await;

    let _ = std::fs::remove_file(&temp_iso);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chd::{extract_from_chd, verify_chd};
    use crate::cso::{CsoFormat, decompress_from_cso};
    use crate::util::NoProgress;

    fn mixed_iso(sectors: usize) -> Vec<u8> {
        crate::chd::test_fixtures::mixed_iso(sectors)
    }

    async fn round_trip_cso_to_chd(format: CsoFormat) {
        let dir = tempfile::tempdir().unwrap();
        let iso = mixed_iso(11);
        let iso_path = dir.path().join("game.iso");
        std::fs::write(&iso_path, &iso).unwrap();

        let cso_path = dir.path().join(format!("game.{}", format.extension()));
        compress_to_cso_cancellable(
            &NoProgress,
            iso_path,
            cso_path.clone(),
            CsoCompressOptions {
                format,
                block_size: None,
                force: false,
            },
            CancelToken::new(),
        )
        .await
        .unwrap();

        let chd_path = dir.path().join("game.chd");
        cso_to_chd_cancellable(
            &NoProgress,
            cso_path,
            chd_path.clone(),
            None,
            ChdDvdOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();

        // The temp ISO never survives a successful chain.
        assert!(!temp_iso_path(&chd_path).exists());

        verify_chd(&NoProgress, chd_path.clone(), None, false)
            .await
            .unwrap();
        let restored = dir.path().join("restored");
        extract_from_chd(&NoProgress, chd_path, restored.clone(), None)
            .await
            .unwrap();
        assert_eq!(std::fs::read(restored.with_extension("iso")).unwrap(), iso);
    }

    #[tokio::test]
    async fn cso_to_chd_round_trips() {
        round_trip_cso_to_chd(CsoFormat::Cso).await;
    }

    #[tokio::test]
    async fn cue_to_cso_round_trips() {
        use crate::cso::decompress_from_cso;

        let dir = tempfile::tempdir().unwrap();
        // Three MODE1/2048 sectors: the extracted ISO is the raw payload.
        let iso = mixed_iso(3);
        let bin_path = dir.path().join("game.bin");
        std::fs::write(&bin_path, &iso).unwrap();
        let cue_path = dir.path().join("game.cue");
        std::fs::write(
            &cue_path,
            "FILE \"game.bin\" BINARY\r\n  TRACK 01 MODE1/2048\r\n    INDEX 01 00:00:00\r\n",
        )
        .unwrap();

        let zso_path = dir.path().join("game.zso");
        cue_to_cso(
            &NoProgress,
            cue_path,
            zso_path.clone(),
            CsoFormat::Zso,
            false,
        )
        .await
        .unwrap();

        assert!(!temp_iso_path(&zso_path).exists());

        let restored = dir.path().join("restored.iso");
        decompress_from_cso(&NoProgress, zso_path, restored.clone(), false)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&restored).unwrap(), iso);
    }

    #[tokio::test]
    async fn zso_to_chd_round_trips() {
        round_trip_cso_to_chd(CsoFormat::Zso).await;
    }

    #[tokio::test]
    async fn chd_to_cso_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let iso = mixed_iso(11);
        let iso_path = dir.path().join("game.iso");
        std::fs::write(&iso_path, &iso).unwrap();

        let chd_path = dir.path().join("game.chd");
        convert_disc_to_chd_cancellable(
            &NoProgress,
            iso_path,
            chd_path.clone(),
            None,
            ChdDvdOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();

        let cso_path = dir.path().join("game.cso");
        chd_to_cso_cancellable(
            &NoProgress,
            chd_path,
            cso_path.clone(),
            CsoCompressOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();

        assert!(!temp_iso_path(&cso_path).exists());

        let restored = dir.path().join("restored.iso");
        decompress_from_cso(&NoProgress, cso_path, restored.clone(), false)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&restored).unwrap(), iso);
    }

    #[tokio::test]
    async fn chd_to_cso_rejects_cd_mode() {
        use crate::chd::convert_to_chd;

        let dir = tempfile::tempdir().unwrap();
        let bin_path = dir.path().join("game.bin");
        std::fs::write(&bin_path, vec![0u8; 4 * 2352]).unwrap();
        let cue_path = dir.path().join("game.cue");
        std::fs::write(
            &cue_path,
            "FILE \"game.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n",
        )
        .unwrap();

        let chd_path = dir.path().join("game.chd");
        convert_to_chd(
            &NoProgress,
            cue_path,
            chd_path.clone(),
            false,
            CancelToken::new(),
        )
        .await
        .unwrap();

        let cso_path = dir.path().join("game.cso");
        let err = chd_to_cso_cancellable(
            &NoProgress,
            chd_path,
            cso_path.clone(),
            CsoCompressOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("CD-mode"));
        assert!(!cso_path.exists());
        assert!(!temp_iso_path(&cso_path).exists());
    }

    #[tokio::test]
    async fn cso_to_chd_rejects_unsupported_extension() {
        let dir = tempfile::tempdir().unwrap();
        let iso_path = dir.path().join("game.iso");
        std::fs::write(&iso_path, b"not a container").unwrap();

        let chd_path = dir.path().join("game.chd");
        let err = cso_to_chd_cancellable(
            &NoProgress,
            iso_path,
            chd_path.clone(),
            None,
            ChdDvdOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("unsupported input format"));
        assert!(!chd_path.exists());
        assert!(!temp_iso_path(&chd_path).exists());
    }

    #[tokio::test]
    async fn cso_to_chd_cancel_leaves_no_temp_iso_or_output() {
        let dir = tempfile::tempdir().unwrap();
        let iso = mixed_iso(11);
        let iso_path = dir.path().join("game.iso");
        std::fs::write(&iso_path, &iso).unwrap();

        let cso_path = dir.path().join("game.cso");
        compress_to_cso_cancellable(
            &NoProgress,
            iso_path,
            cso_path.clone(),
            CsoCompressOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();

        let chd_path = dir.path().join("game.chd");
        let cancel = CancelToken::new();
        cancel.cancel();
        let err = cso_to_chd_cancellable(
            &NoProgress,
            cso_path,
            chd_path.clone(),
            None,
            ChdDvdOptions::default(),
            cancel,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("cancelled"));
        assert!(!chd_path.exists());
        assert!(!temp_iso_path(&chd_path).exists());
    }
}
