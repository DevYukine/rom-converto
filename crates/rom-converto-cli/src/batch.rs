//! The CLI's bridge to the shared runner: a conversion command builds a
//! [`RunRequest`] here and this module renders the response the way the CLI
//! has always rendered it. The verify and hash batches below still drive the
//! library directly: they print a per-file `[OK]`/`[FAIL]` or digest row, and
//! `hash` walks every file rather than a fixed extension set.

use crate::commands::support::{print_hash_row, resolve_input_off_runtime};
use crate::util::{
    CliProgress, IndicatifProgress, TotalProgress, WriteDecision, file_len, totals_from,
};
use anyhow::Result;
use log::{info, warn};
use rom_converto_lib::runner::models::{
    OrganizeRow, PlaylistsData, RunData, RunOptions, RunRequest, RunResponse, RunRow,
};
use rom_converto_lib::runner::{VERIFIED_VALID, run_request};
use rom_converto_lib::util::fs::{collect_all_files, is_os_junk_dir};
use rom_converto_lib::util::{
    CancelToken, Cancelled, CollectedInput, FileDigests, FileStatus, HASH_MEMBER_EXTS, HashAlgo,
    HashCache, HashReportRecord, InputScan, ProgressReporter, ReportFormat, ReportRecord,
    ReportTotals, ResolvedInput, Tally, TallyDirection, collect_input_entries, hash_file,
    resolve_for_hash, write_hash_report,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

/// The flags every conversion command shares, in the shape the runner wants.
pub struct Common {
    pub recursive: bool,
    pub output_dir: Option<PathBuf>,
    pub output_template: Option<String>,
    pub max_depth: Option<usize>,
    pub report: Option<PathBuf>,
    pub policy: rom_converto_lib::util::ConflictPolicy,
    pub skip_space_check: bool,
}

impl From<Common> for RunOptions {
    fn from(c: Common) -> Self {
        // A template with no --output-dir resolves against the working
        // directory; the runner would otherwise root it at the input's parent.
        let templated = c.output_template.is_some();
        RunOptions {
            recursive: Some(c.recursive),
            output_dir: c
                .output_dir
                .or_else(|| templated.then(|| PathBuf::from("."))),
            output_template: c.output_template,
            max_depth: c.max_depth,
            report: c.report,
            on_conflict: Some(crate::util::policy_name(c.policy).to_string()),
            skip_space_check: Some(c.skip_space_check),
            ..RunOptions::default()
        }
    }
}

/// What every command run shares with the runner, whatever it dispatches.
pub struct BatchRun<'a> {
    pub progress: &'a IndicatifProgress,
    pub total_progress: &'a TotalProgress,
    pub cache: &'a Arc<HashCache>,
    pub cancel: &'a CancelToken,
    /// `--config` and `--preset`, so the runner layers defaults from the same
    /// config file the CLI resolved instead of re-searching the default paths.
    pub config: Option<PathBuf>,
    pub preset: Option<String>,
    pub dry_run: bool,
}

