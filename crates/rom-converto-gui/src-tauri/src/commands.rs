use crate::info_cache::InfoCache;
use crate::progress::TauriProgress;
use rom_converto_lib::chd::{
    ChdDvdOptions, DiscMode, convert_disc_to_chd_cancellable, extract_from_chd_cancellable,
    verify_chd_cancellable,
};
use rom_converto_lib::cso::{
    CsoCompressOptions, CsoFormat, compress_to_cso_cancellable, decompress_from_cso_cancellable,
    verify_cso,
};
use rom_converto_lib::cue::merge::merge_bin;
use rom_converto_lib::info::{InfoOptions, InfoResult, read_info};
use rom_converto_lib::nintendo::ctr::convert::{convert_rom_cancellable, derive_converted_path};
use rom_converto_lib::nintendo::ctr::verify::{CtrVerifyOptions, verify_ctr};
use rom_converto_lib::nintendo::ctr::z3ds::{
    compress_rom_cancellable, decompress_rom_cancellable, derive_compressed_path,
    derive_decompressed_path,
};
use rom_converto_lib::nintendo::ctr::{
    CdnToCiaOptions, convert_cdn_to_cia_cancellable, decrypt_rom_cancellable,
    derive_decrypted_path, generate_ticket_from_cdn,
};
use rom_converto_lib::nintendo::dol::verify::{DolVerifyOptions, verify_dol};
use rom_converto_lib::nintendo::nx::{
    KeySet, NczMode, NxCompressOptions, compress_container_async_cancellable,
    decompress_container_async_cancellable, derive_compressed_path as nx_derive_compressed_path,
    derive_decompressed_path as nx_derive_decompressed_path, detect_container, load_keyset,
    verify_container_async,
};
use rom_converto_lib::nintendo::rvl::verify::{RvlVerifyOptions, verify_rvl};
use rom_converto_lib::nintendo::rvz::{
    RvzCompressOptions, compress_disc_cancellable, decompress_disc_cancellable,
    decompress_disc_to_wbfs_cancellable, derive_disc_path, derive_rvz_path,
};
use rom_converto_lib::nintendo::wup::{
    TitleInput, WupCompressOptions, compress_titles_async_cancellable,
    decrypt_nus_title_async_cancellable, verify_wup_async,
};
use rom_converto_lib::playlist::{PlaylistMode, PlaylistOptions, plan_playlists};
use rom_converto_lib::util::fs::{collect_all_files, collect_files_with_exts};
use rom_converto_lib::util::{
    CancelToken, ConflictPolicy, ConflictResolution, DEFAULT_SPACE_HEADROOM, FileStatus, HashAlgo,
    PlanLine, ReportFormat, ReportRecord, ReportTotals, TemplateTokens, apply_template,
    available_space, format_bytes, hash_file, parse_algos, resolve_conflict, space_shortfall,
    write_report,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, State};

fn err_to_string(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Result of a report-capable command. The message drives the operation log;
/// the optional record is accumulated client-side and handed back to
/// `cmd_write_report` once the run finishes, matching how the CLI collects
/// records and writes a single report at the end.
#[derive(serde::Serialize)]
pub struct RunOutcome {
    message: String,
    record: Option<ReportRecord>,
}

impl RunOutcome {
    /// A conflict-policy skip. When reporting is on it carries a skipped record
    /// so the run report matches the CLI, which records every skipped file.
    fn skipped(report: bool, input: &Path, operation: &str, desired: &Path) -> Self {
        Self {
            message: format!("skipped existing {}", desired.display()),
            record: build_skip_record(report, input, operation),
        }
    }
}

#[derive(serde::Deserialize)]
pub struct ReportPayload {
    records: Vec<ReportRecord>,
    totals: ReportTotals,
}

/// Resolve an output-path template to a concrete path, mirroring the CLI's
/// `templated_output`: metadata is read best-effort (a failed read degrades
/// the identity tokens to the input basename) and the relative result is
/// joined under the input's directory, matching the GUI's output-next-to-source
/// default. A malformed template still surfaces as an error.
fn resolve_templated_output(
    template: &str,
    input: &Path,
    output_ext: &str,
    keys_path: Option<&Path>,
    dry_run: bool,
) -> Result<PathBuf, String> {
    let info = read_info(
        input,
        &InfoOptions {
            keys_path: keys_path.map(Path::to_path_buf),
            parent_path: None,
        },
    )
    .ok();
    let tokens = TemplateTokens::new(info.as_ref(), input, output_ext);
    let rel = apply_template(template, &tokens).map_err(err_to_string)?;
    let base = input.parent().unwrap_or_else(|| Path::new("."));
    let joined = base.join(rel);
    if !dry_run && let Some(parent) = joined.parent() {
        std::fs::create_dir_all(parent).map_err(err_to_string)?;
    }
    Ok(joined)
}

fn ext_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string()
}

/// Pick the output path for a write command. When a template is given it
/// supersedes any explicit output, mirroring the CLI's `conflicts_with` rule;
/// supplying both at once is rejected.
#[allow(clippy::too_many_arguments)]
fn pick_output(
    explicit: Option<PathBuf>,
    template: Option<&str>,
    input: &Path,
    output_ext: &str,
    keys_path: Option<&Path>,
    default: impl FnOnce() -> PathBuf,
    dry_run: bool,
) -> Result<PathBuf, String> {
    match template {
        Some(tmpl) => {
            if explicit.is_some() {
                return Err("output template conflicts with an explicit output path".into());
            }
            resolve_templated_output(tmpl, input, output_ext, keys_path, dry_run)
        }
        None => Ok(explicit.unwrap_or_else(default)),
    }
}

fn build_record(
    enabled: bool,
    input: &Path,
    output: &Path,
    operation: &str,
    input_bytes: u64,
    output_bytes: u64,
    elapsed: Duration,
) -> Option<ReportRecord> {
    if !enabled {
        return None;
    }
    let elapsed_ms = elapsed.as_millis().min(u64::MAX as u128) as u64;
    Some(ReportRecord::new(
        input.display().to_string(),
        output.display().to_string(),
        operation,
        FileStatus::Ok,
        input_bytes,
        output_bytes,
        elapsed_ms,
        None,
    ))
}

/// Build a skipped record for a conflict-policy skip, matching the CLI's
/// `skipped_record(input, op, None)`: empty output path, zero bytes, no error.
fn build_skip_record(enabled: bool, input: &Path, operation: &str) -> Option<ReportRecord> {
    if !enabled {
        return None;
    }
    Some(ReportRecord::new(
        input.display().to_string(),
        String::new(),
        operation,
        FileStatus::Skipped,
        0,
        0,
        0,
        None,
    ))
}

