//! The organize output-cleaning pass: remove stale files the run did not
//! place, and delete source directories emptied by `move_source`.

use crate::runner::invalid_arg;
use crate::runner::models::{OrganizeRow, RunOptions};
use crate::util::{CancelToken, Cancelled, ConflictPolicy, FileStatus, ProgressReporter};
use anyhow::{Context, Result};
use globset::{GlobSet, GlobSetBuilder};
#[cfg(unix)]
use rustix::fs::{AtFlags, FileType, Mode, OFlags, Stat};
#[cfg(unix)]
use rustix::io::Errno;
#[cfg(unix)]
use std::collections::HashSet;
use std::collections::{BTreeSet, HashMap};
#[cfg(unix)]
use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::io::{AsFd, OwnedFd};
use std::path::{Component, Path, PathBuf};
#[cfg(unix)]
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};

/// Options for cleaning the output directory.
#[derive(Clone)]
pub(super) struct CleanOptions {
    /// Compiled `clean_exclude` globs (relative to the output dir) of paths
    /// cleaning must keep.
    pub exclude: GlobSet,
    /// Directory stale files are moved to instead of deleted.
    pub backup: Option<PathBuf>,
    /// Backup slots a dry run has previewed so far, shared by every pass of
    /// the run: nothing is created, so each planned slot is reserved here.
    planned_backups: Arc<Mutex<BTreeSet<PathBuf>>>,
    /// Whether the backup volume folds case, so planned slots compare the
    /// way the real run's lookups would.
    backup_folds_case: bool,
}

impl CleanOptions {
    /// Options with no slot reserved yet.
    fn new(exclude: GlobSet, backup: Option<PathBuf>) -> Self {
        let backup_folds_case = backup
            .as_deref()
            .is_some_and(super::select::probe_case_insensitive);
        Self {
            exclude,
            backup,
            planned_backups: Arc::default(),
            backup_folds_case,
        }
    }

    /// `Some` when `clean` is enabled. The globs compile here, before any
    /// scanning: an invalid glob is an invalid argument, never a failure
    /// after outputs were placed and sources moved.
    pub(super) fn from_options(options: &RunOptions) -> Result<Option<Self>> {
        if options.clean != Some(true) {
            return Ok(None);
        }
        let exclude = compile_clean_exclude(options.clean_exclude.as_deref().unwrap_or(&[]))?;
        Ok(Some(Self::new(exclude, options.clean_backup.clone())))
    }
}

/// When `move_source` deletes emptied source directories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MoveDeleteDirs {
    Never,
    Auto,
    Always,
}

/// Parses the `move_delete_dirs` option; `auto` is the default.
pub(super) fn move_delete_dirs(options: &RunOptions) -> Result<MoveDeleteDirs> {
    match options.move_delete_dirs.as_deref() {
        None => Ok(MoveDeleteDirs::Auto),
        Some("never") => Ok(MoveDeleteDirs::Never),
        Some("auto") => Ok(MoveDeleteDirs::Auto),
        Some("always") => Ok(MoveDeleteDirs::Always),
        Some(other) => Err(invalid_arg(format!(
            "invalid move_delete_dirs {other:?}; expected \"never\", \"auto\" or \"always\""
        ))),
    }
}

/// Compiles user globs (`input_exclude`, `zip_exclude`) into a set.
/// `*` and `?` stay within one path segment; `**` crosses
/// directories. A bad glob is an invalid argument.
pub(super) fn compile_globs(globs: &[String], option: &str) -> Result<GlobSet> {
    build_globs(globs, option, false)
}

/// Compiles `clean_exclude` like [`compile_globs`], but ignoring case:
/// `**/*.sav` must protect `Game.SAV` on a volume that folds case, and a
/// keep rule that matches too much only keeps a file.
fn compile_clean_exclude(globs: &[String]) -> Result<GlobSet> {
    build_globs(globs, "clean_exclude", true)
}

fn build_globs(globs: &[String], option: &str, fold_case: bool) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for glob in globs {
        builder.add(
            globset::GlobBuilder::new(glob)
                .literal_separator(true)
                .case_insensitive(fold_case)
                .build()
                .map_err(|err| invalid_arg(format!("invalid {option} glob {glob:?}: {err}")))?,
        );
    }
    builder
        .build()
        .map_err(|err| invalid_arg(format!("invalid {option} glob: {err}")))
}

/// True when `path` matches the set, tried relative to `root` first, then
/// absolute, so `**/*.sav` and a spelled-out path both work.
pub(super) fn glob_matches(set: &GlobSet, root: &Path, path: &Path) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    set.is_match(relative) || set.is_match(path)
}

/// A pass's view of its own backup dir: where it resolves (its contents
/// are already archived, so they are never re-collected) and its own
/// directory entry, which may itself be a symlink and must never be a clean
/// candidate. The dir is resolved through its deepest existing ancestor
/// because it may not exist yet, and an entry is recognised by location (in
/// the platform's case rules) and, on unix, by the link's own inode, so no
/// spelling of the backup slips through.
struct BackupSpot {
    dir: PathBuf,
    entry: PathBuf,
    #[cfg(unix)]
    link_inode: Option<(u64, u64)>,
}

impl BackupSpot {
    fn of(backup: &Path) -> Self {
        let absolute = std::path::absolute(backup).unwrap_or_else(|_| backup.to_path_buf());
        #[cfg(unix)]
        let link_inode = {
            use std::os::unix::fs::MetadataExt;
            std::fs::symlink_metadata(&absolute)
                .ok()
                .filter(|meta| meta.is_symlink())
                .map(|meta| (meta.dev(), meta.ino()))
        };
        Self {
            dir: super::real_layout(&absolute),
            entry: super::entry_location(&absolute),
            #[cfg(unix)]
            link_inode,
        }
    }

    /// True when `location` is inside the backup dir or is the backup dir's
    /// own entry.
    fn covers(&self, location: &Path) -> bool {
        if location.starts_with(&self.dir) || location == self.entry {
            return true;
        }
        if cfg!(any(target_os = "macos", windows)) {
            let fold = |path: &Path| PathBuf::from(path.to_string_lossy().to_lowercase());
            let location = fold(location);
            if location.starts_with(fold(&self.dir)) || location == fold(&self.entry) {
                return true;
            }
        }
        false
    }

    /// True when `path` is the backup dir's own entry by the link's inode,
    /// whatever its spelling.
    #[cfg(unix)]
    fn is_entry_link(&self, path: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        self.link_inode.is_some_and(|link| {
            std::fs::symlink_metadata(path).is_ok_and(|meta| (meta.dev(), meta.ino()) == link)
        })
    }
}

/// One stale-file candidate: its walked spelling (for rows and
/// `clean_exclude` globs) and its on-disk size, plus on unix the written
/// dir's root descriptor and the relative chain to the entry, re-opened
/// at removal so only one fd per written dir stays open across the pass.
struct Candidate {
    path: PathBuf,
    size: u64,
    /// The written dir's root descriptor, verified at open: the entry is
    /// re-anchored through it, never through a re-resolved path.
    #[cfg(unix)]
    root: Rc<CleanDir>,
    /// The component chain from the root to the entry's parent.
    #[cfg(unix)]
    parents: Vec<OsString>,
    /// The entry's name in its parent.
    #[cfg(unix)]
    name: OsString,
    /// The entry's (device, inode) at collection, rechecked before removal.
    #[cfg(unix)]
    identity: (u64, u64),
}

#[cfg(unix)]
impl Candidate {
    /// The entry's location: the root's verified canonical spelling with
    /// the chain and name appended (what a re-opened chain rebuilds).
    fn location(&self) -> PathBuf {
        let mut location = self.root.canonical.clone();
        for name in &self.parents {
            location.push(name);
        }
        location.push(&self.name);
        location
    }

    /// Re-opens the entry's parent from the written dir's root descriptor,
    /// no-follow per component: collection keeps only the root fd alive
    /// per written dir, so the chain is walked again at removal time.
    /// `Ok(None)` when a component raced away or became a symlink — the
    /// entry is then skipped like one that vanished between walk and
    /// removal.
    fn open_parent(&self) -> std::io::Result<Option<CleanDir>> {
        let mut dir = CleanDir {
            canonical: self.root.canonical.clone(),
            fd: self.root.fd.as_fd().try_clone_to_owned()?,
        };
        for name in &self.parents {
            match dir.open_child(name)? {
                Some(child) => dir = child,
                None => return Ok(None),
            }
        }
        Ok(Some(dir))
    }
}

/// A cleaned directory held open without following symlinks: its entries
/// are enumerated, judged, and removed through this descriptor, so a
/// directory swapped for a symlink after the checks cannot redirect a
/// removal to wherever the new link points.
#[cfg(unix)]
struct CleanDir {
    /// The directory's canonical path, verified against the descriptor at
    /// open: a candidate's location is built from it, never re-resolved.
    canonical: PathBuf,
    fd: OwnedFd,
}

#[cfg(unix)]
impl CleanDir {
    /// Opens the directory at `path` no-follow. `Ok(None)` when the path
    /// raced away or is now a symlink — a written directory replaced by a
    /// link is never entered, its contents belong to the link's target.
    /// Other errors are the caller's failed clean row.
    fn open(path: &Path) -> std::io::Result<Option<Self>> {
        let fd = match rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            // The path is gone, or is now a symlink: platforms disagree
            // between `ELOOP` and `ENOTDIR` for that refusal.
            Err(Errno::NOENT | Errno::LOOP | Errno::NOTDIR) => return Ok(None),
            Err(err) => return Err(err.into()),
        };
        // The canonical spelling is resolved once, and the descriptor must
        // still be the directory it names: a swap on either side of the
        // open is skipped instead of cleaned through a stranger.
        let canonical = match std::fs::canonicalize(path) {
            Ok(canonical) => canonical,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        let opened = rustix::fs::fstat(&fd)?;
        let spelled = match rustix::fs::stat(&canonical) {
            Ok(spelled) => spelled,
            Err(Errno::NOENT) => return Ok(None),
            Err(err) => return Err(err.into()),
        };
        if (opened.st_dev, opened.st_ino) != (spelled.st_dev, spelled.st_ino) {
            return Ok(None);
        }
        Ok(Some(Self { canonical, fd }))
    }

    /// Opens the child directory `name`, no-follow through this (already
    /// verified) descriptor. `Ok(None)` when the entry raced away or
    /// became a symlink since the walk read it.
    fn open_child(&self, name: &OsStr) -> std::io::Result<Option<Self>> {
        match rustix::fs::openat(
            &self.fd,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => Ok(Some(Self {
                canonical: self.canonical.join(name),
                fd,
            })),
            // Gone, or now a symlink (`ELOOP`/`ENOTDIR` per platform).
            Err(Errno::NOENT | Errno::LOOP | Errno::NOTDIR) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    /// The entry's own metadata: never traverses a symlink, and never
    /// re-resolves the walked spelling.
    fn stat(&self, name: &OsStr) -> std::io::Result<Stat> {
        rustix::fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(std::io::Error::from)
    }

    /// The entry's link text, through the held directory.
    fn link_text(&self, name: &OsStr) -> std::io::Result<PathBuf> {
        let text =
            rustix::fs::readlinkat(&self.fd, name, Vec::new()).map_err(std::io::Error::from)?;
        Ok(PathBuf::from(OsStr::from_bytes(text.to_bytes())))
    }

    /// Unlinks the entry, whatever kind it is (the link itself, never its
    /// target).
    fn unlink(&self, name: &OsStr) -> std::io::Result<()> {
        rustix::fs::unlinkat(&self.fd, name, AtFlags::empty()).map_err(std::io::Error::from)
    }
}

/// `st_dev`/`st_ino` widths differ per platform (`dev_t` is `i32` on
/// Apple, `u64` on Linux), so the identity is widened explicitly.
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)]
fn stat_identity(meta: &Stat) -> (u64, u64) {
    (meta.st_dev as u64, meta.st_ino as u64)
}

