//! Cross-format helpers shared by every conversion pipeline: conflict
//! resolution, hashing, dry-run planning, run reports and tallies, output
//! templating, and the worker pool that drives compression on background
//! threads.

pub mod aes;
pub mod archive;
pub mod bytes;
pub mod conflict;
pub mod deflate;
pub mod footgun;
pub mod fs;
pub mod group_reader;
pub mod hash;
pub mod hash_cache;
pub mod http;
pub mod iso9660;
pub mod maker_codes;
pub mod path;
pub mod pixel;
pub mod plan;
pub mod pread;
pub mod report;
pub mod sfo;
pub mod tally;
pub mod template;
pub mod verify;
pub mod worker_pool;

pub use archive::{ArchiveMember, ResolvedInput, is_archive_path, list_members, resolve_input};
pub use conflict::{ConflictPolicy, ConflictResolution, resolve_conflict};
pub use footgun::{
    DREAMCAST_CHD_WARNING, NX_DAT_UNSUPPORTED_HINT, dreamcast_boot_signature,
    mixed_playlist_extensions, oversized_rvz_chunk,
};
pub use fs::{DEFAULT_SPACE_HEADROOM, available_space, space_shortfall};
pub use hash::{
    ChecksumBounds, FileDigests, HashAlgo, hash_file, parse_algos, parse_checksum_bound,
};
pub use hash_cache::{CachedTrack, CueDigests, HashCache};
pub use path::{contract_tilde, expand_tilde, with_tag};
pub use plan::{PlanDecision, PlanLine, classify};
pub use report::{
    HashReportRecord, ReportFormat, ReportRecord, ReportRecordInput, ReportTotals,
    write_dat_report, write_hash_report, write_report,
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

/// The cancellation error itself. Every module error enum wraps it in a
/// `Cancelled` variant that keeps it as `source()`, so [`Cancelled::in_chain`]
/// finds it by type wherever it surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("operation cancelled")]
pub struct Cancelled;

impl Cancelled {
    /// True when any error in `err`'s chain is a cancellation, including
    /// one carried as the payload of an `io::Error` (std does not expose
    /// the payload through `source()`, so it must be unwrapped explicitly).
    pub fn in_chain(err: &anyhow::Error) -> bool {
        err.chain().any(|cause| {
            cause.is::<Cancelled>()
                || cause
                    .downcast_ref::<std::io::Error>()
                    .and_then(std::io::Error::get_ref)
                    .is_some_and(|inner| inner.is::<Cancelled>())
        })
    }
}

/// A sibling temp path in the output directory so an interrupted write
/// never lands on the final name and a pre-existing overwrite target
/// survives until the rename. Creates the output's parent directories
/// when missing, so every writer accepts a not-yet-existing output dir.
pub(crate) fn scratch_output_path(output: &std::path::Path) -> std::io::Result<tempfile::TempPath> {
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut prefix = std::ffi::OsString::from(".");
    prefix.push(output.file_name().unwrap_or_default());
    prefix.push(".");
    std::fs::create_dir_all(parent)?;
    tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(".tmp")
        .tempfile_in(parent)
        .map(tempfile::NamedTempFile::into_temp_path)
}

pub(crate) fn publish_temp(
    temp: tempfile::TempPath,
    output: &std::path::Path,
    overwrite: bool,
) -> std::io::Result<()> {
    let result = if overwrite {
        temp.persist(output)
    } else {
        temp.persist_noclobber(output)
    };
    result.map(|_| ()).map_err(|err| err.error)
}

/// Write `output` through [`scratch_output_path`]: `write` fills a sibling
/// temp file that is published to the final name only once it returns, so a
/// failed or interrupted write leaves the existing file untouched.
pub(crate) fn atomic_write<E, F>(
    output: &std::path::Path,
    overwrite: bool,
    write: F,
) -> Result<(), E>
where
    E: From<std::io::Error>,
    F: FnOnce(&mut std::fs::File) -> Result<(), E>,
{
    let temp = scratch_output_path(output)?;
    let mut file = std::fs::File::create(&temp)?;
    write(&mut file)?;
    drop(file);
    publish_temp(temp, output, overwrite)?;
    Ok(())
}

pub(crate) fn backup_existing(
    path: &std::path::Path,
) -> std::io::Result<Option<tempfile::TempPath>> {
    if !path.exists() {
        return Ok(None);
    }
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a regular file: {}", path.display()),
        ));
    }
    let backup = scratch_output_path(path)?;
    std::fs::remove_file(&backup)?;
    std::fs::rename(path, &backup)?;
    Ok(Some(backup))
}

pub(crate) fn restore_temp(
    temp: tempfile::TempPath,
    path: &std::path::Path,
) -> std::io::Result<()> {
    match temp.persist(path) {
        Ok(_) => Ok(()),
        Err(err) => {
            let error = err.error;
            let _ = err.path.keep();
            Err(error)
        }
    }
}

