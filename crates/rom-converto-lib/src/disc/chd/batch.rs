//! Directory-wide CHD operations: collect inputs by extension, mirror
//! each output under an optional destination tree, and keep going past
//! a per-file failure.

use crate::disc::chd::error::ChdResult;
use crate::util::{ProgressReporter, place_in_dir_mirrored};
use log::{info, warn};
use std::path::{Path, PathBuf};

use super::*;

/// Everything [`run_file_batch`] needs that is not a closure.
pub(crate) struct BatchSpec<'a> {
    pub input_dir: &'a Path,
    pub output_dir: Option<&'a Path>,
    pub exts: &'a [&'a str],
    pub max_depth: Option<usize>,
    /// Progress-bar verb, e.g. `"Extracting"`.
    pub verb: &'a str,
    /// Failure-log verb, e.g. `"extract"`.
    pub action: &'a str,
    /// Progress-bar noun, e.g. `"chd files"`.
    pub noun: &'a str,
}

/// Drive `each` over every matching file under `spec.input_dir`.
///
/// `output_for` maps an input to the path it should be written to, or
/// `None` to skip that file (the closure logs why). Per-file failures
/// are logged and counted rather than propagated; the returned counts
/// are `(ok, failed)`.
pub(crate) async fn run_file_batch<E: std::fmt::Display>(
    spec: BatchSpec<'_>,
    total_progress: &dyn ProgressReporter,
    mut output_for: impl FnMut(&Path) -> Option<PathBuf>,
    mut each: impl AsyncFnMut(PathBuf, PathBuf) -> Result<(), E>,
) -> std::io::Result<(usize, usize)> {
    let inputs = crate::util::fs::collect_files_with_exts(
        spec.input_dir,
        spec.exts,
        spec.max_depth,
        &CancelToken::new(),
    )?;
    if inputs.is_empty() {
        let exts = spec
            .exts
            .iter()
            .map(|e| format!(".{e}"))
            .collect::<Vec<_>>()
            .join(", ");
        warn!("No {exts} inputs found in {}", spec.input_dir.display());
        return Ok((0, 0));
    }

    if let Some(dir) = spec.output_dir {
        std::fs::create_dir_all(dir)?;
    }

    let total = inputs.len();
    total_progress.start(
        total as u64,
        &format!("{} {total} {}", spec.verb, spec.noun),
    );

    let (mut ok, mut failed) = (0usize, 0usize);
    for input in inputs {
        if let Some(output) = output_for(&input) {
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)?;
            }
            match each(input.clone(), output).await {
                Ok(()) => ok += 1,
                Err(err) => {
                    failed += 1;
                    warn!("Failed to {} {}: {err}", spec.action, input.display());
                }
            }
        }
        total_progress.inc(1);
    }

    total_progress.finish();
    Ok((ok, failed))
}

/// Compress every `.cue`, `.iso`, and `.avi` under `input_dir`, descending
/// into subdirectories up to `max_depth` (`None` for unlimited). Outputs
/// land next to their inputs with the extension replaced by `.chd`, or
/// mirror the source tree under `output_dir` when one is given.
pub async fn convert_disc_to_chd_batch(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &std::path::Path,
    opts: ChdOptions,
    output_dir: Option<&std::path::Path>,
    max_depth: Option<usize>,
) -> ChdResult<()> {
    run_file_batch(
        BatchSpec {
            input_dir,
            output_dir,
            exts: &["cue", "iso", "avi"],
            max_depth,
            verb: "Compressing",
            action: "compress",
            noun: "discs",
        },
        total_progress,
        |path| {
            Some(place_in_dir_mirrored(
                &path.with_extension("chd"),
                input_dir,
                output_dir,
            ))
        },
        async |input, output| {
            convert_disc_to_chd(
                progress,
                input,
                output,
                None,
                opts.clone(),
                CancelToken::new(),
            )
            .await
        },
    )
    .await?;
    Ok(())
}

/// Extract every `.chd` in `input_dir` beside its input: CD-mode CHDs
/// become `.cue` + `.bin`, DVD-mode CHDs become `.iso` (the output
/// extension is derived per file by [`extract_from_chd`]). A failure
/// on one file is logged and skipped rather than aborting the batch.
pub async fn extract_from_chd_batch(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: PathBuf,
    output_dir: Option<&std::path::Path>,
    max_depth: Option<usize>,
) -> ChdResult<()> {
    run_file_batch(
        BatchSpec {
            input_dir: &input_dir,
            output_dir,
            exts: &["chd"],
            max_depth,
            verb: "Extracting",
            action: "extract",
            noun: "chd files",
        },
        total_progress,
        |path| {
            Some(place_in_dir_mirrored(
                &path.with_extension(""),
                &input_dir,
                output_dir,
            ))
        },
        async |input, output| {
            extract_from_chd(progress, input, output, None, CancelToken::new()).await
        },
    )
    .await?;
    Ok(())
}

/// Verify every `.chd` in `input_dir`. Logs a per-file failure and a final
/// `Verified N files: X OK, Y failed` summary.
pub async fn verify_chd_batch(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: PathBuf,
    fix: bool,
    max_depth: Option<usize>,
) -> ChdResult<()> {
    let (ok, failed) = run_file_batch(
        BatchSpec {
            input_dir: &input_dir,
            output_dir: None,
            exts: &["chd"],
            max_depth,
            verb: "Verifying",
            action: "verify",
            noun: "chd files",
        },
        total_progress,
        |path| Some(path.to_path_buf()),
        async |input, _output| verify_chd(progress, input, None, fix, CancelToken::new()).await,
    )
    .await?;
    info!("Verified {} files: {ok} OK, {failed} failed", ok + failed);
    Ok(())
}

/// Batch twin of [`migrate_chd_to_v5`], mirroring
/// [`convert_disc_to_chd_batch`]: every `.chd` under `input_dir` that is
/// not already V5 is migrated.
pub async fn migrate_chd_to_v5_batch(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &std::path::Path,
    opts: ChdOptions,
    output_dir: Option<&std::path::Path>,
    max_depth: Option<usize>,
    in_place: bool,
) -> ChdResult<()> {
    run_file_batch(
        BatchSpec {
            input_dir,
            output_dir,
            exts: &["chd"],
            max_depth,
            verb: "Migrating",
            action: "migrate",
            noun: "chd files",
        },
        total_progress,
        |path| {
            match legacy::peek_chd_version(path) {
                Ok(Some(5)) => {
                    warn!("Skipping {}: already CHD V5", path.display());
                    return None;
                }
                Err(err) => {
                    warn!("Skipping {}: {err}", path.display());
                    return None;
                }
                _ => {}
            }
            Some(match (in_place, output_dir) {
                (true, _) => path.to_path_buf(),
                (false, Some(_)) => place_in_dir_mirrored(path, input_dir, output_dir),
                (false, None) => migrated_chd_path(path),
            })
        },
        async |input, output| {
            migrate_chd_to_v5(progress, input, output, opts.clone(), CancelToken::new()).await
        },
    )
    .await?;
    Ok(())
}