/// Removes files under the output directory that this run did not write or
/// keep, returning one row per removed/backed-up file. Runs on the blocking
/// pool: the walk, renames, and copies are blocking file work.
///
/// Only directories written this run are eligible: the output root itself is
/// cleaned non-recursively (direct files only), every other written dir
/// recursively. Symlinked directories are never descended (only the link is
/// removable), and files whose identity is in `keep` survive, as do
/// `clean_exclude` matches. Every candidate whose own location is under
/// `input_root` (the canonical input root) is skipped whatever it is: clean
/// never deletes files under the input directory, however the output tree
/// reaches them. A per-file failure (an unreadable
/// directory, an undeletable file, a backup move that could not happen)
/// becomes a failed clean row and the pass continues; only cancellation
/// returns `Err`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn clean_output(
    output_dir: &Path,
    written_dirs: &BTreeSet<PathBuf>,
    keep: BTreeSet<PathBuf>,
    input_root: &Path,
    options: &CleanOptions,
    policy: ConflictPolicy,
    dry_run: bool,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<Vec<OrganizeRow>> {
    if policy == ConflictPolicy::Rename {
        // A rename this run is protected through its realized row output, but
        // " (n)" slots from earlier runs are indistinguishable from stale
        // files and would be cleaned.
        progress.warn(
            "clean combined with on-conflict rename: outputs renamed to \" (n)\" slots by \
             earlier runs may be cleaned; use clean_exclude to protect them",
        );
    }
    if written_dirs.is_empty() {
        // Nothing written (or planned) this run: the accidental-destruction
        // guard skips cleaning entirely.
        progress.warn("clean: nothing was written, skipping clean");
        return Ok(Vec::new());
    }
    let output_dir = output_dir.to_path_buf();
    let written_dirs = written_dirs.clone();
    let input_root = input_root.to_path_buf();
    let options = options.clone();
    let cancel = cancel.clone();
    crate::util::spawn_blocking_with_progress(progress, move |progress| {
        clean_output_pass(
            &output_dir,
            &written_dirs,
            keep,
            &input_root,
            &options,
            dry_run,
            progress,
            &cancel,
        )
    })
    .await
}

/// The blocking clean pass behind [`clean_output`].
#[allow(clippy::too_many_arguments)]
fn clean_output_pass(
    output_dir: &Path,
    written_dirs: &BTreeSet<PathBuf>,
    keep: BTreeSet<PathBuf>,
    input_root: &Path,
    options: &CleanOptions,
    dry_run: bool,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<Vec<OrganizeRow>> {
    let keep = KeepSet::new(&keep);
    let backup = options.backup.as_deref().map(BackupSpot::of);
    let mut rows = Vec::new();
    let mut backup_made = false;
    #[cfg(unix)]
    let root_identity = super::FileIdentity::of(input_root);
    #[cfg(not(unix))]
    let root_identity = None;
    let mut parents = HashMap::new();
    // The set iterates sorted, so a written dir inside an already-walked
    // (recursive) one is covered and skipped.
    let mut covered: Vec<(PathBuf, bool)> = Vec::new();
    for dir in written_dirs {
        // A symlinked "written dir" is never entered: its contents belong
        // to whatever the link points at, not to this run's output tree.
        // (Unix re-checks this atomically at the no-follow open below.)
        #[cfg(not(unix))]
        if std::fs::symlink_metadata(dir)
            .map(|meta| meta.is_symlink())
            .unwrap_or(false)
        {
            continue;
        }
        let recursive = dir != output_dir;
        if covered
            .iter()
            .any(|(walked, deep)| *deep && dir != walked && dir.starts_with(walked))
        {
            continue;
        }
        covered.push((dir.clone(), recursive));
        let mut candidates = Vec::new();
        let mut read_errors = Vec::new();
        // The written dir is held open once, no-follow, and verified
        // against its canonical spelling: it is the root every candidate
        // is re-anchored through, so a parent replaced by a symlink
        // mid-pass cannot redirect the removal, and only one fd per
        // written dir is ever held (the chain re-opens at removal time).
        #[cfg(unix)]
        let held = match CleanDir::open(dir) {
            Ok(Some(held)) => Rc::new(held),
            Ok(None) => continue,
            Err(err) => {
                progress.warn(&format!("could not clean {}: {err}", dir.display()));
                rows.push(failed_clean_row(dir.clone(), err, dry_run));
                continue;
            }
        };
        #[cfg(unix)]
        collect_candidates(
            dir,
            &held,
            &held,
            &[],
            recursive,
            &mut candidates,
            &mut read_errors,
            cancel,
        )?;
        #[cfg(not(unix))]
        collect_candidates(
            dir,
            None,
            recursive,
            &mut candidates,
            &mut read_errors,
            cancel,
        )?;
        // A directory that could not be read is one failed clean row: its
        // contents stay untouched, and the pass continues elsewhere.
        for (path, err) in read_errors {
            progress.warn(&format!("could not clean {}: {err}", path.display()));
            rows.push(failed_clean_row(path, err, dry_run));
        }
        for candidate in candidates {
            if cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            // The entry's own location (the root's verified canonical
            // spelling plus the chain and name) decides everything below,
            // so a link is judged by where it sits and its target is never
            // followed.
            #[cfg(unix)]
            let location = candidate.location();
            #[cfg(not(unix))]
            let location = super::entry_location(&candidate.path);
            if backup
                .as_ref()
                .is_some_and(|backup| backup.covers(&location))
            {
                // Already moved into the backup dir (or the backup dir's
                // own entry): never re-collected.
                continue;
            }
            #[cfg(unix)]
            if backup
                .as_ref()
                .is_some_and(|backup| backup.is_entry_link(&candidate.path))
            {
                continue;
            }
            // Nothing whose own location is under the input root is a clean
            // candidate, whatever it is (a file, a link, OS junk, or inside
            // a junk directory).
            if super::entry_location_is_under_root(
                &location,
                input_root,
                root_identity.as_ref(),
                &mut parents,
            ) {
                continue;
            }
            if glob_matches(&options.exclude, output_dir, &candidate.path)
                || keep.contains(&candidate.path)
            {
                continue;
            }
            // The chain from the written dir's root is re-opened no-follow
            // now, and the entry is judged and removed through it: every
            // check below is anchored to descriptors, never to a
            // re-resolved path.
            #[cfg(unix)]
            let held_parent = match candidate.open_parent() {
                Ok(Some(dir)) => dir,
                // A component raced away or became a link: the entry is
                // skipped like one that vanished between walk and removal.
                Ok(None) => continue,
                Err(err) => {
                    progress.warn(&format!(
                        "could not clean {}: {err}",
                        candidate.path.display()
                    ));
                    rows.push(failed_clean_row(candidate.path.clone(), err, dry_run));
                    continue;
                }
            };
            #[cfg(unix)]
            let anchor = Some((&held_parent, candidate.name.as_os_str(), candidate.identity));
            #[cfg(not(unix))]
            let anchor = None;
            if let Some(row) = remove_candidate(
                &candidate.path,
                candidate.size,
                anchor,
                options,
                &mut backup_made,
                dry_run,
            ) {
                if row.status == FileStatus::Failed {
                    progress.warn(&format!(
                        "could not clean {}: {}",
                        row.input.display(),
                        row.detail.clone().unwrap_or_default()
                    ));
                }
                rows.push(row);
            }
        }
    }
    Ok(rows)
}

/// Deletes `.m3u` files a pre-clean playlist plan planned but the post-clean
/// plan no longer derives: clean removed the discs the playlist was derived
/// from, so the derived file is stale too. Runs on the blocking pool.
/// Backup-aware like `clean_output` (a dry run only plans the row): paths
/// already inside the backup dir are never re-collected, and the backup dir
/// is created once for the whole pass. A path the keep set protects (by
/// spelling, identity, or link) never goes: it is this run's output or
/// source, whatever staleness says. `clean_exclude` protects a stale
/// playlist the same way it would have during the clean pass. A per-file
/// failure becomes a failed clean row and the pass continues; only
/// cancellation returns `Err`. A directory is never removed (`clean` itself
/// only ever removes files).
#[allow(clippy::too_many_arguments)]
pub(super) async fn delete_stale_playlists(
    output_dir: &Path,
    paths: &[PathBuf],
    keep: BTreeSet<PathBuf>,
    input_root: &Path,
    options: &CleanOptions,
    dry_run: bool,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<Vec<OrganizeRow>> {
    let output_dir = output_dir.to_path_buf();
    let paths = paths.to_vec();
    let input_root = input_root.to_path_buf();
    let options = options.clone();
    let cancel = cancel.clone();
    crate::util::spawn_blocking_with_progress(progress, move |progress| {
        delete_stale_playlists_pass(
            &output_dir,
            &paths,
            keep,
            &input_root,
            &options,
            dry_run,
            progress,
            &cancel,
        )
    })
    .await
}

/// The blocking stale-playlist pass behind [`delete_stale_playlists`].
#[allow(clippy::too_many_arguments)]
fn delete_stale_playlists_pass(
    output_dir: &Path,
    paths: &[PathBuf],
    keep: BTreeSet<PathBuf>,
    input_root: &Path,
    options: &CleanOptions,
    dry_run: bool,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<Vec<OrganizeRow>> {
    let keep = KeepSet::new(&keep);
    let backup = options.backup.as_deref().map(BackupSpot::of);
    let mut rows = Vec::new();
    let mut backup_made = false;
    #[cfg(unix)]
    let root_identity = super::FileIdentity::of(input_root);
    #[cfg(not(unix))]
    let root_identity = None;
    let mut parents = HashMap::new();
    for path in paths {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        if keep.contains(path) {
            continue;
        }
        // A symlinked playlist is judged by where it sits: one whose own
        // location is under the input root is never deleted.
        let location = super::entry_location(path);
        if super::entry_location_is_under_root(
            &location,
            input_root,
            root_identity.as_ref(),
            &mut parents,
        ) {
            continue;
        }
        if backup
            .as_ref()
            .is_some_and(|backup| backup.covers(&location))
        {
            // Already moved into the backup dir: never re-collected.
            continue;
        }
        #[cfg(unix)]
        if backup
            .as_ref()
            .is_some_and(|backup| backup.is_entry_link(path))
        {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            continue;
        };
        if meta.is_dir() {
            continue;
        }
        if glob_matches(&options.exclude, output_dir, path) {
            continue;
        }
        if let Some(row) =
            remove_candidate(path, meta.len(), None, options, &mut backup_made, dry_run)
        {
            if row.status == FileStatus::Failed {
                progress.warn(&format!(
                    "could not clean {}: {}",
                    row.input.display(),
                    row.detail.clone().unwrap_or_default()
                ));
            }
            rows.push(row);
        }
    }
    Ok(rows)
}

/// The walked anchor for descriptor-relative removal: the held directory,
/// the entry's name in it, and the entry's identity at collection.
#[cfg(unix)]
type DirAnchor<'a> = (&'a CleanDir, &'a OsStr, (u64, u64));
/// No anchor on platforms without directory descriptors: removals and
/// backup moves go by pathname.
#[cfg(not(unix))]
type DirAnchor<'a> = ();

/// Backup slots tried for one name before giving up: the move reserves
/// each slot atomically, so a hostile backup directory planted with
/// lookalike names cannot spin the pass; the row reports the failure.
const BACKUP_SLOT_LIMIT: usize = 1000;

/// True when the anchored entry still is the file the checks judged: a
/// name replaced (or removed) between the checks and the removal is
/// skipped, never removed.
fn entry_unchanged(anchor: Option<DirAnchor<'_>>) -> bool {
    #[cfg(unix)]
    if let Some((dir, name, identity)) = anchor {
        return dir
            .stat(name)
            .is_ok_and(|meta| stat_identity(&meta) == identity);
    }
    #[cfg(not(unix))]
    let _ = anchor;
    true
}

/// Removes one stale path, or moves it into the backup dir when one is
/// configured: the one removal step both cleaning passes share. A dry run
/// only plans the row. `Ok(None)` means the file raced away between walk
/// and removal: nothing this pass did to it, so it produces no row. A
/// failure is the caller's failed clean row, never a pass-level abort. A
/// walked `anchor` removes the entry through its held directory; anchor
/// less callers (the stale-playlist pass) remove by pathname.
fn remove_candidate(
    path: &Path,
    size: u64,
    anchor: Option<DirAnchor<'_>>,
    options: &CleanOptions,
    backup_made: &mut bool,
    dry_run: bool,
) -> Option<OrganizeRow> {
    let outcome: Result<(Option<String>, Option<PathBuf>)> = if dry_run {
        Ok((
            Some(match &options.backup {
                Some(backup) => {
                    // A dry run creates nothing, so each planned slot is
                    // reserved (for every pass of the run): two files of
                    // one name preview distinct targets, exactly as the
                    // real run names them.
                    let mut planned = options
                        .planned_backups
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    let (target, _) =
                        backup_target(backup, path, &planned, options.backup_folds_case);
                    planned.insert(target.clone());
                    format!("would back up to {}", target.display())
                }
                None => "would delete".to_string(),
            }),
            None,
        ))
    } else if !entry_unchanged(anchor) {
        Ok((None, None))
    } else if let Some(backup) = &options.backup {
        if !*backup_made {
            if let Err(err) = std::fs::create_dir_all(backup)
                .with_context(|| format!("creating backup dir {}", backup.display()))
            {
                // The requested backup is unavailable: the file stays, and
                // the row reports why instead of deleting unprotected.
                return Some(failed_clean_row(path.to_path_buf(), err, dry_run));
            }
            *backup_made = true;
        }
        // The free-slot probe is only the first guess: the move itself
        // reserves the slot atomically, and a slot occupied after the
        // probe comes back as `AlreadyExists` for the next one (the probe
        // reports its slot, so occupied slots are never re-tried).
        let (mut target, mut slot) = backup_target(backup, path, &BTreeSet::new(), false);
        loop {
            match move_to_backup_held(path, &target, anchor) {
                Ok(BackupOutcome::BackedUp) => {
                    break Ok((
                        Some(format!("backed up to {}", target.display())),
                        Some(target.clone()),
                    ));
                }
                // Raced away between walk and backup: already gone.
                Ok(BackupOutcome::AlreadyGone) => break Ok((None, None)),
                Err(err) if err.kind() == ErrorKind::AlreadyExists && slot < BACKUP_SLOT_LIMIT => {
                    slot += 1;
                    target = backup_slot(backup, path, slot);
                }
                Err(err) => {
                    break Err(err).with_context(|| {
                        format!("backing up {} to {}", path.display(), target.display())
                    });
                }
            }
        }
    } else {
        match remove_entry_held(path, anchor) {
            Ok(true) => Ok((Some("deleted".to_string()), None)),
            // Raced away between walk and delete: already gone.
            Ok(false) => Ok((None, None)),
            Err(err) => Err(err.into()),
        }
    };
    match outcome {
        Ok((Some(detail), output)) => Some(OrganizeRow {
            input: path.to_path_buf(),
            output,
            console: None,
            action: "clean".to_string(),
            status: FileStatus::Ok,
            planned: dry_run,
            verify: None,
            detail: Some(detail),
            input_bytes: size,
            output_bytes: 0,
            elapsed_ms: 0,
        }),
        Ok((None, _)) => None,
        Err(err) => Some(failed_clean_row(path.to_path_buf(), err, dry_run)),
    }
}

/// A failed clean row: the path stays untouched and the detail says why.
fn failed_clean_row(path: PathBuf, err: impl std::fmt::Display, dry_run: bool) -> OrganizeRow {
    OrganizeRow {
        input: path,
        output: None,
        console: None,
        action: "clean".to_string(),
        status: FileStatus::Failed,
        planned: dry_run,
        verify: None,
        detail: Some(err.to_string()),
        input_bytes: 0,
        output_bytes: 0,
        elapsed_ms: 0,
    }
}

/// Gathers the removable entries under `dir`: plain files plus symlinks (the
/// link itself; its target is never read or touched). Directories descend
/// only when `recursive`, and symlinked directories are never followed. On
/// unix every entry's type, size, and identity come from the held
/// descriptor (see [`CleanDir`]), so the walked spelling is only reported,
/// never resolved again; candidates keep only the written dir's root
/// descriptor (`root`, with the `parents` chain to the entry) and the walk
/// itself holds `held` just while descending. Elsewhere the entry's own
/// path is the only handle. A directory that cannot be read is recorded in
/// `errors` (one failed clean row for the caller) instead of aborting the
/// walk; only cancellation returns `Err`.
#[allow(clippy::too_many_arguments)]
fn collect_candidates(
    dir: &Path,
    #[cfg(unix)] root: &Rc<CleanDir>,
    #[cfg(unix)] held: &Rc<CleanDir>,
    #[cfg(unix)] parents: &[OsString],
    #[cfg(not(unix))] _held: Option<()>,
    recursive: bool,
    out: &mut Vec<Candidate>,
    errors: &mut Vec<(PathBuf, std::io::Error)>,
    cancel: &CancelToken,
) -> Result<()> {
    // A planned (dry-run) output dir may not exist yet: nothing to clean there.
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            errors.push((dir.to_path_buf(), err));
            return Ok(());
        }
    };
    for entry in entries {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let Ok(entry) = entry else { continue };
        // Names come from the path walk, but on unix everything below is
        // decided and done through the held descriptor.
        #[cfg(unix)]
        {
            let name = entry.file_name();
            let path = dir.join(&name);
            // The held descriptor decides what the entry is: never a
            // re-read of the walked spelling.
            let Ok(meta) = held.stat(&name) else {
                continue;
            };
            let file_type = FileType::from_raw_mode(meta.st_mode);
            if file_type.is_symlink() || file_type.is_file() {
                out.push(Candidate {
                    path,
                    size: meta.st_size as u64,
                    root: root.clone(),
                    parents: parents.to_vec(),
                    name,
                    identity: stat_identity(&meta),
                });
            } else if file_type.is_dir() && recursive {
                match held.open_child(&name) {
                    Ok(Some(child)) => {
                        let child = Rc::new(child);
                        let mut child_parents = parents.to_vec();
                        child_parents.push(name);
                        collect_candidates(
                            &path,
                            root,
                            &child,
                            &child_parents,
                            true,
                            out,
                            errors,
                            cancel,
                        )?;
                    }
                    // The entry became a link (or vanished) since the stat:
                    // judge it as it is now, never through a re-resolved
                    // path.
                    Ok(None) => {
                        if let Ok(now) = held.stat(&name)
                            && FileType::from_raw_mode(now.st_mode).is_symlink()
                        {
                            out.push(Candidate {
                                path,
                                size: now.st_size as u64,
                                root: root.clone(),
                                parents: parents.to_vec(),
                                name,
                                identity: stat_identity(&now),
                            });
                        }
                    }
                    Err(err) => errors.push((path, err)),
                }
            }
        }
        #[cfg(not(unix))]
        {
            let path = entry.path();
            // symlink_metadata never traverses the link: a symlinked dir
            // stays unentered and a symlinked file is a candidate as the
            // link itself.
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_symlink() || meta.is_file() {
                out.push(Candidate {
                    path,
                    size: meta.len(),
                });
            } else if meta.is_dir() && recursive {
                collect_candidates(&path, None, true, out, errors, cancel)?;
            }
        }
    }
    Ok(())
}

/// The paths cleaning must never remove, matched by identity: exact spelling,
/// canonical form, or (on unix) device + inode, so a differently spelled or
/// symlinked path to a kept file still counts as kept. A symlink candidate is
/// only ever its own link (lexical path plus the link's own identity): its
/// resolved target never counts, or a stale link pointing at a kept source
/// would survive forever.
struct KeepSet {
    raw: BTreeSet<PathBuf>,
    canon: BTreeSet<PathBuf>,
    /// Identities of kept non-symlink paths (the file itself).
    #[cfg(unix)]
    inodes: HashSet<(u64, u64)>,
    /// Identities of kept symlinks (the link itself).
    #[cfg(unix)]
    link_inodes: HashSet<(u64, u64)>,
}

impl KeepSet {
    fn new(paths: &BTreeSet<PathBuf>) -> Self {
        let mut canon = BTreeSet::new();
        #[cfg(unix)]
        let (mut inodes, mut link_inodes) = (HashSet::new(), HashSet::new());
        for path in paths {
            canon.insert(std::fs::canonicalize(path).unwrap_or_else(|_| path.clone()));
            #[cfg(unix)]
            if let Ok(meta) = std::fs::symlink_metadata(path) {
                use std::os::unix::fs::MetadataExt;
                let identity = (meta.dev(), meta.ino());
                if meta.is_symlink() {
                    link_inodes.insert(identity);
                } else {
                    inodes.insert(identity);
                }
            }
        }
        Self {
            raw: paths.clone(),
            canon,
            #[cfg(unix)]
            inodes,
            #[cfg(unix)]
            link_inodes,
        }
    }

    fn contains(&self, path: &Path) -> bool {
        if self.raw.contains(path) {
            return true;
        }
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return false;
        };
        if meta.is_symlink() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                return self.link_inodes.contains(&(meta.dev(), meta.ino()));
            }
            #[cfg(not(unix))]
            {
                // Windows has no inode-based identity for symlinks; a kept
                // symlink spelled with different casing (the filesystem's
                // own case-insensitivity) still counts as kept.
                if cfg!(windows) {
                    let lower = path.to_string_lossy().to_lowercase();
                    return self
                        .raw
                        .iter()
                        .any(|kept| kept.to_string_lossy().to_lowercase() == lower);
                }
                return false;
            }
        }
        if let Ok(canon) = std::fs::canonicalize(path)
            && self.canon.contains(&canon)
        {
            return true;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if self.inodes.contains(&(meta.dev(), meta.ino())) {
                return true;
            }
        }
        false
    }
}