/// Runs one operation through the runner, streaming its rows to the progress
/// bars and the log, then logs the closing tally and fails the process when
/// any file failed. An empty recursive input is a warning, not an error.
pub async fn run(
    ctx: &BatchRun<'_>,
    operation: &str,
    input: PathBuf,
    output: Option<PathBuf>,
    options: RunOptions,
) -> Result<RunResponse> {
    let recursive = options.recursive == Some(true);
    let direction = direction(operation);
    let media = plan_media(operation, &options, output.as_deref());
    let mut req = RunRequest {
        schema: None,
        operation: operation.to_string(),
        input: Some(input),
        output,
        config: ctx.config.clone(),
        preset: ctx.preset.clone(),
        options,
        dry_run: ctx.dry_run,
        ctx: Default::default(),
    };
    req.ctx.hash_cache = Some(ctx.cache.clone());
    let reporter = CliProgress {
        file: ctx.progress,
        total: ctx.total_progress,
        print_row: if ctx.dry_run {
            print_plan_row
        } else {
            print_row
        },
        count_units: false,
    };
    let started = Instant::now();
    let response = run_request(req, &reporter, ctx.cancel.clone()).await;
    ctx.total_progress.finish_bar();
    let response = match response {
        Ok(response) => response,
        // A directory with nothing to convert is a warning, not a failure.
        Err(err) if recursive && is_empty_input(&err) => {
            warn!("{err}");
            return Ok(RunResponse::ok("No matching files.", None));
        }
        Err(err) => return Err(err),
    };
    // Single runs never stream rows, so their plan line and skip note print here.
    match &response.data {
        Some(RunData::Plan(line)) => {
            let mut line = line.clone();
            if line.media.is_none() {
                line.media = media;
            }
            info!("{}", plan_text(&line));
        }
        Some(RunData::Playlists(data)) => print_playlists(data, &response.records, ctx.dry_run),
        _ => {}
    }
    let skipped = (!recursive)
        .then(|| response.records.first())
        .flatten()
        .filter(|record| record.status == FileStatus::Skipped)
        // A planned skip already printed its decision on the plan line.
        .filter(|record| !ctx.dry_run || record.output_path.is_empty());
    if skipped.is_some_and(is_reported_skip) {
        info!("{}", response.message);
    }
    // A single run that skipped closes with its skip note alone, the way the
    // per-command single paths always did; a dry run always summarizes.
    if let Some(totals) = &response.totals
        && (ctx.dry_run || recursive || skipped.is_none())
    {
        finish(totals, direction, ctx.dry_run, started)?;
    }
    Ok(response)
}

/// One line per planned or written `.m3u`, in the runner's plan order. A dry
/// run also echoes the playlist body so the disc order is reviewable.
fn print_playlists(data: &PlaylistsData, records: &[ReportRecord], dry_run: bool) {
    for (playlist, record) in data.playlists.iter().zip(records) {
        let skipped = record.status == FileStatus::Skipped;
        if !dry_run {
            match skipped {
                true => info!("Skipped existing {}", playlist.output.display()),
                false => info!(
                    "Wrote {} ({} discs)",
                    playlist.output.display(),
                    playlist.disc_count
                ),
            }
            continue;
        }
        let decision = match skipped {
            true => WriteDecision::Skip,
            false => WriteDecision::Write(playlist.output.clone()),
        };
        crate::dry_run::log_plan(
            "write",
            Path::new(&record.input_path),
            &playlist.output,
            &decision,
            None,
            None,
        );
        for line in playlist.contents.lines() {
            info!("    {line}");
        }
    }
}

/// A skip this module has to announce itself: a conflict-policy skip is
/// already logged by the runner, an "input is already in this format" skip
/// carries its reason in the record and is not.
fn is_reported_skip(record: &ReportRecord) -> bool {
    // Existing outputs are already logged by the runner ("Skipped, output
    // exists" / "Kept, output verified valid"), so they are not repeated.
    record.status == FileStatus::Skipped
        && record.error.as_deref().is_some_and(|reason| {
            !reason.contains("output already exists") && reason != VERIFIED_VALID
        })
}

/// The runner rejects an empty batch input as an invalid argument: "no files
/// with extensions ... found in ...", or "no title directories found in ..."
/// for a CDN dump.
fn is_empty_input(err: &anyhow::Error) -> bool {
    let text = err.to_string();
    text.starts_with("no ") && text.contains(" found in ")
}

/// The summary line a run closes with, plus the failed-file bail that gives
/// the process its exit code. `summary_line` counts entries, so the runner's
/// totals are replayed one entry per file with the aggregate byte counts on
/// the first success.
pub(crate) fn finish(
    totals: &ReportTotals,
    direction: TallyDirection,
    dry_run: bool,
    started: Instant,
) -> Result<()> {
    let mut tally = Tally::new();
    tally.backdate(started);
    for _ in 0..totals.skipped {
        tally.record_skipped();
    }
    for _ in 0..totals.failed {
        tally.record_failed();
    }
    for i in 0..totals.ok {
        let (input, output) = match i {
            0 => (totals.total_input_bytes, totals.total_output_bytes),
            _ => (0, 0),
        };
        tally.record_ok(input, output, Duration::ZERO);
    }
    let direction = if dry_run {
        TallyDirection::DryRun
    } else {
        direction
    };
    info!("{}", tally.summary_line(direction));
    if totals.failed > 0 {
        anyhow::bail!("{} of {} files failed", totals.failed, totals.total_files);
    }
    Ok(())
}

