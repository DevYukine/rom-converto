pub mod http;

use crate::commands::ConflictPolicyArg;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use rom_converto_lib::util::{ConflictPolicy, ProgressReporter};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

pub fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

pub fn totals_from(tally: &rom_converto_lib::util::Tally) -> rom_converto_lib::util::ReportTotals {
    rom_converto_lib::util::ReportTotals {
        total_files: tally.count(),
        ok: tally.ok_count(),
        skipped: tally.skipped_count(),
        failed: tally.failed_count(),
        total_input_bytes: tally.total_input_bytes(),
        total_output_bytes: tally.total_output_bytes(),
        elapsed_ms: tally.elapsed().as_millis().min(u64::MAX as u128) as u64,
    }
}

pub fn ok_str(b: bool) -> &'static str {
    if b { "OK" } else { "FAIL" }
}

pub enum WriteDecision {
    Write(PathBuf),
    Skip,
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

pub fn ensure_input_exists(path: &std::path::Path) -> anyhow::Result<()> {
    if !path.exists() {
        anyhow::bail!("input not found: {}", path.display());
    }
    Ok(())
}

/// Expands a leading `~` in a raw CLI argument (or in a `--flag=~/...`
/// value) before clap parses it, since Windows shells don't expand `~`
/// themselves. Non-UTF-8 arguments and anything without a leading `~`
/// pass through unchanged.
pub fn expand_tilde_arg(arg: std::ffi::OsString) -> std::ffi::OsString {
    let Some(s) = arg.to_str() else {
        return arg;
    };
    if s.starts_with('~') {
        return rom_converto_lib::util::expand_tilde(Path::new(s)).into_os_string();
    }
    if s.starts_with('-')
        && let Some((flag, value)) = s.split_once('=')
        && value.starts_with('~')
    {
        let expanded = rom_converto_lib::util::expand_tilde(Path::new(value));
        let mut out = std::ffi::OsString::from(flag);
        out.push("=");
        out.push(expanded.as_os_str());
        return out;
    }
    arg
}

const PROGRESS_TEMPLATE: &str = "{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({binary_bytes_per_sec}, {eta})";

const COUNT_TEMPLATE: &str =
    "{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} files ({eta})";

/// A poisoned progress-bar mutex only means a panic happened while a bar was
/// being swapped; the bar itself stays usable, so the guard is recovered
/// rather than propagating a second panic out of a progress callback.
fn bar(slot: &Mutex<Option<ProgressBar>>) -> MutexGuard<'_, Option<ProgressBar>> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

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

    fn start_styled(&self, total: u64, msg: &str, style: ProgressStyle) {
        let pg = self.mp.add(ProgressBar::new(total));
        pg.set_style(style);
        pg.set_message(msg.to_string());
        *bar(&self.bar) = Some(pg);
    }

    /// A span whose units are files, not bytes. An unknown total (the one-shot
    /// network phases) has nothing to count, so it spins instead.
    pub fn start_count(&self, total: u64, msg: &str) {
        let style = if total == 0 {
            ProgressStyle::default_spinner()
                .template("{spinner} {msg}")
                .expect("valid progress template")
        } else {
            ProgressStyle::default_bar()
                .template(COUNT_TEMPLATE)
                .expect("valid progress template")
                .progress_chars("#>-")
        };
        self.start_styled(total, msg, style);
    }
}

impl ProgressReporter for IndicatifProgress {
    fn start(&self, total: u64, msg: &str) {
        let style = ProgressStyle::default_bar()
            .template(PROGRESS_TEMPLATE)
            .expect("valid progress template")
            .progress_chars("#>-");
        self.start_styled(total, msg, style);
    }

    fn inc(&self, delta: u64) {
        if let Some(bar) = bar(&self.bar).as_ref() {
            bar.inc(delta);
        }
    }

    fn finish(&self) {
        if let Some(bar) = bar(&self.bar).take() {
            bar.finish_and_clear();
        }
    }