/// The flat backup path for `file` in slot `slot`: `name.ext`, or
/// `name (slot).ext` past the first.
fn backup_slot(backup_dir: &Path, file: &Path, slot: usize) -> PathBuf {
    let name = file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    if slot == 0 {
        return backup_dir.join(name);
    }
    let stem = Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(name);
    let ext = Path::new(name)
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy()))
        .unwrap_or_default();
    backup_dir.join(format!("{stem} ({slot}){ext}"))
}

/// The flat backup path for `file`: `name.ext`, or past a collision
/// (an existing entry or a slot in `planned`) `name (1).ext`,
/// `name (2).ext`, ... The slot number comes back with the path, so a
/// collision found on the move continues from the next slot instead of
/// re-probing occupied ones.
fn backup_target(
    backup_dir: &Path,
    file: &Path,
    planned: &BTreeSet<PathBuf>,
    folds_case: bool,
) -> (PathBuf, usize) {
    let mut slot = 0;
    let mut target = backup_slot(backup_dir, file, slot);
    // A dangling link at the slot must count as taken, and so must a slot a
    // dry run already planned.
    let planned_slot = |target: &Path| {
        if folds_case {
            let fold = |path: &Path| path.to_string_lossy().to_lowercase();
            planned.iter().any(|slot| fold(slot) == fold(target))
        } else {
            planned.contains(target)
        }
    };
    while std::fs::symlink_metadata(&target).is_ok() || planned_slot(&target) {
        slot += 1;
        target = backup_slot(backup_dir, file, slot);
    }
    (target, slot)
}

/// Outcome of a backup move: whether `file` reached `target`, or had already
/// raced away. Only a `NotFound` on the move's own source (the rename, or
/// the copy that reads it) counts as gone; a `NotFound` removing the source
/// after the copy landed still leaves the backup in place, so it counts as
/// backed up.
#[derive(Debug)]
enum BackupOutcome {
    /// The file now sits at the backup target.
    BackedUp,
    /// The source was already gone; nothing was moved.
    AlreadyGone,
}