/// Closing-summary shape per operation, matching what each command reported
/// before it went through the runner.
pub(crate) fn direction(operation: &str) -> TallyDirection {
    match operation {
        "cso.compress" | "chd.compress" | "dol.compress" | "rvl.compress" | "rvz.compress"
        | "ctr.compress" | "nx.compress" | "wup.compress" | "wup.to_wux" | "xenon.compress"
        | "cso.to_chd" | "chd.to_cso" | "cue.to_cso" => TallyDirection::Compress,
        "cso.decompress" | "ctr.decompress" | "dol.decompress" | "rvl.decompress"
        | "rvz.decompress" | "nx.decompress" | "wup.to_wud" => TallyDirection::Decompress,
        "chd.extract" | "psp.extract" | "vita.extract" | "xbox.extract" | "xenon.extract"
        | "xenon.convert" | "nx.split" | "ctr.unbundle" | "wup.decrypt" | "playlist.write" => {
            TallyDirection::CountOnly
        }
        _ => TallyDirection::Convert,
    }
}

/// A plan line with the short verb the CLI has always printed: `cue.to_iso`
/// reads as `Would to-iso ...`.
fn plan_text(line: &rom_converto_lib::util::PlanLine) -> String {
    let mut line = line.clone();
    line.operation = line
        .operation
        .rsplit('.')
        .next()
        .unwrap_or(&line.operation)
        .replace('_', "-");
    line.display_text()
}

/// The media tag a dry-run plan line carries when the output format is not
/// implied by its extension. `chd.*` tags come from the runner itself.
fn plan_media(operation: &str, options: &RunOptions, output: Option<&Path>) -> Option<String> {
    match operation {
        "cso.compress" | "cue.to_cso" => {
            Some(options.format.as_deref().unwrap_or("cso").to_uppercase())
        }
        "dol.compress" | "rvl.compress" | "rvz.compress" => Some("RVZ".to_string()),
        "cue.merge" => output.map(|p| format!("+ {}", p.with_extension("bin").display())),
        _ => None,
    }
}

/// One organize row: `[status] console · action · input -> output (detail)`.
/// Dry-run rows carry the same `Would ` prefix the plan lines use.
fn organize_line(row: &OrganizeRow) -> String {
    let status = match row.status {
        FileStatus::Ok => "ok",
        FileStatus::Skipped => "skipped",
        FileStatus::Failed => "failed",
    };
    let target = row
        .output
        .as_deref()
        .map_or_else(|| "-".to_string(), |p| p.display().to_string());
    let line = format!(
        "[{}] {} · {} · {} -> {}",
        status,
        row.console.as_deref().unwrap_or("-"),
        row.action,
        row.input.display(),
        target,
    );
    let line = match &row.detail {
        Some(detail) => format!("{line} ({detail})"),
        None => line,
    };
    match row.planned {
        true => format!("Would {line}"),
        false => line,
    }
}

/// Streamed rows of a dry run: a planned skip already printed its decision
/// on the plan line, so only skips with no plan behind them (no output path:
/// the input is already in the target format) still report themselves.
pub(crate) fn print_plan_row(row: &RunRow) {
    match row {
        RunRow::Record(record)
            if record.status == FileStatus::Skipped && !record.output_path.is_empty() => {}
        row => print_row(row),
    }
}

/// Streamed rows: the plan lines of a recursive dry run and the per-file
/// skips and failures each batch arm used to report about itself.
pub(crate) fn print_row(row: &RunRow) {
    match row {
        RunRow::Plan(line) => info!("{}", plan_text(line)),
        RunRow::Record(record) if record.status == FileStatus::Failed => warn!(
            "Failed to {} {}: {}",
            record.operation,
            record.input_path,
            record.error.as_deref().unwrap_or("unknown error")
        ),
        RunRow::Record(record) if is_reported_skip(record) => info!(
            "Skipped, {}: {}",
            record.error.as_deref().unwrap_or_default(),
            record.input_path
        ),
        RunRow::Organize(row) => match row.status {
            FileStatus::Failed if !row.planned => warn!("{}", organize_line(row)),
            _ => info!("{}", organize_line(row)),
        },
        _ => {}
    }
}