    fn set_phase(&self, label: &str) {
        if let Some(bar) = bar(&self.bar).as_ref() {
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
        *bar(&self.bar) = Some(pg);
        self.taskbar_percent.store(0, Ordering::Relaxed);
        osc_taskbar(Some(0));
    }

    /// Advance by one finished file (`file_bytes` long, skipped or failed
    /// files included) so the bar reaches 100% once every input has been
    /// accounted for.
    pub fn advance(&self, file_bytes: u64) {
        let done = self.done.fetch_add(1, Ordering::Relaxed) + 1;
        let total = self.total_files.load(Ordering::Relaxed);
        if let Some(bar) = bar(&self.bar).as_ref() {
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

    pub fn finish_bar(&self) {
        if let Some(bar) = bar(&self.bar).take() {
            bar.finish_and_clear();
        }
        osc_taskbar(None);
    }
}

/// The runner's view of the CLI: per-file bars on `file`, the aggregate bar
/// on `total`, and streamed rows printed by the command that owns them.
pub struct CliProgress<'a> {
    pub file: &'a IndicatifProgress,
    pub total: &'a TotalProgress,
    pub print_row: fn(&rom_converto_lib::runner::models::RunRow),
    /// Set by the dat commands, whose outer spans count files rather than
    /// bytes; the nested per-file reporters keep counting bytes.
    pub count_units: bool,
}

impl ProgressReporter for CliProgress<'_> {
    fn start(&self, total: u64, msg: &str) {
        if self.count_units {
            self.file.start_count(total, msg);
        } else {
            self.file.start(total, msg);
        }
    }

    fn inc(&self, delta: u64) {
        self.file.inc(delta);
    }

    fn finish(&self) {
        self.file.finish();
    }

    fn set_phase(&self, label: &str) {
        self.file.set_phase(label);
    }

    fn row(&self, row: &rom_converto_lib::runner::models::RunRow) {
        (self.print_row)(row);
    }

    fn batch_start(&self, total_files: u64, total_bytes: u64) {
        self.total.begin(total_files, total_bytes);
    }

    fn batch_advance(&self, bytes: u64) {
        self.total.advance(bytes);
    }

    /// Its own bar slot on the same `MultiProgress`, so a nested per-file bar
    /// is drawn under the outer one instead of replacing it.
    fn child(&self, _suffix: &str) -> Box<dyn ProgressReporter + Send + Sync> {
        Box::new(IndicatifProgress::new(self.file.mp.clone()))
    }
}

/// The runner's `on_conflict` option value for a resolved policy.
pub fn policy_name(policy: ConflictPolicy) -> &'static str {
    match policy {
        ConflictPolicy::Error => "error",
        ConflictPolicy::Overwrite => "overwrite",
        ConflictPolicy::Skip => "skip",
        ConflictPolicy::Rename => "rename",
        ConflictPolicy::OverwriteInvalid => "overwrite_invalid",
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
        self.finish_bar();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        tp.finish_bar();
    }

    #[test]
    fn total_progress_zero_bytes_does_not_panic() {
        let tp = TotalProgress::new(hidden_multi_progress());
        tp.begin(0, 0);
        tp.advance(0);
        tp.finish_bar();
    }

    #[test]
    fn expand_tilde_arg_expands_leading_tilde() {
        let Some(home) = dirs::home_dir() else {
            // No home directory resolvable in this environment; nothing to assert.
            return;
        };
        assert_eq!(
            expand_tilde_arg(std::ffi::OsString::from("~/x")),
            home.join("x").into_os_string()
        );
    }

    #[test]
    fn expand_tilde_arg_expands_flag_value() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let mut expected = std::ffi::OsString::from("--report=");
        expected.push(home.join("x").as_os_str());
        assert_eq!(
            expand_tilde_arg(std::ffi::OsString::from("--report=~/x")),
            expected
        );
    }

    #[test]
    fn expand_tilde_arg_leaves_unrelated_flag_value_unchanged() {
        assert_eq!(
            expand_tilde_arg(std::ffi::OsString::from("--level=5")),
            std::ffi::OsString::from("--level=5")
        );
    }

    #[test]
    fn expand_tilde_arg_leaves_plain_arg_unchanged() {
        assert_eq!(
            expand_tilde_arg(std::ffi::OsString::from("plain.cue")),
            std::ffi::OsString::from("plain.cue")
        );
    }

    #[test]
    fn expand_tilde_arg_leaves_short_flag_unchanged() {
        assert_eq!(
            expand_tilde_arg(std::ffi::OsString::from("-o")),
            std::ffi::OsString::from("-o")
        );
    }
}