/// The path a relative link text names from a flat backup dir: the text's
/// leading `..` components are folded lexically onto the link's canonical
/// parent (exact, because that parent is canonical), and the rest of the
/// text is appended unchanged, so the backup link keeps naming the same
/// file without climbing through the original directory.
fn resolved_link_text(canonical_dir: &Path, text: &Path) -> PathBuf {
    if text.is_absolute() {
        return text.to_path_buf();
    }
    let mut base = canonical_dir.to_path_buf();
    let mut rest = PathBuf::new();
    let mut leading = true;
    for component in text.components() {
        match component {
            Component::ParentDir if leading => {
                base.pop();
            }
            Component::CurDir if leading => {}
            other => {
                leading = false;
                rest.push(other.as_os_str());
            }
        }
    }
    base.join(rest)
}

/// Recreates the symlink `file` at `target`, pointing at the same file
/// (see [`resolved_link_text`]). The link itself is copied, never its
/// target; a Windows directory link stays a directory link.
#[cfg(windows)]
fn recreate_symlink(file: &Path, target: &Path) -> std::io::Result<()> {
    let points_to = resolved_link_text(
        &super::entry_location(file)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default(),
        &std::fs::read_link(file)?,
    );
    use std::os::windows::fs::FileTypeExt;
    if std::fs::symlink_metadata(file)?
        .file_type()
        .is_symlink_dir()
    {
        std::os::windows::fs::symlink_dir(points_to, target)
    } else {
        std::os::windows::fs::symlink_file(points_to, target)
    }
}

/// Removes one candidate entry by pathname. A Windows directory link (or
/// junction) is a directory entry there and goes with `remove_dir`, which
/// removes the link itself; everything else is a plain `remove_file`.
#[cfg(windows)]
fn remove_entry(path: &Path) -> std::io::Result<()> {
    use std::os::windows::fs::FileTypeExt;
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink_dir()) {
        return std::fs::remove_dir(path);
    }
    std::fs::remove_file(path)
}

/// Removes one candidate entry, returning `Ok(false)` when nothing was
/// removed because the entry had already raced away. A walked anchor
/// unlinks through the held directory (unix); the anchorless unix branch
/// opens the parent itself, no-follow (as [`move_to_backup_held`] does),
/// so a parent swapped for a symlink cannot redirect the removal;
/// elsewhere the removal goes by pathname. A Windows directory link (or
/// junction) is a directory entry there and goes with `remove_dir`, which
/// removes the link itself; everything else is a plain `remove_file`.
fn remove_entry_held(path: &Path, anchor: Option<DirAnchor<'_>>) -> std::io::Result<bool> {
    #[cfg(unix)]
    {
        let opened;
        let (dir, name) = if let Some((dir, name, _)) = anchor {
            (dir, name)
        } else {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            opened = CleanDir::open(parent)?;
            // Gone, or now a link: whatever sits behind it is unreachable
            // without traversing, so nothing was removed.
            match opened.as_ref() {
                Some(dir) => (dir, path.file_name().unwrap_or_default()),
                None => return Ok(false),
            }
        };
        match dir.unlink(name) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = anchor;
        match remove_entry(path) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err),
        }
    }
}

/// Moves `file` to `target`. A regular file is renamed, or copied and
/// removed when the backup dir sits on another filesystem. A symlink whose
/// text is absolute is renamed too (the text stays correct, and no symlink
/// privilege is needed); a relative one is recreated at `target` with the
/// text resolved from its original directory and the original removed,
/// never copied through. `target` is always the path the backup ends up
/// at. A walked `anchor` (unix) does every lookup descriptor-relative and
/// reserves the destination atomically: a slot occupied after the
/// free-slot probe fails with `AlreadyExists` instead of replacing or
/// writing through whatever appeared there.
fn move_to_backup_held(
    file: &Path,
    target: &Path,
    anchor: Option<DirAnchor<'_>>,
) -> std::io::Result<BackupOutcome> {
    #[cfg(unix)]
    {
        if let Some((dir, name, _)) = anchor {
            return move_to_backup_at(dir, name, target);
        }
        // No walked anchor: open the parent once, no-follow, so a parent
        // swapped for a symlink cannot redirect the move.
        let parent = file
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let dir = match CleanDir::open(parent) {
            Ok(Some(dir)) => dir,
            // Gone, or now a link: the outcome depends on the file itself.
            Ok(None) => {
                return if std::fs::symlink_metadata(file).is_err() {
                    Ok(BackupOutcome::AlreadyGone)
                } else {
                    Err(std::io::Error::new(
                        ErrorKind::NotFound,
                        format!("no directory at {}", parent.display()),
                    ))
                };
            }
            Err(err) => return Err(err),
        };
        let name = file.file_name().unwrap_or_default();
        move_to_backup_at(&dir, name, target)
    }
    #[cfg(not(unix))]
    {
        let _ = anchor;
        let is_link = std::fs::symlink_metadata(file).is_ok_and(|meta| meta.is_symlink());
        let relative_link =
            is_link && std::fs::read_link(file).is_ok_and(|text| text.is_relative());
        if !relative_link && std::fs::rename(file, target).is_ok() {
            return Ok(BackupOutcome::BackedUp);
        }
        if is_link {
            if let Err(err) = recreate_symlink(file, target) {
                if err.kind() == ErrorKind::NotFound && std::fs::symlink_metadata(file).is_err() {
                    return Ok(BackupOutcome::AlreadyGone);
                }
                return Err(err);
            }
        } else {
            let mut src = match std::fs::File::open(file) {
                Ok(src) => src,
                // Only a source that is really gone (it raced away before
                // the copy could read it) is nothing to back up; a
                // `NotFound` with the source still present names the backup
                // side, and is a failure the row must report.
                Err(err)
                    if err.kind() == ErrorKind::NotFound
                        && std::fs::symlink_metadata(file).is_err() =>
                {
                    return Ok(BackupOutcome::AlreadyGone);
                }
                Err(err) => return Err(err),
            };
            // The destination only opens as a fresh reservation: a slot
            // occupied after the free-slot probe is refused, never
            // written through.
            if let Err(err) = copy_to_reserved(&mut src, target) {
                if err.kind() == ErrorKind::NotFound && std::fs::symlink_metadata(file).is_err() {
                    return Ok(BackupOutcome::AlreadyGone);
                }
                return Err(err);
            }
        }
        // A `NotFound` here is the source vanishing after the copy landed:
        // the backup exists, so the move still counts as done.
        if let Err(err) = remove_entry(file)
            && err.kind() != ErrorKind::NotFound
        {
            return Err(err);
        }
        Ok(BackupOutcome::BackedUp)
    }
}

/// The unix core of a backup move for the entry `name` in the held
/// directory `dir` (see [`move_to_backup_held`]). Same-filesystem moves
/// use a no-replace rename; filesystems without one fall back to a plain
/// rename whose failure decides. Cross-filesystem moves copy the open
/// source into a destination reserved with `create_new`, so a name planted
/// after the free-slot probe is never written through.
#[cfg(unix)]
fn move_to_backup_at(
    dir: &CleanDir,
    name: &OsStr,
    target: &Path,
) -> std::io::Result<BackupOutcome> {
    let meta = match dir.stat(name) {
        Ok(meta) => meta,
        // Raced away between walk and backup: already gone.
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(BackupOutcome::AlreadyGone),
        Err(err) => return Err(err),
    };
    let is_link = FileType::from_raw_mode(meta.st_mode).is_symlink();
    let relative_link = is_link
        && match dir.link_text(name) {
            Ok(text) => text.is_relative(),
            Err(err) if err.kind() == ErrorKind::NotFound => {
                return Ok(BackupOutcome::AlreadyGone);
            }
            Err(err) => return Err(err),
        };
    if !relative_link {
        match rename_no_replace(dir, name, target) {
            Ok(()) => return Ok(BackupOutcome::BackedUp),
            // The slot was occupied after the probe: the caller retries the
            // next one. The reservation is the real check.
            Err(Errno::EXIST) => return Err(ErrorKind::AlreadyExists.into()),
            // Backup dir on another filesystem: copy below.
            Err(Errno::XDEV) => {}
            Err(err) => return backup_move_raced(dir, name, err.into()),
        }
    }
    if is_link {
        let text = match dir.link_text(name) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                return Ok(BackupOutcome::AlreadyGone);
            }
            Err(err) => return Err(err),
        };
        let points_to = resolved_link_text(&dir.canonical, &text);
        // A symlink refuses an existing target: an occupied slot comes
        // back as `AlreadyExists` for the next-slot retry.
        if let Err(err) = std::os::unix::fs::symlink(&points_to, target) {
            return backup_move_raced(dir, name, err);
        }
        remove_moved_source(dir, name)?;
        return Ok(BackupOutcome::BackedUp);
    }
    // Cross-filesystem copy: the source opens no-follow through the held
    // directory, the destination only as a fresh reservation.
    let mut src = match rustix::fs::openat(
        &dir.fd,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => std::fs::File::from(fd),
        // The entry became a link after the stat: back up the link.
        Err(Errno::LOOP) => {
            let text = match dir.link_text(name) {
                Ok(text) => text,
                Err(err) if err.kind() == ErrorKind::NotFound => {
                    return Ok(BackupOutcome::AlreadyGone);
                }
                Err(err) => return Err(err),
            };
            let points_to = resolved_link_text(&dir.canonical, &text);
            if let Err(err) = std::os::unix::fs::symlink(&points_to, target) {
                return backup_move_raced(dir, name, err);
            }
            remove_moved_source(dir, name)?;
            return Ok(BackupOutcome::BackedUp);
        }
        Err(err) => return backup_move_raced(dir, name, err.into()),
    };
    // An occupied slot comes back as `AlreadyExists` for the next-slot
    // retry; any other failure names the backup side (the source is open,
    // so it exists) and is a failure the row reports.
    copy_to_reserved(&mut src, target)?;
    remove_moved_source(dir, name)?;
    Ok(BackupOutcome::BackedUp)
}

/// Renames `name` in `dir` onto `target` without replacing an existing
/// entry where the platform offers that (Linux, Android, Apple). A
/// filesystem without the primitive, and every other unix, falls back to
/// a plain rename whose failure decides.
#[cfg(all(
    unix,
    any(target_os = "linux", target_os = "android", target_vendor = "apple")
))]
fn rename_no_replace(dir: &CleanDir, name: &OsStr, target: &Path) -> Result<(), Errno> {
    match rustix::fs::renameat_with(
        &dir.fd,
        name,
        rustix::fs::CWD,
        target,
        rustix::fs::RenameFlags::NOREPLACE,
    ) {
        // Apple reports ENOTSUP as `NOTSUP`, distinct from `OPNOTSUPP`;
        // elsewhere the two are one value, so the guard form avoids an
        // unreachable-pattern lint.
        Err(err)
            if matches!(err, Errno::NOSYS | Errno::INVAL | Errno::OPNOTSUPP)
                || err == Errno::NOTSUP =>
        {
            rustix::fs::renameat(&dir.fd, name, rustix::fs::CWD, target)
        }
        other => other,
    }
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android", target_vendor = "apple"))
))]
fn rename_no_replace(dir: &CleanDir, name: &OsStr, target: &Path) -> Result<(), Errno> {
    rustix::fs::renameat(&dir.fd, name, rustix::fs::CWD, target)
}

/// A `NotFound` on a backup move counts as already gone only when the held
/// entry itself is gone; a `NotFound` naming the backup side is a failure
/// the row must report.
#[cfg(unix)]
fn backup_move_raced(
    dir: &CleanDir,
    name: &OsStr,
    err: std::io::Error,
) -> std::io::Result<BackupOutcome> {
    if err.kind() == ErrorKind::NotFound && dir.stat(name).is_err() {
        Ok(BackupOutcome::AlreadyGone)
    } else {
        Err(err)
    }
}

