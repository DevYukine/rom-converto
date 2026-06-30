use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};

const MAX_RENAME_SLOTS: u32 = 9999;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictPolicy {
    Error,
    Overwrite,
    Skip,
    Rename,
    OverwriteInvalid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConflictResolution {
    Write(PathBuf),
    Skip,
}

/// Decide what to do with `desired` under `policy` when the path may
/// already exist. The caller never writes until this returns `Write`,
/// so `Skip` and `Error` leave no partial output behind.
pub fn resolve_conflict(
    desired: &Path,
    policy: ConflictPolicy,
) -> std::io::Result<ConflictResolution> {
    if !desired.exists() {
        return Ok(ConflictResolution::Write(desired.to_path_buf()));
    }
    match policy {
        ConflictPolicy::Overwrite => Ok(ConflictResolution::Write(desired.to_path_buf())),
        ConflictPolicy::Skip => Ok(ConflictResolution::Skip),
        ConflictPolicy::Error => Err(Error::new(
            ErrorKind::AlreadyExists,
            format!(
                "output file already exists, use --on-conflict overwrite to replace it: {}",
                desired.display()
            ),
        )),
        ConflictPolicy::Rename => Ok(ConflictResolution::Write(first_free_slot(desired)?)),
        // The integrity check is async and format specific, so it cannot run
        // here. Default to keeping the existing output; the caller overrides
        // this to a rewrite when it can verify the output and finds it broken.
        ConflictPolicy::OverwriteInvalid => Ok(ConflictResolution::Skip),
    }
}

fn first_free_slot(desired: &Path) -> std::io::Result<PathBuf> {
    let stem = desired
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let ext_dot = desired
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let parent = desired.parent();

    for n in 1..=MAX_RENAME_SLOTS {
        let name = format!("{stem} ({n}){ext_dot}");
        let candidate = match parent {
            Some(dir) if !dir.as_os_str().is_empty() => dir.join(name),
            _ => PathBuf::from(name),
        };
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(Error::new(
        ErrorKind::AlreadyExists,
        format!(
            "no free renamed output slot for {} after {} attempts",
            desired.display(),
            MAX_RENAME_SLOTS
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn error_policy_passes_when_absent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.chd");
        let res = resolve_conflict(&path, ConflictPolicy::Error).unwrap();
        assert_eq!(res, ConflictResolution::Write(path));
    }

    #[test]
    fn error_policy_rejects_existing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.chd");
        std::fs::write(&path, b"x").unwrap();
        let err = resolve_conflict(&path, ConflictPolicy::Error).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
    }

    #[test]
    fn overwrite_policy_allows_existing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.chd");
        std::fs::write(&path, b"x").unwrap();
        let res = resolve_conflict(&path, ConflictPolicy::Overwrite).unwrap();
        assert_eq!(res, ConflictResolution::Write(path));
    }

    #[test]
    fn skip_policy_signals_skip_for_existing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.chd");
        std::fs::write(&path, b"x").unwrap();
        let res = resolve_conflict(&path, ConflictPolicy::Skip).unwrap();
        assert_eq!(res, ConflictResolution::Skip);
    }

    #[test]
    fn skip_policy_writes_when_absent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.chd");
        let res = resolve_conflict(&path, ConflictPolicy::Skip).unwrap();
        assert_eq!(res, ConflictResolution::Write(path));
    }

    #[test]
    fn overwrite_invalid_writes_when_absent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.chd");
        let res = resolve_conflict(&path, ConflictPolicy::OverwriteInvalid).unwrap();
        assert_eq!(res, ConflictResolution::Write(path));
    }

    #[test]
    fn overwrite_invalid_keeps_existing_until_caller_verifies() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.chd");
        std::fs::write(&path, b"x").unwrap();
        let res = resolve_conflict(&path, ConflictPolicy::OverwriteInvalid).unwrap();
        assert_eq!(res, ConflictResolution::Skip);
    }

    #[test]
    fn rename_returns_original_when_absent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.chd");
        let res = resolve_conflict(&path, ConflictPolicy::Rename).unwrap();
        assert_eq!(res, ConflictResolution::Write(path));
    }

    #[test]
    fn rename_first_free_slot_is_one() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.chd");
        std::fs::write(&path, b"x").unwrap();
        let res = resolve_conflict(&path, ConflictPolicy::Rename).unwrap();
        assert_eq!(
            res,
            ConflictResolution::Write(dir.path().join("game (1).chd"))
        );
    }

    #[test]
    fn rename_skips_taken_slots() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.chd");
        std::fs::write(&path, b"x").unwrap();
        std::fs::write(dir.path().join("game (1).chd"), b"x").unwrap();
        let res = resolve_conflict(&path, ConflictPolicy::Rename).unwrap();
        assert_eq!(
            res,
            ConflictResolution::Write(dir.path().join("game (2).chd"))
        );
    }

    #[test]
    fn rename_preserves_real_extension() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.tar.gz");
        std::fs::write(&path, b"x").unwrap();
        let res = resolve_conflict(&path, ConflictPolicy::Rename).unwrap();
        let ConflictResolution::Write(p) = res else {
            panic!("expected write");
        };
        assert_eq!(p.extension().and_then(|e| e.to_str()), Some("gz"));
        assert_eq!(p, dir.path().join("game.tar (1).gz"));
    }
}
