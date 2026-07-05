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
use rom_converto_lib::dat::model::{
    BulkIdentifyIdsResult, BulkIdentifyItem, BulkItemStatus, GameAndRelationMatchResult,
    GameFileMatchSearch,
};
use rom_converto_lib::dat::rename::{RenameAction, RenameCandidate, RenamePlan, plan_renames};
use rom_converto_lib::dat::verdict::{DatVerdict, MatchStrength, match_strength, reconcile_tracks};
use rom_converto_lib::dat::{
    DEFAULT_API_BASE, PlaymatchClient, RomDigests, TrackDigests, digest_inner_async,
};
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
    decompress_disc_to_wbfs_cancellable, derive_disc_path, derive_rvz_path, verify_rvz_structure,
};
use rom_converto_lib::nintendo::wup::{
    TitleInput, WupCompressOptions, compress_titles_async_cancellable,
    decrypt_nus_title_async_cancellable, verify_wup_async,
};
use rom_converto_lib::playlist::{PlaylistMode, PlaylistOptions, plan_playlists};
use rom_converto_lib::util::fs::{collect_all_files, collect_files_with_exts};
use rom_converto_lib::util::{
    CancelToken, ConflictPolicy, ConflictResolution, DEFAULT_SPACE_HEADROOM, FileStatus, HashAlgo,
    PlanLine, ProgressReporter, ReportFormat, ReportRecord, ReportTotals, TemplateTokens,
    apply_template, available_space, format_bytes, hash_file_cancellable, parse_algos,
    resolve_conflict, space_shortfall, write_report,
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
/// records and writes a single report at the end. `input_bytes`/`output_bytes`
/// are always populated (independent of the report toggle) so the batch
/// completion notification can total up space saved without requiring the
/// user to turn reporting on.
#[derive(serde::Serialize)]
pub struct RunOutcome {
    message: String,
    record: Option<ReportRecord>,
    input_bytes: u64,
    output_bytes: u64,
    comparison: Option<ComparisonSummary>,
}

impl RunOutcome {
    /// A conflict-policy skip. When reporting is on it carries a skipped record
    /// so the run report matches the CLI, which records every skipped file.
    fn skipped(report: bool, input: &Path, operation: &str, desired: &Path) -> Self {
        Self {
            message: format!("Skipped existing {}", desired.display()),
            record: build_skip_record(report, input, operation),
            input_bytes: 0,
            output_bytes: 0,
            comparison: None,
        }
    }

    /// A plain message outcome with no record or size data (dry-run plans).
    fn text(message: String) -> Self {
        Self {
            message,
            record: None,
            input_bytes: 0,
            output_bytes: 0,
            comparison: None,
        }
    }
}

/// Verify verdict shown on a comparison card. `round_trip` is only set when
/// the format's check re-decodes the whole output (Chd, Cso, Nx) and that
/// check actually ran and passed. Formats with no integrity check
/// (`OutputVerify::None`) never produce a `VerifyReport` at all, so the card
/// cannot show a "Verified" badge for a check that never ran.
#[derive(serde::Serialize)]
pub struct VerifyReport {
    ok: bool,
    round_trip: bool,
    message: String,
}

/// Before/after summary for one conversion, shown as a comparison card once
/// the run finishes. Always populated on success regardless of the report
/// toggle; `verify` is only filled in when the caller asked for a
/// post-conversion check, since that re-reads the whole output file.
#[derive(serde::Serialize)]
pub struct ComparisonSummary {
    input_bytes: u64,
    output_bytes: u64,
    ratio_pct: Option<f64>,
    input_format: String,
    output_format: String,
    output_sha1: Option<String>,
    verify: Option<VerifyReport>,
}

fn verify_report(ok: bool, round_trip: bool, message: impl Into<String>) -> Option<VerifyReport> {
    Some(VerifyReport {
        ok,
        round_trip,
        message: message.into(),
    })
}

/// Run the format-specific integrity check for the comparison card. Unlike
/// `verify_existing_output` (used for the `--on-conflict overwrite-invalid`
/// keep-versus-rewrite decision), this never treats "could not check" as a
/// pass: a missing NX header key or a verify error is reported as its own
/// unverified state rather than a green "Verified" badge.
async fn run_comparison_verify(
    progress: &dyn ProgressReporter,
    output: &Path,
    target: rom_converto_lib::util::OutputVerify,
    cancel: &CancelToken,
) -> Option<VerifyReport> {
    use rom_converto_lib::util::OutputVerify;
    match target {
        OutputVerify::None => None,
        OutputVerify::Chd => {
            let ok =
                verify_chd_cancellable(progress, output.to_path_buf(), None, false, cancel.clone())
                    .await
                    .is_ok();
            verify_report(
                ok,
                ok,
                if ok {
                    "Verified"
                } else {
                    "Verification failed"
                },
            )
        }
        OutputVerify::Cso => {
            let ok = verify_cso(progress, output.to_path_buf(), true)
                .await
                .is_ok();
            verify_report(
                ok,
                ok,
                if ok {
                    "Verified"
                } else {
                    "Verification failed"
                },
            )
        }
        OutputVerify::Rvz => {
            let ok = verify_rvz_structure(output)
                .map(|r| r.ok())
                .unwrap_or(false);
            verify_report(
                ok,
                false,
                if ok {
                    "Verified"
                } else {
                    "Verification failed"
                },
            )
        }
        OutputVerify::Nx(keys) => {
            if keys.header_key.is_none() {
                return verify_report(false, false, "Could not verify: keyset has no header key");
            }
            match verify_container_async(output.to_path_buf(), *keys, progress).await {
                Ok(result) => verify_report(
                    result.ok,
                    result.ok,
                    if result.ok {
                        "Verified"
                    } else {
                        "Verification failed"
                    },
                ),
                Err(e) => verify_report(false, false, format!("Could not verify: {e}")),
            }
        }
    }
}

/// Size/ratio/format comparison for one conversion, with no verify pass and
/// no output hash. Used directly by operations that never re-read their
/// output (decompress, extract) and as the base for `build_comparison`.
fn comparison_sizes(
    input: &Path,
    output: &Path,
    input_bytes: u64,
    output_bytes: u64,
) -> ComparisonSummary {
    let ratio_pct = if input_bytes > 0 {
        let saved = (1.0 - output_bytes as f64 / input_bytes as f64) * 100.0;
        Some((saved * 10.0).round() / 10.0)
    } else {
        None
    };
    ComparisonSummary {
        input_bytes,
        output_bytes,
        ratio_pct,
        input_format: ext_of(input).to_ascii_uppercase(),
        output_format: ext_of(output).to_ascii_uppercase(),
        output_sha1: None,
        verify: None,
    }
}

/// Build the comparison card data for a successful conversion. Sizes, the
/// ratio, and format labels are always computed; the verify pass and output
/// hash only run when `verify_after` is set, since both re-read the output
/// file in full. The output hash is computed under `spawn_blocking` so a
/// multi-GB file doesn't stall the async runtime, and observes `cancel` so
/// it can be interrupted like the conversion it follows.
#[allow(clippy::too_many_arguments)]
async fn build_comparison(
    progress: Arc<dyn ProgressReporter>,
    input: &Path,
    output: &Path,
    input_bytes: u64,
    output_bytes: u64,
    target: rom_converto_lib::util::OutputVerify,
    verify_after: bool,
    cancel: &CancelToken,
) -> ComparisonSummary {
    let mut summary = comparison_sizes(input, output, input_bytes, output_bytes);

    let (verify, output_sha1) = if verify_after {
        let verify = run_comparison_verify(progress.as_ref(), output, target, cancel).await;

        let output_owned = output.to_path_buf();
        let progress_for_hash = progress.clone();
        let cancel_for_hash = cancel.clone();
        let sha1 = tokio::task::spawn_blocking(move || {
            hash_file_cancellable(
                &output_owned,
                &[HashAlgo::Sha1],
                progress_for_hash.as_ref(),
                &cancel_for_hash,
            )
        })
        .await
        .ok()
        .and_then(|r| r.ok())
        .and_then(|d| d.sha1);

        (verify, sha1)
    } else {
        (None, None)
    };

    summary.output_sha1 = output_sha1;
    summary.verify = verify;
    summary
}

/// Total bytes written by a CHD extraction: the named output plus, for cue
/// sheets, every data file the sheet references.
fn extracted_output_size(output: &Path) -> u64 {
    let mut total = input_size(output);
    if ext_of(output).eq_ignore_ascii_case("cue")
        && let Ok(text) = std::fs::read_to_string(output)
    {
        let dir = output.parent().unwrap_or_else(|| Path::new("."));
        for line in text.lines() {
            if let Some(rest) = line.trim().strip_prefix("FILE ")
                && let Some(name) = rest.split('"').nth(1)
            {
                total += input_size(&dir.join(name));
            }
        }
    }
    total
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
    .map_err(|_| {
        "The operation failed unexpectedly. Try again, and report a bug if it keeps happening."
            .to_string()
    });
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
    Ok(format!("Wrote {out_display}"))
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
) -> Result<RunOutcome, String> {
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
        return Ok(RunOutcome::text(line.display_text()));
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
            return Ok(RunOutcome::text(format!(
                "Skipped existing {}",
                desired.display()
            )));
        }
    };
    let out_display = output.display().to_string();
    preflight_space(
        output.parent().unwrap_or(&output),
        input_size(&input),
        skip_space_check,
    )?;
    let record_input = input.clone();
    let record_output = output.clone();
    let in_bytes = input_size(&input);
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
    .map_err(|_| {
        "The operation failed unexpectedly. Try again, and report a bug if it keeps happening."
            .to_string()
    });
    finish(&state).await;
    result??;
    let out_bytes = input_size(&record_output);
    Ok(RunOutcome {
        message: format!("Wrote {out_display}"),
        record: None,
        input_bytes: in_bytes,
        output_bytes: out_bytes,
        comparison: Some(comparison_sizes(
            &record_input,
            &record_output,
            in_bytes,
            out_bytes,
        )),
    })
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
) -> Result<RunOutcome, String> {
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
        return Ok(RunOutcome::text(line.display_text()));
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
            return Ok(RunOutcome::text(format!(
                "Skipped existing {}",
                desired.display()
            )));
        }
    };
    let out_display = output.display().to_string();
    preflight_space(
        output.parent().unwrap_or(&output),
        input_size(&input),
        skip_space_check,
    )?;
    let record_input = input.clone();
    let record_output = output.clone();
    let in_bytes = input_size(&input);
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
    let out_bytes = input_size(&record_output);
    Ok(RunOutcome {
        message: format!("Wrote {out_display}"),
        record: None,
        input_bytes: in_bytes,
        output_bytes: out_bytes,
        comparison: Some(comparison_sizes(
            &record_input,
            &record_output,
            in_bytes,
            out_bytes,
        )),
    })
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
) -> Result<RunOutcome, String> {
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
        return Ok(RunOutcome::text(line.display_text()));
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
            return Ok(RunOutcome::text(format!(
                "Skipped existing {}",
                desired.display()
            )));
        }
    };
    let out_display = output.display().to_string();
    preflight_space(
        output.parent().unwrap_or(&output),
        input_size(&input),
        skip_space_check,
    )?;
    let record_input = input.clone();
    let record_output = output.clone();
    let in_bytes = input_size(&input);
    let token = begin(&state).await;
    let result = tokio::spawn(async move {
        decompress_rom_cancellable(&input, &output, progress.as_ref(), token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    let out_bytes = input_size(&record_output);
    Ok(RunOutcome {
        message: format!("Wrote {out_display}"),
        record: None,
        input_bytes: in_bytes,
        output_bytes: out_bytes,
        comparison: Some(comparison_sizes(
            &record_input,
            &record_output,
            in_bytes,
            out_bytes,
        )),
    })
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
    verify_after: Option<bool>,
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
            input_bytes: 0,
            output_bytes: 0,
            comparison: None,
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
    let progress_for_verify = progress.clone();
    let token = begin(&state).await;
    let token_for_verify = token.clone();
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
    let out_bytes = input_size(&record_output);
    let record = build_record(
        report.unwrap_or(false),
        &record_input,
        &record_output,
        "compress",
        in_bytes,
        out_bytes,
        started.elapsed(),
    );
    let comparison = build_comparison(
        progress_for_verify,
        &record_input,
        &record_output,
        in_bytes,
        out_bytes,
        rom_converto_lib::util::OutputVerify::Chd,
        verify_after.unwrap_or(false),
        &token_for_verify,
    )
    .await;
    Ok(RunOutcome {
        message: format!("Wrote {out_display}"),
        record,
        input_bytes: in_bytes,
        output_bytes: out_bytes,
        comparison: Some(comparison),
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
    verify_after: Option<bool>,
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
            input_bytes: 0,
            output_bytes: 0,
            comparison: None,
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
    let progress_for_verify = progress.clone();
    let token = begin(&state).await;
    let token_for_verify = token.clone();
    let started = Instant::now();
    let result = tokio::spawn(async move {
        compress_to_cso_cancellable(progress.as_ref(), input_path, output, opts, token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    let out_bytes = input_size(&record_output);
    let record = build_record(
        report.unwrap_or(false),
        &record_input,
        &record_output,
        "compress",
        in_bytes,
        out_bytes,
        started.elapsed(),
    );
    let comparison = build_comparison(
        progress_for_verify,
        &record_input,
        &record_output,
        in_bytes,
        out_bytes,
        rom_converto_lib::util::OutputVerify::Cso,
        verify_after.unwrap_or(false),
        &token_for_verify,
    )
    .await;
    Ok(RunOutcome {
        message: format!("Wrote {out_display}"),
        record,
        input_bytes: in_bytes,
        output_bytes: out_bytes,
        comparison: Some(comparison),
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
            input_bytes: 0,
            output_bytes: 0,
            comparison: None,
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
        message: format!("Wrote {out_display}"),
        record,
        input_bytes: in_bytes,
        output_bytes: input_size(&record_output),
        comparison: Some(comparison_sizes(
            &record_input,
            &record_output,
            in_bytes,
            input_size(&record_output),
        )),
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
        None => return Ok(format!("Skipped existing {}", output.display())),
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
    Ok(format!("Wrote {out_display}"))
}

// CHD extract and verify use deeply nested async types from ChdReader
// that exceed the compiler's recursion limit for Send inference. They run
// on a dedicated thread with its own tokio runtime to sidestep the issue.

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
            input_bytes: 0,
            output_bytes: 0,
            comparison: None,
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
    // limit, so it runs on a dedicated thread with its own tokio runtime.
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
    .map_err(|_| {
        "The operation failed unexpectedly. Try again, and report a bug if it keeps happening."
            .to_string()
    });
    finish(&state).await;
    result??;
    let in_bytes = input_size(&record_input);
    let out_bytes = extracted_output_size(&record_output);
    let record = build_record(
        report.unwrap_or(false),
        &record_input,
        &record_output,
        "extract",
        in_bytes,
        out_bytes,
        started.elapsed(),
    );
    Ok(RunOutcome {
        message: format!("Wrote {out_display}"),
        record,
        input_bytes: in_bytes,
        output_bytes: out_bytes,
        comparison: Some(comparison_sizes(
            &record_input,
            &record_output,
            in_bytes,
            out_bytes,
        )),
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
    .map_err(|_| {
        "The operation failed unexpectedly. Try again, and report a bug if it keeps happening."
            .to_string()
    });
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
    verify_after: Option<bool>,
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
            input_bytes: 0,
            output_bytes: 0,
            comparison: None,
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
    let progress_for_verify = progress.clone();
    let token = begin(&state).await;
    let token_for_verify = token.clone();
    let started = Instant::now();
    let result = tokio::spawn(async move {
        compress_disc_cancellable(&input, &output, opts, progress.as_ref(), token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    let out_bytes = input_size(&record_output);
    let record = build_record(
        report.unwrap_or(false),
        &record_input,
        &record_output,
        "compress",
        in_bytes,
        out_bytes,
        started.elapsed(),
    );
    let comparison = build_comparison(
        progress_for_verify,
        &record_input,
        &record_output,
        in_bytes,
        out_bytes,
        rom_converto_lib::util::OutputVerify::Rvz,
        verify_after.unwrap_or(false),
        &token_for_verify,
    )
    .await;
    Ok(RunOutcome {
        message: format!("Wrote {out_display}"),
        record,
        input_bytes: in_bytes,
        output_bytes: out_bytes,
        comparison: Some(comparison),
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
            input_bytes: 0,
            output_bytes: 0,
            comparison: None,
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
        message: format!("Wrote {out_display}"),
        record,
        input_bytes: in_bytes,
        output_bytes: input_size(&record_output),
        comparison: Some(comparison_sizes(
            &record_input,
            &record_output,
            in_bytes,
            input_size(&record_output),
        )),
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
        None => return Ok(format!("Skipped existing {}", output.display())),
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
    Ok(format!("Wrote {out_display}"))
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
        None => return Ok(format!("Skipped existing {}", output.display())),
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
    Ok(format!("Wrote {out_display}"))
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
    verify_after: Option<bool>,
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
            input_bytes: 0,
            output_bytes: 0,
            comparison: None,
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
    let progress_for_verify = progress.clone();
    let keys_for_verify = keys.clone();
    let token = begin(&state).await;
    let token_for_verify = token.clone();
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
    let out_bytes = input_size(&record_output);
    let record = build_record(
        report.unwrap_or(false),
        &record_input,
        &record_output,
        "compress",
        in_bytes,
        out_bytes,
        started.elapsed(),
    );
    let comparison = build_comparison(
        progress_for_verify,
        &record_input,
        &record_output,
        in_bytes,
        out_bytes,
        rom_converto_lib::util::OutputVerify::Nx(Box::new(keys_for_verify)),
        verify_after.unwrap_or(false),
        &token_for_verify,
    )
    .await;
    Ok(RunOutcome {
        message: format!("Wrote {out_display}"),
        record,
        input_bytes: in_bytes,
        output_bytes: out_bytes,
        comparison: Some(comparison),
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
            input_bytes: 0,
            output_bytes: 0,
            comparison: None,
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
        message: format!("Wrote {out_display}"),
        record,
        input_bytes: in_bytes,
        output_bytes: input_size(&record_output),
        comparison: Some(comparison_sizes(
            &record_input,
            &record_output,
            in_bytes,
            input_size(&record_output),
        )),
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
    verify_after: Option<bool>,
    dry_run: Option<bool>,
) -> Result<RunOutcome, String> {
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
        return Ok(RunOutcome {
            message: line.display_text(),
            record: None,
            input_bytes: 0,
            output_bytes: 0,
            comparison: None,
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
            return Ok(RunOutcome::skipped(false, &input, "convert", &desired));
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
    let progress_for_verify = progress.clone();
    let token = begin(&state).await;
    let token_for_verify = token.clone();
    let result = tokio::spawn(async move {
        convert_rom_cancellable(&input, &output, progress.as_ref(), token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    let out_bytes = input_size(&record_output);
    let comparison = build_comparison(
        progress_for_verify,
        &record_input,
        &record_output,
        in_bytes,
        out_bytes,
        rom_converto_lib::util::OutputVerify::None,
        verify_after.unwrap_or(false),
        &token_for_verify,
    )
    .await;
    Ok(RunOutcome {
        message: format!("Wrote {out_display}"),
        record: None,
        input_bytes: in_bytes,
        output_bytes: out_bytes,
        comparison: Some(comparison),
    })
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
        extract_icon_png(&info).ok_or_else(|| "This file has no icon to save.".to_string())?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(err_to_string)?;
    }
    std::fs::write(&dest, &bytes).map_err(err_to_string)?;
    Ok(dest.display().to_string())
}

#[tauri::command]
pub async fn cmd_hash(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    algos: Vec<String>,
    recursive: bool,
    max_depth: Option<usize>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "hash"));
    let token = begin(&state).await;
    let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let parsed = parse_algos(&algos.join(","))?;
        let mut lines = Vec::new();
        if recursive {
            let files = collect_all_files(&input, max_depth).map_err(err_to_string)?;
            if files.is_empty() {
                return Ok(format!("no files found in {}", input.display()));
            }
            for file in files {
                if token.is_cancelled() {
                    return Err("operation cancelled".to_string());
                }
                let digests = hash_file_cancellable(&file, &parsed, progress.as_ref(), &token)
                    .map_err(err_to_string)?;
                lines.push(render_hash_row(&file, &digests, &parsed));
            }
        } else {
            let digests = hash_file_cancellable(&input, &parsed, progress.as_ref(), &token)
                .map_err(err_to_string)?;
            lines.push(render_hash_row(&input, &digests, &parsed));
        }
        Ok(lines.join("\n"))
    })
    .await
    .map_err(err_to_string);
    finish(&state).await;
    result?
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

// ---- dat verify/scan/rename ----
//
// Digesting and Playmatch calls are both async-native (digest_inner_async
// already runs the blocking decode in spawn_blocking; PlaymatchClient is
// plain reqwest), so these commands run on the Tauri async runtime directly,
// unlike the CHD extract/verify commands above.

#[derive(serde::Serialize)]
struct DatTrackCheckJson {
    track: u32,
    ok: bool,
    algo: Option<String>,
    #[serde(rename = "matchedFile")]
    matched_file: Option<String>,
}

#[derive(serde::Serialize)]
struct ExternalIdJson {
    provider: String,
    id: String,
}

#[derive(serde::Serialize)]
struct DatVerifyResult {
    kind: &'static str,
    path: String,
    verdict: &'static str,
    #[serde(rename = "matchAlgo")]
    match_algo: Option<String>,
    #[serde(rename = "gameName")]
    game_name: Option<String>,
    platform: Option<String>,
    #[serde(rename = "signatureGroup")]
    signature_group: Option<String>,
    #[serde(rename = "datVersion")]
    dat_version: Option<String>,
    #[serde(rename = "externalIds")]
    external_ids: Vec<ExternalIdJson>,
    tracks: Option<Vec<DatTrackCheckJson>>,
    error: Option<String>,
}

/// External ids shown to the user: automatic or manual matches with a
/// non-null provider id, matching the CLI's `identify` filter.
fn external_ids_from(matched: &GameAndRelationMatchResult) -> Vec<ExternalIdJson> {
    matched
        .external_metadata
        .iter()
        .filter(|m| matches!(m.match_type.as_str(), "Automatic" | "Manual"))
        .filter_map(|m| {
            m.provider_id.clone().map(|id| ExternalIdJson {
                provider: m.provider_name.clone(),
                id,
            })
        })
        .collect()
}

#[tauri::command]
pub async fn cmd_dat_verify(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "dat-verify"));
    let token = begin(&state).await;
    let result = run_dat_verify(progress, input.clone(), token).await;
    finish(&state).await;
    // Cancellation propagates as an Err carrying "operation cancelled",
    // matching the other verify commands and scan/rename. Any other failure
    // still renders as a single Failed card so genuine digest/API errors do
    // not abort the whole invoke.
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(rom_converto_lib::dat::DatError::Cancelled) => {
            return Err("operation cancelled".to_string());
        }
        Err(e) => DatVerifyResult {
            kind: "verify",
            path: input.display().to_string(),
            verdict: DatVerdict::Failed.as_str(),
            match_algo: None,
            game_name: None,
            platform: None,
            signature_group: None,
            dat_version: None,
            external_ids: Vec::new(),
            tracks: None,
            error: Some(e.to_string()),
        },
    };
    serde_json::to_string(&outcome).map_err(err_to_string)
}

async fn run_dat_verify(
    progress: Arc<TauriProgress>,
    input: PathBuf,
    token: CancelToken,
) -> Result<DatVerifyResult, rom_converto_lib::dat::DatError> {
    let algos = [
        rom_converto_lib::util::HashAlgo::Crc32,
        rom_converto_lib::util::HashAlgo::Sha1,
    ];
    let digests = digest_inner_async(
        input.clone(),
        algos.to_vec(),
        progress.as_ref(),
        token.clone(),
    )
    .await?;

    let client = PlaymatchClient::new(Some(DEFAULT_API_BASE));
    progress.set_phase("Querying matches");
    let path_str = input.display().to_string();

    match digests {
        RomDigests::Single(d) => {
            let file_name = input
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file")
                .to_string();
            let search = GameFileMatchSearch::from_digests(&file_name, &d);
            let matched = client.identify_relations(&search, &token).await?;
            Ok(verify_result_from_single(path_str, &matched))
        }
        RomDigests::Tracks { tracks, whole } => {
            let stem = input.file_stem().and_then(|n| n.to_str()).unwrap_or("file");
            let whole_name = format!("{stem}.bin");
            let whole_search = GameFileMatchSearch::from_digests(&whole_name, &whole);
            let whole_match = client.identify_relations(&whole_search, &token).await?;
            if match_strength(whole_match.game_match_type).is_verified() {
                return Ok(verify_result_from_single(path_str, &whole_match));
            }

            let first_name = format!("{stem} (Track 1).bin");
            let first_digests = &tracks[0].digests;
            let first_search = GameFileMatchSearch::from_digests(&first_name, first_digests);
            let track_match = client.identify_relations(&first_search, &token).await?;
            Ok(verify_result_from_tracks(path_str, &track_match, &tracks))
        }
    }
}

fn verify_result_from_single(
    path: String,
    matched: &GameAndRelationMatchResult,
) -> DatVerifyResult {
    let strength = match_strength(matched.game_match_type);
    let verdict = match strength {
        MatchStrength::Verified(_) => DatVerdict::Verified,
        MatchStrength::NameSizeHint => DatVerdict::Hint,
        MatchStrength::NoMatch => DatVerdict::Unknown,
    };
    let match_algo = match strength {
        MatchStrength::Verified(a) => Some(a.label().to_string()),
        _ => None,
    };
    DatVerifyResult {
        kind: "verify",
        path,
        verdict: verdict.as_str(),
        match_algo,
        game_name: matched.game.as_ref().map(|g| g.name.clone()),
        platform: matched.platform.as_ref().map(|p| p.name.clone()),
        signature_group: matched.signature_group.as_ref().map(|g| g.name.clone()),
        dat_version: matched.dat_file_import.as_ref().map(|i| i.version.clone()),
        external_ids: external_ids_from(matched),
        tracks: None,
        error: None,
    }
}

fn verify_result_from_tracks(
    path: String,
    matched: &GameAndRelationMatchResult,
    tracks: &[TrackDigests],
) -> DatVerifyResult {
    let reconciliation = reconcile_tracks(tracks, &matched.game_files);
    // A track set is Verified only when every local track reconciles by a real
    // hash. A hash-verified track 1 with an unreconciled other track is not
    // whole-set verification.
    let verdict = if reconciliation.all_ok {
        DatVerdict::Verified
    } else if match_strength(matched.game_match_type) == MatchStrength::NameSizeHint {
        DatVerdict::Hint
    } else {
        DatVerdict::Unknown
    };
    let track_checks = reconciliation
        .tracks
        .iter()
        .map(|t| DatTrackCheckJson {
            track: t.track_number,
            ok: t.ok,
            algo: t.algo.map(|a| a.label().to_string()),
            matched_file: t.matched_file.clone(),
        })
        .collect();
    DatVerifyResult {
        kind: "verify",
        path,
        verdict: verdict.as_str(),
        match_algo: None,
        game_name: matched.game.as_ref().map(|g| g.name.clone()),
        platform: matched.platform.as_ref().map(|p| p.name.clone()),
        signature_group: matched.signature_group.as_ref().map(|g| g.name.clone()),
        dat_version: matched.dat_file_import.as_ref().map(|i| i.version.clone()),
        external_ids: external_ids_from(matched),
        tracks: Some(track_checks),
        error: None,
    }
}

#[derive(serde::Serialize)]
struct DatScanRow {
    path: String,
    status: &'static str,
    #[serde(rename = "gameName")]
    game_name: Option<String>,
    #[serde(rename = "canonicalStem")]
    canonical_stem: Option<String>,
    error: Option<String>,
}

#[derive(serde::Serialize)]
struct DatScanResult {
    kind: &'static str,
    matched: u32,
    misnamed: u32,
    hint: u32,
    unknown: u32,
    unsupported: u32,
    failed: u32,
    rows: Vec<DatScanRow>,
}

/// One local file plus its computed digests, carried through the scan/rename
/// pipeline so a digest failure becomes a row instead of aborting the batch.
enum DigestedUnit {
    Ok { path: PathBuf, digests: RomDigests },
    Unsupported { path: PathBuf },
    Failed { path: PathBuf, error: String },
}

impl DigestedUnit {
    fn path(&self) -> &Path {
        match self {
            DigestedUnit::Ok { path, .. }
            | DigestedUnit::Unsupported { path }
            | DigestedUnit::Failed { path, .. } => path,
        }
    }
}

/// Digest every file under `input_dir`, bucketing unsupported formats and
/// per-file failures instead of aborting the whole scan/rename run, matching
/// the CLI driver's read-only semantics.
async fn digest_all(
    progress: &TauriProgress,
    input_dir: &Path,
    max_depth: Option<usize>,
    token: &CancelToken,
) -> Result<Vec<DigestedUnit>, String> {
    // Cue sheets and playlists are set descriptors, not hashable images: they
    // are handled via cue grouping (rename) and would otherwise digest to an
    // InvalidInput failure and surface as a spurious Failed row in scan.
    let files: Vec<PathBuf> = collect_all_files(input_dir, max_depth)
        .map_err(err_to_string)?
        .into_iter()
        .filter(|f| {
            !f.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("cue") || e.eq_ignore_ascii_case("m3u"))
        })
        .collect();
    progress.set_phase("Hashing files");
    let algos = vec![
        rom_converto_lib::util::HashAlgo::Crc32,
        rom_converto_lib::util::HashAlgo::Sha1,
    ];
    let mut units = Vec::with_capacity(files.len());
    for file in files {
        if token.is_cancelled() {
            return Err("operation cancelled".to_string());
        }
        match digest_inner_async(file.clone(), algos.clone(), progress, token.clone()).await {
            Ok(digests) => units.push(DigestedUnit::Ok {
                path: file,
                digests,
            }),
            Err(rom_converto_lib::dat::DatError::UnsupportedInnerHash { .. }) => {
                units.push(DigestedUnit::Unsupported { path: file })
            }
            Err(rom_converto_lib::dat::DatError::Cancelled) => {
                return Err("operation cancelled".to_string());
            }
            Err(e) => units.push(DigestedUnit::Failed {
                path: file,
                error: e.to_string(),
            }),
        }
    }
    Ok(units)
}

/// The strongest single search key for one digested unit: the whole-image
/// digest for track sets, the sole digest otherwise. This is what bulk
/// scan/rename queries key on; per-track reconciliation is verify-only.
fn primary_digests(digests: &RomDigests) -> &rom_converto_lib::util::FileDigests {
    match digests {
        RomDigests::Single(d) => d,
        RomDigests::Tracks { whole, .. } => whole,
    }
}

#[tauri::command]
pub async fn cmd_dat_scan(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    #[allow(non_snake_case)] maxDepth: Option<usize>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "dat-scan"));
    let token = begin(&state).await;
    let result = run_dat_scan(progress, input, maxDepth, token).await;
    finish(&state).await;
    let outcome = result?;
    serde_json::to_string(&outcome).map_err(err_to_string)
}

async fn run_dat_scan(
    progress: Arc<TauriProgress>,
    input: PathBuf,
    max_depth: Option<usize>,
    token: CancelToken,
) -> Result<DatScanResult, String> {
    let units = digest_all(progress.as_ref(), &input, max_depth, &token).await?;

    // Slot per unit, filled in either immediately (unsupported/failed) or
    // after the bulk query resolves (queryable). Keeps output row order
    // matching the walk order regardless of query completion order.
    let mut slots: Vec<Option<DatScanRow>> = Vec::with_capacity(units.len());
    let mut queryable = Vec::new();
    for (i, unit) in units.iter().enumerate() {
        match unit {
            DigestedUnit::Ok { path, digests } => {
                let file_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file")
                    .to_string();
                queryable.push((i, path.clone(), file_name, primary_digests(digests).clone()));
                slots.push(None);
            }
            DigestedUnit::Unsupported { path } => slots.push(Some(DatScanRow {
                path: path.display().to_string(),
                status: DatVerdict::Unsupported.as_str(),
                game_name: None,
                canonical_stem: None,
                error: None,
            })),
            DigestedUnit::Failed { path, error } => slots.push(Some(DatScanRow {
                path: path.display().to_string(),
                status: DatVerdict::Failed.as_str(),
                game_name: None,
                canonical_stem: None,
                error: Some(error.clone()),
            })),
        }
    }

    let client = PlaymatchClient::new(Some(DEFAULT_API_BASE));
    progress.set_phase("Querying matches");
    let items: Vec<BulkIdentifyItem> = queryable
        .iter()
        .map(|(_, _, name, digests)| BulkIdentifyItem {
            search: GameFileMatchSearch::from_digests(name, digests),
            key: None,
        })
        .collect();
    let bulk_results = client
        .identify_bulk_ids(items, &token)
        .await
        .map_err(err_to_string)?;

    let mut matched_ids: Vec<String> = Vec::new();
    for r in &bulk_results {
        if r.status == BulkItemStatus::Ok
            && let Some(m) = &r.matched
            && match_strength(m.game_match_type).is_verified()
            && let Some(id) = &m.id
        {
            matched_ids.push(id.clone());
        }
    }
    matched_ids.sort();
    matched_ids.dedup();
    let games = if matched_ids.is_empty() {
        Vec::new()
    } else {
        client
            .games_bulk(matched_ids, &token)
            .await
            .map_err(err_to_string)?
    };
    let name_for_id = |id: &str| -> Option<String> {
        games
            .iter()
            .find(|g| g.id == id)
            .and_then(|g| g.data.as_ref())
            .map(|d| d.name.clone())
    };

    for (queryable_idx, (unit_idx, path, _, _)) in queryable.iter().enumerate() {
        let result = bulk_results.iter().find(|r| r.index == queryable_idx);
        slots[*unit_idx] = Some(scan_row_for(path, result, &name_for_id));
    }

    let mut tally = ScanTally::default();
    let rows: Vec<DatScanRow> = slots
        .into_iter()
        .map(|slot| slot.expect("every unit gets exactly one row"))
        .inspect(|row| tally.count(row.status))
        .collect();

    Ok(DatScanResult {
        kind: "scan",
        matched: tally.matched,
        misnamed: tally.misnamed,
        hint: tally.hint,
        unknown: tally.unknown,
        unsupported: tally.unsupported,
        failed: tally.failed,
        rows,
    })
}

#[derive(Default)]
struct ScanTally {
    matched: u32,
    misnamed: u32,
    hint: u32,
    unknown: u32,
    unsupported: u32,
    failed: u32,
}

impl ScanTally {
    fn count(&mut self, status: &str) {
        match status {
            "matched" => self.matched += 1,
            "misnamed" => self.misnamed += 1,
            "hint" => self.hint += 1,
            "unknown" => self.unknown += 1,
            "unsupported" => self.unsupported += 1,
            _ => self.failed += 1,
        }
    }
}

/// Classify one file's bulk-ids result into a scan row: a non-ok status
/// becomes Failed (never silently dropped), NoMatch is Unknown, a
/// FileNameAndSize match is Hint, and a hash-verified match is Matched
/// unless the local stem differs from the canonical name (Misnamed).
fn scan_row_for(
    path: &Path,
    result: Option<&BulkIdentifyIdsResult>,
    name_for_id: &impl Fn(&str) -> Option<String>,
) -> DatScanRow {
    let path_str = path.display().to_string();
    let Some(result) = result else {
        return DatScanRow {
            path: path_str,
            status: DatVerdict::Failed.as_str(),
            game_name: None,
            canonical_stem: None,
            error: Some("no result returned for this file".to_string()),
        };
    };
    if result.status != BulkItemStatus::Ok {
        let msg = result
            .error
            .as_ref()
            .map(|e| e.message.clone())
            .unwrap_or_else(|| "bulk identify item failed".to_string());
        return DatScanRow {
            path: path_str,
            status: DatVerdict::Failed.as_str(),
            game_name: None,
            canonical_stem: None,
            error: Some(msg),
        };
    }
    let Some(matched) = &result.matched else {
        return DatScanRow {
            path: path_str,
            status: DatVerdict::Unknown.as_str(),
            game_name: None,
            canonical_stem: None,
            error: None,
        };
    };
    match match_strength(matched.game_match_type) {
        MatchStrength::NoMatch => DatScanRow {
            path: path_str,
            status: DatVerdict::Unknown.as_str(),
            game_name: None,
            canonical_stem: None,
            error: None,
        },
        MatchStrength::NameSizeHint => DatScanRow {
            path: path_str,
            status: DatVerdict::Hint.as_str(),
            game_name: None,
            canonical_stem: None,
            error: None,
        },
        MatchStrength::Verified(_) => {
            let game_name = matched.id.as_deref().and_then(name_for_id);
            let local_stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            // Scan's "matched" bucket has no DatVerdict counterpart (verify's
            // Verified and scan's matched are the same hash-rung outcome, but
            // the scan status here is spelled "matched"); misnamed does map onto
            // DatVerdict::Misnamed and goes through as_str() as usual.
            let status = match &game_name {
                Some(name) if !name.eq_ignore_ascii_case(local_stem) => {
                    DatVerdict::Misnamed.as_str()
                }
                _ => "matched",
            };
            DatScanRow {
                path: path_str,
                status,
                game_name: game_name.clone(),
                canonical_stem: game_name,
                error: None,
            }
        }
    }
}

#[derive(serde::Serialize)]
struct DatRenameRow {
    from: String,
    to: Option<String>,
    action: &'static str,
    detail: Option<String>,
}

#[derive(serde::Serialize)]
struct DatRenameResult {
    kind: &'static str,
    #[serde(rename = "dryRun")]
    dry_run: bool,
    renamed: u32,
    skipped: u32,
    failed: u32,
    rows: Vec<DatRenameRow>,
}

#[tauri::command]
pub async fn cmd_dat_rename(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    #[allow(non_snake_case)] maxDepth: Option<usize>,
    #[allow(non_snake_case)] dryRun: bool,
    #[allow(non_snake_case)] onConflict: String,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "dat-rename"));
    let token = begin(&state).await;
    let result = run_dat_rename(progress, input, maxDepth, dryRun, onConflict, token).await;
    finish(&state).await;
    let outcome = result?;
    serde_json::to_string(&outcome).map_err(err_to_string)
}

/// Group cue members under `input_dir` into sets: each entry is a `.cue` path
/// and its member .bin paths. Used to keep rename cue-aware so a member .bin is
/// never renamed in isolation, which would dangle the cue's FILE line.
async fn cue_sets_under(
    input_dir: &Path,
    max_depth: Option<usize>,
) -> Result<Vec<(PathBuf, Vec<PathBuf>)>, String> {
    use rom_converto_lib::cue::CueParser;
    let files = collect_all_files(input_dir, max_depth).map_err(err_to_string)?;
    let mut sets = Vec::new();
    for cue in files.iter().filter(|f| {
        f.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("cue"))
    }) {
        let parent = cue.parent().unwrap_or_else(|| Path::new("."));
        let Ok(sheet) = CueParser::new(cue).parse().await else {
            continue;
        };
        let bins: Vec<PathBuf> = sheet
            .files
            .iter()
            .map(|f| parent.join(&f.filename))
            .collect();
        if !bins.is_empty() {
            sets.push((cue.clone(), bins));
        }
    }
    Ok(sets)
}

async fn run_dat_rename(
    progress: Arc<TauriProgress>,
    input: PathBuf,
    max_depth: Option<usize>,
    dry_run: bool,
    on_conflict: String,
    token: CancelToken,
) -> Result<DatRenameResult, String> {
    let policy = conflict_policy(Some(on_conflict.as_str()));
    let cue_sets = cue_sets_under(&input, max_depth).await?;
    let cue_covered: std::collections::HashSet<PathBuf> = cue_sets
        .iter()
        .flat_map(|(cue, bins)| std::iter::once(cue.clone()).chain(bins.iter().cloned()))
        .collect();
    let units = digest_all(progress.as_ref(), &input, max_depth, &token).await?;

    let client = PlaymatchClient::new(Some(DEFAULT_API_BASE));
    progress.set_phase("Querying matches");

    // Queryable units carry their local path and primary search digest; failed
    // and unsupported units become non-participating rows and never rename.
    let mut queryable: Vec<PathBuf> = Vec::new();
    let mut items: Vec<BulkIdentifyItem> = Vec::new();
    let mut rows: Vec<DatRenameRow> = Vec::new();
    let mut renamed = 0u32;
    let mut skipped = 0u32;
    let mut failed = 0u32;

    // One skip row per cue set; members are never renamed in isolation.
    for (cue, _) in &cue_sets {
        skipped += 1;
        rows.push(DatRenameRow {
            from: cue.display().to_string(),
            to: None,
            action: "skip-unmatched",
            detail: Some("cue set: rename skipped to keep FILE lines consistent".to_string()),
        });
    }

    for unit in &units {
        // Cue files and their member bins are handled as a set above.
        if cue_covered.contains(unit.path()) {
            continue;
        }
        match unit {
            DigestedUnit::Ok { path, digests } => {
                let file_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file")
                    .to_string();
                items.push(BulkIdentifyItem {
                    search: GameFileMatchSearch::from_digests(&file_name, primary_digests(digests)),
                    key: None,
                });
                queryable.push(path.clone());
            }
            DigestedUnit::Unsupported { path } => {
                skipped += 1;
                rows.push(DatRenameRow {
                    from: path.display().to_string(),
                    to: None,
                    action: "skip-unmatched",
                    detail: Some("unsupported format".to_string()),
                });
            }
            DigestedUnit::Failed { path, error } => {
                failed += 1;
                rows.push(DatRenameRow {
                    from: path.display().to_string(),
                    to: None,
                    action: "failed",
                    detail: Some(error.clone()),
                });
            }
        }
    }

    let bulk_results = client
        .identify_bulk_relations(items, &token)
        .await
        .map_err(err_to_string)?;

    let candidates: Vec<RenameCandidate> = queryable
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let matched = bulk_results
                .iter()
                .find(|r| r.index == i)
                .filter(|r| r.status == BulkItemStatus::Ok)
                .and_then(|r| r.matched.as_ref());
            candidate_from_match(path, matched)
        })
        .collect();

    for plan in &plan_renames(&candidates) {
        let row = execute_rename_plan(plan, dry_run, policy);
        match row.action {
            "renamed" | "would-rename" => renamed += 1,
            "failed" => failed += 1,
            _ => skipped += 1,
        }
        rows.push(row);
    }

    Ok(DatRenameResult {
        kind: "rename",
        dry_run,
        renamed,
        skipped,
        failed,
        rows,
    })
}

/// Build a rename candidate from a file's relations match. `verified` is set
/// only for a hash-rung match (hints never rename); the file-level name
/// is taken from the single matching gameFiles entry when the game has one.
fn candidate_from_match(
    path: &Path,
    matched: Option<&GameAndRelationMatchResult>,
) -> RenameCandidate {
    let Some(matched) = matched else {
        return RenameCandidate {
            path: path.to_path_buf(),
            game_id: None,
            game_name: None,
            file_name: None,
            verified: false,
        };
    };
    let verified = match_strength(matched.game_match_type).is_verified();
    let file_name = if matched.game_files.len() == 1 {
        Some(matched.game_files[0].file_name.clone())
    } else {
        None
    };
    RenameCandidate {
        path: path.to_path_buf(),
        game_id: matched.game.as_ref().map(|g| g.id.clone()),
        game_name: matched.game.as_ref().map(|g| g.name.clone()),
        file_name,
        verified,
    }
}

/// Turn one planned rename into a row, executing the filesystem move unless
/// `dry_run`. A `Rename` action resolves the target against `policy`: a Skip
/// resolution (target exists, policy Skip/Error/OverwriteInvalid) records a
/// skip, a Write resolution moves the file. std::fs::rename replaces an
/// existing destination on Windows, so no separate delete is needed.
fn execute_rename_plan(plan: &RenamePlan, dry_run: bool, policy: ConflictPolicy) -> DatRenameRow {
    let from = plan.from.display().to_string();
    match plan.action {
        RenameAction::AlreadyCanonical => DatRenameRow {
            from,
            to: plan.to.as_ref().map(|p| p.display().to_string()),
            action: "already-canonical",
            detail: plan.detail.clone(),
        },
        RenameAction::SkipUnmatched => DatRenameRow {
            from,
            to: None,
            action: "skip-unmatched",
            detail: plan.detail.clone(),
        },
        RenameAction::SkipWeakMatch => DatRenameRow {
            from,
            to: None,
            action: "skip-weak",
            detail: plan.detail.clone(),
        },
        RenameAction::SkipCollision => DatRenameRow {
            from,
            to: None,
            action: "skip-collision",
            detail: plan.detail.clone(),
        },
        RenameAction::SkipDiscSetConflict => DatRenameRow {
            from,
            to: None,
            action: "skip-disc-set",
            detail: plan.detail.clone(),
        },
        RenameAction::Rename => {
            let Some(target) = &plan.to else {
                return DatRenameRow {
                    from,
                    to: None,
                    action: "failed",
                    detail: Some("rename plan missing target".to_string()),
                };
            };
            let to = target.display().to_string();
            if dry_run {
                return DatRenameRow {
                    from,
                    to: Some(to),
                    action: "would-rename",
                    detail: plan.detail.clone(),
                };
            }
            match resolve_conflict(target, policy) {
                Ok(ConflictResolution::Skip) => DatRenameRow {
                    from,
                    to: Some(to),
                    action: "skip-collision",
                    detail: Some("target exists".to_string()),
                },
                Ok(ConflictResolution::Write(dest)) => match std::fs::rename(&plan.from, &dest) {
                    Ok(()) => DatRenameRow {
                        from,
                        to: Some(dest.display().to_string()),
                        action: "renamed",
                        detail: plan.detail.clone(),
                    },
                    Err(e) => DatRenameRow {
                        from,
                        to: Some(to),
                        action: "failed",
                        detail: Some(e.to_string()),
                    },
                },
                Err(e) => DatRenameRow {
                    from,
                    to: Some(to),
                    action: "failed",
                    detail: Some(e.to_string()),
                },
            }
        }
    }
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

#[cfg(test)]
mod dat_result_serde_tests {
    use super::*;
    use serde_json::Value;

    // Assert an object has exactly `expected` keys, catching a stray snake_case
    // field before it reaches the TS contract.
    fn assert_keys(v: &Value, expected: &[&str]) {
        let obj = v.as_object().expect("object");
        let mut got: Vec<&str> = obj.keys().map(String::as_str).collect();
        got.sort_unstable();
        let mut want: Vec<&str> = expected.to_vec();
        want.sort_unstable();
        assert_eq!(got, want, "serialized keys");
    }

    #[test]
    fn verify_result_keys_are_camel_case() {
        let r = DatVerifyResult {
            kind: "verify",
            path: "d/x.chd".into(),
            verdict: "verified",
            match_algo: Some("sha1".into()),
            game_name: Some("Some Game".into()),
            platform: Some("Platform".into()),
            signature_group: Some("SG".into()),
            dat_version: Some("2024".into()),
            external_ids: vec![ExternalIdJson {
                provider: "prov".into(),
                id: "abc".into(),
            }],
            tracks: Some(vec![DatTrackCheckJson {
                track: 1,
                ok: true,
                algo: Some("sha1".into()),
                matched_file: Some("Some Game (Track 1).bin".into()),
            }]),
            error: None,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_keys(
            &v,
            &[
                "kind",
                "path",
                "verdict",
                "matchAlgo",
                "gameName",
                "platform",
                "signatureGroup",
                "datVersion",
                "externalIds",
                "tracks",
                "error",
            ],
        );
        assert_keys(&v["externalIds"][0], &["provider", "id"]);
        assert_keys(&v["tracks"][0], &["track", "ok", "algo", "matchedFile"]);
    }

    #[test]
    fn scan_result_keys_are_camel_case() {
        let r = DatScanResult {
            kind: "scan",
            matched: 1,
            misnamed: 0,
            hint: 0,
            unknown: 0,
            unsupported: 0,
            failed: 0,
            rows: vec![DatScanRow {
                path: "d/x.chd".into(),
                status: "matched",
                game_name: Some("Some Game".into()),
                canonical_stem: Some("Some Game".into()),
                error: None,
            }],
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_keys(
            &v,
            &[
                "kind",
                "matched",
                "misnamed",
                "hint",
                "unknown",
                "unsupported",
                "failed",
                "rows",
            ],
        );
        assert_keys(
            &v["rows"][0],
            &["path", "status", "gameName", "canonicalStem", "error"],
        );
    }

    #[test]
    fn rename_result_keys_are_camel_case() {
        let r = DatRenameResult {
            kind: "rename",
            dry_run: true,
            renamed: 1,
            skipped: 0,
            failed: 0,
            rows: vec![DatRenameRow {
                from: "d/x.chd".into(),
                to: Some("d/Some Game.chd".into()),
                action: "would-rename",
                detail: None,
            }],
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_keys(
            &v,
            &["kind", "dryRun", "renamed", "skipped", "failed", "rows"],
        );
        assert_keys(&v["rows"][0], &["from", "to", "action", "detail"]);
    }
}

#[cfg(test)]
mod comparison_tests {
    use super::*;
    use rom_converto_lib::util::NoProgress;
    use serde_json::Value;
    use tempfile::tempdir;

    // Assert an object has exactly `expected` keys, catching a stray
    // camelCase field before it reaches the snake_case TS contract.
    fn assert_keys(v: &Value, expected: &[&str]) {
        let obj = v.as_object().expect("object");
        let mut got: Vec<&str> = obj.keys().map(String::as_str).collect();
        got.sort_unstable();
        let mut want: Vec<&str> = expected.to_vec();
        want.sort_unstable();
        assert_eq!(got, want);
    }

    #[tokio::test]
    async fn ratio_and_formats_derive_from_extensions() {
        let dir = tempdir().unwrap();
        let input = dir.path().join("game.iso");
        let output = dir.path().join("game.rvz");
        std::fs::write(&input, vec![0u8; 1024]).unwrap();
        std::fs::write(&output, vec![0u8; 256]).unwrap();

        let summary = build_comparison(
            Arc::new(NoProgress),
            &input,
            &output,
            1024,
            256,
            rom_converto_lib::util::OutputVerify::Rvz,
            false,
            &CancelToken::new(),
        )
        .await;

        assert_eq!(summary.input_format, "ISO");
        assert_eq!(summary.output_format, "RVZ");
        assert_eq!(summary.ratio_pct, Some(75.0));
    }

    #[tokio::test]
    async fn negative_ratio_when_output_grew() {
        let dir = tempdir().unwrap();
        let input = dir.path().join("game.cso");
        let output = dir.path().join("game.iso");
        std::fs::write(&input, vec![0u8; 256]).unwrap();
        std::fs::write(&output, vec![0u8; 1024]).unwrap();

        let summary = build_comparison(
            Arc::new(NoProgress),
            &input,
            &output,
            256,
            1024,
            rom_converto_lib::util::OutputVerify::None,
            false,
            &CancelToken::new(),
        )
        .await;

        assert!(summary.ratio_pct.unwrap() < 0.0);
    }

    #[tokio::test]
    async fn verify_after_false_skips_verify_and_hash() {
        let dir = tempdir().unwrap();
        let input = dir.path().join("game.iso");
        let output = dir.path().join("game.chd");
        std::fs::write(&input, vec![0u8; 8]).unwrap();
        std::fs::write(&output, vec![0u8; 8]).unwrap();

        let summary = build_comparison(
            Arc::new(NoProgress),
            &input,
            &output,
            8,
            8,
            rom_converto_lib::util::OutputVerify::Chd,
            false,
            &CancelToken::new(),
        )
        .await;

        assert!(summary.verify.is_none());
        assert!(summary.output_sha1.is_none());
    }

    #[tokio::test]
    async fn corrupt_chd_output_fails_verify() {
        let dir = tempdir().unwrap();
        let input = dir.path().join("game.iso");
        let output = dir.path().join("game.chd");
        std::fs::write(&input, b"not a real disc image").unwrap();
        std::fs::write(&output, b"not a real chd container").unwrap();

        let summary = build_comparison(
            Arc::new(NoProgress),
            &input,
            &output,
            22,
            25,
            rom_converto_lib::util::OutputVerify::Chd,
            true,
            &CancelToken::new(),
        )
        .await;

        let verify = summary.verify.expect("verify_after=true fills in verify");
        assert!(!verify.ok);
        assert!(!verify.round_trip);
        assert!(summary.output_sha1.is_some());
    }

    #[tokio::test]
    async fn corrupt_rvz_output_fails_verify() {
        let dir = tempdir().unwrap();
        let input = dir.path().join("game.iso");
        let output = dir.path().join("game.rvz");
        std::fs::write(&input, b"not a real disc image").unwrap();
        std::fs::write(&output, b"not a real rvz container").unwrap();

        let summary = build_comparison(
            Arc::new(NoProgress),
            &input,
            &output,
            22,
            24,
            rom_converto_lib::util::OutputVerify::Rvz,
            true,
            &CancelToken::new(),
        )
        .await;

        let verify = summary.verify.expect("verify_after=true fills in verify");
        assert!(!verify.ok);
        // Rvz's check is structural only, never a full round-trip decode.
        assert!(!verify.round_trip);
    }

    #[tokio::test]
    async fn nx_missing_header_key_is_not_reported_as_verified() {
        let dir = tempdir().unwrap();
        let input = dir.path().join("game.nsp");
        let output = dir.path().join("game.nsz");
        std::fs::write(&input, vec![0u8; 4]).unwrap();
        std::fs::write(&output, vec![0u8; 4]).unwrap();

        let summary = build_comparison(
            Arc::new(NoProgress),
            &input,
            &output,
            4,
            4,
            rom_converto_lib::util::OutputVerify::Nx(Box::new(KeySet::default())),
            true,
            &CancelToken::new(),
        )
        .await;

        // A keyset with no header key can't actually run the check, so this
        // must not be reported as a passed round-trip verification.
        let verify = summary.verify.expect("verify_after=true fills in verify");
        assert!(!verify.ok);
        assert!(!verify.round_trip);
    }

    #[tokio::test]
    async fn no_verify_target_reports_no_verify() {
        let dir = tempdir().unwrap();
        let input = dir.path().join("game.cia");
        let output = dir.path().join("game.3ds");
        std::fs::write(&input, vec![0u8; 4]).unwrap();
        std::fs::write(&output, vec![0u8; 4]).unwrap();

        let summary = build_comparison(
            Arc::new(NoProgress),
            &input,
            &output,
            4,
            4,
            rom_converto_lib::util::OutputVerify::None,
            true,
            &CancelToken::new(),
        )
        .await;

        // No integrity check exists for this format, so the card must not
        // show a "Verified" badge for a check that never ran. The output
        // hash is still computed since it doesn't depend on a check.
        assert!(summary.verify.is_none());
        assert!(summary.output_sha1.is_some());
    }

    #[test]
    fn comparison_summary_keys_are_snake_case() {
        let summary = ComparisonSummary {
            input_bytes: 1024,
            output_bytes: 256,
            ratio_pct: Some(75.0),
            input_format: "ISO".into(),
            output_format: "RVZ".into(),
            output_sha1: Some("abc123".into()),
            verify: Some(VerifyReport {
                ok: true,
                round_trip: false,
                message: "Verified".into(),
            }),
        };
        let v = serde_json::to_value(&summary).unwrap();
        assert_keys(
            &v,
            &[
                "input_bytes",
                "output_bytes",
                "ratio_pct",
                "input_format",
                "output_format",
                "output_sha1",
                "verify",
            ],
        );
        assert_keys(&v["verify"], &["ok", "round_trip", "message"]);
    }
}