struct VerifyTally {
    total: usize,
    ok: usize,
    failed: usize,
}

/// Sum of on-disk sizes for the aggregate progress bar's total byte length.
fn files_bytes(files: &[CollectedInput]) -> u64 {
    files.iter().map(|p| file_len(&p.path)).sum()
}

async fn collect_or_warn(
    input_dir: &Path,
    exts: &'static [&'static str],
    max_depth: Option<usize>,
    cancel: &CancelToken,
) -> Result<Vec<CollectedInput>> {
    let dir = input_dir.to_path_buf();
    let cancel = cancel.clone();
    let files = tokio::task::spawn_blocking(move || {
        collect_input_entries(
            &dir,
            &InputScan {
                exts,
                member_exts: None,
                max_depth,
                skip_unpacked_twins: false,
            },
            &cancel,
        )
    })
    .await??;
    if files.is_empty() {
        warn!(
            "No matching files found in {} (looked for {:?})",
            input_dir.display(),
            exts
        );
    }
    Ok(files)
}

fn finish_verify(tally: VerifyTally) -> Result<()> {
    info!(
        "Verified {} files: {} OK, {} failed",
        tally.total, tally.ok, tally.failed
    );
    if tally.failed > 0 {
        anyhow::bail!("verification failed");
    }
    Ok(())
}

fn fail_unreadable(path: &Path, reason: &str, failed: &mut usize) {
    *failed += 1;
    warn!("[FAIL] {}: {reason}", path.display());
}

/// Stages one walked input off the async runtime. A staging failure counts
/// the file as failed instead of aborting the batch.
async fn stage_or_fail(
    path: &Path,
    exts: &'static [&'static str],
    failed: &mut usize,
) -> Option<ResolvedInput> {
    match resolve_input_off_runtime(path.to_path_buf(), exts).await {
        Ok(resolved) => Some(resolved),
        Err(e) => {
            *failed += 1;
            warn!("[FAIL] {}: {e}", path.display());
            None
        }
    }
}
pub async fn cso_verify(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    full: bool,
    max_depth: Option<usize>,
    cancel: &CancelToken,
) -> Result<()> {
    use rom_converto_lib::cso::verify_cso;

    const EXTS: &[&str] = &["cso", "zso", "dax"];
    let files = collect_or_warn(input_dir, EXTS, max_depth, cancel).await?;
    if files.is_empty() {
        return Ok(());
    }
    let total = files.len();
    total_progress.begin(total as u64, files_bytes(&files));
    let mut ok = 0usize;
    let mut failed = 0usize;
    for collected in files {
        let path = collected.path;
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let bytes = file_len(&path);
        if let Some(reason) = collected.unreadable {
            fail_unreadable(&path, &reason, &mut failed);
            total_progress.advance(bytes);
            continue;
        }
        let Some(resolved) = stage_or_fail(&path, EXTS, &mut failed).await else {
            total_progress.advance(bytes);
            continue;
        };
        match verify_cso(
            progress,
            resolved.path().to_path_buf(),
            full,
            cancel.clone(),
        )
        .await
        {
            Ok(()) => {
                ok += 1;
                info!("[OK] {}", path.display());
            }
            Err(e) => {
                if cancel.is_cancelled() {
                    return Err(Cancelled.into());
                }
                failed += 1;
                warn!("[FAIL] {}: {e}", path.display());
            }
        }
        total_progress.advance(bytes);
    }
    total_progress.finish_bar();
    finish_verify(VerifyTally { total, ok, failed })
}
pub async fn dol_verify(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    full: bool,
    max_depth: Option<usize>,
    cancel: &CancelToken,
) -> Result<()> {
    use rom_converto_lib::nintendo::dol::verify::{DolVerifyOptions, verify_dol};

    const EXTS: &[&str] = &["iso", "gcm", "rvz"];
    let files = collect_or_warn(input_dir, EXTS, max_depth, cancel).await?;
    if files.is_empty() {
        return Ok(());
    }
    let total = files.len();
    let opts = DolVerifyOptions { full };
    total_progress.begin(total as u64, files_bytes(&files));
    let mut ok = 0usize;
    let mut failed = 0usize;
    for collected in files {
        let path = collected.path;
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let bytes = file_len(&path);
        if let Some(reason) = collected.unreadable {
            fail_unreadable(&path, &reason, &mut failed);
            total_progress.advance(bytes);
            continue;
        }
        let Some(resolved) = stage_or_fail(&path, EXTS, &mut failed).await else {
            total_progress.advance(bytes);
            continue;
        };
        match verify_dol(resolved.path(), &opts, progress, cancel) {
            Ok(result) if result.ok => {
                ok += 1;
                info!("[OK] {}", path.display());
            }
            Ok(result) => {
                failed += 1;
                warn!("[FAIL] {}", path.display());
                if let Some(note) = &result.rvz_note {
                    warn!("  {note}");
                }
            }
            Err(e) => {
                if cancel.is_cancelled() {
                    return Err(Cancelled.into());
                }
                failed += 1;
                warn!("[FAIL] {}: {e}", path.display());
            }
        }
        total_progress.advance(bytes);
    }
    total_progress.finish_bar();
    finish_verify(VerifyTally { total, ok, failed })
}

