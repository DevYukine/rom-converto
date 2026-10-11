//! Directory-wide CHD operations: collect inputs by extension, mirror
//! each output under an optional destination tree, and keep going past
//! a per-file failure.

use crate::disc::chd::error::{ChdError, ChdResult};
use crate::util::{ProgressReporter, place_in_dir_mirrored};
use log::{info, warn};
use std::path::{Path, PathBuf};

use super::*;

/// Everything [`run_file_batch`] needs that is not a closure.
pub(crate) struct BatchSpec<'a> {
    pub input_dir: &'a Path,
    pub output_dir: Option<&'a Path>,
    pub exts: &'static [&'static str],
    pub max_depth: Option<usize>,
    /// Progress-bar verb, e.g. `"Extracting"`.
    pub verb: &'a str,
    /// Failure-log verb, e.g. `"extract"`.
    pub action: &'a str,
    /// Progress-bar noun, e.g. `"chd files"`.
    pub noun: &'a str,
    /// Archives holding a member with one of `exts` are inputs too.
    pub open_archives: bool,
}

/// Drive `each` over every matching file under `spec.input_dir`.
///
/// `output_for` maps an input to the path it should be written to, or
/// `None` to skip that file (the closure logs why). Per-file failures
/// are logged and counted rather than propagated; the returned counts
/// are `(ok, failed)`. Cancelling `cancel` stops the walk or the loop
/// with an `Interrupted` error.
pub(crate) async fn run_file_batch<E: std::fmt::Display>(
    spec: BatchSpec<'_>,
    total_progress: &dyn ProgressReporter,
    cancel: &CancelToken,
    mut output_for: impl FnMut(&Path) -> Option<PathBuf>,
    mut each: impl AsyncFnMut(PathBuf, PathBuf) -> Result<(), E>,
) -> std::io::Result<(usize, usize)> {
    let input_dir = spec.input_dir.to_path_buf();
    let exts = spec.exts;
    let max_depth = spec.max_depth;
    let open_archives = spec.open_archives;
    let walk_cancel = cancel.clone();
    let inputs = tokio::task::spawn_blocking(move || {
        if open_archives {
            crate::util::collect_inputs(
                &input_dir,
                &crate::util::InputScan {
                    exts,
                    member_exts: None,
                    max_depth,
                    skip_unpacked_twins: false,
                },
                &walk_cancel,
            )
        } else {
            crate::util::fs::collect_files_with_exts(&input_dir, exts, max_depth, &walk_cancel)
        }
    })
    .await
    .map_err(std::io::Error::other)??;
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
        if cancel.is_cancelled() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                crate::util::Cancelled,
            ));
        }
        if let Some(output) = output_for(&input) {
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)?;
            }
            match each(input.clone(), output).await {
                Ok(()) => ok += 1,
                Err(err) => {
                    // A cancelled token must surface as an interrupt, not as
                    // one more counted per-file failure.
                    if cancel.is_cancelled() {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            crate::util::Cancelled,
                        ));
                    }
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

/// Compress every `.cue`, `.gdi`, `.iso`, and `.avi` under `input_dir`, descending
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
            exts: &["cue", "gdi", "iso", "avi"],
            max_depth,
            open_archives: false,
            verb: "Compressing",
            action: "compress",
            noun: "discs",
        },
        total_progress,
        &CancelToken::new(),
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
/// become `.cue` plus one `.bin` per track, DVD-mode CHDs become `.iso`
/// (the output extension is derived per file by [`extract_from_chd`]). A failure
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
            open_archives: false,
            verb: "Extracting",
            action: "extract",
            noun: "chd files",
        },
        total_progress,
        &CancelToken::new(),
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
/// `Verified N files: X OK, Y failed` summary and returns `(ok, failed)` so
/// the caller can fail the run. Unless `plain_only` is set, archives holding
/// a `.chd` member are verified too: a fix rewrites the file in place, which
/// an archive member cannot be, so a fix run sets it. `fix` alone says
/// whether the header is rewritten, which a dry run withholds without
/// changing which files are walked.
pub async fn verify_chd_batch(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: PathBuf,
    fix: bool,
    plain_only: bool,
    max_depth: Option<usize>,
    cancel: &CancelToken,
) -> ChdResult<(usize, usize)> {
    let (ok, failed) = run_file_batch(
        BatchSpec {
            input_dir: &input_dir,
            output_dir: None,
            exts: &["chd"],
            max_depth,
            open_archives: !plain_only,
            verb: "Verifying",
            action: "verify",
            noun: "chd files",
        },
        total_progress,
        cancel,
        |path| Some(path.to_path_buf()),
        async |input, _output| -> anyhow::Result<()> {
            let resolved =
                tokio::task::spawn_blocking(move || crate::util::resolve_input(&input, &["chd"]))
                    .await??;
            verify_chd(
                progress,
                resolved.path().to_path_buf(),
                None,
                fix,
                cancel.clone(),
            )
            .await?;
            Ok(())
        },
    )
    .await
    .map_err(batch_error)?;
    info!("Verified {} files: {ok} OK, {failed} failed", ok + failed);
    Ok((ok, failed))
}

/// Batch twin of [`migrate_chd_to_v5`], mirroring
/// [`convert_disc_to_chd_batch`]: every `.chd` under `input_dir` that is
/// not already V5 is migrated. Returns `(ok, failed)`; files skipped as
/// already V5 are in neither count.
#[allow(clippy::too_many_arguments)]
pub async fn migrate_chd_to_v5_batch(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &std::path::Path,
    opts: ChdOptions,
    output_dir: Option<&std::path::Path>,
    max_depth: Option<usize>,
    in_place: bool,
    cancel: &CancelToken,
) -> ChdResult<(usize, usize)> {
    run_file_batch(
        BatchSpec {
            input_dir,
            output_dir,
            exts: &["chd"],
            max_depth,
            open_archives: false,
            verb: "Migrating",
            action: "migrate",
            noun: "chd files",
        },
        total_progress,
        cancel,
        |path| {
            // A file whose header cannot be read is migrated anyway so its
            // open error is counted as a failure, not logged as a skip.
            if let Ok(Some(5)) = legacy::peek_chd_version(path) {
                warn!("Skipping {}: already CHD V5", path.display());
                return None;
            }
            Some(match (in_place, output_dir) {
                (true, _) => path.to_path_buf(),
                (false, Some(_)) => place_in_dir_mirrored(path, input_dir, output_dir),
                (false, None) => migrated_chd_path(path),
            })
        },
        async |input, output| {
            migrate_chd_to_v5(progress, input, output, opts.clone(), cancel.clone()).await
        },
    )
    .await
    .map_err(batch_error)
}

/// A cancel must surface as a cancellation, not as an I/O failure.
fn batch_error(err: std::io::Error) -> ChdError {
    if err
        .get_ref()
        .is_some_and(|e| e.downcast_ref::<crate::util::Cancelled>().is_some())
    {
        ChdError::Cancelled(crate::util::Cancelled)
    } else {
        ChdError::IoError(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::NoProgress;

    /// A cancel that lands while the batch runs must surface as the cancel
    /// variant, not as an I/O failure, so the caller maps it to the
    /// cancelled exit status.
    #[tokio::test]
    async fn verify_batch_reports_a_cancel_as_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.chd"), b"not a chd").unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        let err = verify_chd_batch(
            &NoProgress,
            &NoProgress,
            dir.path().to_path_buf(),
            false,
            false,
            None,
            &cancel,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ChdError::Cancelled(_)), "{err}");
    }
}