fn conflict_policy(s: Option<&str>) -> ConflictPolicy {
    match s {
        Some("error") => ConflictPolicy::Error,
        Some("skip") => ConflictPolicy::Skip,
        Some("rename") => ConflictPolicy::Rename,
        Some("overwrite-invalid") => ConflictPolicy::OverwriteInvalid,
        _ => ConflictPolicy::Overwrite,
    }
}

/// Resolve where to write `desired` under the chosen policy. Returns `Ok(None)`
/// when an existing file is kept, in which case the caller skips the operation;
/// the lib is otherwise called with force=true since the force-only lib
/// functions cannot express skip or rename themselves.
///
/// `overwrite-invalid` is executed here: `resolve_conflict` reports `Skip` for
/// `OverwriteInvalid`, so the keep-versus-rewrite decision is made by verifying
/// the existing file. A valid output is kept (skip), an invalid one is
/// rewritten. This uses the same verify call, target, and mapping as
/// `plan_line`, so the dry-run preview and the real run agree.
async fn resolve_output(
    progress: &dyn rom_converto_lib::util::ProgressReporter,
    desired: &Path,
    on_conflict: Option<&str>,
    verify: rom_converto_lib::util::OutputVerify,
) -> Result<Option<PathBuf>, String> {
    use rom_converto_lib::util::{VerifyOutcome, verify_existing_output};
    let policy = conflict_policy(on_conflict);
    match resolve_conflict(desired, policy).map_err(err_to_string)? {
        ConflictResolution::Write(p) => Ok(Some(p)),
        ConflictResolution::Skip => {
            if policy == ConflictPolicy::OverwriteInvalid && desired.exists() {
                let outcome = verify_existing_output(progress, desired, verify).await;
                Ok(match outcome {
                    VerifyOutcome::Valid => None,
                    VerifyOutcome::Invalid => Some(desired.to_path_buf()),
                })
            } else {
                Ok(None)
            }
        }
    }
}

/// Build the dry-run plan line for a single write, mirroring the CLI's
/// per-file planning: resolve the conflict, and for `overwrite-invalid` run the
/// read-only verify to choose keep-valid versus rewrite-invalid. Nothing is
/// written; the only filesystem access is reading the input and the read-only
/// verify. The resulting `PlanLine` renders byte-identically to the CLI plan.
#[allow(clippy::too_many_arguments)]
async fn plan_line(
    progress: &dyn rom_converto_lib::util::ProgressReporter,
    operation: &str,
    input: &Path,
    desired: &Path,
    on_conflict: Option<&str>,
    media: Option<String>,
    verify: rom_converto_lib::util::OutputVerify,
    missing_keys: Option<String>,
) -> Result<PlanLine, String> {
    use rom_converto_lib::util::{PlanDecision, VerifyOutcome, classify, verify_existing_output};
    let policy = conflict_policy(on_conflict);
    let resolution = resolve_conflict(desired, policy).map_err(err_to_string)?;
    let (output, decision) = if policy == ConflictPolicy::OverwriteInvalid && desired.exists() {
        match verify_existing_output(progress, desired, verify).await {
            VerifyOutcome::Valid => (desired.to_path_buf(), PlanDecision::KeepValid),
            VerifyOutcome::Invalid => (desired.to_path_buf(), PlanDecision::RewriteInvalid),
        }
    } else {
        let out = match &resolution {
            ConflictResolution::Write(p) => p.clone(),
            ConflictResolution::Skip => desired.to_path_buf(),
        };
        (out, classify(desired, &resolution))
    };
    Ok(PlanLine {
        operation: operation.to_string(),
        input: input.to_path_buf(),
        output,
        decision,
        media,
        missing_keys,
    })
}

/// Best-effort media label for a CHD dry-run plan line, mirroring the CLI:
/// cue inputs imply a CD, ISO inputs read a header to predict the disc kind.
fn chd_media_label(input: &Path) -> Option<String> {
    let ext = input
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("cue") => Some("CD".to_string()),
        Some("iso") => rom_converto_lib::util::iso9660::detect_disc_kind(input)
            .ok()
            .map(|k| k.label().to_string()),
        _ => None,
    }
}

fn input_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn preflight_space(output_dir: &Path, required_bytes: u64, skip: bool) -> Result<(), String> {
    if skip {
        return Ok(());
    }
    let probe = output_dir
        .ancestors()
        .find(|p| p.exists())
        .unwrap_or(output_dir);
    match available_space(probe) {
        Ok(available) => {
            if space_shortfall(available, required_bytes, DEFAULT_SPACE_HEADROOM).is_some() {
                return Err(format!(
                    "Not enough free space at {}: need about {}, only {} available. Turn on Skip free space check to proceed anyway.",
                    output_dir.display(),
                    format_bytes(required_bytes.saturating_add(DEFAULT_SPACE_HEADROOM)),
                    format_bytes(available),
                ));
            }
            Ok(())
        }
        Err(_) => Ok(()),
    }
}

fn render_hash_row(
    path: &Path,
    d: &rom_converto_lib::util::FileDigests,
    algos: &[HashAlgo],
) -> String {
    let cells: Vec<String> = algos
        .iter()
        .map(|a| format!("{}={}", a.label(), d.value(*a).unwrap_or("")))
        .collect();
    format!("{}  {}", path.display(), cells.join("  "))
}

/// Single-slot holder for the token of the operation currently running.
/// Only one conversion runs at a time per command invocation, so a
/// single slot is enough; `cmd_cancel` fires whatever is in it.
pub type ActiveCancel = Arc<tokio::sync::Mutex<Option<CancelToken>>>;

async fn begin(state: &State<'_, ActiveCancel>) -> CancelToken {
    let token = CancelToken::new();
    *state.lock().await = Some(token.clone());
    token
}

async fn finish(state: &State<'_, ActiveCancel>) {
    *state.lock().await = None;
}

#[tauri::command]
pub async fn cmd_cancel(state: State<'_, ActiveCancel>) -> Result<(), String> {
    if let Some(token) = state.lock().await.as_ref() {
        token.cancel();
    }
    Ok(())
}

