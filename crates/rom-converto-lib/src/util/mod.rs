//! Cross-format helpers shared by every conversion pipeline: conflict
//! resolution, hashing, dry-run planning, run reports and tallies, output
//! templating, and the worker pool that drives compression on background
//! threads.

pub mod conflict;
pub mod fs;
pub mod group_reader;
pub mod hash;
pub mod http;
pub mod iso9660;
pub mod maker_codes;
pub mod pixel;
pub mod plan;
pub mod pread;
pub mod report;
pub mod tally;
pub mod template;
pub mod verify;
pub mod worker_pool;

pub use conflict::{ConflictPolicy, ConflictResolution, resolve_conflict};
pub use fs::{DEFAULT_SPACE_HEADROOM, available_space, space_shortfall};
pub use hash::{FileDigests, HashAlgo, hash_file, hash_file_cancellable, parse_algos};
pub use plan::{PlanDecision, PlanLine, classify};
pub use report::{
    HashReportRecord, ReportFormat, ReportRecord, ReportTotals, write_hash_report, write_report,
};
pub use tally::{FileEntry, FileStatus, Tally, TallyDirection, format_bytes};
pub use template::{TemplateTokens, apply_template};
pub use verify::{OutputVerify, VerifyOutcome, verify_existing_output};

pub const BYTES_PER_MB: f64 = 1_000_000.0;

/// Cooperative cancellation handle threaded into the long-running
/// compress/decompress/extract loops. The blocking codec pipelines
/// observe it at chunk/hunk/block boundaries and stop with the codec's
/// `Cancelled` error.
pub type CancelToken = tokio_util::sync::CancellationToken;

/// A sibling temp path in the output directory so an interrupted write
/// never lands on the final name and a pre-existing overwrite target
/// survives until the rename.
pub(crate) fn scratch_output_path(output: &std::path::Path) -> std::path::PathBuf {
    let mut name = output.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    output.with_file_name(name)
}

/// Re-root a derived output filename into `output_dir`, or return it unchanged
/// when no directory is given.
pub fn place_in_dir(
    derived: &std::path::Path,
    output_dir: Option<&std::path::Path>,
) -> std::path::PathBuf {
    match output_dir {
        Some(dir) => dir.join(
            derived
                .file_name()
                .expect("a derived output path always has a file name"),
        ),
        None => derived.to_path_buf(),
    }
}

/// Re-root a derived output path under `output_dir`, preserving the input's
/// subpath relative to `scan_root` so a recursive batch mirrors the source
/// tree instead of flattening every file into one directory. With no
/// `output_dir` the derived path is returned unchanged, since outputs then
/// land beside their input and already mirror the tree. Falls back to the
/// file name when `derived` is not under `scan_root`.
pub fn place_in_dir_mirrored(
    derived: &std::path::Path,
    scan_root: &std::path::Path,
    output_dir: Option<&std::path::Path>,
) -> std::path::PathBuf {
    match output_dir {
        Some(dir) => match derived.strip_prefix(scan_root) {
            Ok(rel) => dir.join(rel),
            Err(_) => dir.join(
                derived
                    .file_name()
                    .expect("a derived output path always has a file name"),
            ),
        },
        None => derived.to_path_buf(),
    }
}

/// Trait for reporting progress from library operations.
///
/// Consumers implement this to bridge progress updates to their
/// preferred UI (CLI progress bars, GUI events, and similar).
pub trait ProgressReporter: Send + Sync {
    fn start(&self, total: u64, msg: &str);
    fn inc(&self, delta: u64);
    fn finish(&self);
    /// Announce the active phase of a multi-step operation. The label
    /// replaces the operation message until the next phase or `start`, and
    /// is cleared when the operation finishes. Reporters that do not surface
    /// a label leave this a no-op.
    fn set_phase(&self, _label: &str) {}
}

pub struct NoProgress;

