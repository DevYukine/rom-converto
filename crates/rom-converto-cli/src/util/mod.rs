pub mod http;

use crate::commands::ConflictPolicyArg;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use rom_converto_lib::util::{ConflictPolicy, ConflictResolution, ProgressReporter, resolve_conflict};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub enum WriteDecision {
    Write(PathBuf),
    Skip,
}

/// Map the lone `--force` shorthand onto a policy. `--force` and
/// `--on-conflict` are mutually exclusive in clap, so a set `force`
/// always means the policy is its default and overwrite is intended.
pub fn policy_of(on_conflict: ConflictPolicyArg, force: bool) -> ConflictPolicy {
    if force {
        ConflictPolicy::Overwrite
    } else {
        on_conflict.into()
    }
}

pub fn resolve_output(path: &Path, policy: ConflictPolicy) -> anyhow::Result<WriteDecision> {
    match resolve_conflict(path, policy)? {
        ConflictResolution::Write(p) => Ok(WriteDecision::Write(p)),
        ConflictResolution::Skip => Ok(WriteDecision::Skip),
    }
}

/// Directory outputs cannot auto-number, so `rename` is rejected here.
/// `skip` returns `Skip` when the directory already holds files, and
/// `error`/`overwrite` keep the original refuse/replace behavior.
pub fn resolve_output_dir(path: &Path, policy: ConflictPolicy) -> anyhow::Result<WriteDecision> {
    if !path.exists() {
        return Ok(WriteDecision::Write(path.to_path_buf()));
    }
    if path.is_file() {
        match policy {
            ConflictPolicy::Overwrite => return Ok(WriteDecision::Write(path.to_path_buf())),
            ConflictPolicy::Skip => return Ok(WriteDecision::Skip),
            _ => anyhow::bail!(
                "output path exists and is a file, use --on-conflict overwrite to replace it: {}",
                path.display()
            ),
        }
    }
    let non_empty = std::fs::read_dir(path)?.next().is_some();
    if !non_empty {
        return Ok(WriteDecision::Write(path.to_path_buf()));
    }
    match policy {
        ConflictPolicy::Overwrite => Ok(WriteDecision::Write(path.to_path_buf())),
        ConflictPolicy::Skip => Ok(WriteDecision::Skip),
        ConflictPolicy::Rename => anyhow::bail!(
            "rename is not supported for directory outputs, use --on-conflict overwrite/skip/error: {}",
            path.display()
        ),
        ConflictPolicy::Error => anyhow::bail!(
            "output directory is not empty, use --on-conflict overwrite to replace it: {}",
            path.display()
        ),
    }
}

pub fn ensure_input_exists(path: &std::path::Path) -> anyhow::Result<()> {
    if !path.exists() {
        anyhow::bail!("input not found: {}", path.display());
    }
    Ok(())
}

const PROGRESS_TEMPLATE: &str = "{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})";

/// Bridges the library's `ProgressReporter` trait to indicatif `ProgressBar`.
pub struct IndicatifProgress {
    mp: MultiProgress,
    bar: Mutex<Option<ProgressBar>>,
}

impl IndicatifProgress {
    pub fn new(mp: MultiProgress) -> Self {
        Self {
            mp,
            bar: Mutex::new(None),
        }
    }
}

impl ProgressReporter for IndicatifProgress {
    fn start(&self, total: u64, msg: &str) {
        let pg = self.mp.add(ProgressBar::new(total));
        let style = ProgressStyle::default_bar()
            .template(PROGRESS_TEMPLATE)
            .expect("valid progress template")
            .progress_chars("#>-");
        pg.set_style(style);
        pg.set_message(msg.to_string());
        *self.bar.lock().unwrap() = Some(pg);
    }

    fn inc(&self, delta: u64) {
        if let Some(bar) = self.bar.lock().unwrap().as_ref() {
            bar.inc(delta);
        }
    }

    fn finish(&self) {
        if let Some(bar) = self.bar.lock().unwrap().take() {
            bar.finish_and_clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn policy_of_force_maps_to_overwrite() {
        assert_eq!(
            policy_of(ConflictPolicyArg::Error, true),
            ConflictPolicy::Overwrite
        );
    }

    #[test]
    fn policy_of_uses_on_conflict_when_no_force() {
        assert_eq!(
            policy_of(ConflictPolicyArg::Skip, false),
            ConflictPolicy::Skip
        );
    }

    #[test]
    fn resolve_output_error_existing_bails() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.chd");
        std::fs::write(&path, b"x").unwrap();
        assert!(resolve_output(&path, ConflictPolicy::Error).is_err());
    }

    #[test]
    fn resolve_output_skip_existing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.chd");
        std::fs::write(&path, b"x").unwrap();
        assert!(matches!(
            resolve_output(&path, ConflictPolicy::Skip).unwrap(),
            WriteDecision::Skip
        ));
    }

    #[test]
    fn resolve_output_rename_existing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.chd");
        std::fs::write(&path, b"x").unwrap();
        let WriteDecision::Write(p) = resolve_output(&path, ConflictPolicy::Rename).unwrap() else {
            panic!("expected write");
        };
        assert_eq!(p, dir.path().join("game (1).chd"));
    }

    #[test]
    fn resolve_output_overwrite_existing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.chd");
        std::fs::write(&path, b"x").unwrap();
        let WriteDecision::Write(p) =
            resolve_output(&path, ConflictPolicy::Overwrite).unwrap()
        else {
            panic!("expected write");
        };
        assert_eq!(p, path);
    }

    #[test]
    fn resolve_output_dir_rejects_rename() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a"), b"x").unwrap();
        assert!(resolve_output_dir(dir.path(), ConflictPolicy::Rename).is_err());
    }
}
