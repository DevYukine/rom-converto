//! Conversion between the CIA and CCI/3DS container formats, dispatched by
//! input file extension. See [`cia_to_cci`] and [`cci_to_cia`].

mod cci_to_cia;
mod cia_to_cci;
mod template;

pub use cci_to_cia::cci_to_cia;
pub use cia_to_cci::cia_to_cci;

use crate::nintendo::ctr::util::{mirrored_output, run_batch};
use crate::util::{CancelToken, ProgressReporter};
use anyhow::{Result, bail};
use log::debug;
use std::path::{Path, PathBuf};

const CIA_EXTS: &[&str] = &["cia"];
const CCI_EXTS: &[&str] = &["3ds", "cci"];
const CONVERT_EXTS: &[&str] = &["cia", "3ds", "cci"];

/// Derives the output path for a format conversion: `.cia` becomes `.3ds`,
/// `.3ds`/`.cci` become `.cia`, anything else becomes `.out`.
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

/// Convert `input` to the other CTR container, picking the direction
/// from its extension.
pub async fn convert_rom(
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
        cia_to_cci(input, output, progress, cancel).await
    } else if CCI_EXTS.contains(&ext.as_str()) {
        cci_to_cia(input, output, progress, cancel).await
    } else {
        bail!(
            "input extension '{}' is not convertible (expected .cia, .3ds, or .cci)",
            ext
        )
    }
}

/// Convert every CTR container under `input_dir`.
pub async fn convert_rom_batch(
    input_dir: &Path,
    output_dir: Option<&Path>,
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    max_depth: Option<usize>,
    cancel: CancelToken,
) -> Result<()> {
    run_batch(
        input_dir,
        CONVERT_EXTS,
        ("Converting", "convert"),
        max_depth,
        total_progress,
        &cancel,
        async |path| {
            let output =
                mirrored_output(&derive_converted_path(path), input_dir, output_dir).await?;
            debug!("Converting {} -> {}", path.display(), output.display());
            convert_rom(path, &output, progress, cancel.clone()).await
        },
    )
    .await
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