/// Write a run report from records the frontend accumulated during a run. The
/// format is inferred from the path extension and the file is written directly,
/// bypassing the on-conflict machinery, exactly as the CLI does.
#[tauri::command]
pub async fn cmd_write_report(path: PathBuf, payload: ReportPayload) -> Result<(), String> {
    let format = ReportFormat::from_path(&path);
    tokio::task::spawn_blocking(move || {
        write_report(&path, &payload.records, &payload.totals, format)
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)
}

/// Size of `path` in bytes, or 0 if it cannot be read. Used to fill the
/// `input_bytes` field of a failed record on the frontend, matching the CLI,
/// whose `failed_record` carries the input file size.
#[tauri::command]
pub fn cmd_file_size(path: PathBuf) -> u64 {
    input_size(&path)
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn cmd_cdn_to_cia(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    cdn_dir: PathBuf,
    output: Option<PathBuf>,
    decrypt: bool,
    compress: bool,
    cleanup: bool,
    recursive: bool,
    ensure_ticket_exists: bool,
    on_conflict: Option<String>,
    skip_space_check: bool,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app.clone(), "cdn-to-cia"));
    let total_progress = Arc::new(TauriProgress::new(app, "cdn-to-cia-total"));
    let opts = CdnToCiaOptions {
        cdn_dir,
        output,
        cleanup,
        recursive,
        ensure_ticket_exists,
        decrypt,
        compress,
        output_dir: None,
        on_conflict: conflict_policy(on_conflict.as_deref()),
    };
    let required: u64 = collect_all_files(&opts.cdn_dir, None)
        .map(|files| files.iter().map(|p| input_size(p)).sum())
        .unwrap_or(0);
    let probe_dir = opts
        .output
        .as_deref()
        .and_then(|p| p.parent())
        .or_else(|| opts.cdn_dir.parent())
        .unwrap_or(opts.cdn_dir.as_path());
    preflight_space(probe_dir, required, skip_space_check)?;
    let token = begin(&state).await;
    // The streaming decrypt holds the worker-pool receiver across await points,
    // so its future is not Send; run on a dedicated thread with its own runtime.
    let result = std::thread::spawn(move || -> Result<(), String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(err_to_string)?;
        rt.block_on(convert_cdn_to_cia_cancellable(
            opts,
            progress.as_ref(),
            total_progress.as_ref(),
            token,
        ))
        .map_err(err_to_string)
    })
    .join()
    .map_err(|_| "task panicked".to_string());
    finish(&state).await;
    result??;
    Ok("CDN to CIA conversion complete".to_string())
}