pub async fn rvl_verify(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    full: bool,
    max_depth: Option<usize>,
    cancel: &CancelToken,
) -> Result<()> {
    use rom_converto_lib::nintendo::rvl::verify::{RvlVerifyOptions, verify_rvl};

    const EXTS: &[&str] = &["iso", "wbfs", "rvz"];
    let files = collect_or_warn(input_dir, EXTS, max_depth, cancel).await?;
    if files.is_empty() {
        return Ok(());
    }
    let total = files.len();
    let opts = RvlVerifyOptions { full };
    total_progress.begin(total as u64, files_bytes(&files));
    let mut ok = 0usize;
    let mut failed = 0usize;
    for collected in files {
        let path = collected.path;
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let bytes = file_len(&path);
        if let Some(reason) = collected.unreadable {
            fail_unreadable(&path, &reason, &mut failed);
            total_progress.advance(bytes);
            continue;
        }
        let Some(resolved) = stage_or_fail(&path, EXTS, &mut failed).await else {
            total_progress.advance(bytes);
            continue;
        };
        match verify_rvl(resolved.path(), &opts, progress, cancel) {
            Ok(result) if result.ok => {
                ok += 1;
                info!("[OK] {}", path.display());
            }
            Ok(result) => {
                failed += 1;
                warn!("[FAIL] {}", path.display());
                if let Some(note) = &result.rvz_note {
                    warn!("  {note}");
                }
            }
            Err(e) => {
                if cancel.is_cancelled() {
                    return Err(Cancelled.into());
                }
                failed += 1;
                warn!("[FAIL] {}: {e}", path.display());
            }
        }
        total_progress.advance(bytes);
    }
    total_progress.finish_bar();
    finish_verify(VerifyTally { total, ok, failed })
}
pub async fn nx_verify(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    keys: rom_converto_lib::nintendo::nx::KeySet,
    max_depth: Option<usize>,
    cancel: &CancelToken,
) -> Result<()> {
    use rom_converto_lib::nintendo::nx::verify_container_async;

    const EXTS: &[&str] = &["nsp", "xci", "nsz", "xcz", "dnsp", "dxci"];
    let files = collect_or_warn(input_dir, EXTS, max_depth, cancel).await?;
    if files.is_empty() {
        return Ok(());
    }
    let total = files.len();
    total_progress.begin(total as u64, files_bytes(&files));
    let mut ok = 0usize;
    let mut failed = 0usize;
    for collected in files {
        let path = collected.path;
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let bytes = file_len(&path);
        if let Some(reason) = collected.unreadable {
            fail_unreadable(&path, &reason, &mut failed);
            total_progress.advance(bytes);
            continue;
        }
        let Some(resolved) = stage_or_fail(&path, EXTS, &mut failed).await else {
            total_progress.advance(bytes);
            continue;
        };
        match verify_container_async(
            resolved.path().to_path_buf(),
            keys.clone(),
            progress,
            cancel.clone(),
        )
        .await
        {
            Ok(result) if result.ok => {
                ok += 1;
                info!("[OK] {}", path.display());
            }
            Ok(_) => {
                failed += 1;
                warn!("[FAIL] {}", path.display());
            }
            Err(e) => {
                if cancel.is_cancelled() {
                    return Err(Cancelled.into());
                }
                failed += 1;
                warn!("[FAIL] {}: {e}", path.display());
            }
        }
        total_progress.advance(bytes);
    }
    total_progress.finish_bar();
    finish_verify(VerifyTally { total, ok, failed })
}