/// Removes the moved source once the backup holds the entry. A `NotFound`
/// is the source vanishing after the move landed: still a done move.
#[cfg(unix)]
fn remove_moved_source(dir: &CleanDir, name: &OsStr) -> std::io::Result<()> {
    match dir.unlink(name) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Copies `src` into a freshly created file at `target`: the destination is
/// reserved atomically by its `create_new` open, so a name planted after
/// the free-slot probe is refused instead of written through, and the
/// copy's permissions come from the open source, not a re-read of the path.
fn copy_to_reserved(src: &mut std::fs::File, target: &Path) -> std::io::Result<()> {
    let mut dst = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    std::io::copy(src, &mut dst)?;
    dst.set_permissions(src.metadata()?.permissions())
}

/// Deletes source directories emptied by `move_source`, returning the removed
/// directory paths. `Auto` prunes each touched dir and its emptied ancestors
/// up to (but excluding) `root`; `Always` prunes every empty dir under
/// `root`; `Never` does nothing.
pub(super) fn delete_empty_dirs(
    root: &Path,
    touched: &BTreeSet<PathBuf>,
    mode: MoveDeleteDirs,
) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    match mode {
        MoveDeleteDirs::Never => {}
        MoveDeleteDirs::Auto => {
            for dir in touched {
                for ancestor in dir.ancestors() {
                    if ancestor == root || !ancestor.starts_with(root) {
                        break;
                    }
                    // Only an empty dir removes cleanly; the first surviving
                    // file stops the walk up.
                    match std::fs::remove_dir(ancestor) {
                        Ok(()) => removed.push(ancestor.to_path_buf()),
                        Err(_) => break,
                    }
                }
            }
        }
        MoveDeleteDirs::Always => {
            let Ok(entries) = std::fs::read_dir(root) else {
                return removed;
            };
            for entry in entries.flatten() {
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                // file_type does not traverse symlinks: linked dirs stay.
                if !kind.is_dir() {
                    continue;
                }
                let child = entry.path();
                prune_empty(&child, &mut removed);
                if std::fs::remove_dir(&child).is_ok() {
                    removed.push(child);
                }
            }
        }
    }
    removed
}

