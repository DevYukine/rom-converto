//! Cross-format helpers shared by every conversion pipeline: conflict
//! resolution, hashing, dry-run planning, run reports and tallies, output
//! templating, and the worker pool that drives compression on background
//! threads.

pub mod aes;
pub mod archive;
pub mod bounded_line;
pub mod bytes;
pub mod conflict;
pub mod deflate;
pub mod footgun;
pub mod frontends;
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
pub mod positional_reader;
pub mod pread;
pub mod report;
pub mod sfo;
pub mod tally;
pub mod template;
pub mod torrentzip;
pub mod verify;
pub mod worker_pool;
pub mod zip_write;

pub use archive::{
    ArchiveMember, ArchiveSelection, NoMatchingMember, ResolvedInput, TempSpaceShortfall,
    is_archive_path, list_members, probe_archive, resolve_input, resolve_input_with_selection,
};
pub use conflict::{
    ConflictPolicy, ConflictResolution, OutputExists, resolve_conflict, resolve_conflict_by,
};
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
pub use plan::{PlanDecision, PlanLine, chd_media_label, classify};
pub use report::{
    HashReportRecord, ReportFormat, ReportRecord, ReportRecordInput, ReportTotals,
    write_dat_report, write_hash_report, write_report,
};
pub use tally::{FileEntry, FileStatus, Tally, TallyDirection, format_bytes};
pub use template::{TemplateTokens, apply_template};
pub use torrentzip::{ZipEntry, ZipFormat, ZipMember, validate_torrentzip, write_torrentzip};
pub use verify::{OutputVerify, VerifyOutcome, verify_existing_cached, verify_existing_output};
pub use zip_write::write_zip;

pub const BYTES_PER_MB: f64 = 1_000_000.0;

/// A dirent name safe to append to a real filesystem path: an untrusted
/// image can otherwise plant an absolute or `..` name and escape
/// `output_dir` (zip-slip).
pub(crate) fn is_safe_dirent_name(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    if name.contains(['/', '\\', '\0']) {
        return false;
    }
    if cfg!(windows) && name.contains(':') {
        return false;
    }
    true
}

/// True when `path`, joined onto a folder, lexically names a file inside it:
/// relative, never climbing above the folder with `..`, and every named
/// component passing [`is_safe_dirent_name`] (empty and `.` components are
/// skipped). Components split on the host's separators, the way the OS
/// resolves them. A Unix component holding `\` is refused on purpose, as in
/// the zip-slip rule: a sheet written on Windows means it as a separator.
/// A symlinked folder inside can still lead out.
pub(crate) fn is_safe_relative_path(path: &str) -> bool {
    if path.starts_with(['/', '\\']) {
        return false;
    }
    let mut depth = 0usize;
    for component in path.split(std::path::is_separator) {
        match component {
            "" | "." => {}
            ".." => match depth.checked_sub(1) {
                Some(parent) => depth = parent,
                None => return false,
            },
            name if is_safe_dirent_name(name) => depth += 1,
            _ => return false,
        }
    }
    depth > 0
}

/// Compute `offset + size` when that extent stays within `len`: `Some(end)`
/// when the addition does not overflow and `end <= len`, else `None`. The
/// error-type-agnostic core of [`validate_extent`] for callers that map an
/// overrun onto their own error type.
pub fn extent_end(offset: u64, size: u64, len: u64) -> Option<u64> {
    let end = offset.checked_add(size)?;
    (end <= len).then_some(end)
}

/// Check that an extent read out of an untrusted header stays within
/// `file_len`, so a hostile `offset + size` cannot address past the file.
/// Overflow of `offset + size` is reported like any other overrun.
pub fn validate_extent(offset: u64, size: u64, file_len: u64, what: &str) -> std::io::Result<()> {
    match extent_end(offset, size, file_len) {
        Some(_) => Ok(()),
        None => Err(std::io::Error::other(format!(
            "{what} extent {offset:#x}+{size:#x} exceeds file size {file_len:#x}"
        ))),
    }
}

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