/// A direct subdirectory of `input_dir` is a NUS title dir when it
/// holds a `title.tmd` or any community `tmd.<N>` file, mirroring the
/// NUS layout discovery in the wup loader.
fn is_nus_title_dir(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name == "title.tmd" {
            return true;
        }
        if let Some(rest) = name.strip_prefix("tmd.")
            && rest.parse::<u32>().is_ok()
        {
            return true;
        }
    }
    false
}

pub async fn wup_verify(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    max_depth: Option<usize>,
    cancel: &CancelToken,
) -> Result<()> {
    use rom_converto_lib::nintendo::wup::verify_wup_async;

    const FILE_EXTS: &[&str] = &["wud", "wux"];
    let dir = input_dir.to_path_buf();
    let mut inputs = {
        let cancel = cancel.clone();
        tokio::task::spawn_blocking(move || {
            collect_input_entries(
                &dir,
                &InputScan {
                    exts: FILE_EXTS,
                    member_exts: None,
                    max_depth,
                    skip_unpacked_twins: false,
                },
                &cancel,
            )
        })
        .await??
    };
    if let Ok(entries) = std::fs::read_dir(input_dir) {
        let mut dirs: Vec<CollectedInput> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_none_or(|n| !is_os_junk_dir(n))
                    && is_nus_title_dir(p)
            })
            .map(CollectedInput::plain)
            .collect();
        dirs.sort_by(|a, b| a.path.cmp(&b.path));
        inputs.append(&mut dirs);
    }
    inputs.sort_by(|a, b| a.path.cmp(&b.path));

    if inputs.is_empty() {
        warn!(
            "No .wud / .wux discs or NUS title directories found in {}",
            input_dir.display()
        );
        return Ok(());
    }

    let total = inputs.len();
    total_progress.begin(total as u64, files_bytes(&inputs));
    let mut ok = 0usize;
    let mut failed = 0usize;
    for collected in inputs {
        let path = collected.path;
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let bytes = file_len(&path);
        if let Some(reason) = collected.unreadable {
            fail_unreadable(&path, &reason, &mut failed);
            total_progress.advance(bytes);
            continue;
        }
        // A NUS title directory is verified in place; disc files resolve
        // through an archive when one holds the .wud/.wux member. The
        // staging stays alive until the verify below has finished.
        let staging = if path.is_dir() {
            None
        } else {
            match stage_or_fail(&path, FILE_EXTS, &mut failed).await {
                Some(resolved) => Some(resolved),
                None => {
                    total_progress.advance(bytes);
                    continue;
                }
            }
        };
        let target = staging
            .as_ref()
            .map_or_else(|| path.clone(), |r| r.path().to_path_buf());
        match verify_wup_async(target, None, progress, cancel.clone()).await {
            Ok(result) if result.ok => {
                ok += 1;
                info!("[OK] {}", path.display());
            }
            Ok(_) => {
                failed += 1;
                warn!("[FAIL] {}", path.display());
            }
            Err(e) => {
                if cancel.is_cancelled() {
                    return Err(Cancelled.into());
                }
                failed += 1;
                warn!("[FAIL] {}: {e}", path.display());
            }
        }
        total_progress.advance(bytes);
    }
    total_progress.finish_bar();
    finish_verify(VerifyTally { total, ok, failed })
}
fn hash_ok_record(path: &Path, d: &FileDigests, started: Instant) -> HashReportRecord {
    HashReportRecord {
        path: path.display().to_string(),
        crc32: d.crc32.clone(),
        sha1: d.sha1.clone(),
        md5: d.md5.clone(),
        sha256: d.sha256.clone(),
        size_bytes: d.size_bytes,
        status: FileStatus::Ok,
        elapsed_ms: elapsed_ms(started),
        error: None,
    }
}