/// Post-order removal of every empty directory under `dir` (exclusive).
fn prune_empty(dir: &Path, removed: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() {
            continue;
        }
        let child = entry.path();
        prune_empty(&child, removed);
        if std::fs::remove_dir(&child).is_ok() {
            removed.push(child);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::RecordingProgress;
    use crate::runner::models::ProgressEvent;
    use std::fs;

    fn write(path: impl AsRef<Path>, bytes: &[u8]) -> PathBuf {
        let path = path.as_ref().to_path_buf();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }

    fn options(exclude: &[&str], backup: Option<&Path>) -> CleanOptions {
        let owned: Vec<String> = exclude.iter().map(|glob| glob.to_string()).collect();
        CleanOptions::new(
            compile_clean_exclude(&owned).unwrap(),
            backup.map(Path::to_path_buf),
        )
    }

    fn keep_set(paths: &[&Path]) -> BTreeSet<PathBuf> {
        paths.iter().map(|path| path.to_path_buf()).collect()
    }

    fn warnings(progress: &RecordingProgress) -> Vec<String> {
        progress
            .take_events()
            .into_iter()
            .filter_map(|event| match event {
                ProgressEvent::Warn { message } => Some(message),
                _ => None,
            })
            .collect()
    }

    fn clean_paths(rows: &[OrganizeRow]) -> Vec<String> {
        rows.iter()
            .map(|row| row.input.display().to_string())
            .collect()
    }

    /// The backup move reports the two races apart: a source that is still
    /// there lands at the target, and a `NotFound` on the source itself
    /// (rename or copy) is already gone, not a failed backup.
    #[test]
    fn move_to_backup_splits_landed_from_already_gone() {
        let out = tempfile::tempdir().unwrap();
        let backup = tempfile::tempdir().unwrap();

        // A live file renames straight into the backup dir.
        let live = write(out.path().join("Game.zip"), b"payload");
        let target = backup.path().join("Game.zip");
        assert!(matches!(
            move_to_backup_held(&live, &target, None).unwrap(),
            BackupOutcome::BackedUp
        ));
        assert_eq!(fs::read(&target).unwrap(), b"payload");
        assert!(!live.exists());

        // A vanished source counts as already gone: nothing was backed up.
        let gone = out.path().join("Raced.zip");
        assert!(matches!(
            move_to_backup_held(&gone, &backup.path().join("Raced.zip"), None).unwrap(),
            BackupOutcome::AlreadyGone
        ));
        assert!(!backup.path().join("Raced.zip").exists());
    }

    #[tokio::test]
    async fn stale_files_in_written_dirs_are_removed() {
        let out = tempfile::tempdir().unwrap();
        let gba = write(out.path().join("GBA").join("Kept.zip"), b"kept");
        let stale = write(out.path().join("GBA").join("Old.zip"), b"stale");
        let deep = write(out.path().join("GBA").join("sub").join("Deep.zip"), b"deep");
        // The output root was not written to, so its files stay.
        let root_stale = write(out.path().join("Root.zip"), b"root");

        let mut written = BTreeSet::new();
        written.insert(out.path().join("GBA"));
        let rows = clean_output(
            out.path(),
            &written,
            keep_set(&[gba.as_path()]),
            Path::new("/disjoint-input-root"),
            &options(&[], None),
            ConflictPolicy::Error,
            false,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();

        let mut cleaned = clean_paths(&rows);
        cleaned.sort();
        let mut expected = vec![deep.display().to_string(), stale.display().to_string()];
        expected.sort();
        assert_eq!(cleaned, expected);
        assert!(rows.iter().all(|row| row.action == "clean"
            && row.console.is_none()
            && row.status == FileStatus::Ok
            && !row.planned
            && row.detail.as_deref() == Some("deleted")
            && row.output.is_none()));
        assert!(!stale.exists());
        assert!(!deep.exists());
        assert!(gba.exists());
        assert!(root_stale.exists());
    }

    /// A stale file in every one of hundreds of subdirectories of one
    /// written dir still cleans: the candidates hold only the written
    /// dir's own descriptor and re-open their parent chain at removal, so
    /// hundreds of directories do not mean hundreds of descriptors at
    /// once (under a default 256 soft fd limit the per-candidate held
    /// directories of the previous scheme hit EMFILE here).
    #[cfg(unix)]
    #[tokio::test]
    async fn cleans_stale_files_spread_over_hundreds_of_directories() {
        let out = tempfile::tempdir().unwrap();
        let written_dir = out.path().join("GBA");
        for i in 0..400 {
            write(
                written_dir.join(format!("d{i:03}")).join("Stale.zip"),
                b"stale",
            );
        }
        let mut written = BTreeSet::new();
        written.insert(written_dir.clone());

        let rows = clean_output(
            out.path(),
            &written,
            keep_set(&[]),
            Path::new("/disjoint-input-root"),
            &options(&[], None),
            ConflictPolicy::Error,
            false,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(rows.len(), 400);
        assert!(
            rows.iter().all(|row| row.status == FileStatus::Ok),
            "every clean must land, none may fail"
        );
        for i in 0..400 {
            assert!(
                !written_dir
                    .join(format!("d{i:03}"))
                    .join("Stale.zip")
                    .exists()
            );
        }
    }

    /// The accidental-destruction guard: with nothing written this run
    /// (dry run, or every row skipped) nothing is cleaned.
    #[tokio::test]
    async fn nothing_written_skips_clean_with_a_warning() {
        let out = tempfile::tempdir().unwrap();
        let stale = write(out.path().join("GBA").join("Old.zip"), b"stale");

        let progress = RecordingProgress::default();
        let rows = clean_output(
            out.path(),
            &BTreeSet::new(),
            keep_set(&[]),
            Path::new("/disjoint-input-root"),
            &options(&[], None),
            ConflictPolicy::Error,
            true,
            &progress,
            &CancelToken::new(),
        )
        .await
        .unwrap();

        assert!(rows.is_empty());
        assert!(stale.exists());
        assert!(
            warnings(&progress)
                .iter()
                .any(|message| message.contains("nothing was written"))
        );
    }

    /// A dry run reports every stale file as planned without touching disk.
    #[tokio::test]
    async fn dry_run_reports_would_delete_without_deleting() {
        let out = tempfile::tempdir().unwrap();
        let stale = write(out.path().join("GBA").join("Old.zip"), b"stale");
        let backup = out.path().join("backup");

        let mut written = BTreeSet::new();
        written.insert(out.path().join("GBA"));
        let rows = clean_output(
            out.path(),
            &written,
            keep_set(&[]),
            Path::new("/disjoint-input-root"),
            &options(&[], Some(&backup)),
            ConflictPolicy::Error,
            true,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();

        assert!(rows[0].planned);
        let expected = format!("would back up to {}", backup.join("Old.zip").display());
        assert_eq!(rows[0].detail.as_deref(), Some(expected.as_str()));
        assert!(stale.exists());
        assert!(!backup.exists(), "dry run must not create the backup dir");
    }

    /// The keep set protects outputs this run skipped because they existed,
    /// inputs that share the output dir, and playlist files, while stale
    /// neighbours still go.
    #[tokio::test]
    async fn keep_set_protects_skipped_outputs_inputs_and_playlists() {
        let lib = tempfile::tempdir().unwrap();
        let out = lib.path().join("out");
        let gba = write(out.join("GBA").join("Game.zip"), b"skipped existing output");
        let input = write(lib.path().join("in").join("Kept.nes"), b"input");
        let playlist = write(out.join("GBA").join("Game.m3u"), b"playlist");
        let stale = write(out.join("GBA").join("Old.zip"), b"stale");

        let mut written = BTreeSet::new();
        written.insert(gba.parent().unwrap().to_path_buf());
        let rows = clean_output(
            &out,
            &written,
            keep_set(&[gba.as_path(), input.as_path(), playlist.as_path()]),
            Path::new("/disjoint-input-root"),
            &options(&[], None),
            ConflictPolicy::Error,
            false,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(clean_paths(&rows), vec![stale.display().to_string()]);
        assert!(gba.exists());
        assert!(input.exists());
        assert!(playlist.exists());
        assert!(!stale.exists());
    }

    /// A cue set survives through every file it occupies; a bin outside the
    /// keep set is still stale.
    #[tokio::test]
    async fn cue_set_is_protected_through_all_its_files() {
        let out = tempfile::tempdir().unwrap();
        let cue = write(out.path().join("PSX").join("Game.cue"), b"cue");
        let bin = write(out.path().join("PSX").join("Game.bin"), b"bin");
        let orphan = write(out.path().join("PSX").join("Orphan.bin"), b"orphan");

        let mut written = BTreeSet::new();
        written.insert(out.path().join("PSX"));
        let rows = clean_output(
            out.path(),
            &written,
            keep_set(&[cue.as_path(), bin.as_path()]),
            Path::new("/disjoint-input-root"),
            &options(&[], None),
            ConflictPolicy::Error,
            false,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(clean_paths(&rows), vec![orphan.display().to_string()]);
        assert!(cue.exists());
        assert!(bin.exists());
        assert!(!orphan.exists());
    }

    /// The root written dir cleans only its direct files: a nested dir that
    /// was not itself written is never entered.
    #[tokio::test]
    async fn root_written_dir_cleans_only_direct_files() {
        let out = tempfile::tempdir().unwrap();
        let stale = write(out.path().join("Old.zip"), b"stale");
        let nested = write(out.path().join("GBA").join("Old.zip"), b"nested");

        let mut written = BTreeSet::new();
        written.insert(out.path().to_path_buf());
        let rows = clean_output(
            out.path(),
            &written,
            keep_set(&[]),
            Path::new("/disjoint-input-root"),
            &options(&[], None),
            ConflictPolicy::Error,
            false,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(clean_paths(&rows), vec![stale.display().to_string()]);
        assert!(nested.exists());
    }

    /// Nested written dirs are covered once: the parent's recursive walk
    /// removes both levels without duplicating rows.
    #[tokio::test]
    async fn nested_written_dirs_are_covered_once() {
        let out = tempfile::tempdir().unwrap();
        let stale = write(out.path().join("Old.zip"), b"stale");
        let deep = write(out.path().join("GBA").join("Old.zip"), b"deep");

        let mut written = BTreeSet::new();
        written.insert(out.path().to_path_buf());
        written.insert(out.path().join("GBA"));
        let rows = clean_output(
            out.path(),
            &written,
            keep_set(&[]),
            Path::new("/disjoint-input-root"),
            &options(&[], None),
            ConflictPolicy::Error,
            false,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(clean_paths(&rows).len(), 2);
        assert!(!stale.exists());
        assert!(!deep.exists());
    }

    /// Kept files match by identity, not spelling: a keep path spelled
    /// through a symlinked directory still protects the real file.
    #[cfg(unix)]
    #[tokio::test]
    async fn keep_matches_by_identity_not_spelling() {
        let out = tempfile::tempdir().unwrap();
        let gba = out.path().join("GBA");
        let kept = write(gba.join("Kept.zip"), b"kept");
        std::os::unix::fs::symlink(&gba, out.path().join("spelled")).unwrap();

        let mut written = BTreeSet::new();
        written.insert(gba);
        let rows = clean_output(
            out.path(),
            &written,
            keep_set(&[out.path().join("spelled").join("Kept.zip").as_path()]),
            Path::new("/disjoint-input-root"),
            &options(&[], None),
            ConflictPolicy::Error,
            false,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();

        assert!(rows.is_empty());
        assert!(kept.exists());
    }

    /// Symlinked directories are never descended, and every unkept symlink,
    /// including one pointing at a kept source, is removed as a link (its
    /// target is never read or touched).
    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_dirs_are_not_followed_and_stale_links_are_removed() {
        let out = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let inside = write(target.path().join("Inside.zip"), b"inside");
        let input = write(out.path().join("Kept.nes"), b"kept input");

        let gba = out.path().join("GBA");
        fs::create_dir_all(&gba).unwrap();
        let stale_link = gba.join("linked");
        std::os::unix::fs::symlink(target.path(), &stale_link).unwrap();
        let stale_link_to_kept = gba.join("to-input");
        std::os::unix::fs::symlink(&input, &stale_link_to_kept).unwrap();

        let mut written = BTreeSet::new();
        written.insert(gba);
        let rows = clean_output(
            out.path(),
            &written,
            keep_set(&[input.as_path()]),
            Path::new("/disjoint-input-root"),
            &options(&[], None),
            ConflictPolicy::Error,
            false,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();

        let cleaned = clean_paths(&rows);
        assert!(
            cleaned.contains(&stale_link.display().to_string()),
            "{cleaned:?}"
        );
        assert!(
            cleaned.contains(&stale_link_to_kept.display().to_string()),
            "a link is only its own link: pointing at a kept source does not keep it, \
             {cleaned:?}"
        );
        assert!(cleaned.iter().all(|path| !path.contains("Inside.zip")));
        assert!(!stale_link.exists(), "the link is gone");
        assert!(!stale_link_to_kept.exists(), "the stale link is gone");
        assert!(inside.exists(), "the link target is untouched");
        assert!(input.exists());
    }

    /// A symlink kept by its own path and identity (a placed link output)
    /// survives clean, even while dangling.
    #[cfg(unix)]
    #[tokio::test]
    async fn kept_symlink_survives_by_its_own_identity() {
        let out = tempfile::tempdir().unwrap();
        let gba = out.path().join("GBA");
        fs::create_dir_all(&gba).unwrap();
        let placed = gba.join("Game.gba");
        std::os::unix::fs::symlink(out.path().join("elsewhere.gba"), &placed).unwrap();

        let mut written = BTreeSet::new();
        written.insert(gba);
        let rows = clean_output(
            out.path(),
            &written,
            keep_set(&[placed.as_path()]),
            Path::new("/disjoint-input-root"),
            &options(&[], None),
            ConflictPolicy::Error,
            false,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();

        assert!(rows.is_empty(), "{:?}", clean_paths(&rows));
        assert!(
            std::fs::symlink_metadata(&placed).unwrap().is_symlink(),
            "the kept link survives (it is dangling, so exists() would lie)"
        );
    }

    /// Windows has no inode identity for symlinks: a keep-set entry spelled
    /// with different casing than the symlink on disk still counts as kept,
    /// via the raw-path case-insensitive fallback.
    #[cfg(windows)]
    #[tokio::test]
    async fn kept_symlink_survives_case_insensitively_on_windows() {
        let out = tempfile::tempdir().unwrap();
        let gba = out.path().join("GBA");
        fs::create_dir_all(&gba).unwrap();
        let placed = gba.join("Game.gba");
        std::os::windows::fs::symlink_file(out.path().join("elsewhere.gba"), &placed).unwrap();
        let differently_cased = gba.join("GAME.gba");

        let mut written = BTreeSet::new();
        written.insert(gba);
        let rows = clean_output(
            out.path(),
            &written,
            keep_set(&[differently_cased.as_path()]),
            Path::new("/disjoint-input-root"),
            &options(&[], None),
            ConflictPolicy::Error,
            false,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();

        assert!(rows.is_empty(), "{:?}", clean_paths(&rows));
    }

    /// A backup dir inside a written dir is never re-collected: its contents
    /// are already archived, so a second clean leaves them alone.
    #[tokio::test]
    async fn backup_dir_inside_a_written_dir_is_not_recollected() {
        let out = tempfile::tempdir().unwrap();
        let kept = write(out.path().join("GBA").join("Kept.zip"), b"kept");
        let stale = write(out.path().join("GBA").join("Old.zip"), b"stale");
        let backup = out.path().join("GBA").join("backup");

        let mut written = BTreeSet::new();
        written.insert(out.path().join("GBA"));
        let run = || async {
            clean_output(
                out.path(),
                &written,
                keep_set(&[kept.as_path()]),
                Path::new("/disjoint-input-root"),
                &options(&[], Some(&backup)),
                ConflictPolicy::Error,
                false,
                &RecordingProgress::default(),
                &CancelToken::new(),
            )
            .await
            .unwrap()
        };

        let first = run().await;
        assert_eq!(clean_paths(&first), vec![stale.display().to_string()]);
        assert_eq!(fs::read(backup.join("Old.zip")).unwrap(), b"stale");

        let second = run().await;
        assert!(second.is_empty(), "{:?}", clean_paths(&second));
        assert_eq!(fs::read(backup.join("Old.zip")).unwrap(), b"stale");
        assert!(!backup.join("Old (1).zip").exists(), "not re-collected");
    }

    /// Stale playlists already inside the backup dir are never
    /// re-collected, and the pass creates the backup dir once.
    #[tokio::test]
    async fn stale_playlists_inside_the_backup_dir_are_skipped() {
        let out = tempfile::tempdir().unwrap();
        let backup = out.path().join("backup");
        let kept = write(backup.join("Old.m3u"), b"old");
        let stale = write(out.path().join("G").join("New.m3u"), b"new");

        let rows = delete_stale_playlists(
            out.path(),
            &[kept.clone(), stale.clone()],
            keep_set(&[]),
            Path::new("/disjoint-input-root"),
            &options(&[], Some(&backup)),
            false,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(clean_paths(&rows), vec![stale.display().to_string()]);
        assert_eq!(
            fs::read(&kept).unwrap(),
            b"old",
            "a backed-up playlist is never re-collected"
        );
        assert!(!kept.with_file_name("Old (1).m3u").exists());
        assert!(!stale.exists());
        assert_eq!(fs::read(backup.join("New.m3u")).unwrap(), b"new");
    }

    /// A stale playlist whose path the keep set protects is never deleted:
    /// it is this run's output or source, whatever staleness says. The
    /// guard matches by identity too: a kept file's canonical spelling is
    /// skipped even when the stale list spells it differently.
    #[tokio::test]
    async fn a_kept_path_is_never_deleted_as_stale() {
        let out = tempfile::tempdir().unwrap();
        let stale = write(out.path().join("G").join("Stale.m3u"), b"stale");
        let kept = write(out.path().join("G").join("Kept.m3u"), b"kept output");

        let rows = delete_stale_playlists(
            out.path(),
            &[stale.clone(), kept.clone()],
            keep_set(&[kept.as_path()]),
            Path::new("/disjoint-input-root"),
            &options(&[], None),
            false,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(clean_paths(&rows), vec![stale.display().to_string()]);
        assert!(kept.exists(), "the kept path survives");
        assert!(!stale.exists());
    }

    /// A stale-looking playlist whose own location is under the input root
    /// is never deleted, whatever staleness says.
    #[tokio::test]
    async fn a_stale_playlist_under_the_input_root_survives() {
        let out = tempfile::tempdir().unwrap();
        let stale = write(out.path().join("G").join("Old.m3u"), b"old");

        let rows = delete_stale_playlists(
            out.path(),
            std::slice::from_ref(&stale),
            keep_set(&[]),
            &fs::canonicalize(out.path()).unwrap(),
            &options(&[], None),
            false,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();

        assert!(rows.is_empty(), "{:?}", clean_paths(&rows));
        assert!(stale.exists(), "an under-input playlist is never cleaned");
    }

    /// A symlink candidate is backed up as the link itself, never as a
    /// copy of its target: the backup holds a link to the same target and
    /// the target file is untouched.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_candidate_is_backed_up_as_a_link() {
        let out = tempfile::tempdir().unwrap();
        let backup = out.path().join("backup");
        let target = tempfile::tempdir().unwrap();
        let real = write(target.path().join("Real.zip"), b"real");
        let link = out.path().join("G").join("Linked.zip");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let mut written = BTreeSet::new();
        written.insert(out.path().join("G"));
        let rows = clean_output(
            out.path(),
            &written,
            keep_set(&[]),
            Path::new("/disjoint-input-root"),
            &options(&[], Some(&backup)),
            ConflictPolicy::Error,
            false,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(clean_paths(&rows), vec![link.display().to_string()]);
        assert!(
            std::fs::symlink_metadata(&link).is_err(),
            "the candidate link is gone"
        );
        assert_eq!(
            std::fs::read(&real).unwrap(),
            b"real",
            "the link's target is untouched"
        );
        let moved = backup.join("Linked.zip");
        assert!(
            std::fs::symlink_metadata(&moved).unwrap().is_symlink(),
            "the backup holds the link itself under the reported name"
        );
        assert_eq!(std::fs::read_link(&moved).unwrap(), real);
        // The row reports the path the link actually sits at.
        assert_eq!(rows[0].output.as_deref(), Some(moved.as_path()));
        assert_eq!(
            rows[0].detail.as_deref(),
            Some(format!("backed up to {}", moved.display()).as_str())
        );
    }

    /// A dangling link at a backup slot counts as taken: the probe reads
    /// the link's own metadata, so the rename never replaces it.
    #[cfg(unix)]
    #[test]
    fn backup_target_never_reuses_a_dangling_slot() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(dir.path().join("gone"), dir.path().join("Old.zip")).unwrap();
        let (target, slot) = backup_target(
            dir.path(),
            Path::new("/elsewhere/Old.zip"),
            &BTreeSet::new(),
            false,
        );
        assert_eq!(target, dir.path().join("Old (1).zip"));
        assert_eq!(slot, 1);
    }

    /// A planned dry-run slot counts as taken: `Old.zip` is skipped for
    /// `Old (1).zip`, and with a case-folding backup volume `old.zip`
    /// collides with the planned `Old.zip` too.
    #[test]
    fn backup_target_skips_planned_slots_in_the_volumes_case_rules() {
        let dir = tempfile::tempdir().unwrap();
        let planned = BTreeSet::from([dir.path().join("Old.zip")]);
        let file = Path::new("/elsewhere/Old.zip");
        assert_eq!(
            backup_target(dir.path(), file, &planned, false).0,
            dir.path().join("Old (1).zip")
        );
        let lower = Path::new("/elsewhere/old.zip");
        assert_eq!(
            backup_target(dir.path(), lower, &planned, false).0,
            dir.path().join("old.zip"),
            "a case-sensitive volume keeps the two apart"
        );
        assert_eq!(
            backup_target(dir.path(), lower, &planned, true).0,
            dir.path().join("old (1).zip"),
            "a case-folding volume names the same slot"
        );
    }

    /// A backup slot occupied after the free-slot probe is never replaced
    /// or written through: the move refuses with `AlreadyExists` (the
    /// next-slot retry signal), the occupant — here a symlink to a victim
    /// file — keeps its victim untouched, and the retry lands the file in
    /// the next slot.
    #[cfg(unix)]
    #[test]
    fn a_planted_symlink_at_a_backup_slot_is_never_written_through() {
        let out = tempfile::tempdir().unwrap();
        let victim_dir = tempfile::tempdir().unwrap();
        let victim = write(victim_dir.path().join("victim.gba"), b"victim");
        let backup = out.path().join("backup");
        let planted = backup.join("Old.zip");
        fs::create_dir_all(&backup).unwrap();
        std::os::unix::fs::symlink(&victim, &planted).unwrap();

        // The race window: the slot is occupied after `backup_target`'s
        // probe chose it, so the move is called on the occupied name.
        let file = write(out.path().join("GBA").join("Old.zip"), b"stale");
        assert!(
            matches!(
                move_to_backup_held(&file, &planted, None)
                    .unwrap_err()
                    .kind(),
                ErrorKind::AlreadyExists
            ),
            "an occupied slot is refused"
        );
        assert_eq!(
            fs::read(&victim).unwrap(),
            b"victim",
            "the victim behind the planted link is untouched"
        );
        assert!(
            fs::symlink_metadata(&planted)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the occupant stays in its slot"
        );
        assert!(file.exists(), "the source is untouched by the refused move");

        // The retry loop then reserves the next slot atomically: the file
        // lands there, and the victim is still untouched.
        let mut backup_made = false;
        let row = remove_candidate(
            &file,
            5,
            None,
            &options(&[], Some(&backup)),
            &mut backup_made,
            false,
        )
        .unwrap();
        let landed = backup.join("Old (1).zip");
        assert_eq!(row.output.as_deref(), Some(landed.as_path()), "{row:?}");
        assert_eq!(fs::read(&landed).unwrap(), b"stale");
        assert!(!file.exists());
        assert_eq!(
            fs::read(&victim).unwrap(),
            b"victim",
            "the victim survives the whole backup"
        );
    }

    /// The cross-filesystem fallback copies through a reserved destination:
    /// `create_new` refuses an occupied slot (the next-slot retry signal)
    /// without touching the occupant, and a free slot receives the open
    /// source's contents and permissions.
    #[cfg(unix)]
    #[test]
    fn a_copy_reservation_refuses_an_occupied_slot_and_copies_through() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let src = write(dir.path().join("Old.zip"), b"payload");
        fs::set_permissions(&src, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut src_file = fs::File::open(&src).unwrap();

        let occupied = write(dir.path().join("taken.zip"), b"mine");
        assert!(matches!(
            copy_to_reserved(&mut src_file, &occupied)
                .unwrap_err()
                .kind(),
            ErrorKind::AlreadyExists
        ));
        assert_eq!(
            fs::read(&occupied).unwrap(),
            b"mine",
            "the occupant is untouched"
        );

        let free = dir.path().join("free.zip");
        copy_to_reserved(&mut src_file, &free).unwrap();
        assert_eq!(fs::read(&free).unwrap(), b"payload");
        assert_eq!(
            fs::metadata(&free).unwrap().permissions().mode() & 0o777,
            0o600,
            "permissions travel from the open source"
        );
    }

    /// A written dir that is itself a symlink is skipped entirely: its
    /// target's contents are never walked, so nothing inside is removed.
    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_written_dir_is_never_entered() {
        let out = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let inside = write(target.path().join("Inside.zip"), b"inside");
        let linked = out.path().join("linked");
        std::os::unix::fs::symlink(target.path(), &linked).unwrap();

        let mut written = BTreeSet::new();
        written.insert(linked);
        let rows = clean_output(
            out.path(),
            &written,
            keep_set(&[]),
            Path::new("/disjoint-input-root"),
            &options(&[], None),
            ConflictPolicy::Error,
            false,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();

        assert!(rows.is_empty(), "{:?}", clean_paths(&rows));
        assert!(inside.exists());
    }

    /// `clean_exclude` globs protect files from deletion, whatever the case
    /// of the file name.
    #[tokio::test]
    async fn exclude_globs_protect_files() {
        let out = tempfile::tempdir().unwrap();
        let save = write(out.path().join("GBA").join("Game.SAV"), b"save");
        let stale = write(out.path().join("GBA").join("Old.zip"), b"stale");

        let mut written = BTreeSet::new();
        written.insert(out.path().join("GBA"));
        let rows = clean_output(
            out.path(),
            &written,
            keep_set(&[]),
            Path::new("/disjoint-input-root"),
            &options(&["**/*.sav"], None),
            ConflictPolicy::Error,
            false,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(clean_paths(&rows), vec![stale.display().to_string()]);
        assert!(save.exists());
        assert!(!stale.exists());
    }

    /// Backups land flat in the backup dir; a collision gets ` (1)`.
    #[tokio::test]
    async fn backup_moves_flat_with_collision_suffixes() {
        let out = tempfile::tempdir().unwrap();
        let stale = write(out.path().join("GBA").join("Old.zip"), b"stale");
        let other = write(out.path().join("SNES").join("Old.zip"), b"other");
        let backup = out.path().join("backup");

        let mut written = BTreeSet::new();
        written.insert(out.path().join("GBA"));
        written.insert(out.path().join("SNES"));
        let rows = clean_output(
            out.path(),
            &written,
            keep_set(&[]),
            Path::new("/disjoint-input-root"),
            &options(&[], Some(&backup)),
            ConflictPolicy::Error,
            false,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();

        let first = backup.join("Old.zip");
        let second = backup.join("Old (1).zip");
        assert_eq!(
            rows.iter()
                .map(|row| row.output.clone().unwrap())
                .collect::<Vec<_>>(),
            vec![first.clone(), second.clone()]
        );
        assert!(
            rows.iter()
                .all(|row| row.status == FileStatus::Ok && !row.planned)
        );
        assert_eq!(fs::read(&first).unwrap(), b"stale");
        assert_eq!(fs::read(&second).unwrap(), b"other");
        assert!(!stale.exists());
        assert!(!other.exists());
    }

    /// `on_conflict rename` and clean together warn about earlier " (n)"
    /// slots once; other policies do not.
    #[tokio::test]
    async fn rename_policy_warns_once_about_numbered_slots() {
        let out = tempfile::tempdir().unwrap();
        let gba = out.path().join("GBA");
        fs::create_dir_all(&gba).unwrap();
        let mut written = BTreeSet::new();
        written.insert(gba);

        for (policy, expected) in [
            (ConflictPolicy::Rename, true),
            (ConflictPolicy::Overwrite, false),
        ] {
            let progress = RecordingProgress::default();
            clean_output(
                out.path(),
                &written,
                keep_set(&[]),
                Path::new("/disjoint-input-root"),
                &options(&[], None),
                policy,
                true,
                &progress,
                &CancelToken::new(),
            )
            .await
            .unwrap();
            let rename_warns = warnings(&progress)
                .iter()
                .filter(|message| message.contains("on-conflict rename"))
                .count();
            assert_eq!(rename_warns, usize::from(expected), "{policy:?}");
        }
    }

    #[test]
    fn auto_prunes_touched_dirs_up_to_the_root() {
        let root = tempfile::tempdir().unwrap();
        let moved_from = root.path().join("lib").join("a");
        fs::create_dir_all(root.path().join("lib").join("b")).unwrap();
        fs::create_dir_all(&moved_from).unwrap();
        fs::write(root.path().join("lib").join("b").join("kept.rom"), b"kept").unwrap();

        let mut touched = BTreeSet::new();
        touched.insert(moved_from.clone());
        let removed = delete_empty_dirs(root.path(), &touched, MoveDeleteDirs::Auto);

        assert_eq!(
            removed,
            vec![root.path().join("lib").join("a")],
            "the walk stops at the still-populated parent"
        );
        assert!(!moved_from.exists());
        assert!(root.path().join("lib").exists());
    }

    #[test]
    fn auto_walks_up_through_emptied_ancestors() {
        let root = tempfile::tempdir().unwrap();
        let moved_from = root.path().join("sets").join("inner");
        fs::create_dir_all(&moved_from).unwrap();

        let mut touched = BTreeSet::new();
        touched.insert(moved_from.clone());
        let removed = delete_empty_dirs(root.path(), &touched, MoveDeleteDirs::Auto);

        assert_eq!(
            removed,
            vec![moved_from.clone(), root.path().join("sets")],
            "children first, then the emptied parent, never the root"
        );
        assert!(!moved_from.exists());
        assert!(root.path().exists());
    }

    #[test]
    fn never_mode_removes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("empty");
        fs::create_dir_all(&dir).unwrap();
        let mut touched = BTreeSet::new();
        touched.insert(dir.clone());
        assert!(delete_empty_dirs(root.path(), &touched, MoveDeleteDirs::Never).is_empty());
        assert!(dir.exists());
    }

    #[test]
    fn always_prunes_every_empty_dir_except_the_root() {
        let root = tempfile::tempdir().unwrap();
        let empty_deep = root.path().join("a").join("b");
        fs::create_dir_all(&empty_deep).unwrap();
        let kept = write(root.path().join("full").join("rom.nes"), b"rom");

        #[cfg(unix)]
        let linked_target = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(linked_target.path(), root.path().join("linked")).unwrap();

        let removed = delete_empty_dirs(root.path(), &BTreeSet::new(), MoveDeleteDirs::Always);

        assert!(removed.contains(&empty_deep));
        assert!(removed.contains(&root.path().join("a")));
        assert!(!removed.contains(&root.path().join("full")));
        assert!(!empty_deep.exists());
        assert!(
            !root.path().join("a").exists(),
            "emptied parents are pruned too"
        );
        assert!(root.path().join("full").exists());
        assert!(kept.exists());
        #[cfg(unix)]
        assert!(
            root.path().join("linked").exists(),
            "symlinked dirs are not pruned"
        );
    }

    #[test]
    fn invalid_move_delete_dirs_is_rejected() {
        let options = RunOptions {
            move_delete_dirs: Some("sometimes".to_string()),
            ..RunOptions::default()
        };
        assert!(move_delete_dirs(&options).is_err());
    }

    #[test]
    fn invalid_exclude_glob_is_rejected() {
        let err = compile_clean_exclude(&["[unclosed".to_string()]).unwrap_err();
        assert!(err.to_string().contains("clean_exclude"), "{err}");
        assert!(err.to_string().contains("invalid"), "{err}");
    }

    /// A per-file clean failure is a failed clean row and the pass
    /// continues: an undeletable file inside a read-only directory fails
    /// its own row while a deletable sibling is still removed, and the pass
    /// returns `Ok`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_stuck_file_fails_its_clean_row_and_the_pass_continues() {
        use std::os::unix::fs::PermissionsExt;

        let out = tempfile::tempdir().unwrap();
        let deletable = write(out.path().join("gone.sav"), b"stale");
        let stuck_dir = out.path().join("locked");
        std::fs::create_dir_all(&stuck_dir).unwrap();
        let stuck = write(stuck_dir.join("stuck.sav"), b"stale");
        std::fs::set_permissions(&stuck_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let progress = RecordingProgress::default();
        let written = keep_set(&[out.path(), stuck_dir.as_path()]);
        let result = clean_output(
            out.path(),
            &written,
            keep_set(&[]),
            Path::new("/disjoint-input-root"),
            &options(&[], None),
            crate::util::ConflictPolicy::Error,
            false,
            &progress,
            &CancelToken::new(),
        )
        .await;

        std::fs::set_permissions(&stuck_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let rows = result.expect("a per-file failure never aborts the pass");
        let failed: Vec<&OrganizeRow> = rows
            .iter()
            .filter(|row| row.status == FileStatus::Failed)
            .collect();
        assert_eq!(failed.len(), 1, "{rows:?}");
        assert_eq!(failed[0].input, stuck);
        assert!(failed[0].detail.is_some(), "{:?}", failed[0]);
        assert!(stuck.exists(), "the undeletable file stays");
        assert!(!deletable.exists(), "the deletable sibling still goes");
        assert!(
            rows.iter()
                .any(|row| row.status == FileStatus::Ok
                    && row.detail.as_deref() == Some("deleted")),
            "{rows:?}"
        );
    }
    /// An output dir reached through a symlinked prefix: `base/out` is a
    /// link to `base/real`, so every spelling below goes through the link
    /// while the canonical form does not.
    #[cfg(unix)]
    fn linked_out() -> (tempfile::TempDir, PathBuf) {
        let base = tempfile::tempdir().unwrap();
        fs::create_dir_all(base.path().join("real")).unwrap();
        let out = base.path().join("out");
        std::os::unix::fs::symlink(base.path().join("real"), &out).unwrap();
        (base, out)
    }

    /// A backup dir spelled through a symlinked prefix and not created yet
    /// is resolved through its deepest existing ancestor: the second pass
    /// never re-collects what the first backed up, on every unix host.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_backup_dir_behind_a_symlinked_prefix_is_not_recollected() {
        let (_base, out) = linked_out();
        let kept = write(out.join("GBA").join("Kept.zip"), b"kept");
        let stale = write(out.join("GBA").join("Old.zip"), b"stale");
        let backup = out.join("GBA").join("backup");

        let mut written = BTreeSet::new();
        written.insert(out.join("GBA"));
        let run = || async {
            clean_output(
                &out,
                &written,
                keep_set(&[kept.as_path()]),
                Path::new("/disjoint-input-root"),
                &options(&[], Some(&backup)),
                ConflictPolicy::Error,
                false,
                &RecordingProgress::default(),
                &CancelToken::new(),
            )
            .await
            .unwrap()
        };
        let first = run().await;
        assert_eq!(clean_paths(&first), vec![stale.display().to_string()]);
        let second = run().await;
        assert!(second.is_empty(), "{:?}", clean_paths(&second));
        assert!(!backup.join("Old (1).zip").exists(), "not re-collected");
    }

    /// Stale playlists already inside a backup dir spelled through a
    /// symlinked prefix are skipped, on every unix host.
    #[cfg(unix)]
    #[tokio::test]
    async fn stale_playlists_behind_a_symlinked_backup_prefix_are_skipped() {
        let (_base, out) = linked_out();
        let backup = out.join("nested").join("backup");
        let kept = write(backup.join("Old.m3u"), b"old");
        let stale = write(out.join("G").join("New.m3u"), b"new");

        let rows = delete_stale_playlists(
            &out,
            &[kept.clone(), stale.clone()],
            keep_set(&[]),
            Path::new("/disjoint-input-root"),
            &options(&[], Some(&backup)),
            false,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(clean_paths(&rows), vec![stale.display().to_string()]);
        assert_eq!(fs::read(&kept).unwrap(), b"old");
    }

    /// A stale playlist reached through a symlinked dir into the input
    /// root survives with a disjoint root spelling: the entry's own
    /// location decides, not the spelling of the output tree.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_stale_playlist_reached_through_a_link_into_the_input_survives() {
        let base = tempfile::tempdir().unwrap();
        let input = base.path().join("in");
        let out = base.path().join("out");
        fs::create_dir_all(input.join("GameCube")).unwrap();
        fs::create_dir_all(&out).unwrap();
        std::os::unix::fs::symlink(input.join("GameCube"), out.join("GameCube")).unwrap();
        let real = write(input.join("GameCube").join("Old.m3u"), b"mine");
        let spelled = out.join("GameCube").join("Old.m3u");

        let rows = delete_stale_playlists(
            &out,
            std::slice::from_ref(&spelled),
            keep_set(&[]),
            &fs::canonicalize(&input).unwrap(),
            &options(&[], None),
            false,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert!(rows.is_empty(), "{:?}", clean_paths(&rows));
        assert!(real.exists());
    }

    /// A `--clean-backup` that is itself a symlink inside a written dir is
    /// never a clean candidate: the stale files behind it are still backed
    /// up through the link, and the link survives.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_backup_dir_is_not_a_candidate() {
        let base = tempfile::tempdir().unwrap();
        let out = base.path().join("out");
        let elsewhere = base.path().join("elsewhere");
        fs::create_dir_all(out.join("GC")).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        let backup = out.join("GC").join(".bak");
        std::os::unix::fs::symlink(&elsewhere, &backup).unwrap();
        let stale = write(out.join("GC").join("Old.zip"), b"stale");

        let mut written = BTreeSet::new();
        written.insert(out.join("GC"));
        let rows = clean_output(
            &out,
            &written,
            keep_set(&[]),
            Path::new("/disjoint-input-root"),
            &options(&[], Some(&backup)),
            ConflictPolicy::Error,
            false,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            clean_paths(&rows),
            vec![stale.display().to_string()],
            "{rows:?}"
        );
        assert_eq!(fs::read(elsewhere.join("Old.zip")).unwrap(), b"stale");
        assert!(
            fs::symlink_metadata(&backup).unwrap().is_symlink(),
            "the backup link itself survives"
        );
    }

    /// A stale relative link is backed up as a link that still names the
    /// same file from the flat backup dir: the relative text is resolved
    /// from the original link's directory.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_relative_symlink_is_backed_up_still_pointing_at_its_target() {
        let base = tempfile::tempdir().unwrap();
        let out = base.path().join("out");
        let real = write(base.path().join("lib").join("Old.gba"), b"rom");
        let link = out.join("GBA").join("Old.zip");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink("../../lib/Old.gba", &link).unwrap();
        // A backup dir at another depth: the link text unchanged would name
        // `out/lib/Old.gba` from there, not the real file.
        let backup = out.join("nest").join("backup");

        let mut written = BTreeSet::new();
        written.insert(out.join("GBA"));
        let rows = clean_output(
            &out,
            &written,
            keep_set(&[]),
            Path::new("/disjoint-input-root"),
            &options(&[], Some(&backup)),
            ConflictPolicy::Error,
            false,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        let moved = backup.join("Old.zip");
        assert!(fs::symlink_metadata(&moved).unwrap().is_symlink());
        assert_eq!(
            fs::read(&moved).unwrap(),
            b"rom",
            "the backup link resolves to the same file"
        );
        // The link text is folded onto the canonical parent: no `..` climbs
        // through the original output folder, so pruning that folder cannot
        // break the backup.
        let text = fs::read_link(&moved).unwrap();
        assert!(
            !text.components().any(|c| c == Component::ParentDir),
            "{text:?}"
        );
        fs::remove_dir_all(out.join("GBA")).unwrap();
        assert_eq!(fs::read(&moved).unwrap(), b"rom", "still resolves");
        assert!(real.exists());
    }

    /// A dry run reserves each planned backup slot: two stale files of one
    /// name preview the distinct targets the real run would name.
    #[tokio::test]
    async fn a_dry_run_previews_distinct_backup_slots_for_one_name() {
        let out = tempfile::tempdir().unwrap();
        let backup = out.path().join("backup");
        write(out.path().join("GBA").join("Old.zip"), b"a");
        write(out.path().join("SNES").join("Old.zip"), b"b");

        let mut written = BTreeSet::new();
        written.insert(out.path().join("GBA"));
        written.insert(out.path().join("SNES"));
        let rows = clean_output(
            out.path(),
            &written,
            keep_set(&[]),
            Path::new("/disjoint-input-root"),
            &options(&[], Some(&backup)),
            ConflictPolicy::Error,
            true,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();
        let details: Vec<&str> = rows
            .iter()
            .filter_map(|row| row.detail.as_deref())
            .collect();
        assert_eq!(details.len(), 2, "{details:?}");
        assert_ne!(details[0], details[1], "{details:?}");
        assert!(
            details.iter().any(|detail| detail.ends_with("Old (1).zip")),
            "{details:?}"
        );
    }
    /// A `NotFound` from the backup side (its parent is missing) while the
    /// source is still there is a failed backup, never "already gone": the
    /// stale file stays and the row reports why.
    #[test]
    fn a_missing_backup_parent_is_a_failure_not_already_gone() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(dir.path().join("Old.zip"), b"stale");
        let target = dir.path().join("no-such-dir").join("Old.zip");
        assert!(move_to_backup_held(&file, &target, None).is_err());
        assert!(file.exists(), "the file stays");

        // Only a source that is really gone counts as already gone.
        std::fs::remove_file(&file).unwrap();
        assert!(matches!(
            move_to_backup_held(&file, &target, None),
            Ok(BackupOutcome::AlreadyGone)
        ));
    }

    /// The same holds for a link candidate: a missing backup parent fails
    /// the backup and the link stays, and only a link that is really gone
    /// is already gone.
    #[cfg(unix)]
    #[test]
    fn a_missing_backup_parent_fails_a_link_backup_too() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("Old.zip");
        std::os::unix::fs::symlink("../elsewhere.gba", &link).unwrap();
        let target = dir.path().join("no-such-dir").join("Old.zip");
        assert!(move_to_backup_held(&link, &target, None).is_err());
        assert!(fs::symlink_metadata(&link).is_ok(), "the link stays");

        std::fs::remove_file(&link).unwrap();
        assert!(matches!(
            move_to_backup_held(&link, &target, None),
            Ok(BackupOutcome::AlreadyGone)
        ));
    }

    /// A link with an absolute text is renamed, not recreated: the same
    /// link (same inode) lands in the backup and the text stays correct.
    #[cfg(unix)]
    #[test]
    fn an_absolute_link_is_backed_up_by_rename() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().unwrap();
        let real = write(dir.path().join("real.gba"), b"rom");
        let link = dir.path().join("Old.zip");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let inode = fs::symlink_metadata(&link).unwrap().ino();
        let target = dir.path().join("bak").join("Old.zip");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        assert!(matches!(
            move_to_backup_held(&link, &target, None),
            Ok(BackupOutcome::BackedUp)
        ));
        assert_eq!(fs::symlink_metadata(&target).unwrap().ino(), inode);
        assert_eq!(fs::read_link(&target).unwrap(), real);
    }

    /// The backup dir's own entry is recognised under every spelling: by
    /// location, by the link's own inode when the location disagrees, and
    /// by case on a case-folding volume.
    #[cfg(unix)]
    #[test]
    fn a_backup_link_is_recognised_under_any_spelling() {
        let base = tempfile::tempdir().unwrap();
        let elsewhere = base.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        let backup = base.path().join("GC").join(".bak");
        fs::create_dir_all(backup.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &backup).unwrap();

        let spot = BackupSpot::of(&backup);
        let location = super::super::entry_location(&backup);
        assert!(spot.covers(&location));
        // A spelling whose computed location disagrees is still the same
        // link by its own inode.
        assert!(!spot.covers(Path::new("/somewhere/else/.bak")));
        assert!(spot.is_entry_link(&backup));
        // An unrelated file is not covered.
        let other = write(base.path().join("GC").join("Old.zip"), b"x");
        assert!(!spot.covers(&super::super::entry_location(&other)));
        assert!(!spot.is_entry_link(&other));

        // A case-variant spelling names the same entry on a folding volume.
        if base.path().join("gc").exists() {
            let variant = BackupSpot::of(&base.path().join("GC").join(".BAK"));
            assert!(variant.covers(&location));
        }
    }

    /// Dry-run backup slots are reserved once for the whole run: the clean
    /// pass and the stale-playlist pass share the options, so two files of
    /// one name preview distinct targets across passes.
    #[tokio::test]
    async fn dry_run_slots_are_shared_across_passes() {
        let out = tempfile::tempdir().unwrap();
        let backup = out.path().join("backup");
        write(out.path().join("GBA").join("Old.m3u"), b"a");
        let stale = write(out.path().join("GC").join("Old.m3u"), b"b");
        let options = options(&[], Some(&backup));

        let mut written = BTreeSet::new();
        written.insert(out.path().join("GBA"));
        let first = clean_output(
            out.path(),
            &written,
            keep_set(&[]),
            Path::new("/disjoint-input-root"),
            &options,
            ConflictPolicy::Error,
            true,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();
        let second = delete_stale_playlists(
            out.path(),
            std::slice::from_ref(&stale),
            keep_set(&[]),
            Path::new("/disjoint-input-root"),
            &options,
            true,
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();
        let first = first[0].detail.clone().unwrap();
        let second = second[0].detail.clone().unwrap();
        assert!(first.ends_with("backup/Old.m3u"), "{first}");
        assert!(second.ends_with("Old (1).m3u"), "{second}");
    }
}