impl ProgressReporter for NoProgress {
    fn start(&self, _: u64, _: &str) {}
    fn inc(&self, _: u64) {}
    fn finish(&self) {}
}

/// Await a blocking pipeline while draining its shared byte counter
/// into `progress` every 100 ms; calls `progress.finish()` at the end
/// either way.
pub(crate) async fn await_with_progress<T, E>(
    progress: &dyn ProgressReporter,
    bytes_done: &std::sync::Arc<std::sync::atomic::AtomicU64>,
    mut handle: tokio::task::JoinHandle<Result<T, E>>,
) -> Result<T, E>
where
    E: From<tokio::task::JoinError>,
{
    use std::sync::atomic::Ordering;

    let result = loop {
        match tokio::time::timeout(std::time::Duration::from_millis(100), &mut handle).await {
            Ok(result) => break result,
            Err(_) => {
                let delta = bytes_done.swap(0, Ordering::Relaxed);
                if delta > 0 {
                    progress.inc(delta);
                }
            }
        }
    };
    let remaining = bytes_done.swap(0, Ordering::Relaxed);
    if remaining > 0 {
        progress.inc(remaining);
    }
    progress.finish();
    result?
}

/// Like [`await_with_progress`], but also watches `cancel`. The blocking
/// pipeline observes the same token at its own loop boundaries and
/// returns the codec's `Cancelled` error promptly; this helper only
/// covers the rare race where the pipeline finishes a unit just as the
/// token fires, mapping any non-error outcome to `on_cancel()`. The
/// `on_cancel` closure performs the partial-output cleanup and returns
/// the codec's `Cancelled` variant.
pub(crate) async fn await_with_progress_cancel<T, E>(
    progress: &dyn ProgressReporter,
    bytes_done: &std::sync::Arc<std::sync::atomic::AtomicU64>,
    mut handle: tokio::task::JoinHandle<Result<T, E>>,
    cancel: &CancelToken,
    on_cancel: impl FnOnce() -> E,
) -> Result<T, E>
where
    E: From<tokio::task::JoinError>,
{
    use std::sync::atomic::Ordering;

    let result = loop {
        match tokio::time::timeout(std::time::Duration::from_millis(100), &mut handle).await {
            Ok(result) => break result,
            Err(_) => {
                let delta = bytes_done.swap(0, Ordering::Relaxed);
                if delta > 0 {
                    progress.inc(delta);
                }
            }
        }
    };
    let remaining = bytes_done.swap(0, Ordering::Relaxed);
    if remaining > 0 {
        progress.inc(remaining);
    }
    progress.finish();

    let value = result?;
    if value.is_ok() && cancel.is_cancelled() {
        return Err(on_cancel());
    }
    value
}

#[cfg(test)]
mod tests {
    use super::place_in_dir_mirrored;
    use crate::util::{NoProgress, ProgressReporter};
    use std::path::{Path, PathBuf};

    #[test]
    fn no_progress_set_phase_is_a_no_op() {
        NoProgress.set_phase("anything");
    }

    #[test]
    fn place_in_dir_mirrored_preserves_subpath() {
        let out = place_in_dir_mirrored(
            Path::new("/root/a/b/game.chd"),
            Path::new("/root"),
            Some(Path::new("/out")),
        );
        assert_eq!(out, PathBuf::from("/out/a/b/game.chd"));
    }

    #[test]
    fn place_in_dir_mirrored_none_returns_derived() {
        let derived = Path::new("/root/a/game.chd");
        let out = place_in_dir_mirrored(derived, Path::new("/root"), None);
        assert_eq!(out, derived.to_path_buf());
    }

    #[test]
    fn place_in_dir_mirrored_fallback_to_file_name() {
        let out = place_in_dir_mirrored(
            Path::new("/elsewhere/game.chd"),
            Path::new("/root"),
            Some(Path::new("/out")),
        );
        assert_eq!(out, PathBuf::from("/out/game.chd"));
    }
}
