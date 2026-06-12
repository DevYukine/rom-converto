use std::path::{Path, PathBuf};

pub fn has_any_extension(path: &Path, exts: &[&str]) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| exts.iter().any(|want| e.eq_ignore_ascii_case(want)))
            .unwrap_or(false)
}

/// Top-level (non-recursive) listing of files in `dir` whose extension
/// matches any of `exts` (case-insensitive), sorted for deterministic
/// processing order.
pub fn collect_files_with_exts(dir: &Path, exts: &[&str]) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if has_any_extension(&path, exts) {
            out.push(path);
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

        let found = collect_files_with_exts(dir.path(), &["iso", "wbfs"]).unwrap();
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|p| p.extension().is_some()));

        let single = collect_files_with_exts(dir.path(), &["rvz"]).unwrap();
        assert_eq!(single.len(), 1);
    }

    #[test]
    fn collect_files_with_exts_is_sorted() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["z.iso", "a.iso", "m.iso"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        let found = collect_files_with_exts(dir.path(), &["iso"]).unwrap();
        let mut sorted = found.clone();
        sorted.sort();
        assert_eq!(found, sorted);
    }
}
