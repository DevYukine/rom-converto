//! Filesystem helpers: extension matching, OS junk file detection, and
//! free-space checks used by the disk-space preflight.

use std::path::{Path, PathBuf};

use super::CancelToken;

pub fn has_any_extension(path: &Path, exts: &[&str]) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| exts.iter().any(|want| e.eq_ignore_ascii_case(want)))
            .unwrap_or(false)
}

pub fn is_os_junk_file(name: &str) -> bool {
    name.starts_with("._")
        || matches!(
            name,
            ".DS_Store" | "Thumbs.db" | "desktop.ini" | ".localized"
        )
}

pub fn is_os_junk_dir(name: &str) -> bool {
    matches!(
        name,
        "@eaDir"
            | ".@__thumb"
            | ".Spotlight-V100"
            | ".Trashes"
            | ".fseventsd"
            | ".TemporaryItems"
            | ".AppleDouble"
            | ".AppleDB"
            | ".AppleDesktop"
            | "#recycle"
            | "@Recently-Snapshot"
            | "lost+found"
    ) || name.eq_ignore_ascii_case("$RECYCLE.BIN")
        || name.eq_ignore_ascii_case("RECYCLER")
        // Windows reserves this name on case-insensitive volumes.
        || name.eq_ignore_ascii_case("System Volume Information")
}

/// Recursive listing of files under `dir` whose extension matches any of
/// `exts` (case-insensitive), sorted for deterministic processing order.
///
/// `max_depth` counts directory levels below the scan root: files directly
/// in `dir` are depth 1. `None` descends without limit, `Some(1)` returns
/// only the top-level files (no descent), and `Some(N)` descends at most
/// `N` levels. Symlinked directories are not followed, so cycles cannot
/// cause infinite recursion.
pub fn collect_files_with_exts(
    dir: &Path,
    exts: &[&str],
    max_depth: Option<usize>,
) -> std::io::Result<Vec<PathBuf>> {
    collect_files_with_exts_cancellable(dir, exts, max_depth, &CancelToken::new())
}