/// An open sibling temp file in the output directory so an interrupted write
/// never lands on the final name and a pre-existing overwrite target
/// survives until the rename. Creates the output's parent directories
/// when missing, so every writer accepts a not-yet-existing output dir.
pub(crate) fn scratch_output_file(
    output: &std::path::Path,
) -> std::io::Result<tempfile::NamedTempFile> {
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut prefix = std::ffi::OsString::from(".");
    prefix.push(output.file_name().unwrap_or_default());
    prefix.push(".");
    std::fs::create_dir_all(parent)?;
    // The scratch file is created with the process's default mode (0666 &
    // !umask on unix), not tempfile's private 0600: whatever is published
    // through it must stay readable like any File::create.
    let mut builder = tempfile::Builder::new();
    builder.prefix(&prefix).suffix(".tmp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    builder.tempfile_in(parent)
}

pub(crate) fn scratch_output_path(output: &std::path::Path) -> std::io::Result<tempfile::TempPath> {
    scratch_output_file(output).map(tempfile::NamedTempFile::into_temp_path)
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

/// Write `output` through [`scratch_output_file`]: `write` fills a sibling
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
    let (mut file, temp) = scratch_output_file(output)?.into_parts();
    write(&mut file)?;
    drop(file);
    publish_temp(temp, output, overwrite)?;
    Ok(())
}

pub(crate) fn backup_existing(
    path: &std::path::Path,
) -> std::io::Result<Option<tempfile::TempPath>> {
    let file_type = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata.file_type(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    if !file_type.is_file() && !file_type.is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a file or symlink: {}", path.display()),
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

fn restore_output(
    path: &std::path::Path,
    backup: Option<tempfile::TempPath>,
) -> std::io::Result<()> {
    if path.exists()
        && let Err(err) = std::fs::remove_file(path)
    {
        if let Some(backup) = backup {
            let _ = backup.keep();
        }
        return Err(err);
    }
    if let Some(backup) = backup {
        restore_temp(backup, path)?;
    }
    Ok(())
}

/// Publish all members or none: a failure rolls back published files and
/// restores every overwritten target, leaving unpublished targets untouched.
pub(crate) fn publish_set(
    members: Vec<(tempfile::TempPath, &std::path::Path)>,
    overwrite: bool,
) -> std::io::Result<()> {
    let mut backups: Vec<(&std::path::Path, Option<tempfile::TempPath>)> =
        Vec::with_capacity(members.len());
    for (_, path) in &members {
        let backup = if overwrite {
            match backup_existing(path) {
                Ok(backup) => backup,
                Err(err) => {
                    let mut restore_error = None;
                    for (path, backup) in backups {
                        if let Err(err) = restore_output(path, backup) {
                            restore_error.get_or_insert(err);
                        }
                    }
                    return Err(restore_error.unwrap_or(err));
                }
            }
        } else {
            None
        };
        backups.push((path, backup));
    }

    for (index, (temp, path)) in members.into_iter().enumerate() {
        if let Err(err) = publish_temp(temp, path, overwrite) {
            let mut restore_error = None;
            for (member, (path, backup)) in backups.into_iter().enumerate() {
                if (overwrite || member < index)
                    && let Err(err) = restore_output(path, backup)
                {
                    restore_error.get_or_insert(err);
                }
            }
            return Err(restore_error.unwrap_or(err));
        }
    }
    Ok(())
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

    /// Surface a finished record or plan line as it is produced. Reporters
    /// that only render progress leave this a no-op.
    fn row(&self, _row: &crate::runner::models::RunRow) {}

    /// Announce the size of a batch before its first unit is processed, for
    /// reporters that render an aggregate bar above the per-file one.
    fn batch_start(&self, _total_files: u64, _total_bytes: u64) {}

    /// Mark one batch unit of `bytes` done, whatever its outcome.
    fn batch_advance(&self, _bytes: u64) {}

    /// A nested reporter on a `suffix`-derived channel, for per-item progress
    /// that must not clobber this reporter's own counter. Reporters with a
    /// single channel keep the default sink and simply drop it.
    fn child(&self, _suffix: &str) -> Box<dyn ProgressReporter + Send + Sync> {
        Box::new(NoProgress)
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

enum Relayed {
    Start(u64, String),
    Inc(u64),
    Finish,
    Phase(String),
    Warn(String),
}

/// [`ProgressReporter`] handed to a blocking job so its reports can cross
/// the `spawn_blocking` boundary back to the async caller's reporter.
struct RelayProgress(std::sync::mpsc::Sender<Relayed>);

impl ProgressReporter for RelayProgress {
    fn start(&self, total: u64, msg: &str) {
        let _ = self.0.send(Relayed::Start(total, msg.to_string()));
    }
    fn inc(&self, delta: u64) {
        let _ = self.0.send(Relayed::Inc(delta));
    }
    fn finish(&self) {
        let _ = self.0.send(Relayed::Finish);
    }
    fn set_phase(&self, label: &str) {
        let _ = self.0.send(Relayed::Phase(label.to_string()));
    }
    fn warn(&self, message: &str) {
        let _ = self.0.send(Relayed::Warn(message.to_string()));
    }
}

/// Run a synchronous, progress-reporting job on the blocking pool and
/// forward everything it reports to `progress` while it runs.
pub(crate) async fn spawn_blocking_with_progress<T, E>(
    progress: &dyn ProgressReporter,
    job: impl FnOnce(&dyn ProgressReporter) -> Result<T, E> + Send + 'static,
) -> Result<T, E>
where
    T: Send + 'static,
    E: From<tokio::task::JoinError> + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    let mut handle = tokio::task::spawn_blocking(move || job(&RelayProgress(tx)));
    let drain = || {
        while let Ok(event) = rx.try_recv() {
            match event {
                Relayed::Start(total, msg) => progress.start(total, &msg),
                Relayed::Inc(delta) => progress.inc(delta),
                Relayed::Finish => progress.finish(),
                Relayed::Phase(label) => progress.set_phase(&label),
                Relayed::Warn(message) => progress.warn(&message),
            }
        }
    };
    let result = loop {
        match tokio::time::timeout(std::time::Duration::from_millis(100), &mut handle).await {
            Ok(result) => break result,
            Err(_) => drain(),
        }
    };
    drain();
    result?
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
        std::fs::File,
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
    let (file, write_path) = scratch_output_file(output)?.into_parts();
    let bytes_done = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let handle = tokio::task::spawn_blocking({
        let bytes_done = bytes_done.clone();
        let cancel = cancel.clone();
        move || job(file, bytes_done, cancel)
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
    use super::{
        extent_end, is_safe_relative_path, place_in_dir_mirrored, publish_set, publish_temp,
        scratch_output_path,
    };
    use crate::util::{NoProgress, ProgressReporter};
    use std::path::{Path, PathBuf};

    #[test]
    fn safe_relative_paths_follow_host_separators() {
        for (path, expected) in [
            ("game (Track 1).bin", true),
            ("track.bin", true),
            ("./track.bin", true),
            ("disc 1/track.bin", true),
            ("sub/../track.bin", true),
            ("../other.bin", false),
            ("disc/../../other.bin", false),
            ("sub/..", false),
            (r"..\other.bin", false),
            ("/etc/passwd", false),
            (r"\\server\share\x.bin", false),
            (r"\x.bin", false),
            (".", false),
            ("", false),
            ("a\0.bin", false),
            (r"a\b/../../../other.bin", false),
            (r"disc 1\track.bin", cfg!(windows)),
            (r"a\b/../../other.bin", cfg!(windows)),
            ("Game: Subtitle.bin", !cfg!(windows)),
            ("C:x.bin", !cfg!(windows)),
            ("a.bin:stream", !cfg!(windows)),
        ] {
            assert_eq!(is_safe_relative_path(path), expected, "{path:?}");
        }
    }

    #[test]
    fn no_progress_set_phase_is_a_no_op() {
        NoProgress.set_phase("anything");
    }

    #[test]
    fn cancelled_is_found_through_module_enums_and_io_payloads() {
        use super::Cancelled;
        let typed = anyhow::Error::from(crate::disc::chd::error::ChdError::from(Cancelled))
            .context("outer");
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

    #[cfg(unix)]
    #[tokio::test]
    async fn scratch_write_does_not_follow_replaced_path() {
        use std::io::Write;
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("out.bin");
        let victim = dir.path().join("victim.bin");
        std::fs::write(&victim, b"unchanged").unwrap();
        let parent = dir.path().to_path_buf();
        let victim_owned = victim.clone();
        let result: anyhow::Result<()> = super::run_scratch_write(
            &output,
            true,
            &NoProgress,
            &super::CancelToken::new(),
            move |mut file, _, _| {
                let scratch = std::fs::read_dir(parent)?
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .find(|entry| entry.file_name().to_string_lossy().starts_with(".out.bin."))
                    .unwrap()
                    .path();
                std::fs::remove_file(&scratch)?;
                symlink(&victim_owned, &scratch)?;
                file.write_all(b"replacement")?;
                Err(super::Cancelled.into())
            },
        )
        .await;

        assert!(result.is_err());
        assert_eq!(std::fs::read(victim).unwrap(), b"unchanged");
        assert!(!output.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
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
    fn publish_set_restores_every_backup_when_a_later_backup_fails() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.bin");
        let second = dir.path().join("second.bin");
        let cue = dir.path().join("game.cue");
        std::fs::write(&first, b"old first").unwrap();
        std::fs::write(&second, b"old second").unwrap();
        std::fs::create_dir(&cue).unwrap();
        let first_temp = scratch_output_path(&first).unwrap();
        let second_temp = scratch_output_path(&second).unwrap();
        let cue_temp = scratch_output_path(&cue).unwrap();
        let scratch_paths = [
            first_temp.to_path_buf(),
            second_temp.to_path_buf(),
            cue_temp.to_path_buf(),
        ];

        assert!(
            publish_set(
                vec![
                    (first_temp, &first),
                    (second_temp, &second),
                    (cue_temp, &cue),
                ],
                true,
            )
            .is_err()
        );
        assert_eq!(std::fs::read(first).unwrap(), b"old first");
        assert_eq!(std::fs::read(second).unwrap(), b"old second");
        assert!(cue.is_dir());
        assert!(scratch_paths.iter().all(|path| !path.exists()));
    }

    #[test]
    fn publish_set_restores_published_and_unpublished_outputs_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = ["first.bin", "second.bin", "third.bin", "game.cue"]
            .into_iter()
            .map(|name| dir.path().join(name))
            .collect();
        let old: [&[u8]; 4] = [b"first", b"second", b"third", b"cue"];
        for (path, bytes) in paths.iter().zip(old) {
            std::fs::write(path, bytes).unwrap();
        }
        let temps: Vec<_> = paths
            .iter()
            .map(|path| {
                let temp = scratch_output_path(path).unwrap();
                std::fs::write(&temp, b"new").unwrap();
                temp
            })
            .collect();
        let scratch_paths: Vec<_> = temps.iter().map(|temp| temp.to_path_buf()).collect();
        std::fs::remove_file(&temps[2]).unwrap();

        assert!(
            publish_set(
                temps
                    .into_iter()
                    .zip(paths.iter().map(PathBuf::as_path))
                    .collect(),
                true,
            )
            .is_err()
        );
        for (path, bytes) in paths.iter().zip(old) {
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
        assert!(scratch_paths.iter().all(|path| !path.exists()));
    }

    #[test]
    fn publish_set_no_clobber_rolls_back_without_touching_later_targets() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.bin");
        let second = dir.path().join("second.bin");
        let third = dir.path().join("third.bin");
        let cue = dir.path().join("game.cue");
        std::fs::write(&third, b"raced third").unwrap();
        std::fs::write(&cue, b"existing cue").unwrap();
        let first_temp = scratch_output_path(&first).unwrap();
        let second_temp = scratch_output_path(&second).unwrap();
        let third_temp = scratch_output_path(&third).unwrap();
        let cue_temp = scratch_output_path(&cue).unwrap();

        assert!(
            publish_set(
                vec![
                    (first_temp, &first),
                    (second_temp, &second),
                    (third_temp, &third),
                    (cue_temp, &cue),
                ],
                false,
            )
            .is_err()
        );
        assert!(!first.exists());
        assert!(!second.exists());
        assert_eq!(std::fs::read(third).unwrap(), b"raced third");
        assert_eq!(std::fs::read(cue).unwrap(), b"existing cue");
    }

    #[cfg(unix)]
    #[test]
    fn publish_set_overwrites_or_restores_symlinks_without_touching_targets() {
        for dangling in [false, true] {
            for fail in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let target = dir.path().join("target.bin");
                if !dangling {
                    std::fs::write(&target, b"original target").unwrap();
                }
                let bin = dir.path().join("game.bin");
                std::os::unix::fs::symlink(&target, &bin).unwrap();
                let cue = dir.path().join("game.cue");
                let bin_temp = scratch_output_path(&bin).unwrap();
                std::fs::write(&bin_temp, b"new bin").unwrap();
                let cue_temp = scratch_output_path(&cue).unwrap();
                if fail {
                    std::fs::remove_file(&cue_temp).unwrap();
                }
                let result = publish_set(vec![(bin_temp, &bin), (cue_temp, &cue)], true);
                if fail {
                    assert!(result.is_err());
                    assert_eq!(std::fs::read_link(&bin).unwrap(), target);
                    assert!(!cue.exists());
                } else {
                    result.unwrap();
                    assert!(std::fs::symlink_metadata(&bin).unwrap().is_file());
                    assert_eq!(std::fs::read(&bin).unwrap(), b"new bin");
                }
                if dangling {
                    assert!(!target.exists());
                } else {
                    assert_eq!(std::fs::read(&target).unwrap(), b"original target");
                }
            }
        }
    }

    #[test]
    fn scratch_output_path_creates_missing_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("missing/nested/out.chd");
        let temp = scratch_output_path(&output).unwrap();
        assert!(output.parent().unwrap().is_dir());
        assert!(temp.exists());
    }

    #[test]
    fn extent_end_bounds_and_overflow() {
        assert_eq!(extent_end(0x100, 0x200, 0x300), Some(0x300));
        assert_eq!(extent_end(0x100, 0x201, 0x300), None);
        assert_eq!(extent_end(u64::MAX - 1, 4, u64::MAX), None);
        assert_eq!(extent_end(0, 0, 0), Some(0));
    }
}
