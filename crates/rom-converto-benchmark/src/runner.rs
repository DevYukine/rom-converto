use anyhow::{Context, Result, bail};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::stats::WarmStats;

#[derive(Clone, Copy)]
pub struct RunConfig {
    pub iterations: usize,
    pub cooldown: Duration,
}

/// Run a command to completion with stdout/stderr captured (mirrors the
/// Python harness `capture_output=True`) and return its wall-clock
/// duration. A non-zero exit becomes an error carrying a truncated stderr.
pub fn run_timed(cmd: &mut Command) -> Result<Duration> {
    cmd.stdin(Stdio::null());
    let start = Instant::now();
    let output = cmd
        .output()
        .with_context(|| format!("failed to spawn {cmd:?}"))?;
    let elapsed = start.elapsed();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let truncated: String = stderr.chars().take(1000).collect();
        bail!(
            "command failed ({}): {cmd:?}\nstderr: {truncated}",
            output.status
        );
    }
    Ok(elapsed)
}

/// Best-effort kill of leftover tool processes between runs, mirroring the
/// scripts' `pkill`/`taskkill`. Exact-name matching keeps the benchmark
/// process (rom-converto-benchmark) from being caught by `rom-converto`.
pub fn kill_residuals(names: &[&str]) {
    for name in names {
        #[cfg(windows)]
        let _ = Command::new("taskkill")
            .args(["/F", "/IM", &format!("{name}.exe")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        #[cfg(not(windows))]
        let _ = Command::new("pkill")
            .args(["-x", name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Interleaved timed loop comparing the external tool against
/// rom-converto: each round kills residuals, runs the external side,
/// cools down, then does the same for rom-converto. Returns warm stats
/// (first run excluded) for both sides. The closures perform their own
/// per-run output cleanup and size capture.
pub fn bench_op_vs<E, R>(
    cfg: &RunConfig,
    kill_names: &[&str],
    ext: &mut E,
    rc: &mut R,
) -> Result<(WarmStats, WarmStats)>
where
    E: FnMut() -> Result<Duration>,
    R: FnMut() -> Result<Duration>,
{
    let mut ext_samples = Vec::with_capacity(cfg.iterations);
    let mut rc_samples = Vec::with_capacity(cfg.iterations);
    for _ in 0..cfg.iterations {
        kill_residuals(kill_names);
        ext_samples.push(ext()?.as_secs_f64());
        std::thread::sleep(cfg.cooldown);

        kill_residuals(kill_names);
        rc_samples.push(rc()?.as_secs_f64());
        std::thread::sleep(cfg.cooldown);
    }
    Ok((
        WarmStats::from_samples(&ext_samples),
        WarmStats::from_samples(&rc_samples),
    ))
}

/// Run an operation against the reference tool when `has_ext` is set,
/// otherwise time rom-converto alone. The `ext` closure is never called
/// when `has_ext` is false, so callers may pass a closure that assumes the
/// reference tool is present. Returns `(ext stats, rom-converto stats)`.
pub fn run_sided<E, R>(
    cfg: &RunConfig,
    kill_names: &[&str],
    has_ext: bool,
    ext: &mut E,
    rc: &mut R,
) -> Result<(Option<WarmStats>, WarmStats)>
where
    E: FnMut() -> Result<Duration>,
    R: FnMut() -> Result<Duration>,
{
    if has_ext {
        let (ext_stats, rc_stats) = bench_op_vs(cfg, kill_names, ext, rc)?;
        Ok((Some(ext_stats), rc_stats))
    } else {
        Ok((None, bench_op(cfg, kill_names, rc)?))
    }
}

/// Timed loop for a rom-converto-only operation (verify, or a decompress
/// whose reference tool is compress-only).
pub fn bench_op<R>(cfg: &RunConfig, kill_names: &[&str], rc: &mut R) -> Result<WarmStats>
where
    R: FnMut() -> Result<Duration>,
{
    let mut rc_samples = Vec::with_capacity(cfg.iterations);
    for _ in 0..cfg.iterations {
        kill_residuals(kill_names);
        rc_samples.push(rc()?.as_secs_f64());
        std::thread::sleep(cfg.cooldown);
    }
    Ok(WarmStats::from_samples(&rc_samples))
}