#[tauri::command]
pub async fn cmd_generate_ticket(cdn_dir: PathBuf, output: PathBuf) -> Result<String, String> {
    let out_display = output.display().to_string();
    tokio::spawn(async move { generate_ticket_from_cdn(&cdn_dir, &output).await })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?;
    Ok(format!("Ticket generated at {out_display}"))
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn cmd_decrypt_rom(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    output: Option<PathBuf>,
    on_conflict: Option<String>,
    skip_space_check: bool,
    output_template: Option<String>,
    dry_run: Option<bool>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "decrypt"));
    let dry_run = dry_run.unwrap_or(false);
    let ext = ext_of(&derive_decrypted_path(&input));
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &input,
        &ext,
        None,
        || derive_decrypted_path(&input),
        dry_run,
    )?;
    if dry_run {
        let line = plan_line(
            progress.as_ref(),
            "decrypt",
            &input,
            &desired,
            on_conflict.as_deref(),
            None,
            rom_converto_lib::util::OutputVerify::None,
            None,
        )
        .await?;
        return Ok(line.display_text());
    }
    let output = match resolve_output(
        progress.as_ref(),
        &desired,
        on_conflict.as_deref(),
        rom_converto_lib::util::OutputVerify::None,
    )
    .await?
    {
        Some(p) => p,
        None => return Ok(format!("skipped existing {}", desired.display())),
    };
    let out_display = output.display().to_string();
    preflight_space(
        output.parent().unwrap_or(&output),
        input_size(&input),
        skip_space_check,
    )?;
    let token = begin(&state).await;
    // The streaming decrypt holds the worker-pool receiver across await points,
    // so its future is not Send; run on a dedicated thread with its own runtime.
    let result = std::thread::spawn(move || -> Result<(), String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(err_to_string)?;
        rt.block_on(decrypt_rom_cancellable(
            &input,
            &output,
            progress.as_ref(),
            token,
        ))
        .map_err(err_to_string)
    })
    .join()
    .map_err(|_| "task panicked".to_string());
    finish(&state).await;
    result??;
    Ok(format!("Decrypted to {out_display}"))
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn cmd_compress_rom(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    output: Option<PathBuf>,
    level: Option<i32>,
    allow_encrypted: bool,
    on_conflict: Option<String>,
    skip_space_check: bool,
    output_template: Option<String>,
    dry_run: Option<bool>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "compress"));
    let dry_run = dry_run.unwrap_or(false);
    let ext = ext_of(&derive_compressed_path(&input));
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &input,
        &ext,
        None,
        || derive_compressed_path(&input),
        dry_run,
    )?;
    if dry_run {
        let line = plan_line(
            progress.as_ref(),
            "compress",
            &input,
            &desired,
            on_conflict.as_deref(),
            None,
            rom_converto_lib::util::OutputVerify::None,
            None,
        )
        .await?;
        return Ok(line.display_text());
    }
    let output = match resolve_output(
        progress.as_ref(),
        &desired,
        on_conflict.as_deref(),
        rom_converto_lib::util::OutputVerify::None,
    )
    .await?
    {
        Some(p) => p,
        None => return Ok(format!("skipped existing {}", desired.display())),
    };
    let out_display = output.display().to_string();
    preflight_space(
        output.parent().unwrap_or(&output),
        input_size(&input),
        skip_space_check,
    )?;
    let token = begin(&state).await;
    let result = tokio::spawn(async move {
        compress_rom_cancellable(
            &input,
            &output,
            level,
            allow_encrypted,
            progress.as_ref(),
            token,
        )
        .await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    Ok(format!("Compressed to {out_display}"))
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn cmd_decompress_rom(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    output: Option<PathBuf>,
    on_conflict: Option<String>,
    skip_space_check: bool,
    output_template: Option<String>,
    dry_run: Option<bool>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "decompress"));
    let dry_run = dry_run.unwrap_or(false);
    let ext = ext_of(&derive_decompressed_path(&input));
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &input,
        &ext,
        None,
        || derive_decompressed_path(&input),
        dry_run,
    )?;
    if dry_run {
        let line = plan_line(
            progress.as_ref(),
            "decompress",
            &input,
            &desired,
            on_conflict.as_deref(),
            None,
            rom_converto_lib::util::OutputVerify::None,
            None,
        )
        .await?;
        return Ok(line.display_text());
    }
    let output = match resolve_output(
        progress.as_ref(),
        &desired,
        on_conflict.as_deref(),
        rom_converto_lib::util::OutputVerify::None,
    )
    .await?
    {
        Some(p) => p,
        None => return Ok(format!("skipped existing {}", desired.display())),
    };
    let out_display = output.display().to_string();
    preflight_space(
        output.parent().unwrap_or(&output),
        input_size(&input),
        skip_space_check,
    )?;
    let token = begin(&state).await;
    let result = tokio::spawn(async move {
        decompress_rom_cancellable(&input, &output, progress.as_ref(), token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    Ok(format!("Decompressed to {out_display}"))
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn cmd_chd_compress(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input_path: PathBuf,
    output: Option<PathBuf>,
    zstd: Option<bool>,
    hunk_size: Option<u32>,
    mode: Option<String>,
    on_conflict: Option<String>,
    skip_space_check: bool,
    output_template: Option<String>,
    report: Option<bool>,
    dry_run: Option<bool>,
) -> Result<RunOutcome, String> {
    let progress = Arc::new(TauriProgress::new(app, "chd-compress"));
    let dry_run = dry_run.unwrap_or(false);
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &input_path,
        "chd",
        None,
        || input_path.with_extension("chd"),
        dry_run,
    )?;
    if dry_run {
        let line = plan_line(
            progress.as_ref(),
            "compress",
            &input_path,
            &desired,
            on_conflict.as_deref(),
            chd_media_label(&input_path),
            rom_converto_lib::util::OutputVerify::Chd,
            None,
        )
        .await?;
        return Ok(RunOutcome {
            message: line.display_text(),
            record: None,
        });
    }
    let output = match resolve_output(
        progress.as_ref(),
        &desired,
        on_conflict.as_deref(),
        rom_converto_lib::util::OutputVerify::Chd,
    )
    .await?
    {
        Some(p) => p,
        None => {
            return Ok(RunOutcome::skipped(
                report.unwrap_or(false),
                &input_path,
                "compress",
                &desired,
            ));
        }
    };
    let out_display = output.display().to_string();
    preflight_space(
        output.parent().unwrap_or(&output),
        input_size(&input_path),
        skip_space_check,
    )?;
    let mode = match mode.as_deref() {
        Some("cd") => Some(DiscMode::Cd),
        Some("dvd") => Some(DiscMode::Dvd),
        _ => None,
    };
    let opts = ChdDvdOptions {
        hunk_size,
        allow_zstd: zstd.unwrap_or(false),
        force: true,
    };
    let in_bytes = input_size(&input_path);
    let record_input = input_path.clone();
    let record_output = output.clone();
    let token = begin(&state).await;
    let started = Instant::now();
    let result = tokio::spawn(async move {
        convert_disc_to_chd_cancellable(progress.as_ref(), input_path, output, mode, opts, token)
            .await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    let record = build_record(
        report.unwrap_or(false),
        &record_input,
        &record_output,
        "compress",
        in_bytes,
        input_size(&record_output),
        started.elapsed(),
    );
    Ok(RunOutcome {
        message: format!("CHD created at {out_display}"),
        record,
    })
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn cmd_cso_compress(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input_path: PathBuf,
    output: Option<PathBuf>,
    format: String,
    block_size: Option<u32>,
    on_conflict: Option<String>,
    skip_space_check: bool,
    output_template: Option<String>,
    report: Option<bool>,
    dry_run: Option<bool>,
) -> Result<RunOutcome, String> {
    let format = match format.as_str() {
        "zso" => CsoFormat::Zso,
        _ => CsoFormat::Cso,
    };
    let format_name = format.name();
    let progress = Arc::new(TauriProgress::new(app, "cso-compress"));
    let dry_run = dry_run.unwrap_or(false);
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &input_path,
        format.extension(),
        None,
        || input_path.with_extension(format.extension()),
        dry_run,
    )?;
    if dry_run {
        let line = plan_line(
            progress.as_ref(),
            "compress",
            &input_path,
            &desired,
            on_conflict.as_deref(),
            Some(format_name.to_string()),
            rom_converto_lib::util::OutputVerify::Cso,
            None,
        )
        .await?;
        return Ok(RunOutcome {
            message: line.display_text(),
            record: None,
        });
    }
    let output = match resolve_output(
        progress.as_ref(),
        &desired,
        on_conflict.as_deref(),
        rom_converto_lib::util::OutputVerify::Cso,
    )
    .await?
    {
        Some(p) => p,
        None => {
            return Ok(RunOutcome::skipped(
                report.unwrap_or(false),
                &input_path,
                "compress",
                &desired,
            ));
        }
    };
    let out_display = output.display().to_string();
    preflight_space(
        output.parent().unwrap_or(&output),
        input_size(&input_path),
        skip_space_check,
    )?;
    let opts = CsoCompressOptions {
        format,
        block_size,
        force: true,
    };
    let in_bytes = input_size(&input_path);
    let record_input = input_path.clone();
    let record_output = output.clone();
    let token = begin(&state).await;
    let started = Instant::now();
    let result = tokio::spawn(async move {
        compress_to_cso_cancellable(progress.as_ref(), input_path, output, opts, token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    let record = build_record(
        report.unwrap_or(false),
        &record_input,
        &record_output,
        "compress",
        in_bytes,
        input_size(&record_output),
        started.elapsed(),
    );
    Ok(RunOutcome {
        message: format!("{format_name} created at {out_display}"),
        record,
    })
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn cmd_cso_decompress(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input_path: PathBuf,
    output: Option<PathBuf>,
    on_conflict: Option<String>,
    skip_space_check: bool,
    output_template: Option<String>,
    report: Option<bool>,
    dry_run: Option<bool>,
) -> Result<RunOutcome, String> {
    let progress = Arc::new(TauriProgress::new(app, "cso-decompress"));
    let dry_run = dry_run.unwrap_or(false);
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &input_path,
        "iso",
        None,
        || input_path.with_extension("iso"),
        dry_run,
    )?;
    if dry_run {
        let line = plan_line(
            progress.as_ref(),
            "decompress",
            &input_path,
            &desired,
            on_conflict.as_deref(),
            None,
            rom_converto_lib::util::OutputVerify::None,
            None,
        )
        .await?;
        return Ok(RunOutcome {
            message: line.display_text(),
            record: None,
        });
    }
    let output = match resolve_output(
        progress.as_ref(),
        &desired,
        on_conflict.as_deref(),
        rom_converto_lib::util::OutputVerify::None,
    )
    .await?
    {
        Some(p) => p,
        None => {
            return Ok(RunOutcome::skipped(
                report.unwrap_or(false),
                &input_path,
                "decompress",
                &desired,
            ));
        }
    };
    let out_display = output.display().to_string();
    preflight_space(
        output.parent().unwrap_or(&output),
        input_size(&input_path),
        skip_space_check,
    )?;
    let in_bytes = input_size(&input_path);
    let record_input = input_path.clone();
    let record_output = output.clone();
    let token = begin(&state).await;
    let started = Instant::now();
    let result = tokio::spawn(async move {
        decompress_from_cso_cancellable(progress.as_ref(), input_path, output, true, token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    let record = build_record(
        report.unwrap_or(false),
        &record_input,
        &record_output,
        "decompress",
        in_bytes,
        input_size(&record_output),
        started.elapsed(),
    );
    Ok(RunOutcome {
        message: format!("ISO restored at {out_display}"),
        record,
    })
}

#[tauri::command]
pub async fn cmd_cso_verify(
    app: AppHandle,
    input_path: PathBuf,
    full: bool,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "cso-verify"));
    tokio::spawn(async move { verify_cso(progress.as_ref(), input_path.clone(), full).await })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?;
    Ok(if full {
        "Index structure OK, all blocks decoded successfully".to_string()
    } else {
        "Index structure OK".to_string()
    })
}

#[tauri::command]
pub async fn cmd_cue_merge(
    app: AppHandle,
    cue_path: PathBuf,
    output: PathBuf,
    on_conflict: Option<String>,
    skip_space_check: bool,
    dry_run: Option<bool>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "cue-merge"));
    if dry_run.unwrap_or(false) {
        let line = plan_line(
            progress.as_ref(),
            "merge",
            &cue_path,
            &output,
            on_conflict.as_deref(),
            Some(format!("+ {}", output.with_extension("bin").display())),
            rom_converto_lib::util::OutputVerify::None,
            None,
        )
        .await?;
        return Ok(line.display_text());
    }
    let output = match resolve_output(
        progress.as_ref(),
        &output,
        on_conflict.as_deref(),
        rom_converto_lib::util::OutputVerify::None,
    )
    .await?
    {
        Some(p) => p,
        None => return Ok(format!("skipped existing {}", output.display())),
    };
    let out_display = output.display().to_string();
    preflight_space(
        output.parent().unwrap_or(&output),
        input_size(&cue_path),
        skip_space_check,
    )?;
    tokio::spawn(async move { merge_bin(progress.as_ref(), cue_path, output, true).await })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?;
    Ok(format!("Merged bin/cue created at {out_display}"))
}

// CHD extract and verify use deeply nested async types from ChdReader
// that exceed the compiler's recursion limit for Send inference. We run
// these on a dedicated thread with its own tokio runtime to sidestep the issue.

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn cmd_chd_extract(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    output: Option<PathBuf>,
    parent: Option<PathBuf>,
    skip_space_check: bool,
    output_template: Option<String>,
    report: Option<bool>,
    dry_run: Option<bool>,
) -> Result<RunOutcome, String> {
    let progress = Arc::new(TauriProgress::new(app, "chd-extract"));
    let dry_run = dry_run.unwrap_or(false);
    let output = pick_output(
        output,
        output_template.as_deref(),
        &input,
        "cue",
        None,
        || input.with_extension("cue"),
        dry_run,
    )?;
    if dry_run {
        let line = plan_line(
            progress.as_ref(),
            "extract",
            &input,
            &output,
            None,
            None,
            rom_converto_lib::util::OutputVerify::None,
            None,
        )
        .await?;
        return Ok(RunOutcome {
            message: line.display_text(),
            record: None,
        });
    }
    let out_display = output.display().to_string();
    preflight_space(
        output.parent().unwrap_or(&output),
        input_size(&input),
        skip_space_check,
    )?;
    let record_input = input.clone();
    let record_output = output.clone();
    let token = begin(&state).await;
    let started = Instant::now();
    // ChdReader's deeply nested async types exceed the compiler's Send recursion
    // limit, so we run on a dedicated thread with its own tokio runtime.
    let result = std::thread::spawn(move || -> Result<(), String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(err_to_string)?;
        rt.block_on(extract_from_chd_cancellable(
            progress.as_ref(),
            input,
            output,
            parent,
            token,
        ))
        .map_err(err_to_string)
    })
    .join()
    .map_err(|_| "task panicked".to_string());
    finish(&state).await;
    result??;
    let record = build_record(
        report.unwrap_or(false),
        &record_input,
        &record_output,
        "extract",
        0,
        0,
        started.elapsed(),
    );
    Ok(RunOutcome {
        message: format!("Extracted to {out_display}"),
        record,
    })
}

#[tauri::command]
pub async fn cmd_chd_verify(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    parent: Option<PathBuf>,
    fix: bool,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "chd-verify"));
    let token = begin(&state).await;
    let result = std::thread::spawn(move || -> Result<(), String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(err_to_string)?;
        rt.block_on(verify_chd_cancellable(
            progress.as_ref(),
            input,
            parent,
            fix,
            token,
        ))
        .map_err(err_to_string)
    })
    .join()
    .map_err(|_| "task panicked".to_string());
    finish(&state).await;
    result??;
    Ok("CHD verification passed".to_string())
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn cmd_compress_disc(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    output: Option<PathBuf>,
    level: Option<i32>,
    chunk_size: Option<u32>,
    task_id: String,
    on_conflict: Option<String>,
    skip_space_check: bool,
    output_template: Option<String>,
    report: Option<bool>,
    dry_run: Option<bool>,
) -> Result<RunOutcome, String> {
    let progress = Arc::new(TauriProgress::new(app, &task_id));
    let dry_run = dry_run.unwrap_or(false);
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &input,
        "rvz",
        None,
        || derive_rvz_path(&input),
        dry_run,
    )?;
    if dry_run {
        let line = plan_line(
            progress.as_ref(),
            "compress",
            &input,
            &desired,
            on_conflict.as_deref(),
            Some("RVZ".to_string()),
            rom_converto_lib::util::OutputVerify::Rvz,
            None,
        )
        .await?;
        return Ok(RunOutcome {
            message: line.display_text(),
            record: None,
        });
    }
    let output = match resolve_output(
        progress.as_ref(),
        &desired,
        on_conflict.as_deref(),
        rom_converto_lib::util::OutputVerify::Rvz,
    )
    .await?
    {
        Some(p) => p,
        None => {
            return Ok(RunOutcome::skipped(
                report.unwrap_or(false),
                &input,
                "compress",
                &desired,
            ));
        }
    };
    let out_display = output.display().to_string();
    preflight_space(
        output.parent().unwrap_or(&output),
        input_size(&input),
        skip_space_check,
    )?;
    let opts = RvzCompressOptions {
        compression_level: level.unwrap_or(RvzCompressOptions::default().compression_level),
        chunk_size: chunk_size.unwrap_or(RvzCompressOptions::default().chunk_size),
        ..RvzCompressOptions::default()
    };
    let in_bytes = input_size(&input);
    let record_input = input.clone();
    let record_output = output.clone();
    let token = begin(&state).await;
    let started = Instant::now();
    let result = tokio::spawn(async move {
        compress_disc_cancellable(&input, &output, opts, progress.as_ref(), token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    let record = build_record(
        report.unwrap_or(false),
        &record_input,
        &record_output,
        "compress",
        in_bytes,
        input_size(&record_output),
        started.elapsed(),
    );
    Ok(RunOutcome {
        message: format!("Compressed to {out_display}"),
        record,
    })
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn cmd_decompress_disc(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    output: Option<PathBuf>,
    task_id: String,
    on_conflict: Option<String>,
    skip_space_check: bool,
    output_template: Option<String>,
    report: Option<bool>,
    dry_run: Option<bool>,
) -> Result<RunOutcome, String> {
    let progress = Arc::new(TauriProgress::new(app, &task_id));
    let dry_run = dry_run.unwrap_or(false);
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &input,
        "iso",
        None,
        || derive_disc_path(&input),
        dry_run,
    )?;
    if dry_run {
        let line = plan_line(
            progress.as_ref(),
            "decompress",
            &input,
            &desired,
            on_conflict.as_deref(),
            None,
            rom_converto_lib::util::OutputVerify::None,
            None,
        )
        .await?;
        return Ok(RunOutcome {
            message: line.display_text(),
            record: None,
        });
    }
    let output = match resolve_output(
        progress.as_ref(),
        &desired,
        on_conflict.as_deref(),
        rom_converto_lib::util::OutputVerify::None,
    )
    .await?
    {
        Some(p) => p,
        None => {
            return Ok(RunOutcome::skipped(
                report.unwrap_or(false),
                &input,
                "decompress",
                &desired,
            ));
        }
    };
    let out_display = output.display().to_string();
    preflight_space(
        output.parent().unwrap_or(&output),
        input_size(&input),
        skip_space_check,
    )?;
    let to_wbfs = output
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.eq_ignore_ascii_case("wbfs"))
        .unwrap_or(false);
    let in_bytes = input_size(&input);
    let record_input = input.clone();
    let record_output = output.clone();
    let token = begin(&state).await;
    let started = Instant::now();
    let result = tokio::spawn(async move {
        if to_wbfs {
            decompress_disc_to_wbfs_cancellable(&input, &output, progress.as_ref(), token).await
        } else {
            decompress_disc_cancellable(&input, &output, progress.as_ref(), token).await
        }
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    let record = build_record(
        report.unwrap_or(false),
        &record_input,
        &record_output,
        "decompress",
        in_bytes,
        input_size(&record_output),
        started.elapsed(),
    );
    Ok(RunOutcome {
        message: format!("Decompressed to {out_display}"),
        record,
    })
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn cmd_wup_compress(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    inputs: Vec<PathBuf>,
    output: PathBuf,
    level: Option<i32>,
    keys: Option<Vec<PathBuf>>,
    on_conflict: Option<String>,
    skip_space_check: bool,
    dry_run: Option<bool>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "wup-compress"));
    if dry_run.unwrap_or(false) {
        use rom_converto_lib::nintendo::wup::compress::{TitleInputFormat, detect_title_format};
        let media = inputs
            .first()
            .and_then(|p| detect_title_format(p).ok())
            .map(|f| match f {
                TitleInputFormat::Loadiine => "Loadiine",
                TitleInputFormat::Nus => "NUS",
                TitleInputFormat::Disc => "disc",
            });
        let input = inputs.first().cloned().unwrap_or_else(|| output.clone());
        let line = plan_line(
            progress.as_ref(),
            "compress",
            &input,
            &output,
            on_conflict.as_deref(),
            media.map(str::to_string),
            rom_converto_lib::util::OutputVerify::None,
            None,
        )
        .await?;
        return Ok(line.display_text());
    }
    let output = match resolve_output(
        progress.as_ref(),
        &output,
        on_conflict.as_deref(),
        rom_converto_lib::util::OutputVerify::None,
    )
    .await?
    {
        Some(p) => p,
        None => return Ok(format!("skipped existing {}", output.display())),
    };
    let out_display = output.display().to_string();
    let required: u64 = inputs.iter().map(|p| input_size(p)).sum();
    preflight_space(
        output.parent().unwrap_or(&output),
        required,
        skip_space_check,
    )?;
    let opts = WupCompressOptions {
        zstd_level: level.unwrap_or(WupCompressOptions::default().zstd_level),
    };
    // Pair each supplied key with the next disc input in positional
    // order. Non-disc inputs do not consume a key slot.
    let mut key_iter = keys.unwrap_or_default().into_iter();
    let titles: Vec<TitleInput> = inputs
        .into_iter()
        .map(|p| {
            let is_disc = p
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("wud") || s.eq_ignore_ascii_case("wux"))
                .unwrap_or(false)
                && p.is_file();
            let mut t = TitleInput::auto(p);
            if is_disc {
                t.key_path = key_iter.next();
            }
            t
        })
        .collect();
    let token = begin(&state).await;
    let result = tokio::spawn(async move {
        compress_titles_async_cancellable(titles, output, opts, progress.as_ref(), token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    Ok(format!("Compressed to {out_display}"))
}

#[tauri::command]
pub async fn cmd_wup_decrypt(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    output: PathBuf,
    on_conflict: Option<String>,
    skip_space_check: bool,
    dry_run: Option<bool>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "wup-decrypt"));
    if dry_run.unwrap_or(false) {
        let line = plan_line(
            progress.as_ref(),
            "decrypt",
            &input,
            &output,
            on_conflict.as_deref(),
            None,
            rom_converto_lib::util::OutputVerify::None,
            None,
        )
        .await?;
        return Ok(line.display_text());
    }
    let output = match resolve_output(
        progress.as_ref(),
        &output,
        on_conflict.as_deref(),
        rom_converto_lib::util::OutputVerify::None,
    )
    .await?
    {
        Some(p) => p,
        None => return Ok(format!("skipped existing {}", output.display())),
    };
    let out_display = output.display().to_string();
    preflight_space(
        output.parent().unwrap_or(&output),
        input_size(&input),
        skip_space_check,
    )?;
    let token = begin(&state).await;
    let result = tokio::spawn(async move {
        decrypt_nus_title_async_cancellable(input, output, progress.as_ref(), token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    Ok(format!("Decrypted to {out_display}"))
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn cmd_nx_compress(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    output: Option<PathBuf>,
    keys: Option<PathBuf>,
    level: Option<i32>,
    mode: Option<String>,
    block_size_exp: Option<u8>,
    on_conflict: Option<String>,
    skip_space_check: bool,
    output_template: Option<String>,
    report: Option<bool>,
    dry_run: Option<bool>,
) -> Result<RunOutcome, String> {
    let progress = Arc::new(TauriProgress::new(app, "nx-compress"));
    let dry_run = dry_run.unwrap_or(false);
    let kind = detect_container(&input).map_err(err_to_string)?;
    let mut opts = NxCompressOptions::for_kind(kind);
    if let Some(level) = level {
        opts.level = level;
    }
    if let Some(mode) = mode.as_deref() {
        opts.mode = match mode {
            "solid" => NczMode::Solid,
            "block" => NczMode::Block {
                size_exp: block_size_exp.unwrap_or(20),
            },
            other => return Err(format!("unknown mode {other:?}")),
        };
    } else if let Some(exp) = block_size_exp {
        opts.mode = NczMode::Block { size_exp: exp };
    }
    let ext = ext_of(&nx_derive_compressed_path(&input));
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &input,
        &ext,
        keys.as_deref(),
        || nx_derive_compressed_path(&input),
        dry_run,
    )?;
    if dry_run {
        let (keyset, missing) = match load_keyset(keys.as_deref()) {
            Ok(k) => (k, None),
            Err(e) => (KeySet::default(), Some(e.to_string())),
        };
        let line = plan_line(
            progress.as_ref(),
            "compress",
            &input,
            &desired,
            on_conflict.as_deref(),
            Some(format!("{kind:?}")),
            rom_converto_lib::util::OutputVerify::Nx(Box::new(keyset)),
            missing,
        )
        .await?;
        return Ok(RunOutcome {
            message: line.display_text(),
            record: None,
        });
    }
    let keys = load_keyset(keys.as_deref()).map_err(err_to_string)?;
    let output = match resolve_output(
        progress.as_ref(),
        &desired,
        on_conflict.as_deref(),
        rom_converto_lib::util::OutputVerify::Nx(Box::new(keys.clone())),
    )
    .await?
    {
        Some(p) => p,
        None => {
            return Ok(RunOutcome::skipped(
                report.unwrap_or(false),
                &input,
                "compress",
                &desired,
            ));
        }
    };
    let out_display = output.display().to_string();
    preflight_space(
        output.parent().unwrap_or(&output),
        input_size(&input),
        skip_space_check,
    )?;
    let in_bytes = input_size(&input);
    let record_input = input.clone();
    let record_output = output.clone();
    let token = begin(&state).await;
    let started = Instant::now();
    let result = tokio::spawn(async move {
        compress_container_async_cancellable(input, output, opts, keys, progress.as_ref(), token)
            .await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    let record = build_record(
        report.unwrap_or(false),
        &record_input,
        &record_output,
        "compress",
        in_bytes,
        input_size(&record_output),
        started.elapsed(),
    );
    Ok(RunOutcome {
        message: format!("Compressed to {out_display}"),
        record,
    })
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn cmd_nx_decompress(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    output: Option<PathBuf>,
    keys: Option<PathBuf>,
    on_conflict: Option<String>,
    skip_space_check: bool,
    output_template: Option<String>,
    report: Option<bool>,
    dry_run: Option<bool>,
) -> Result<RunOutcome, String> {
    let progress = Arc::new(TauriProgress::new(app, "nx-decompress"));
    let dry_run = dry_run.unwrap_or(false);
    let ext = ext_of(&nx_derive_decompressed_path(&input));
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &input,
        &ext,
        keys.as_deref(),
        || nx_derive_decompressed_path(&input),
        dry_run,
    )?;
    if dry_run {
        let line = plan_line(
            progress.as_ref(),
            "decompress",
            &input,
            &desired,
            on_conflict.as_deref(),
            None,
            rom_converto_lib::util::OutputVerify::None,
            None,
        )
        .await?;
        return Ok(RunOutcome {
            message: line.display_text(),
            record: None,
        });
    }
    let output = match resolve_output(
        progress.as_ref(),
        &desired,
        on_conflict.as_deref(),
        rom_converto_lib::util::OutputVerify::None,
    )
    .await?
    {
        Some(p) => p,
        None => {
            return Ok(RunOutcome::skipped(
                report.unwrap_or(false),
                &input,
                "decompress",
                &desired,
            ));
        }
    };
    let out_display = output.display().to_string();
    preflight_space(
        output.parent().unwrap_or(&output),
        input_size(&input),
        skip_space_check,
    )?;
    let keys = load_keyset(keys.as_deref()).map_err(err_to_string)?;
    let in_bytes = input_size(&input);
    let record_input = input.clone();
    let record_output = output.clone();
    let token = begin(&state).await;
    let started = Instant::now();
    let result = tokio::spawn(async move {
        decompress_container_async_cancellable(input, output, keys, progress.as_ref(), token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    let record = build_record(
        report.unwrap_or(false),
        &record_input,
        &record_output,
        "decompress",
        in_bytes,
        input_size(&record_output),
        started.elapsed(),
    );
    Ok(RunOutcome {
        message: format!("Decompressed to {out_display}"),
        record,
    })
}

#[tauri::command]
pub async fn cmd_nx_verify(
    app: AppHandle,
    input: PathBuf,
    keys: Option<PathBuf>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "nx-verify"));
    let keys = load_keyset(keys.as_deref()).map_err(err_to_string)?;
    let result =
        tokio::spawn(async move { verify_container_async(input, keys, progress.as_ref()).await })
            .await
            .map_err(err_to_string)?
            .map_err(err_to_string)?;
    serde_json::to_string(&result).map_err(err_to_string)
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn cmd_convert_ctr(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    output: Option<PathBuf>,
    on_conflict: Option<String>,
    skip_space_check: bool,
    output_template: Option<String>,
    dry_run: Option<bool>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "ctr-convert"));
    let dry_run = dry_run.unwrap_or(false);
    let ext = ext_of(&derive_converted_path(&input));
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &input,
        &ext,
        None,
        || derive_converted_path(&input),
        dry_run,
    )?;
    if dry_run {
        let line = plan_line(
            progress.as_ref(),
            "convert",
            &input,
            &desired,
            on_conflict.as_deref(),
            None,
            rom_converto_lib::util::OutputVerify::None,
            None,
        )
        .await?;
        return Ok(line.display_text());
    }
    let output = match resolve_output(
        progress.as_ref(),
        &desired,
        on_conflict.as_deref(),
        rom_converto_lib::util::OutputVerify::None,
    )
    .await?
    {
        Some(p) => p,
        None => return Ok(format!("skipped existing {}", desired.display())),
    };
    let out_display = output.display().to_string();
    preflight_space(
        output.parent().unwrap_or(&output),
        input_size(&input),
        skip_space_check,
    )?;
    let token = begin(&state).await;
    let result = tokio::spawn(async move {
        convert_rom_cancellable(&input, &output, progress.as_ref(), token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    Ok(format!("Converted to {out_display}"))
}

#[tauri::command]
pub async fn cmd_verify_ctr(
    app: AppHandle,
    input: PathBuf,
    verify_content: bool,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "ctr-verify"));
    let opts = CtrVerifyOptions {
        verify_content_hashes: verify_content,
    };
    let result = tokio::spawn(async move { verify_ctr(&input, &opts, progress.as_ref()).await })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?;

    serde_json::to_string(&result).map_err(err_to_string)
}

#[tauri::command]
pub async fn cmd_verify_dol(app: AppHandle, input: PathBuf, full: bool) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "dol-verify"));
    let result = tokio::task::spawn_blocking(move || {
        let opts = DolVerifyOptions { full };
        verify_dol(&input, &opts, progress.as_ref())
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)?;

    serde_json::to_string(&result).map_err(err_to_string)
}

#[tauri::command]
pub async fn cmd_verify_rvl(app: AppHandle, input: PathBuf, full: bool) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "rvl-verify"));
    let result = tokio::task::spawn_blocking(move || {
        let opts = RvlVerifyOptions { full };
        verify_rvl(&input, &opts, progress.as_ref())
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)?;

    serde_json::to_string(&result).map_err(err_to_string)
}

#[tauri::command]
pub async fn cmd_wup_verify(
    app: AppHandle,
    input: PathBuf,
    keys: Option<PathBuf>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "wup-verify"));
    let result =
        tokio::spawn(async move { verify_wup_async(input, keys, progress.as_ref()).await })
            .await
            .map_err(err_to_string)?
            .map_err(err_to_string)?;

    serde_json::to_string(&result).map_err(err_to_string)
}

#[tauri::command]
pub async fn cmd_read_info(
    cache: State<'_, Arc<InfoCache>>,
    input: PathBuf,
    keys: Option<PathBuf>,
) -> Result<String, String> {
    let cache_inner = cache.inner().clone();
    let result = tokio::task::spawn_blocking(move || -> Result<Arc<InfoResult>, anyhow::Error> {
        if let Some(key) = InfoCache::key_for(&input)
            && let Some(hit) = cache_inner.get(&key)
        {
            return Ok(hit);
        }
        let opts = InfoOptions {
            keys_path: keys.clone(),
            parent_path: None,
        };
        let info = read_info(&input, &opts)?;
        let arc = Arc::new(info);
        if let Some(key) = InfoCache::key_for(&input) {
            cache_inner.insert(key, arc.clone());
        }
        Ok(arc)
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)?;
    serde_json::to_string(result.as_ref()).map_err(err_to_string)
}

/// The frontend posts back the InfoResult JSON it already holds, so the
/// Rust side does not need to redo the extraction.
#[tauri::command]
pub async fn cmd_save_icon(info_json: String, dest: PathBuf) -> Result<String, String> {
    let info: InfoResult = serde_json::from_str(&info_json).map_err(err_to_string)?;
    let bytes =
        extract_icon_png(&info).ok_or_else(|| "no icon present in info payload".to_string())?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(err_to_string)?;
    }
    std::fs::write(&dest, &bytes).map_err(err_to_string)?;
    Ok(dest.display().to_string())
}

#[tauri::command]
pub async fn cmd_hash(
    app: AppHandle,
    input: PathBuf,
    algos: Vec<String>,
    recursive: bool,
    max_depth: Option<usize>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "hash"));
    tokio::task::spawn_blocking(move || -> Result<String, String> {
        let parsed = parse_algos(&algos.join(","))?;
        let mut lines = Vec::new();
        if recursive {
            let files = collect_all_files(&input, max_depth).map_err(err_to_string)?;
            if files.is_empty() {
                return Ok(format!("no files found in {}", input.display()));
            }
            for file in files {
                let digests =
                    hash_file(&file, &parsed, progress.as_ref()).map_err(err_to_string)?;
                lines.push(render_hash_row(&file, &digests, &parsed));
            }
        } else {
            let digests = hash_file(&input, &parsed, progress.as_ref()).map_err(err_to_string)?;
            lines.push(render_hash_row(&input, &digests, &parsed));
        }
        Ok(lines.join("\n"))
    })
    .await
    .map_err(err_to_string)?
}

#[tauri::command]
pub async fn cmd_playlist(
    scan_dir: PathBuf,
    output_dir: Option<PathBuf>,
    mode: String,
    extensions: String,
    max_depth: Option<usize>,
    on_conflict: Option<String>,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || -> Result<String, String> {
        let exts: Vec<String> = extensions
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        let ext_refs: Vec<&str> = exts.iter().map(String::as_str).collect();
        let pmode = if mode == "always" {
            PlaylistMode::Always
        } else {
            PlaylistMode::Multiple
        };
        let plans = plan_playlists(&PlaylistOptions {
            scan_dir: &scan_dir,
            output_dir: output_dir.as_deref(),
            extensions: &ext_refs,
            mode: pmode,
            max_depth,
        })
        .map_err(err_to_string)?;
        if let Some(dir) = output_dir.as_deref() {
            std::fs::create_dir_all(dir).map_err(err_to_string)?;
        }
        let policy = conflict_policy(on_conflict.as_deref());
        let mut written = 0usize;
        let mut skipped = 0usize;
        for plan in &plans {
            match resolve_conflict(&plan.m3u_path, policy).map_err(err_to_string)? {
                ConflictResolution::Write(p) => {
                    std::fs::write(&p, &plan.contents).map_err(err_to_string)?;
                    written += 1;
                }
                ConflictResolution::Skip => skipped += 1,
            }
        }
        Ok(format!("{written} playlists written, {skipped} skipped"))
    })
    .await
    .map_err(err_to_string)?
}

/// Recursively scan `dir` for files matching `exts`, applying the same junk
/// filter and sort as the CLI batch walk. A non-directory path surfaces as an
/// error so the caller can fall back to treating it as a single file.
#[tauri::command]
pub async fn cmd_scan_dir(
    dir: PathBuf,
    exts: Vec<String>,
    max_depth: Option<usize>,
) -> Result<Vec<PathBuf>, String> {
    tokio::task::spawn_blocking(move || {
        let ext_refs: Vec<&str> = exts.iter().map(String::as_str).collect();
        collect_files_with_exts(&dir, &ext_refs, max_depth).map_err(err_to_string)
    })
    .await
    .map_err(err_to_string)?
}

#[tauri::command]
pub fn app_display_version() -> &'static str {
    env!("ROM_CONVERTO_DISPLAY_VERSION")
}

fn extract_icon_png(info: &InfoResult) -> Option<Vec<u8>> {
    match info {
        InfoResult::Ctr(c) => c.icon.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::Dol(d) => d.banner_image.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::Rvl(r) => r.image.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::Wup(w) => w.image.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::Nx(n) => n
            .full
            .as_ref()
            .and_then(|f| f.control.as_ref())
            .and_then(|c| c.icon.as_ref())
            .map(|i| i.png_bytes.clone()),
        InfoResult::Chd(_) => None,
        InfoResult::Cso(_) => None,
    }
}
