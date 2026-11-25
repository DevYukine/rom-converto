//! Writes extracted filesystem trees. Each target is written to a scratch
//! dir beside it, and every target is published only once all of them
//! were written; a failure or cancel removes the scratch dirs, rolls back
//! what was published, and removes the parent dirs the run created.

use std::fs::{self, File};
use std::io::{self, Cursor, Write};
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::nintendo::nx::constants::{PFS0_ENTRY_SIZE, PFS0_HEADER_SIZE, PFS0_MAGIC};
use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::models::pfs0::Pfs0;
use crate::nintendo::nx::romfs::{MAX_TABLE_SIZE, Tree, TreeFile};
use crate::nintendo::nx::walker::NcaInput;
use crate::util::bytes::u32_le;
use crate::util::{
    CancelToken, Cancelled, OutputExists, ProgressReporter, extent_end, is_safe_dirent_name,
};

const CHUNK: u64 = 4 << 20;

/// Lists the files of the PFS0 (an ExeFS) of `size` bytes at `offset`
/// in `storage`.
///
/// # Errors
/// Fails with [`NxError::InvalidPfs0`] on a header larger than the PFS0
/// or 256 MiB, a file outside the PFS0, or file sizes that add up to
/// more than the PFS0; with [`NxError::UnsafeEntryName`] for a name that
/// cannot be a file name.
pub(crate) fn list_pfs0(storage: &dyn NcaInput, offset: u64, size: u64) -> NxResult<Tree> {
    if size < PFS0_HEADER_SIZE as u64 {
        return Err(NxError::InvalidPfs0("smaller than its header"));
    }
    let mut head = [0u8; PFS0_HEADER_SIZE];
    storage.read_exact_at(&mut head, offset)?;
    if head[..4] != PFS0_MAGIC {
        return Err(NxError::Pfs0BadMagic);
    }
    let header_len = PFS0_HEADER_SIZE as u64
        + u64::from(u32_le(&head, 4)) * PFS0_ENTRY_SIZE as u64
        + u64::from(u32_le(&head, 8));
    if header_len > size.min(MAX_TABLE_SIZE) {
        return Err(NxError::InvalidPfs0(
            "header is larger than the filesystem or 256 MiB",
        ));
    }
    let mut raw = vec![0u8; header_len as usize];
    storage.read_exact_at(&mut raw, offset)?;
    let pfs0 = Pfs0::read(&mut Cursor::new(&raw))?;
    // Extents may overlap or repeat, but files holding more bytes than
    // the PFS0 would let a small input write without bound.
    let mut data_left = size;
    let files = pfs0
        .files
        .into_iter()
        .map(|file| {
            if !is_safe_dirent_name(&file.name) {
                return Err(NxError::UnsafeEntryName(file.name));
            }
            let start = pfs0
                .data_section_offset
                .checked_add(file.data_offset)
                .filter(|start| extent_end(*start, file.size, size).is_some())
                .ok_or(NxError::InvalidPfs0(
                    "file data lies outside the filesystem",
                ))?;
            data_left = data_left
                .checked_sub(file.size)
                .ok_or(NxError::InvalidPfs0(
                    "file sizes add up to more than the filesystem",
                ))?;
            Ok(TreeFile {
                path: PathBuf::from(file.name),
                offset: offset + start,
                size: file.size,
            })
        })
        .collect::<NxResult<_>>()?;
    Ok(Tree {
        dirs: Vec::new(),
        files,
    })
}

/// Writes target `i` of `targets` from the storage and tree `open(i)`
/// returns, then publishes every target. Each target is a `(dir,
/// overwrite)` pair: `overwrite` replaces an existing dir, otherwise
/// only an empty existing dir is replaced. Returns the file bytes
/// written per target.
///
/// # Errors
/// Fails with an [`OutputExists`] refusal before writing anything when a
/// target holds files and its `overwrite` is off; with
/// [`NxError::DuplicateEntryName`] when two entries resolve to one path;
/// with [`NxError::Cancelled`] when `cancel` fires before publishing
/// starts; with [`NxError::RestoreFailed`] when rolling back cannot put
/// a replaced target back, whose contents then stay in the kept scratch
/// dir the error names.
pub(crate) fn write_targets(
    targets: &[(&Path, bool)],
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
    mut open: impl FnMut(usize) -> NxResult<(Box<dyn NcaInput>, Tree)>,
) -> NxResult<Vec<u64>> {
    for &(target, overwrite) in targets {
        check_target(target, overwrite)?;
    }
    let mut created = Vec::new();
    let result = write_and_publish(targets, progress, cancel, &mut open, &mut created);
    if result.is_err() {
        // Leaves a parent that still holds something, such as a kept
        // scratch dir.
        for dir in created.iter().rev() {
            let _ = fs::remove_dir(dir);
        }
    }
    result
}