pub fn collect_files_with_exts_cancellable(
    dir: &Path,
    exts: &[&str],
    max_depth: Option<usize>,
    cancel: &CancelToken,
) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 1usize)];
    while let Some((current, depth)) = stack.pop() {
        cancelled(cancel)?;
        for entry in std::fs::read_dir(&current)? {
            cancelled(cancel)?;
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_str().unwrap_or("");
            if file_type.is_dir() && !file_type.is_symlink() {
                if !is_os_junk_dir(name) && max_depth.is_none_or(|limit| depth < limit) {
                    stack.push((path, depth + 1));
                }
            } else if has_any_extension(&path, exts) && !is_os_junk_file(name) {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Recursive listing of every regular file under `dir`, sorted for
/// deterministic processing order. Unlike `collect_files_with_exts` this
/// applies no extension filter, so it suits format-agnostic operations.
///
/// `max_depth` follows the same convention as `collect_files_with_exts`:
/// files directly in `dir` are depth 1, `None` descends without limit, and
/// symlinked directories are not followed.
pub fn collect_all_files(dir: &Path, max_depth: Option<usize>) -> std::io::Result<Vec<PathBuf>> {
    collect_all_files_cancellable(dir, max_depth, &CancelToken::new())
}

pub fn collect_all_files_cancellable(
    dir: &Path,
    max_depth: Option<usize>,
    cancel: &CancelToken,
) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 1usize)];
    while let Some((current, depth)) = stack.pop() {
        cancelled(cancel)?;
        for entry in std::fs::read_dir(&current)? {
            cancelled(cancel)?;
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_str().unwrap_or("");
            if file_type.is_dir() && !file_type.is_symlink() {
                if !is_os_junk_dir(name) && max_depth.is_none_or(|limit| depth < limit) {
                    stack.push((path, depth + 1));
                }
            } else if file_type.is_file() && !is_os_junk_file(name) {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn cancelled(cancel: &CancelToken) -> std::io::Result<()> {
    if cancel.is_cancelled() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "cancelled",
        ));
    }
    Ok(())
}

pub const DEFAULT_SPACE_HEADROOM: u64 = 256 * 1024 * 1024;

pub fn available_space(path: &Path) -> std::io::Result<u64> {
    fs4::available_space(path)
}

/// Best-effort headroom check. `required` is a heuristic floor (the total
/// input size), not an exact prediction of output size, so this can only
/// catch a clearly too-full destination, not guarantee the write fits.
/// Returns the missing byte count when `available < required + headroom`.
pub fn space_shortfall(available: u64, required: u64, headroom: u64) -> Option<u64> {
    let needed = required.saturating_add(headroom);
    needed.checked_sub(available).filter(|missing| *missing > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_shortfall_returns_none_when_sufficient() {
        assert_eq!(space_shortfall(1000, 500, 100), None);
    }

    #[test]
    fn space_shortfall_returns_none_at_exact_threshold() {
        assert_eq!(space_shortfall(600, 500, 100), None);
    }

    #[test]
    fn space_shortfall_returns_missing_when_one_byte_short() {
        assert_eq!(space_shortfall(599, 500, 100), Some(1));
    }

    #[test]
    fn space_shortfall_returns_missing_when_insufficient() {
        assert_eq!(space_shortfall(100, 500, 100), Some(500));
    }

    #[test]
    fn space_shortfall_saturates_without_panicking() {
        assert_eq!(space_shortfall(0, u64::MAX, u64::MAX), Some(u64::MAX));
    }

    #[test]
    fn available_space_returns_positive_for_real_dir() {
        let dir = tempfile::tempdir().unwrap();
        let n = available_space(dir.path()).unwrap();
        assert!(n > 0);
    }

    #[test]
    fn has_any_extension_is_case_insensitive_and_requires_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let chd = dir.path().join("game.CHD");
        std::fs::write(&chd, b"x").unwrap();
        assert!(has_any_extension(&chd, &["chd"]));
        assert!(has_any_extension(&chd, &["cue", "chd"]));
        assert!(!has_any_extension(&chd, &["cue"]));

        let sub = dir.path().join("nested.chd");
        std::fs::create_dir(&sub).unwrap();
        assert!(!has_any_extension(&sub, &["chd"]));
    }

    #[test]
    fn collect_files_with_exts_matches_multiple_extensions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.iso"), b"x").unwrap();
        std::fs::write(dir.path().join("b.WBFS"), b"x").unwrap();
        std::fs::write(dir.path().join("c.rvz"), b"x").unwrap();
        std::fs::write(dir.path().join("d.txt"), b"x").unwrap();

        let found = collect_files_with_exts(dir.path(), &["iso", "wbfs"], None).unwrap();
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|p| p.extension().is_some()));

        let single = collect_files_with_exts(dir.path(), &["rvz"], None).unwrap();
        assert_eq!(single.len(), 1);
    }

    #[test]
    fn collect_files_with_exts_is_sorted() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["z.iso", "a.iso", "m.iso"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        let found = collect_files_with_exts(dir.path(), &["iso"], None).unwrap();
        let mut sorted = found.clone();
        sorted.sort();
        assert_eq!(found, sorted);
    }

    fn nested_iso_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.iso"), b"x").unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("a.iso"), b"x").unwrap();
        let deep = sub.join("deep");
        std::fs::create_dir(&deep).unwrap();
        std::fs::write(deep.join("c.iso"), b"x").unwrap();
        dir
    }

    #[test]
    fn collect_files_with_exts_recurses_into_subdirectories() {
        let dir = nested_iso_tree();
        let found = collect_files_with_exts(dir.path(), &["iso"], None).unwrap();
        assert_eq!(found.len(), 3);
    }

    #[test]
    fn collect_files_with_exts_max_depth_one_is_top_level_only() {
        let dir = nested_iso_tree();
        let found = collect_files_with_exts(dir.path(), &["iso"], Some(1)).unwrap();
        assert_eq!(found, vec![dir.path().join("b.iso")]);
    }

    #[test]
    fn collect_files_with_exts_max_depth_two_descends_one_level() {
        let dir = nested_iso_tree();
        let found = collect_files_with_exts(dir.path(), &["iso"], Some(2)).unwrap();
        assert_eq!(
            found,
            vec![
                dir.path().join("b.iso"),
                dir.path().join("sub").join("a.iso")
            ]
        );
    }

    #[test]
    fn collect_files_with_exts_sorted_across_levels() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("z.iso"), b"x").unwrap();
        let sub = dir.path().join("a");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("a.iso"), b"x").unwrap();
        std::fs::write(sub.join("m.iso"), b"x").unwrap();

        let found = collect_files_with_exts(dir.path(), &["iso"], None).unwrap();
        let mut sorted = found.clone();
        sorted.sort();
        assert_eq!(found, sorted);
    }

    #[test]
    fn collect_files_with_exts_subpath_under_root() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("a.iso"), b"x").unwrap();

        let found = collect_files_with_exts(dir.path(), &["iso"], None).unwrap();
        assert_eq!(found.len(), 1);
        let rel = found[0].strip_prefix(dir.path()).unwrap();
        assert_eq!(rel, Path::new("sub").join("a.iso"));
    }

    #[test]
    fn collect_files_with_exts_empty_subdirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("empty")).unwrap();
        std::fs::write(dir.path().join("b.iso"), b"x").unwrap();

        let found = collect_files_with_exts(dir.path(), &["iso"], None).unwrap();
        assert_eq!(found, vec![dir.path().join("b.iso")]);
    }

    #[cfg(unix)]
    #[test]
    fn collect_files_with_exts_skips_symlinked_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("a.iso"), b"x").unwrap();
        std::os::unix::fs::symlink(dir.path(), dir.path().join("loop")).unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("link")).unwrap();

        let found = collect_files_with_exts(dir.path(), &["iso"], None).unwrap();
        assert_eq!(found, vec![real.join("a.iso")]);
    }

    #[test]
    fn collect_all_files_ignores_extensions_and_recurses() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.iso"), b"x").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"x").unwrap();
        std::fs::write(dir.path().join("noext"), b"x").unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("c.bin"), b"x").unwrap();

        let found = collect_all_files(dir.path(), None).unwrap();
        assert_eq!(found.len(), 4);
        let mut sorted = found.clone();
        sorted.sort();
        assert_eq!(found, sorted);
    }

    #[test]
    fn collect_all_files_max_depth_one_is_top_level_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.bin"), b"x").unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("b.bin"), b"x").unwrap();

        let found = collect_all_files(dir.path(), Some(1)).unwrap();
        assert_eq!(found, vec![dir.path().join("a.bin")]);
    }

    #[test]
    fn is_os_junk_file_positives() {
        for name in [
            "._game.cia",
            "._",
            ".DS_Store",
            "Thumbs.db",
            "desktop.ini",
            ".localized",
        ] {
            assert!(is_os_junk_file(name), "{name} should be junk");
        }
    }

    #[test]
    fn is_os_junk_file_negatives() {
        for name in ["game.cia", ".hidden", "DS_Store", "thumbs.db.bak", ""] {
            assert!(!is_os_junk_file(name), "{name} should not be junk");
        }
    }

    #[test]
    fn is_os_junk_dir_positives() {
        for name in [
            "@eaDir",
            ".@__thumb",
            "$RECYCLE.BIN",
            "$recycle.bin",
            "RECYCLER",
            "recycler",
            "System Volume Information",
            "system volume information",
            ".Spotlight-V100",
            ".Trashes",
            ".fseventsd",
            ".AppleDouble",
            "#recycle",
            "lost+found",
        ] {
            assert!(is_os_junk_dir(name), "{name} should be junk");
        }
    }

    #[test]
    fn is_os_junk_dir_negatives() {
        for name in [".hidden", "sub", "@media", "System", "recycle", ""] {
            assert!(!is_os_junk_dir(name), "{name} should not be junk");
        }
    }

    #[test]
    fn collect_files_with_exts_skips_appledouble_and_junk() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("game.cia"), b"x").unwrap();
        std::fs::write(root.join("._game.cia"), b"x").unwrap();
        std::fs::write(root.join(".DS_Store"), b"x").unwrap();

        let dotdir = root.join(".dotdir");
        std::fs::create_dir(&dotdir).unwrap();
        std::fs::write(dotdir.join("game2.cia"), b"x").unwrap();

        let eadir = root.join("@eaDir");
        std::fs::create_dir(&eadir).unwrap();
        std::fs::write(eadir.join("junk.cia"), b"x").unwrap();

        let found = collect_files_with_exts(root, &["cia"], None).unwrap();
        assert_eq!(
            found,
            vec![
                root.join(".dotdir").join("game2.cia"),
                root.join("game.cia")
            ]
        );
    }

    #[test]
    fn collect_all_files_skips_junk_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("rom.bin"), b"x").unwrap();
        std::fs::write(root.join("._rom.bin"), b"x").unwrap();
        std::fs::write(root.join(".DS_Store"), b"x").unwrap();

        let eadir = root.join("@eaDir");
        std::fs::create_dir(&eadir).unwrap();
        std::fs::write(eadir.join("junk.bin"), b"x").unwrap();

        let found = collect_all_files(root, None).unwrap();
        assert_eq!(found, vec![root.join("rom.bin")]);
    }
}
