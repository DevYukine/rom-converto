use std::path::{Path, PathBuf};

pub fn has_any_extension(path: &Path, exts: &[&str]) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| exts.iter().any(|want| e.eq_ignore_ascii_case(want)))
            .unwrap_or(false)
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
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 1usize)];
    while let Some((current, depth)) = stack.pop() {
        for entry in std::fs::read_dir(&current)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();
            if file_type.is_dir() && !file_type.is_symlink() {
                if max_depth.is_none_or(|limit| depth < limit) {
                    stack.push((path, depth + 1));
                }
            } else if has_any_extension(&path, exts) {
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
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 1usize)];
    while let Some((current, depth)) = stack.pop() {
        for entry in std::fs::read_dir(&current)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();
            if file_type.is_dir() && !file_type.is_symlink() {
                if max_depth.is_none_or(|limit| depth < limit) {
                    stack.push((path, depth + 1));
                }
            } else if file_type.is_file() {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            vec![dir.path().join("b.iso"), dir.path().join("sub").join("a.iso")]
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
}