fn write_and_publish(
    targets: &[(&Path, bool)],
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
    open: &mut impl FnMut(usize) -> NxResult<(Box<dyn NcaInput>, Tree)>,
    created: &mut Vec<PathBuf>,
) -> NxResult<Vec<u64>> {
    let mut scratches = Vec::with_capacity(targets.len());
    let mut written = Vec::with_capacity(targets.len());
    for (i, &(target, _)) in targets.iter().enumerate() {
        let (storage, tree) = open(i)?;
        let parent = target
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        create_dirs(parent, created)?;
        let scratch = Scratch::new(target, parent)?;
        written.push(write_tree(
            &*storage,
            &tree,
            &scratch.new_tree(),
            progress,
            cancel,
        )?);
        scratches.push(scratch);
    }
    // Publishing runs to the end once it starts, so this is the last
    // point where a cancel takes effect.
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    for i in 0..scratches.len() {
        let Err(mut err) = scratches[i].publish(targets[i].1) else {
            continue;
        };
        for published in scratches[..i].iter_mut().rev() {
            if let Err(kept) = published.unpublish(progress) {
                // The first kept backup becomes the error; it says where
                // the user's files are, which matters more than why
                // publishing failed.
                if matches!(err, NxError::RestoreFailed { .. }) {
                    progress.warn(&kept.to_string());
                } else {
                    err = kept;
                }
            }
        }
        return Err(err);
    }
    Ok(written)
}

/// Refuses an existing `target` unless `overwrite` is set or it is an
/// empty directory.
fn check_target(target: &Path, overwrite: bool) -> NxResult<()> {
    let empty_dir = || fs::read_dir(target).is_ok_and(|mut entries| entries.next().is_none());
    if overwrite || target.symlink_metadata().is_err() || empty_dir() {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        OutputExists(target.to_path_buf()),
    )
    .into())
}