fn hash_failed_record(
    path: &Path,
    started: Instant,
    error: impl std::fmt::Display,
) -> HashReportRecord {
    HashReportRecord {
        path: path.display().to_string(),
        crc32: None,
        sha1: None,
        md5: None,
        sha256: None,
        size_bytes: 0,
        status: FileStatus::Failed,
        elapsed_ms: elapsed_ms(started),
        error: Some(error.to_string()),
    }
}

pub(crate) fn hash_cached(
    path: &Path,
    algos: &[HashAlgo],
    progress: &dyn ProgressReporter,
    cache: &HashCache,
    cancel: &CancelToken,
) -> std::io::Result<FileDigests> {
    match cache.lookup_raw(path, algos) {
        Some(d) => Ok(d),
        None => {
            let computed = hash_file(path, algos, progress, cancel);
            if let Ok(d) = &computed {
                cache.store_raw(path, d);
            }
            computed
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn hash_batch(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    algos: &[HashAlgo],
    max_depth: Option<usize>,
    report_path: Option<&Path>,
    cache: &HashCache,
    cancel: &CancelToken,
) -> Result<()> {
    let files = collect_all_files(input_dir, max_depth, cancel)?;
    if files.is_empty() {
        warn!("No files found in {}", input_dir.display());
        return Ok(());
    }
    let total = files.len();
    total_progress.begin(total as u64, files.iter().map(|p| file_len(p)).sum());
    let mut tally = Tally::new();
    let mut records: Vec<HashReportRecord> = Vec::new();
    for path in files {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let bytes = file_len(&path);
        let started = Instant::now();
        // A staged member lives in a temp dir and its digests are the
        // member's, so it hashes without the cache; `None` hashed the raw
        // bytes at the walked path, which the cache may serve.
        let hashed = {
            let walk = path.clone();
            match tokio::task::spawn_blocking(move || resolve_for_hash(&walk, &HASH_MEMBER_EXTS))
                .await
            {
                Ok(Ok(None)) => hash_cached(&path, algos, progress, cache, cancel),
                Ok(Ok(Some(resolved))) => hash_file(resolved.path(), algos, progress, cancel),
                Ok(Err(e)) => Err(std::io::Error::other(e)),
                Err(e) => Err(std::io::Error::other(e)),
            }
        };
        match hashed {
            Ok(d) => {
                print_hash_row(&path, &d, algos);
                tally.record_ok(d.size_bytes, 0, started.elapsed());
                records.push(hash_ok_record(&path, &d, started));
            }
            Err(e) => {
                if cancel.is_cancelled() {
                    return Err(Cancelled.into());
                }
                warn!("Failed to hash {}: {e}", path.display());
                tally.record_failed();
                records.push(hash_failed_record(&path, started, e));
            }
        }
        total_progress.advance(bytes);
    }
    total_progress.finish_bar();
    info!("{}", tally.summary_line(TallyDirection::CountOnly));
    // Hashing is read-only diagnostics, so a per-file read failure must not
    // abort the run: it is recorded and the run continues. The report is still
    // written so a failing file is captured on disk.
    if let Some(path) = report_path {
        write_hash_report(
            path,
            &records,
            &totals_from(&tally),
            ReportFormat::from_path(path),
            cancel,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn recursive_verify_fails_on_an_unreadable_archive() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("docs.zip"), b"not an archive").unwrap();
        let mp =
            indicatif::MultiProgress::with_draw_target(indicatif::ProgressDrawTarget::hidden());
        let progress = crate::util::IndicatifProgress::new(mp.clone());
        let total = crate::util::TotalProgress::new(mp);
        let result = cso_verify(
            &progress,
            &total,
            dir.path(),
            false,
            None,
            &CancelToken::new(),
        )
        .await;
        assert!(result.is_err(), "{result:?}");
    }
}