#[cfg(test)]
pub(crate) fn scratch_output_exists(output: &std::path::Path) -> std::io::Result<bool> {
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let prefix = format!(
        ".{}.",
        output.file_name().unwrap_or_default().to_string_lossy()
    );
    Ok(std::fs::read_dir(parent)?
        .filter_map(|entry| entry.ok())
        .any(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with(&prefix) && name.ends_with(".tmp")
        }))
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
    /// Begins a new progress span of `total` units, labeled `msg`.
    fn start(&self, total: u64, msg: &str);
    /// Advances the current span by `delta` units.
    fn inc(&self, delta: u64);
    /// Marks the current span complete.
    fn finish(&self);
    /// Announce the active phase of a multi-step operation. The label
    /// replaces the operation message until the next phase or `start`, and
    /// is cleared when the operation finishes. Reporters that do not surface
    /// a label leave this a no-op.
    fn set_phase(&self, _label: &str) {}

    /// Surface an advisory warning without failing the operation. Defaults
    /// to the process log, which terminal consumers already display;
    /// reporters with their own UI override this to show the message there.
    fn warn(&self, message: &str) {
        log::warn!("{message}");
    }
}

/// No-op [`ProgressReporter`] for callers that do not need progress output.
pub struct NoProgress;

impl ProgressReporter for NoProgress {
    fn start(&self, _: u64, _: &str) {}
    fn inc(&self, _: u64) {}
    fn finish(&self) {}
}

/// Bridges a blocking worker's byte counter to the async progress
/// reporter, which cannot cross the `spawn_blocking` boundary.
pub(crate) struct AtomicProgress {
    pub(crate) counter: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl ProgressReporter for AtomicProgress {
    fn start(&self, _: u64, _: &str) {}
    fn inc(&self, delta: u64) {
        self.counter
            .fetch_add(delta, std::sync::atomic::Ordering::Relaxed);
    }
    fn finish(&self) {}
}

/// Drive a blocking writer against a scratch sibling of `output`, relaying
/// its byte counter to `progress`, and publish the scratch file on success.
/// On any error, cancellation included, the scratch file is removed when the
/// [`tempfile::TempPath`] drops.
pub(crate) async fn run_scratch_write<T, E>(
    output: &std::path::Path,
    overwrite: bool,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
    job: impl FnOnce(
        std::path::PathBuf,
        std::sync::Arc<std::sync::atomic::AtomicU64>,
        CancelToken,
    ) -> Result<T, E>
    + Send
    + 'static,
) -> Result<T, E>
where
    T: Send + 'static,
    E: From<Cancelled> + From<std::io::Error> + From<tokio::task::JoinError> + Send + 'static,
{
    let write_path = scratch_output_path(output)?;
    let bytes_done = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let handle = tokio::task::spawn_blocking({
        let write_owned = write_path.to_path_buf();
        let bytes_done = bytes_done.clone();
        let cancel = cancel.clone();
        move || job(write_owned, bytes_done, cancel)
    });
    let value = await_with_progress_cancel(progress, &bytes_done, handle, cancel).await?;
    publish_temp(write_path, output, overwrite)?;
    Ok(value)
}

/// Poll a blocking task, draining `bytes_done` into `progress` until it
/// finishes, while also watching `cancel`. The blocking pipeline observes
/// the same token at its own loop boundaries and returns `Cancelled`
/// promptly; this helper only covers the rare race where the pipeline
/// finishes a unit just as the token fires.
pub(crate) async fn await_with_progress_cancel<T, E>(
    progress: &dyn ProgressReporter,
    bytes_done: &std::sync::Arc<std::sync::atomic::AtomicU64>,
    mut handle: tokio::task::JoinHandle<Result<T, E>>,
    cancel: &CancelToken,
) -> Result<T, E>
where
    E: From<Cancelled> + From<tokio::task::JoinError>,
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
        return Err(Cancelled.into());
    }
    value
}

#[cfg(test)]
mod tests {
    use super::{place_in_dir_mirrored, publish_temp, scratch_output_path};
    use crate::util::{NoProgress, ProgressReporter};
    use std::path::{Path, PathBuf};

    #[test]
    fn no_progress_set_phase_is_a_no_op() {
        NoProgress.set_phase("anything");
    }

    #[test]
    fn cancelled_is_found_through_module_enums_and_io_payloads() {
        use super::Cancelled;
        let typed =
            anyhow::Error::from(crate::chd::error::ChdError::from(Cancelled)).context("outer");
        assert!(Cancelled::in_chain(&typed));
        let io = anyhow::Error::from(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            Cancelled,
        ));
        assert!(Cancelled::in_chain(&io));
        let other = anyhow::anyhow!("operation cancelled");
        assert!(!Cancelled::in_chain(&other));
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

    #[test]
    fn scratch_outputs_are_unique_siblings_and_clean_up_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("game.rvz");
        let first = scratch_output_path(&output).unwrap();
        let second = scratch_output_path(&output).unwrap();
        let first_path = first.to_path_buf();

        assert_ne!(first.to_path_buf(), second.to_path_buf());
        assert_eq!(first.parent(), output.parent());
        assert!(first.exists());
        drop(first);
        assert!(!first_path.exists());
    }

    #[test]
    fn publish_temp_replaces_or_preserves_cross_platform() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("out.bin");
        std::fs::write(&output, b"old").unwrap();

        let no_clobber = scratch_output_path(&output).unwrap();
        std::fs::write(&no_clobber, b"new").unwrap();
        assert!(publish_temp(no_clobber, &output, false).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"old");

        let overwrite = scratch_output_path(&output).unwrap();
        std::fs::write(&overwrite, b"new").unwrap();
        publish_temp(overwrite, &output, true).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"new");
    }

    #[test]
    fn scratch_output_path_creates_missing_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("missing/nested/out.chd");
        let temp = scratch_output_path(&output).unwrap();
        assert!(output.parent().unwrap().is_dir());
        assert!(temp.exists());
    }
}
