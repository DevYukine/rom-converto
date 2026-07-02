//! Conversion between the CIA and CCI/3DS container formats, dispatched by
//! input file extension. See [`cia_to_cci`] and [`cci_to_cia`].

mod cci_to_cia;
mod cia_to_cci;
mod template;

pub use cci_to_cia::{cci_to_cia, cci_to_cia_cancellable};
pub use cia_to_cci::{cia_to_cci, cia_to_cci_cancellable};

use crate::nintendo::ctr::error::NintendoCTRError;
use crate::util::{CancelToken, ProgressReporter};
use anyhow::{Result, bail};
use log::{debug, warn};
use std::path::{Path, PathBuf};
use tokio::fs;

const CIA_EXTS: &[&str] = &["cia"];
const CCI_EXTS: &[&str] = &["3ds", "cci"];
const CONVERT_EXTS: &[&str] = &["cia", "3ds", "cci"];

pub fn derive_converted_path(input: &Path) -> PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
    let ext = input
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let new_ext = match ext.as_str() {
        "cia" => "3ds",
        "3ds" | "cci" => "cia",
        _ => "out",
    };
    input.with_file_name(format!("{stem}.{new_ext}"))
}

pub async fn convert_rom(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
) -> Result<()> {
    convert_rom_cancellable(input, output, progress, CancelToken::new()).await
}

pub async fn convert_rom_cancellable(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<()> {
    let ext = input
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    if CIA_EXTS.contains(&ext.as_str()) {
        cia_to_cci_cancellable(input, output, progress, cancel).await
    } else if CCI_EXTS.contains(&ext.as_str()) {
        cci_to_cia_cancellable(input, output, progress, cancel).await
    } else {
        bail!(
            "input extension '{}' is not convertible (expected .cia, .3ds, or .cci)",
            ext
        )
    }
}

pub async fn convert_rom_batch(
    input_dir: &Path,
    output_dir: Option<&Path>,
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    max_depth: Option<usize>,
) -> Result<()> {
    convert_rom_batch_cancellable(
        input_dir,
        output_dir,
        progress,
        total_progress,
        max_depth,
        CancelToken::new(),
    )
    .await
}

pub async fn convert_rom_batch_cancellable(
    input_dir: &Path,
    output_dir: Option<&Path>,
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    max_depth: Option<usize>,
    cancel: CancelToken,
) -> Result<()> {
    let roms = crate::util::fs::collect_files_with_exts(input_dir, CONVERT_EXTS, max_depth)?;
    if roms.is_empty() {
        warn!(
            "No supported ROM files found in {} (looked for {:?})",
            input_dir.display(),
            CONVERT_EXTS
        );
        return Ok(());
    }

    total_progress.start(
        roms.len() as u64,
        &format!("Converting {} files", roms.len()),
    );

    if let Some(dir) = output_dir {
        fs::create_dir_all(dir).await?;
    }

    for path in roms {
        if cancel.is_cancelled() {
            return Err(NintendoCTRError::Cancelled.into());
        }
        let output = crate::util::place_in_dir_mirrored(
            &derive_converted_path(&path),
            input_dir,
            output_dir,
        );
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).await?;
        }
        debug!("Converting {} -> {}", path.display(), output.display());

        if let Err(err) = convert_rom_cancellable(&path, &output, progress, cancel.clone()).await {
            if matches!(
                err.downcast_ref::<NintendoCTRError>(),
                Some(NintendoCTRError::Cancelled)
            ) {
                return Err(err);
            }
            warn!("Failed to convert {}: {err}", path.display());
        }

        total_progress.inc(1);
    }

    total_progress.finish();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_path_cia_to_3ds() {
        assert_eq!(
            derive_converted_path(Path::new("game.cia")),
            PathBuf::from("game.3ds"),
        );
    }

    #[test]
    fn convert_path_3ds_to_cia() {
        assert_eq!(
            derive_converted_path(Path::new("game.3ds")),
            PathBuf::from("game.cia"),
        );
    }

    #[test]
    fn convert_path_cci_to_cia() {
        assert_eq!(
            derive_converted_path(Path::new("game.cci")),
            PathBuf::from("game.cia"),
        );
    }

    #[test]
    fn convert_path_case_insensitive() {
        assert_eq!(
            derive_converted_path(Path::new("Game.CIA")),
            PathBuf::from("Game.3ds"),
        );
    }

    #[test]
    fn convert_path_preserves_directory() {
        assert_eq!(
            derive_converted_path(Path::new("/roms/game.cia")),
            PathBuf::from("/roms/game.3ds"),
        );
    }

    #[test]
    fn convert_path_unknown_extension_falls_back() {
        assert_eq!(
            derive_converted_path(Path::new("game.bin")),
            PathBuf::from("game.out"),
        );
    }
}
