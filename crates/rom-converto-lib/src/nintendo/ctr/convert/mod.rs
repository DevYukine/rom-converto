//! Conversion between the CIA and CCI/3DS container formats, dispatched by
//! input file extension. See [`cia_to_cci`] and [`cci_to_cia`].

mod cci_to_cia;
mod cia_to_cci;
mod template;

pub use cci_to_cia::cci_to_cia;
use cia_to_cci::cci_image_size;
pub use cia_to_cci::cia_to_cci;

use crate::nintendo::ctr::util::{mirrored_output, run_batch};
use crate::util::{CancelToken, ProgressReporter};
use anyhow::{Result, bail};
use log::{debug, warn};
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
/// from its extension. `trim` drops the trailing card padding when the
/// output is a CCI and is ignored for CIA output.
pub async fn convert_rom(
    input: &Path,
    output: &Path,
    trim: bool,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<()> {
    let ext = input
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    if CIA_EXTS.contains(&ext.as_str()) {
        cia_to_cci(input, output, trim, progress, cancel).await
    } else if CCI_EXTS.contains(&ext.as_str()) {
        if trim {
            warn!(
                "trim only applies to CCI output, ignoring it for {}",
                input.display()
            );
        }
        cci_to_cia(input, output, progress, cancel).await
    } else {
        bail!(
            "input extension '{}' is not convertible (expected .cia, .3ds, or .cci)",
            ext
        )
    }
}

/// Returns the byte size [`convert_rom`] will write for `input`, for free
/// space checks. CCI output is sized from the CIA's partition layout and
/// card padding. CIA output uses the source size, which over-estimates by
/// the card padding of an untrimmed cart image.
pub fn converted_size(input: &Path, trim: bool) -> Result<u64> {
    let ext = input
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if CIA_EXTS.contains(&ext.as_str()) {
        cci_image_size(input, trim)
    } else {
        Ok(std::fs::metadata(input)?.len())
    }
}

/// Convert every CTR container under `input_dir`. See [`convert_rom`]
/// for `trim`.
pub async fn convert_rom_batch(
    input_dir: &Path,
    output_dir: Option<&Path>,
    trim: bool,
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
            convert_rom(path, &output, trim, progress, cancel.clone()).await
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converted_size_of_cci_input_is_its_file_size() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("game.3ds");
        std::fs::write(&path, [0u8; 0x300]).expect("write");
        assert_eq!(converted_size(&path, true).expect("size"), 0x300);
    }

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
