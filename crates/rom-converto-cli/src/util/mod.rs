pub mod http;

use crate::commands::ConflictPolicyArg;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use rom_converto_lib::info::{InfoOptions, read_info};
use rom_converto_lib::util::{
    ConflictPolicy, ConflictResolution, ProgressReporter, TemplateTokens, apply_template,
    place_in_dir_mirrored, resolve_conflict,
};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Resolve an `--output-template` to a concrete output path joined under
/// `base_dir`. Metadata is read best-effort: a failed or key-less read
/// degrades the identity tokens to the input basename rather than aborting
/// the conversion. A malformed template (traversal, empty result) still
/// surfaces as an error since that is a user mistake worth reporting.
pub fn templated_output(
    template: &str,
    input: &Path,
    base_dir: Option<&Path>,
    output_ext: &str,
    keys_path: Option<&Path>,
    dry_run: bool,
) -> anyhow::Result<PathBuf> {
    let info = read_info(
        input,
        &InfoOptions {
            keys_path: keys_path.map(Path::to_path_buf),
            parent_path: None,
        },
    )
    .map_err(|e| log::debug!("metadata unavailable for {}: {e}", input.display()))
    .ok();
    let tokens = TemplateTokens::new(info.as_ref(), input, output_ext);
    let rel = apply_template(template, &tokens)?;
    let base = base_dir.unwrap_or_else(|| Path::new("."));
    let joined = base.join(rel);
    if !dry_run && let Some(parent) = joined.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(joined)
}

/// Per-file output path for a recursive batch. With a template it resolves
/// against the file's metadata and joins under `output_dir`; without one it
/// mirrors the input subtree via `place_in_dir_mirrored`, preserving the
/// existing recursive behavior unchanged.
#[allow(clippy::too_many_arguments)]
pub fn batch_output(
    input: &Path,
    derived: &Path,
    input_dir: &Path,
    output_dir: Option<&Path>,
    output_template: Option<&str>,
    output_ext: &str,
    keys_path: Option<&Path>,
    dry_run: bool,
) -> anyhow::Result<PathBuf> {
    match output_template {
        Some(tmpl) => templated_output(tmpl, input, output_dir, output_ext, keys_path, dry_run),
        None => Ok(place_in_dir_mirrored(derived, input_dir, output_dir)),
    }
}

pub enum WriteDecision {
    Write(PathBuf),
    Skip,
}

pub use rom_converto_lib::util::{OutputVerify, VerifyOutcome, verify_existing_output};

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

/// Resolves the effective conflict policy for commands that read a
/// config/preset fallback. `--force` still wins, then an explicit
/// `--on-conflict`, then the config-provided `fallback`. An unset
/// `--on-conflict` (None) must not clobber the fallback.
pub fn resolve_policy(
    on_conflict: Option<ConflictPolicyArg>,
    force: bool,
    fallback: ConflictPolicy,
) -> ConflictPolicy {
    if force {
        ConflictPolicy::Overwrite
    } else {
        on_conflict.map(Into::into).unwrap_or(fallback)
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
            ConflictPolicy::Skip | ConflictPolicy::OverwriteInvalid => {
                return Ok(WriteDecision::Skip);
            }
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
        ConflictPolicy::Skip | ConflictPolicy::OverwriteInvalid => Ok(WriteDecision::Skip),
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

    fn set_phase(&self, label: &str) {
        if let Some(bar) = self.bar.lock().unwrap().as_ref() {
            bar.set_message(label.to_string());
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
    fn resolve_policy_flag_wins() {
        assert_eq!(
            resolve_policy(Some(ConflictPolicyArg::Skip), false, ConflictPolicy::Error),
            ConflictPolicy::Skip
        );
    }

    #[test]
    fn resolve_policy_force_wins() {
        assert_eq!(
            resolve_policy(None, true, ConflictPolicy::Skip),
            ConflictPolicy::Overwrite
        );
    }

    #[test]
    fn resolve_policy_falls_back() {
        assert_eq!(
            resolve_policy(None, false, ConflictPolicy::Skip),
            ConflictPolicy::Skip
        );
    }

    #[test]
    fn resolve_policy_builtin_when_no_fallback() {
        assert_eq!(
            resolve_policy(None, false, ConflictPolicy::Error),
            ConflictPolicy::Error
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
        let WriteDecision::Write(p) = resolve_output(&path, ConflictPolicy::Overwrite).unwrap()
        else {
            panic!("expected write");
        };
        assert_eq!(p, path);
    }

    #[test]
    fn resolve_output_overwrite_invalid_keeps_existing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.cso");
        std::fs::write(&path, b"x").unwrap();
        assert!(matches!(
            resolve_output(&path, ConflictPolicy::OverwriteInvalid).unwrap(),
            WriteDecision::Skip
        ));
    }

    #[test]
    fn resolve_output_overwrite_invalid_writes_when_absent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.cso");
        let WriteDecision::Write(p) =
            resolve_output(&path, ConflictPolicy::OverwriteInvalid).unwrap()
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

    #[test]
    fn dry_run_flag_parses() {
        use crate::commands::Cli;
        use clap::Parser;
        let cli = Cli::parse_from(["bin", "--dry-run", "cso", "compress", "game.iso"]);
        assert!(cli.dry_run);
        let cli = Cli::parse_from(["bin", "cso", "compress", "game.iso"]);
        assert!(!cli.dry_run);
    }

    #[test]
    fn skip_space_check_flag_parses() {
        use crate::commands::Cli;
        use clap::Parser;
        let cli = Cli::parse_from(["bin", "--skip-space-check", "cso", "compress", "game.iso"]);
        assert!(cli.skip_space_check);
        let cli = Cli::parse_from(["bin", "cso", "compress", "game.iso"]);
        assert!(!cli.skip_space_check);
    }

    #[test]
    fn templated_output_dry_run_skips_mkdir() {
        let dir = tempdir().unwrap();
        let input = dir.path().join("game.iso");
        std::fs::write(&input, b"x").unwrap();

        let out = templated_output(
            "sub/{basename}.cso",
            &input,
            Some(dir.path()),
            "cso",
            None,
            true,
        )
        .unwrap();
        assert_eq!(out, dir.path().join("sub/game.cso"));
        assert!(!dir.path().join("sub").exists());

        templated_output(
            "sub/{basename}.cso",
            &input,
            Some(dir.path()),
            "cso",
            None,
            false,
        )
        .unwrap();
        assert!(dir.path().join("sub").is_dir());
    }
}