/// Creates `dir` and its missing ancestors, recording each one it made.
fn create_dirs(dir: &Path, created: &mut Vec<PathBuf>) -> io::Result<()> {
    let missing: Vec<&Path> = dir
        .ancestors()
        .take_while(|d| !d.as_os_str().is_empty() && !d.exists())
        .collect();
    for dir in missing.into_iter().rev() {
        match fs::create_dir(dir) {
            Ok(()) => created.push(dir.to_path_buf()),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn write_tree(
    storage: &dyn NcaInput,
    tree: &Tree,
    root: &Path,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> NxResult<u64> {
    fs::create_dir(root)?;
    for dir in &tree.dirs {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        fs::create_dir(root.join(dir)).map_err(|err| name_clash(err, dir))?;
    }
    let mut files: Vec<&TreeFile> = tree.files.iter().collect();
    // Reading in data order keeps a solid NCZ decoder moving forward.
    files.sort_by_key(|file| file.offset);
    let largest = files.iter().map(|file| file.size).max().unwrap_or(0);
    let mut buf = vec![0u8; largest.min(CHUNK) as usize];
    let mut written = 0;
    for file in files {
        let mut out =
            File::create_new(root.join(&file.path)).map_err(|err| name_clash(err, &file.path))?;
        let mut pos = 0;
        loop {
            if cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            let take = (file.size - pos).min(CHUNK) as usize;
            if take == 0 {
                break;
            }
            storage.read_exact_at(&mut buf[..take], file.offset + pos)?;
            out.write_all(&buf[..take])?;
            progress.inc(take as u64);
            pos += take as u64;
        }
        written += file.size;
    }
    Ok(written)
}

fn name_clash(err: io::Error, path: &Path) -> NxError {
    if err.kind() == io::ErrorKind::AlreadyExists {
        NxError::DuplicateEntryName(path.to_path_buf())
    } else {
        err.into()
    }
}

/// A target's scratch dir beside it: `new` holds the written tree and
/// `old` the target it replaced. Dropping it deletes both, which removes
/// a partial tree after a failure and the replaced target after success,
/// unless a failed restore kept it.
struct Scratch {
    dir: TempDir,
    target: PathBuf,
    /// Publishing removed an empty dir at the target.
    removed_empty: bool,
}

impl Scratch {
    fn new(target: &Path, parent: &Path) -> io::Result<Self> {
        let name = target.file_name().unwrap_or_default().to_string_lossy();
        let dir = tempfile::Builder::new()
            .prefix(&format!(".{name}."))
            .suffix(".tmp")
            .tempdir_in(parent)?;
        Ok(Self {
            dir,
            target: target.to_path_buf(),
            removed_empty: false,
        })
    }

    fn new_tree(&self) -> PathBuf {
        self.dir.path().join("new")
    }

    fn old_tree(&self) -> PathBuf {
        self.dir.path().join("old")
    }

    fn publish(&mut self, overwrite: bool) -> NxResult<()> {
        if self.target.symlink_metadata().is_ok() {
            check_target(&self.target, overwrite)?;
            if overwrite {
                fs::rename(&self.target, self.old_tree())?;
            } else {
                // `check_target` found it empty; this fails if another
                // writer filled it since.
                fs::remove_dir(&self.target)?;
                self.removed_empty = true;
            }
        }
        if let Err(err) = fs::rename(self.new_tree(), &self.target) {
            self.restore_old()?;
            return Err(err.into());
        }
        Ok(())
    }

    /// Moves the published tree back into the scratch dir and restores
    /// the target it replaced. When a target that did not exist before
    /// cannot be moved back, it stays published with a warning.
    fn unpublish(&mut self, progress: &dyn ProgressReporter) -> NxResult<()> {
        match fs::rename(&self.target, self.new_tree()) {
            Ok(()) => self.restore_old(),
            Err(err) if self.old_tree().symlink_metadata().is_ok() => self.keep_backup(err),
            Err(err) => {
                progress.warn(&format!(
                    "Could not roll back {} ({err}); it still holds the extracted files",
                    self.target.display()
                ));
                Ok(())
            }
        }
    }

    /// Puts back the target that publishing moved aside or removed.
    fn restore_old(&mut self) -> NxResult<()> {
        let old = self.old_tree();
        if old.symlink_metadata().is_ok() {
            if let Err(err) = fs::rename(&old, &self.target) {
                return self.keep_backup(err);
            }
        } else if self.removed_empty {
            // An empty dir holds nothing to lose, so this stays best effort.
            let _ = fs::create_dir(&self.target);
        }
        Ok(())
    }

    /// Keeps the scratch dir when the target a publish replaced cannot be
    /// put back, so the original survives in `old`.
    fn keep_backup(&mut self, source: io::Error) -> NxResult<()> {
        self.dir.disable_cleanup(true);
        Err(NxError::RestoreFailed {
            target: self.target.clone(),
            backup: self.old_tree(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::nx::romfs::walk_romfs;
    use crate::nintendo::nx::test_fixtures::{MemInput, build_test_nsp, noise, romfs_image};
    use crate::util::NoProgress;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// Records warnings.
    #[derive(Default)]
    struct Warnings(Mutex<Vec<String>>);

    impl ProgressReporter for Warnings {
        fn start(&self, _total: u64, _msg: &str) {}

        fn inc(&self, _delta: u64) {}

        fn finish(&self) {}

        fn warn(&self, message: &str) {
            self.0.lock().unwrap().push(message.to_string());
        }
    }

    /// Every file under `root` by relative path, plus every directory.
    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        let mut out = BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let rel = path.strip_prefix(root).unwrap().to_path_buf();
                if path.is_dir() {
                    out.insert(rel, None);
                    pending.push(path);
                } else {
                    out.insert(rel, Some(fs::read(&path).unwrap()));
                }
            }
        }
        out
    }

    fn pfs0_storage(files: &[(&str, Vec<u8>)]) -> (MemInput, u64) {
        let owned: Vec<(String, Vec<u8>)> = files
            .iter()
            .map(|(n, d)| (n.to_string(), d.clone()))
            .collect();
        let pfs0 = build_test_nsp(&owned);
        let len = pfs0.len() as u64;
        (MemInput(pfs0), len)
    }

    type Files = Vec<(&'static str, Vec<u8>)>;

    /// Writes one ExeFS-style target per `(dir, files)` pair.
    fn write(
        targets: &[(&Path, Files)],
        overwrite: bool,
        cancel: &CancelToken,
    ) -> NxResult<Vec<u64>> {
        let dirs: Vec<(&Path, bool)> = targets.iter().map(|(d, _)| (*d, overwrite)).collect();
        write_targets(&dirs, &NoProgress, cancel, |i| {
            let (storage, len) = pfs0_storage(&targets[i].1);
            let tree = list_pfs0(&storage, 0, len)?;
            Ok((Box::new(storage), tree))
        })
    }

    fn files(seed: u32) -> Files {
        vec![
            ("main", noise(0x1234, seed)),
            ("main.npdm", noise(0x40, seed + 1)),
        ]
    }

    #[test]
    fn writes_pfs0_files_and_creates_parents() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("out/0100/exefs");
        let written = write(&[(&target, files(1))], false, &CancelToken::new()).unwrap();

        assert_eq!(written, [0x1234 + 0x40]);
        let expected: BTreeMap<PathBuf, Option<Vec<u8>>> = files(1)
            .into_iter()
            .map(|(n, d)| (PathBuf::from(n), Some(d)))
            .collect();
        assert_eq!(snapshot(&target), expected);
        assert_eq!(snapshot(&tmp.path().join("out/0100")).len(), 3);
    }

    #[test]
    fn unsafe_and_duplicate_exefs_names_fail() {
        for name in ["..", "a/b", ""] {
            let (storage, len) = pfs0_storage(&[(name, vec![1])]);
            assert!(
                matches!(
                    list_pfs0(&storage, 0, len),
                    Err(NxError::UnsafeEntryName(_))
                ),
                "{name:?}"
            );
        }
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("exefs");
        let dup = vec![("main", vec![1]), ("main", vec![2])];
        let err = write(&[(&target, dup)], false, &CancelToken::new()).unwrap_err();
        assert!(matches!(err, NxError::DuplicateEntryName(_)));
        assert!(snapshot(tmp.path()).is_empty());
    }

    #[test]
    fn cancel_leaves_no_scratch_target_or_new_parents() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("a/b/exefs");
        let cancel = CancelToken::new();
        cancel.cancel();

        let err = write(&[(&target, files(2))], false, &cancel).unwrap_err();

        assert!(matches!(err, NxError::Cancelled(_)));
        assert!(snapshot(tmp.path()).is_empty());
    }

    #[test]
    fn cancel_after_the_last_write_publishes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("t1/exefs");
        let second = tmp.path().join("t2/exefs");
        let cancel = CancelToken::new();

        let targets = [(first.as_path(), false), (second.as_path(), false)];
        let err = write_targets(&targets, &NoProgress, &cancel, |i| {
            let members = if i == 0 { files(6) } else { Vec::new() };
            let (storage, len) = pfs0_storage(&members);
            let tree = list_pfs0(&storage, 0, len)?;
            if i == 1 {
                // The last tree is empty, so only the check before
                // publishing sees this cancel.
                cancel.cancel();
            }
            Ok((Box::new(storage), tree))
        })
        .unwrap_err();

        assert!(matches!(err, NxError::Cancelled(_)));
        assert!(snapshot(tmp.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn failed_restore_keeps_the_backup_and_names_it() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("out");
        let replaced = parent.join("exefs");
        let created = parent.join("romfs");
        fs::create_dir_all(&replaced).unwrap();
        fs::write(replaced.join("mine"), b"keep").unwrap();
        let mut scratches = [&replaced, &created].map(|target| {
            let scratch = Scratch::new(target, &parent).unwrap();
            fs::create_dir(scratch.new_tree()).unwrap();
            scratch
        });
        scratches[0].publish(true).unwrap();
        scratches[1].publish(false).unwrap();
        let mode = |mode| fs::set_permissions(&parent, fs::Permissions::from_mode(mode));
        mode(0o555).unwrap();
        // Root ignores directory permissions, so nothing can fail here.
        if fs::create_dir(parent.join("probe")).is_ok() {
            mode(0o755).unwrap();
            return;
        }

        let progress = Warnings::default();
        let kept = scratches[0].unpublish(&progress);
        let left = scratches[1].unpublish(&progress);
        mode(0o755).unwrap();

        let Err(NxError::RestoreFailed { backup, .. }) = kept else {
            panic!("{kept:?}");
        };
        drop(scratches);
        assert_eq!(fs::read(backup.join("mine")).unwrap(), b"keep");
        assert!(left.is_ok());
        let warnings = progress.0.lock().unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains(&created.display().to_string()),
            "{warnings:?}"
        );
    }

    #[test]
    fn populated_target_needs_overwrite_and_is_replaced_with_it() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("exefs");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("stale"), b"old").unwrap();
        let before = snapshot(tmp.path());

        let err = write(&[(&target, files(3))], false, &CancelToken::new()).unwrap_err();
        assert!(matches!(err, NxError::IoError(ref e) if e.kind() == io::ErrorKind::AlreadyExists));
        assert_eq!(snapshot(tmp.path()), before);

        write(&[(&target, files(3))], true, &CancelToken::new()).unwrap();
        assert_eq!(snapshot(&target).len(), 2);
        assert!(!target.join("stale").exists());
        assert_eq!(snapshot(tmp.path()).len(), 3);
    }

    #[test]
    fn empty_target_dir_is_replaced_without_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("exefs");
        fs::create_dir(&target).unwrap();
        write(&[(&target, files(4))], false, &CancelToken::new()).unwrap();
        assert_eq!(snapshot(&target).len(), 2);
    }

    #[test]
    fn failed_publish_restores_replaced_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let populated = tmp.path().join("t1/exefs");
        let empty = tmp.path().join("t2/exefs");
        let taken = tmp.path().join("t3/exefs");
        fs::create_dir_all(&populated).unwrap();
        fs::write(populated.join("mine"), b"keep").unwrap();
        fs::create_dir_all(&empty).unwrap();
        fs::create_dir_all(taken.parent().unwrap()).unwrap();

        let targets = [
            (populated.as_path(), true),
            (empty.as_path(), false),
            (taken.as_path(), false),
        ];
        let err = write_targets(&targets, &NoProgress, &CancelToken::new(), |i| {
            if i == 2 {
                // Another writer fills the last target after the up-front
                // check, so its publish fails after the populated target
                // was moved aside and the empty one removed.
                fs::create_dir(&taken).unwrap();
                fs::write(taken.join("theirs"), b"x").unwrap();
            }
            let (storage, len) = pfs0_storage(&files(5));
            let tree = list_pfs0(&storage, 0, len)?;
            Ok((Box::new(storage), tree))
        })
        .unwrap_err();

        assert!(matches!(err, NxError::IoError(ref e) if e.kind() == io::ErrorKind::AlreadyExists));
        let expected: BTreeMap<PathBuf, Option<Vec<u8>>> = [
            ("t1", None),
            ("t1/exefs", None),
            ("t1/exefs/mine", Some(b"keep".to_vec())),
            ("t2", None),
            ("t2/exefs", None),
            ("t3", None),
            ("t3/exefs", None),
            ("t3/exefs/theirs", Some(b"x".to_vec())),
        ]
        .into_iter()
        .map(|(p, d)| (PathBuf::from(p), d))
        .collect();
        assert_eq!(snapshot(tmp.path()), expected);
    }

    /// Rewrites PFS0 entry `i` of `pfs0` to `size` bytes at `data_offset`.
    fn set_entry(pfs0: &mut [u8], i: usize, data_offset: u64, size: u64) {
        let at = PFS0_HEADER_SIZE + i * PFS0_ENTRY_SIZE;
        pfs0[at..at + 8].copy_from_slice(&data_offset.to_le_bytes());
        pfs0[at + 8..at + 16].copy_from_slice(&size.to_le_bytes());
    }

    #[test]
    fn pfs0_files_outside_or_larger_than_the_filesystem_fail() {
        let (MemInput(pfs0), len) = pfs0_storage(&[("a", vec![1; 0x100]), ("b", vec![2; 0x100])]);
        let list = |pfs0: Vec<u8>| list_pfs0(&MemInput(pfs0), 0, len);
        assert_eq!(list(pfs0.clone()).unwrap().files.len(), 2);

        let mut outside = pfs0.clone();
        set_entry(&mut outside, 1, 0x101, 0x100);
        assert!(matches!(list(outside), Err(NxError::InvalidPfs0(_))));

        // Both files fit and overlap, but together hold more than the PFS0.
        let mut repeated = pfs0;
        set_entry(&mut repeated, 0, 0, 0x200);
        set_entry(&mut repeated, 1, 0, 0x200);
        assert!(matches!(list(repeated), Err(NxError::InvalidPfs0(_))));
    }

    #[test]
    fn duplicate_romfs_names_fail() {
        for files in [
            vec![("a", vec![1]), ("a", vec![2])],
            vec![("x", vec![1]), ("x/y", vec![2])],
        ] {
            let (image, _) = romfs_image(&files);
            let len = image.len() as u64;
            let tmp = tempfile::tempdir().unwrap();
            let target = tmp.path().join("romfs");
            let err = write_targets(
                &[(&target, false)],
                &NoProgress,
                &CancelToken::new(),
                |_| {
                    let storage = MemInput(image.clone());
                    let tree = walk_romfs(&storage, 0, len)?;
                    Ok((Box::new(storage), tree))
                },
            )
            .unwrap_err();

            assert!(matches!(err, NxError::DuplicateEntryName(_)), "{files:?}");
            assert!(snapshot(tmp.path()).is_empty());
        }
    }
}
