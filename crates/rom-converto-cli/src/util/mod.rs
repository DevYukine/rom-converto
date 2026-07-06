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
use std::sync::atomic::{AtomicU64, Ordering};

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
    .map_err(|e| log::debug!("Metadata unavailable for {}: {e}", input.display()))
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

pub use rom_converto_lib::util::{HashCache, OutputVerify, VerifyOutcome, verify_existing_output};

/// Format label for the verify cache. `None` means the target is not cached:
/// an output with no integrity check, or an NX container with no usable keyset
/// (its verify is skipped and its output kept, so caching it would be wrong).
fn verify_label(target: &OutputVerify) -> Option<&'static str> {
    match target {
        OutputVerify::Chd => Some("chd"),
        OutputVerify::Cso => Some("cso"),
        OutputVerify::Rvz => Some("rvz"),
        OutputVerify::Nx(keys) if keys.header_key.is_some() => Some("nx"),
        OutputVerify::Nx(_) | OutputVerify::None => None,
    }
}

/// [`verify_existing_output`] with a cache in front. A prior `Valid` verdict for
/// an unchanged output short-circuits the read; only `Valid` is stored, since an
/// `Invalid` output gets rewritten (changing its mtime and invalidating the
/// entry anyway).
pub async fn verify_existing_cached(
    cache: &HashCache,
    progress: &dyn ProgressReporter,
    path: &Path,
    target: OutputVerify,
) -> VerifyOutcome {
    let label = verify_label(&target);
    if let Some(label) = label
        && cache.lookup_verify(path, label)
    {
        return VerifyOutcome::Valid;
    }
    let outcome = verify_existing_output(progress, path, target).await;
    if outcome == VerifyOutcome::Valid
        && let Some(label) = label
    {
        cache.store_verify(path, label, true);
    }
    outcome
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

const PROGRESS_TEMPLATE: &str = "{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({binary_bytes_per_sec}, {eta})";

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

const TOTAL_PROGRESS_TEMPLATE: &str =
    "{msg} [{wide_bar:.green/blue}] {binary_bytes}/{binary_total_bytes} ({eta})";

/// Terminal taskbar progress via OSC 9;4 (Windows Terminal, ConEmu).
/// Unsupported terminals ignore the sequence; skipped entirely when stderr
/// is not a terminal. `None` clears the taskbar state.
fn osc_taskbar(percent: Option<u64>) {
    use std::io::{IsTerminal, Write};
    let mut err = std::io::stderr();
    if !err.is_terminal() {
        return;
    }
    let seq = match percent {
        Some(p) => format!("\x1b]9;4;1;{p}\x07"),
        None => "\x1b]9;4;0;0\x07".to_string(),
    };
    let _ = err.write_all(seq.as_bytes());
    let _ = err.flush();
}

/// Aggregate batch progress bar: files done/total and total bytes across an
/// entire recursive run, pinned above the per-file `IndicatifProgress` bar on
/// the same `MultiProgress`.
pub struct TotalProgress {
    mp: MultiProgress,
    bar: Mutex<Option<ProgressBar>>,
    done: AtomicU64,
    total_files: AtomicU64,
    taskbar_percent: AtomicU64,
}

impl TotalProgress {
    pub fn new(mp: MultiProgress) -> Self {
        Self {
            mp,
            bar: Mutex::new(None),
            done: AtomicU64::new(0),
            total_files: AtomicU64::new(0),
            taskbar_percent: AtomicU64::new(0),
        }
    }

    /// Start (or restart, for a new command) the aggregate bar for
    /// `total_files` files totaling `total_bytes`. Added before any per-file
    /// bar so it stays pinned above them. Zero bytes (empty or unknown-size
    /// batch) falls back to a spinner instead of a stuck full bar.
    pub fn begin(&self, total_files: u64, total_bytes: u64) {
        self.done.store(0, Ordering::Relaxed);
        self.total_files.store(total_files, Ordering::Relaxed);
        let pg = self.mp.add(ProgressBar::new(total_bytes));
        let style = if total_bytes == 0 {
            ProgressStyle::default_spinner()
                .template("{spinner} {msg}")
                .expect("valid progress template")
        } else {
            ProgressStyle::default_bar()
                .template(TOTAL_PROGRESS_TEMPLATE)
                .expect("valid progress template")
                .progress_chars("#>-")
        };
        pg.set_style(style);
        pg.set_message(format!("0/{total_files} files"));
        *self.bar.lock().unwrap() = Some(pg);
        self.taskbar_percent.store(0, Ordering::Relaxed);
        osc_taskbar(Some(0));
    }

    /// Advance by one finished file (`file_bytes` long, skipped or failed
    /// files included) so the bar reaches 100% once every input has been
    /// accounted for.
    pub fn advance(&self, file_bytes: u64) {
        let done = self.done.fetch_add(1, Ordering::Relaxed) + 1;
        let total = self.total_files.load(Ordering::Relaxed);
        if let Some(bar) = self.bar.lock().unwrap().as_ref() {
            bar.set_message(format!("{done}/{total} files"));
            if bar.length() == Some(0) {
                bar.tick();
            } else {
                bar.inc(file_bytes);
            }
            let percent = match bar.length() {
                Some(len) if len > 0 => (bar.position() * 100 / len).min(100),
                _ if total > 0 => (done * 100 / total).min(100),
                _ => 0,
            };
            if self.taskbar_percent.swap(percent, Ordering::Relaxed) != percent {
                osc_taskbar(Some(percent));
            }
        }
    }

    pub fn finish(&self) {
        if let Some(bar) = self.bar.lock().unwrap().take() {
            bar.finish_and_clear();
        }
        osc_taskbar(None);
    }
}

/// Lets library functions outside batch.rs (which don't know the concrete
/// `TotalProgress` byte-aware API) drive the same aggregate bar through the
/// shared trait, one unit at a time.
impl ProgressReporter for TotalProgress {
    fn start(&self, total: u64, _msg: &str) {
        self.begin(total, 0);
    }

    fn inc(&self, delta: u64) {
        self.advance(delta);
    }

    fn finish(&self) {
        TotalProgress::finish(self);
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
    fn verify_label_maps_targets() {
        use rom_converto_lib::nintendo::nx::KeySet;
        assert_eq!(verify_label(&OutputVerify::Chd), Some("chd"));
        assert_eq!(verify_label(&OutputVerify::Cso), Some("cso"));
        assert_eq!(verify_label(&OutputVerify::Rvz), Some("rvz"));
        assert_eq!(verify_label(&OutputVerify::None), None);

        // NX without a header key is not cached: its verify is skipped and the
        // output kept, so a cached "valid" verdict would be wrong.
        let no_key = KeySet::default();
        assert_eq!(verify_label(&OutputVerify::Nx(Box::new(no_key))), None);

        let with_key = KeySet {
            header_key: Some([0u8; 32]),
            ..Default::default()
        };
        assert_eq!(
            verify_label(&OutputVerify::Nx(Box::new(with_key))),
            Some("nx")
        );
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

    fn hidden_multi_progress() -> MultiProgress {
        MultiProgress::with_draw_target(indicatif::ProgressDrawTarget::hidden())
    }

    #[test]
    fn total_progress_advances_bytes_and_file_count() {
        let tp = TotalProgress::new(hidden_multi_progress());
        tp.begin(3, 300);
        tp.advance(100);
        tp.advance(100);
        tp.advance(100);
        assert_eq!(tp.done.load(Ordering::Relaxed), 3);
        assert_eq!(tp.bar.lock().unwrap().as_ref().unwrap().position(), 300);
        tp.finish();
    }

    #[test]
    fn total_progress_zero_bytes_does_not_panic() {
        let tp = TotalProgress::new(hidden_multi_progress());
        tp.begin(0, 0);
        tp.advance(0);
        tp.finish();
    }
}
