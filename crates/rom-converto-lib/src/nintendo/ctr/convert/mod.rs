mod cci_to_cia;
mod cia_to_cci;
mod template;

pub use cci_to_cia::cci_to_cia;
pub use cia_to_cci::cia_to_cci;

use crate::nintendo::ctr::has_matching_extension;
use crate::util::ProgressReporter;
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
    let ext = input
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    if CIA_EXTS.contains(&ext.as_str()) {
        cia_to_cci(input, output, progress).await
    } else if CCI_EXTS.contains(&ext.as_str()) {
        cci_to_cia(input, output, progress).await
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
) -> Result<()> {
    let mut count: u64 = 0;
    let mut scan = fs::read_dir(input_dir).await?;
    while let Ok(Some(entry)) = scan.next_entry().await {
        if has_matching_extension(&entry.path(), CONVERT_EXTS) {
            count += 1;
        }
    }

    if count == 0 {
        warn!(
            "No supported ROM files found in {} (looked for {:?})",
            input_dir.display(),
            CONVERT_EXTS
        );
        return Ok(());
    }

    total_progress.start(count, &format!("Converting {count} files..."));

    if let Some(dir) = output_dir {
        fs::create_dir_all(dir).await?;
    }

    let mut entries = fs::read_dir(input_dir).await?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !has_matching_extension(&path, CONVERT_EXTS) {
            debug!("Skipping {} (not a supported ROM)", path.display());
            continue;
        }

        let output = crate::util::place_in_dir(&derive_converted_path(&path), output_dir);
        debug!("Converting {} -> {}", path.display(), output.display());

        if let Err(err) = convert_rom(&path, &output, progress).await {
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
