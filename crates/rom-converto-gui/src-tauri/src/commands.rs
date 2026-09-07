use crate::info_cache::InfoCache;
use crate::progress::TauriProgress;
use rom_converto_lib::chd::{
    ChdCodec, ChdOptions, DiscMode, convert_disc_to_chd, extract_from_chd, migrate_chd_to_v5,
    verify_chd,
};
use rom_converto_lib::cso::{
    CsoCompressOptions, CsoFormat, compress_to_cso, decompress_from_cso, verify_cso,
};
use rom_converto_lib::cue::merge::merge_bin;
use rom_converto_lib::cue::to_iso::cue_to_iso;
use rom_converto_lib::dat::digest::quick_crc_digest;
use rom_converto_lib::dat::model::{
    BulkIdentifyIdsResult, BulkIdentifyItem, BulkItemStatus, GameAndRelationMatchResult,
    GameFileMatchSearch,
};
use rom_converto_lib::dat::rename::{RenameAction, RenameCandidate, RenamePlan, plan_renames};
use rom_converto_lib::dat::verdict::{DatVerdict, MatchStrength, match_strength, reconcile_tracks};
use rom_converto_lib::dat::{
    DEFAULT_API_BASE, PlaymatchClient, RomDigests, TrackDigests, digest_inner_async,
};
use rom_converto_lib::info::{DiscContent, InfoOptions, InfoResult, read_info};
use rom_converto_lib::microsoft::xbox::{
    XisoCreateOptions, convert_to_xiso, extract_xiso, read_info as xbox_read_info,
};
use rom_converto_lib::microsoft::xenon::{
    convert_to_god, extract_zar, pack_zar, read_info as xenon_read_info, verify_zar,
};
use rom_converto_lib::nintendo::ctr::convert::{convert_rom, derive_converted_path};
use rom_converto_lib::nintendo::ctr::verify::{CtrVerifyOptions, verify_ctr};
use rom_converto_lib::nintendo::ctr::z3ds::{
    compress_rom, decompress_rom, derive_compressed_path, derive_decompressed_path,
};
use rom_converto_lib::nintendo::ctr::{
    CdnToCiaOptions, convert_cdn_to_cia, decrypt_rom, derive_decrypted_path, derive_encrypted_path,
    encrypt_rom, generate_ticket_from_cdn,
};
use rom_converto_lib::nintendo::dol::verify::{DolVerifyOptions, verify_dol};
use rom_converto_lib::nintendo::nds::{
    NdsError, decrypt_nds_rom, derive_decrypted_path as derive_nds_decrypted_path,
    derive_encrypted_path as derive_nds_encrypted_path, encrypt_nds_rom,
};
use rom_converto_lib::nintendo::nx::{
    KeySet, NczMode, NxCompressOptions, NxMergeFormat, compress_container_async,
    decompress_container_async, derive_compressed_path as nx_derive_compressed_path,
    derive_decompressed_path as nx_derive_decompressed_path, detect_container, find_keys_file,
    load_keyset, merge_containers_async, split_container_async, verify_container_async,
};
use rom_converto_lib::nintendo::rvl::verify::{RvlVerifyOptions, verify_rvl};
use rom_converto_lib::nintendo::rvz::{
    RvzCompressOptions, compress_disc, decompress_disc, decompress_disc_to_wbfs, derive_disc_path,
    derive_rvz_path, verify_rvz_structure,
};
use rom_converto_lib::nintendo::wup::{
    TitleInput, WupCompressOptions, compress_titles_async, decrypt_nus_title_async,
    verify_wup_async,
};
use rom_converto_lib::pipeline::{chd_to_cso, cso_to_chd, cue_to_cso};
use rom_converto_lib::playlist::{PlaylistMode, PlaylistOptions, plan_playlists};
use rom_converto_lib::sony::ps3::{
    Ps3Error, decrypt_ps3_iso, derive_decrypted_path as derive_ps3_decrypted_path, resolve_ps3_key,
};
use rom_converto_lib::sony::psp::{extract_segments, to_iso as psp_to_iso};
use rom_converto_lib::sony::vita::pkg::extract as vita_pkg_extract;
use rom_converto_lib::util::HashCache;
use rom_converto_lib::util::NX_DAT_UNSUPPORTED_HINT;
use rom_converto_lib::util::fs::{collect_all_files, collect_files_with_exts};
use rom_converto_lib::util::{
    CancelToken, Cancelled, ConflictPolicy, ConflictResolution, DEFAULT_SPACE_HEADROOM, FileStatus,
    HashAlgo, PlanLine, ProgressReporter, ReportFormat, ReportRecord, ReportRecordInput,
    ReportTotals, TemplateTokens, apply_template, available_space, format_bytes, hash_file,
    mixed_playlist_extensions, oversized_rvz_chunk, parse_algos, resolve_conflict, space_shortfall,
    write_report,
};
use rom_converto_lib::util::{ChecksumBounds, parse_checksum_bound};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, State};

use crate::err_to_string;

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

    /// A skip because the input is already in the target format (PS3's and
    /// NDS's already-done checks), distinct from a conflict-policy skip:
    /// there is no output path, and the record's error carries the
    /// detection reason.
    fn skipped_already_done(
        report: bool,
        input: &Path,
        operation: &str,
        message: &str,
        reason: &impl std::fmt::Display,
    ) -> Self {
        Self {
            message: format!("Skipped {}: {message}", input.display()),
            record: report.then(|| {
                ReportRecord::new(ReportRecordInput {
                    input_path: input.display().to_string(),
                    output_path: String::new(),
                    operation: operation.into(),
                    status: FileStatus::Skipped,
                    input_bytes: 0,
                    output_bytes: 0,
                    elapsed_ms: 0,
                    error: Some(reason.to_string()),
                })
            }),
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
            let ok = verify_chd(progress, output.to_path_buf(), None, false, cancel.clone())
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
            let ok = verify_cso(progress, output.to_path_buf(), true, CancelToken::new())
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
            let ok = verify_rvz_structure(output, &CancelToken::new())
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
            match verify_container_async(output.to_path_buf(), *keys, progress, CancelToken::new())
                .await
            {
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
struct ComparisonInput<'a> {
    input: &'a Path,
    output: &'a Path,
    input_bytes: u64,
    output_bytes: u64,
    target: rom_converto_lib::util::OutputVerify,
    verify_after: bool,
}

async fn build_comparison(
    progress: Arc<dyn ProgressReporter>,
    cancel: &CancelToken,
    args: ComparisonInput<'_>,
) -> ComparisonSummary {
    let ComparisonInput {
        input,
        output,
        input_bytes,
        output_bytes,
        target,
        verify_after,
    } = args;
    let mut summary = comparison_sizes(input, output, input_bytes, output_bytes);

    let (verify, output_sha1) = if verify_after {
        let verify = run_comparison_verify(progress.as_ref(), output, target, cancel).await;

        let output_owned = output.to_path_buf();
        let progress_for_hash = progress.clone();
        let cancel_for_hash = cancel.clone();
        let sha1 = tokio::task::spawn_blocking(move || {
            hash_file(
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

const CTR_DECRYPT_EXTS: &[&str] = &["cia", "3ds", "cci", "cxi"];
const CTR_ENCRYPT_EXTS: &[&str] = &["cia", "3ds", "cci", "cxi"];
const CTR_COMPRESS_EXTS: &[&str] = &["cia", "cci", "3ds", "cxi", "3dsx"];
const CTR_DECOMPRESS_EXTS: &[&str] = &["zcia", "zcci", "zcxi", "z3dsx"];
const CTR_CONVERT_EXTS: &[&str] = &["cia", "3ds", "cci"];
const ALL_IMAGE_EXTS: &[&str] = &[
    "iso", "gcm", "wbfs", "rvz", "gcz", "wia", "nkit", "chd", "cso", "zso", "dax", "cue", "cia",
    "3ds", "cci", "cxi", "3dsx", "zcia", "zcci", "zcxi", "z3dsx", "nsp", "xci", "nca", "nsz",
    "xcz", "ncz", "wud", "wux", "xiso", "zar", "avi",
];

/// Resolve a read input, transparently extracting the first member matching
/// `exts` when it is an archive. Extraction is blocking, so it runs off the
/// async runtime. The returned guard owns the temp dir and must stay alive
/// until the read that uses its `path()` completes.
async fn resolve_archive_input(
    input: PathBuf,
    exts: &'static [&'static str],
) -> Result<rom_converto_lib::util::ResolvedInput, String> {
    tokio::task::spawn_blocking(move || rom_converto_lib::util::resolve_input(&input, exts))
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)
}

/// Pick the output path for a write command. When a template is given it
/// supersedes any explicit output, mirroring the CLI's `conflicts_with` rule;
/// supplying both at once is rejected.
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
    Some(ReportRecord::new(ReportRecordInput {
        input_path: input.display().to_string(),
        output_path: output.display().to_string(),
        operation: operation.into(),
        status: FileStatus::Ok,
        input_bytes,
        output_bytes,
        elapsed_ms,
        error: None,
    }))
}

/// Build a skipped record for a conflict-policy skip, matching the CLI's
/// `skipped_record(input, op, None)`: empty output path, zero bytes, no error.
fn build_skip_record(enabled: bool, input: &Path, operation: &str) -> Option<ReportRecord> {
    if !enabled {
        return None;
    }
    Some(ReportRecord::new(ReportRecordInput {
        input_path: input.display().to_string(),
        output_path: String::new(),
        operation: operation.into(),
        status: FileStatus::Skipped,
        input_bytes: 0,
        output_bytes: 0,
        elapsed_ms: 0,
        error: None,
    }))
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
                let outcome = verify_existing_output(progress, desired, verify, CancelToken::new())
                    .await
                    .unwrap_or(VerifyOutcome::Invalid);
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

/// Whether a directory output path already holds something a write would
/// clobber. A path that exists but is a file counts as occupied.
fn output_dir_occupied(path: &Path) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    if path.is_file() {
        return Ok(true);
    }
    Ok(std::fs::read_dir(path)
        .map_err(err_to_string)?
        .next()
        .is_some())
}

/// Resolve a directory output. Directories cannot auto-number, so `rename`
/// is rejected; `skip` keeps a directory that already holds files and
/// `error` refuses it. Mirrors the CLI's `resolve_output_dir`.
fn resolve_output_dir(path: &Path, on_conflict: Option<&str>) -> Result<Option<PathBuf>, String> {
    if !output_dir_occupied(path)? {
        return Ok(Some(path.to_path_buf()));
    }
    let policy = conflict_policy(on_conflict);
    if path.is_file() {
        return match policy {
            ConflictPolicy::Overwrite => Ok(Some(path.to_path_buf())),
            ConflictPolicy::Skip | ConflictPolicy::OverwriteInvalid => Ok(None),
            _ => Err(format!(
                "output path exists and is a file, use --on-conflict overwrite to replace it: {}",
                path.display()
            )),
        };
    }
    match policy {
        ConflictPolicy::Overwrite => Ok(Some(path.to_path_buf())),
        ConflictPolicy::Skip | ConflictPolicy::OverwriteInvalid => Ok(None),
        ConflictPolicy::Rename => Err(format!(
            "rename is not supported for directory outputs, use overwrite/skip/error: {}",
            path.display()
        )),
        ConflictPolicy::Error => Err(format!(
            "output directory is not empty, use overwrite to replace it: {}",
            path.display()
        )),
    }
}

/// Build the dry-run plan line for a single write, mirroring the CLI's
/// per-file planning: resolve the conflict, and for `overwrite-invalid` run the
/// read-only verify to choose keep-valid versus rewrite-invalid. Nothing is
/// written; the only filesystem access is reading the input and the read-only
/// verify. The resulting `PlanLine` renders byte-identically to the CLI plan.
struct PlanInput<'a> {
    operation: &'a str,
    input: &'a Path,
    desired: &'a Path,
    on_conflict: Option<&'a str>,
    media: Option<String>,
    verify: rom_converto_lib::util::OutputVerify,
    missing_keys: Option<String>,
}

async fn plan_line(
    progress: &dyn rom_converto_lib::util::ProgressReporter,
    args: PlanInput<'_>,
) -> Result<PlanLine, String> {
    let PlanInput {
        operation,
        input,
        desired,
        on_conflict,
        media,
        verify,
        missing_keys,
    } = args;
    use rom_converto_lib::util::{PlanDecision, VerifyOutcome, classify, verify_existing_output};
    let policy = conflict_policy(on_conflict);
    let resolution = resolve_conflict(desired, policy).map_err(err_to_string)?;
    let (output, decision) = if policy == ConflictPolicy::OverwriteInvalid && desired.exists() {
        match verify_existing_output(progress, desired, verify, CancelToken::new())
            .await
            .unwrap_or(VerifyOutcome::Invalid)
        {
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
        Some("avi") => Some("LaserDisc".to_string()),
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

/// Registry of the cancel tokens for every operation currently running,
/// keyed by task id. Concurrent batch jobs run in their own slot (one
/// task id per slot), so cancelling one must not touch the others.
pub type ActiveCancel = Arc<tokio::sync::Mutex<HashMap<String, CancelToken>>>;

async fn begin(state: &ActiveCancel, key: &str) -> CancelToken {
    let token = CancelToken::new();
    state.lock().await.insert(key.to_string(), token.clone());
    token
}

async fn finish(state: &ActiveCancel, key: &str) {
    state.lock().await.remove(key);
}

/// Cancels one in-flight task by id, or every in-flight task when `task_id`
/// is `None` (the frontend's "abort everything" fallback).
#[tauri::command]
pub async fn cmd_cancel(
    state: State<'_, ActiveCancel>,
    task_id: Option<String>,
) -> Result<(), String> {
    let map = state.lock().await;
    match task_id {
        Some(id) => {
            if let Some(token) = map.get(&id) {
                token.cancel();
            }
        }
        None => {
            for token in map.values() {
                token.cancel();
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod cancel_registry_tests {
    use super::*;

    #[tokio::test]
    async fn cancel_one_leaves_others_live() {
        let state: ActiveCancel = Arc::default();
        let a = {
            let token = CancelToken::new();
            state.lock().await.insert("a".to_string(), token.clone());
            token
        };
        let b = {
            let token = CancelToken::new();
            state.lock().await.insert("b".to_string(), token.clone());
            token
        };
        {
            let map = state.lock().await;
            if let Some(token) = map.get("a") {
                token.cancel();
            }
        }
        assert!(a.is_cancelled());
        assert!(!b.is_cancelled());
    }

    #[tokio::test]
    async fn cancel_all_drains_every_token() {
        let state: ActiveCancel = Arc::default();
        let a = {
            let token = CancelToken::new();
            state.lock().await.insert("a".to_string(), token.clone());
            token
        };
        let b = {
            let token = CancelToken::new();
            state.lock().await.insert("b".to_string(), token.clone());
            token
        };
        {
            let map = state.lock().await;
            for token in map.values() {
                token.cancel();
            }
        }
        assert!(a.is_cancelled());
        assert!(b.is_cancelled());
    }

    #[tokio::test]
    async fn begin_and_finish_track_by_key() {
        let state: ActiveCancel = Arc::default();
        let token = begin(&state, "slot-0").await;
        assert!(state.lock().await.contains_key("slot-0"));
        finish(&state, "slot-0").await;
        assert!(!state.lock().await.contains_key("slot-0"));
        assert!(!token.is_cancelled());
    }
}

/// The conflict/space/cancel/dry-run tail shared by every write command.
/// Flattened into each args struct, so the wire format stays flat.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommonArgs {
    on_conflict: Option<String>,
    skip_space_check: bool,
    dry_run: Option<bool>,
    task_id: Option<String>,
}

/// Runs `body` on a dedicated thread with its own current-thread runtime, for
/// conversions whose futures are not `Send`: the streaming crypt pipelines hold
/// their worker-pool receiver across await points, and `ChdReader`'s nested
/// async types exceed the compiler's Send-inference recursion limit.
fn on_dedicated_runtime<F>(body: F) -> Result<(), String>
where
    F: FnOnce(&tokio::runtime::Runtime) -> Result<(), String> + Send + 'static,
{
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(err_to_string)?;
        body(&rt)
    })
    .join()
    .map_err(|_| {
        "The operation failed unexpectedly. Try again, and report a bug if it keeps happening."
            .to_string()
    })?
}

/// Load the NX keyset. A dry run falls back to an empty keyset and reports why
/// the real one is missing, so the plan line can flag it; a real run refuses.
fn keyset_for_run(keys: Option<&Path>, dry_run: bool) -> Result<(KeySet, Option<String>), String> {
    match load_keyset(keys) {
        Ok(keyset) => Ok((keyset, None)),
        Err(e) if dry_run => Ok((KeySet::default(), Some(e.to_string()))),
        Err(e) => Err(err_to_string(e)),
    }
}

/// What one single-file write needs beyond the conversion call itself.
/// `input` is the path the user staged, which is what plan lines and report
/// records name; the conversion reads whatever the caller resolved, which for
/// an archive input is an extracted member.
struct SingleFileOp<'a> {
    state: &'a ActiveCancel,
    progress: Arc<TauriProgress>,
    key: &'a str,
    operation: &'a str,
    input: &'a Path,
    desired: PathBuf,
    on_conflict: Option<&'a str>,
    verify: rom_converto_lib::util::OutputVerify,
    media: Option<String>,
    missing_keys: Option<String>,
    input_bytes: u64,
    required_bytes: u64,
    skip_space_check: bool,
    report: bool,
    verify_after: bool,
    dry_run: bool,
    /// Bytes written. `extracted_output_size` for outputs that are a cue sheet
    /// plus the data files it references; `input_size` for a single file.
    output_size: fn(&Path) -> u64,
}

/// Drives one single-file write: the dry-run plan, conflict resolution, the
/// free-space preflight, the conversion under a cancel token, then the report
/// record and the comparison card. `run` performs the conversion and may
/// short-circuit with an outcome of its own, which is how the "already in the
/// target format" skips surface.
async fn run_single_file_op<F, Fut>(spec: SingleFileOp<'_>, run: F) -> Result<RunOutcome, String>
where
    F: FnOnce(PathBuf, CancelToken) -> Fut,
    Fut: Future<Output = Result<Option<RunOutcome>, String>> + Send + 'static,
{
    let SingleFileOp {
        state,
        progress,
        key,
        operation,
        input,
        desired,
        on_conflict,
        verify,
        media,
        missing_keys,
        input_bytes,
        required_bytes,
        skip_space_check,
        report,
        verify_after,
        dry_run,
        output_size,
    } = spec;
    if dry_run {
        let line = plan_line(
            progress.as_ref(),
            PlanInput {
                operation,
                input,
                desired: &desired,
                on_conflict,
                media,
                verify: verify.clone(),
                missing_keys,
            },
        )
        .await?;
        return Ok(RunOutcome::text(line.display_text()));
    }
    let Some(output) =
        resolve_output(progress.as_ref(), &desired, on_conflict, verify.clone()).await?
    else {
        return Ok(RunOutcome::skipped(report, input, operation, &desired));
    };
    let out_display = output.display().to_string();
    preflight_space(
        output.parent().unwrap_or(&output),
        required_bytes,
        skip_space_check,
    )?;
    let token = begin(state, key).await;
    let started = Instant::now();
    let joined = tokio::spawn(run(output.clone(), token.clone())).await;
    finish(state, key).await;
    if let Some(outcome) = joined.map_err(err_to_string)?? {
        return Ok(outcome);
    }
    let output_bytes = output_size(&output);
    let record = build_record(
        report,
        input,
        &output,
        operation,
        input_bytes,
        output_bytes,
        started.elapsed(),
    );
    let comparison = build_comparison(
        progress,
        &token,
        ComparisonInput {
            input,
            output: &output,
            input_bytes,
            output_bytes,
            target: verify,
            verify_after,
        },
    )
    .await;
    Ok(RunOutcome {
        message: format!("Wrote {out_display}"),
        record,
        input_bytes,
        output_bytes,
        comparison: Some(comparison),
    })
}

/// Write a run report from records the frontend accumulated during a run. The
/// format is inferred from the path extension and the file is written directly,
/// bypassing the on-conflict machinery, exactly as the CLI does.
#[tauri::command]
pub async fn cmd_write_report(path: PathBuf, payload: ReportPayload) -> Result<(), String> {
    let format = ReportFormat::from_path(&path);
    tokio::task::spawn_blocking(move || {
        write_report(
            &path,
            &payload.records,
            &payload.totals,
            format,
            &CancelToken::new(),
        )
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

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CdnToCiaArgs {
    cdn_dir: PathBuf,
    output: Option<PathBuf>,
    decrypt: bool,
    compress: bool,
    cleanup: bool,
    recursive: bool,
    ensure_ticket_exists: bool,
    on_conflict: Option<String>,
    skip_space_check: bool,
}

#[tauri::command]
pub async fn cmd_cdn_to_cia(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: CdnToCiaArgs,
) -> Result<String, String> {
    let CdnToCiaArgs {
        cdn_dir,
        output,
        decrypt,
        compress,
        cleanup,
        recursive,
        ensure_ticket_exists,
        on_conflict,
        skip_space_check,
    } = args;
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
    let required: u64 = collect_all_files(&opts.cdn_dir, None, &CancelToken::new())
        .map(|files| files.iter().map(|p| input_size(p)).sum())
        .unwrap_or(0);
    let probe_dir = opts
        .output
        .as_deref()
        .and_then(|p| p.parent())
        .or_else(|| opts.cdn_dir.parent())
        .unwrap_or(opts.cdn_dir.as_path());
    preflight_space(probe_dir, required, skip_space_check)?;
    let token = begin(&state, "cdn-to-cia").await;
    // The streaming decrypt holds the worker-pool receiver across await points,
    // so its future is not Send; run on a dedicated thread with its own runtime.
    let result = std::thread::spawn(move || -> Result<(), String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(err_to_string)?;
        rt.block_on(convert_cdn_to_cia(
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
    finish(&state, "cdn-to-cia").await;
    result??;
    Ok("CDN to CIA conversion complete".to_string())
}

#[tauri::command]
pub async fn cmd_generate_ticket(cdn_dir: PathBuf, output: PathBuf) -> Result<String, String> {
    let out_display = output.display().to_string();
    tokio::spawn(
        async move { generate_ticket_from_cdn(&cdn_dir, &output, &CancelToken::new()).await },
    )
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)?;
    Ok(format!("Wrote {out_display}"))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecryptRomArgs {
    input: PathBuf,
    output: Option<PathBuf>,
    output_template: Option<String>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_decrypt_rom(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: DecryptRomArgs,
) -> Result<RunOutcome, String> {
    let DecryptRomArgs {
        input,
        output,
        output_template,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("decrypt");
    let dry_run = common.dry_run.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input.clone(), CTR_DECRYPT_EXTS).await?;
    let basis = resolved.output_basis().to_path_buf();
    let ext = ext_of(&derive_decrypted_path(&basis));
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &basis,
        &ext,
        None,
        || derive_decrypted_path(&basis),
        dry_run,
    )?;
    let source = resolved.path().to_path_buf();
    let runner = progress.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key,
            operation: "decrypt",
            input: &input,
            desired,
            on_conflict: common.on_conflict.as_deref(),
            verify: rom_converto_lib::util::OutputVerify::None,
            media: None,
            missing_keys: None,
            input_bytes: input_size(&input),
            required_bytes: input_size(&source),
            skip_space_check: common.skip_space_check,
            report: false,
            verify_after: false,
            dry_run,
            output_size: input_size,
        },
        move |output, token| async move {
            on_dedicated_runtime(move |rt| {
                rt.block_on(decrypt_rom(&source, &output, runner.as_ref(), token))
                    .map_err(err_to_string)
            })?;
            Ok(None)
        },
    )
    .await
}

#[tauri::command]
pub async fn cmd_encrypt_rom(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: DecryptRomArgs,
) -> Result<RunOutcome, String> {
    let DecryptRomArgs {
        input,
        output,
        output_template,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("encrypt");
    let dry_run = common.dry_run.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input.clone(), CTR_ENCRYPT_EXTS).await?;
    let basis = resolved.output_basis().to_path_buf();
    let ext = ext_of(&derive_encrypted_path(&basis));
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &basis,
        &ext,
        None,
        || derive_encrypted_path(&basis),
        dry_run,
    )?;
    let source = resolved.path().to_path_buf();
    let runner = progress.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key,
            operation: "encrypt",
            input: &input,
            desired,
            on_conflict: common.on_conflict.as_deref(),
            verify: rom_converto_lib::util::OutputVerify::None,
            media: None,
            missing_keys: None,
            input_bytes: input_size(&input),
            required_bytes: input_size(&source),
            skip_space_check: common.skip_space_check,
            report: false,
            verify_after: false,
            dry_run,
            output_size: input_size,
        },
        move |output, token| async move {
            on_dedicated_runtime(move |rt| {
                rt.block_on(encrypt_rom(&source, &output, runner.as_ref(), token))
                    .map_err(err_to_string)
            })?;
            Ok(None)
        },
    )
    .await
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompressRomArgs {
    input: PathBuf,
    output: Option<PathBuf>,
    level: Option<i32>,
    allow_encrypted: bool,
    output_template: Option<String>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_compress_rom(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: CompressRomArgs,
) -> Result<RunOutcome, String> {
    let CompressRomArgs {
        input,
        output,
        level,
        allow_encrypted,
        output_template,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("compress");
    let dry_run = common.dry_run.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input.clone(), CTR_COMPRESS_EXTS).await?;
    let basis = resolved.output_basis().to_path_buf();
    let ext = ext_of(&derive_compressed_path(&basis));
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &basis,
        &ext,
        None,
        || derive_compressed_path(&basis),
        dry_run,
    )?;
    let source = resolved.path().to_path_buf();
    let runner = progress.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key,
            operation: "compress",
            input: &input,
            desired,
            on_conflict: common.on_conflict.as_deref(),
            verify: rom_converto_lib::util::OutputVerify::None,
            media: None,
            missing_keys: None,
            input_bytes: input_size(&input),
            required_bytes: input_size(&source),
            skip_space_check: common.skip_space_check,
            report: false,
            verify_after: false,
            dry_run,
            output_size: input_size,
        },
        move |output, token| async move {
            compress_rom(
                &source,
                &output,
                level,
                allow_encrypted,
                runner.as_ref(),
                token,
            )
            .await
            .map_err(err_to_string)?;
            Ok(None)
        },
    )
    .await
}

#[tauri::command]
pub async fn cmd_decompress_rom(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: DecryptRomArgs,
) -> Result<RunOutcome, String> {
    let DecryptRomArgs {
        input,
        output,
        output_template,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("decompress");
    let dry_run = common.dry_run.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input.clone(), CTR_DECOMPRESS_EXTS).await?;
    let basis = resolved.output_basis().to_path_buf();
    let ext = ext_of(&derive_decompressed_path(&basis));
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &basis,
        &ext,
        None,
        || derive_decompressed_path(&basis),
        dry_run,
    )?;
    let source = resolved.path().to_path_buf();
    let runner = progress.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key,
            operation: "decompress",
            input: &input,
            desired,
            on_conflict: common.on_conflict.as_deref(),
            verify: rom_converto_lib::util::OutputVerify::None,
            media: None,
            missing_keys: None,
            input_bytes: input_size(&input),
            required_bytes: input_size(&source),
            skip_space_check: common.skip_space_check,
            report: false,
            verify_after: false,
            dry_run,
            output_size: input_size,
        },
        move |output, token| async move {
            decompress_rom(&source, &output, runner.as_ref(), token)
                .await
                .map_err(err_to_string)?;
            Ok(None)
        },
    )
    .await
}

/// Builds CHD creation options from explicit args, falling back to the
/// ambient config's `[chd]` defaults, then to the format's built-in
/// defaults.
fn resolve_chd_opts(
    hunk_size: Option<u32>,
    codecs: Option<Vec<String>>,
    level: Option<i32>,
) -> Result<ChdOptions, String> {
    let defaults = rom_converto_lib::config::load_config(None)
        .ok()
        .and_then(|cfg| cfg.chd);
    let codecs = codecs
        .or_else(|| defaults.as_ref().and_then(|d| d.codecs.clone()))
        .map(|names| {
            names
                .iter()
                .map(|name| name.parse::<ChdCodec>())
                .collect::<Result<Vec<_>, _>>()
                .map_err(err_to_string)
        })
        .transpose()?;
    Ok(ChdOptions {
        hunk_size: hunk_size.or_else(|| defaults.as_ref().and_then(|d| d.hunk_size)),
        codecs,
        level: level.or_else(|| defaults.as_ref().and_then(|d| d.level)),
        force: true,
    })
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChdCompressArgs {
    input_path: PathBuf,
    output: Option<PathBuf>,
    codecs: Option<Vec<String>>,
    level: Option<i32>,
    hunk_size: Option<u32>,
    mode: Option<String>,
    output_template: Option<String>,
    report: Option<bool>,
    verify_after: Option<bool>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_chd_compress(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: ChdCompressArgs,
) -> Result<RunOutcome, String> {
    let ChdCompressArgs {
        input_path,
        output,
        codecs,
        level,
        hunk_size,
        mode,
        output_template,
        report,
        verify_after,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("chd-compress");
    let dry_run = common.dry_run.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input_path.clone(), &["iso", "cue", "avi"]).await?;
    let basis = resolved.output_basis().to_path_buf();
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &basis,
        "chd",
        None,
        || basis.with_extension("chd"),
        dry_run,
    )?;
    // A cue sheet's own size says nothing about the data it points at.
    let in_bytes = if ext_of(resolved.path()).eq_ignore_ascii_case("cue") {
        rom_converto_lib::cue::referenced_files_size(resolved.path())
            .await
            .unwrap_or_else(|_| input_size(resolved.path()))
    } else {
        input_size(resolved.path())
    };
    let mode = match mode.as_deref() {
        Some("cd") => Some(DiscMode::Cd),
        Some("dvd") => Some(DiscMode::Dvd),
        Some("ld") => Some(DiscMode::Ld),
        _ => None,
    };
    let opts = resolve_chd_opts(hunk_size, codecs, level)?;
    let source = resolved.path().to_path_buf();
    let runner = progress.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key,
            operation: "compress",
            input: &input_path,
            desired,
            on_conflict: common.on_conflict.as_deref(),
            verify: rom_converto_lib::util::OutputVerify::Chd,
            media: dry_run.then(|| chd_media_label(&input_path)).flatten(),
            missing_keys: None,
            input_bytes: in_bytes,
            required_bytes: in_bytes,
            skip_space_check: common.skip_space_check,
            report: report.unwrap_or(false),
            verify_after: verify_after.unwrap_or(false),
            dry_run,
            output_size: input_size,
        },
        move |output, token| async move {
            convert_disc_to_chd(runner.as_ref(), source, output, mode, opts, token)
                .await
                .map_err(err_to_string)?;
            Ok(None)
        },
    )
    .await
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChdMigrateArgs {
    input_path: PathBuf,
    output: Option<PathBuf>,
    codecs: Option<Vec<String>>,
    level: Option<i32>,
    hunk_size: Option<u32>,
    output_template: Option<String>,
    report: Option<bool>,
    verify_after: Option<bool>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_chd_migrate(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: ChdMigrateArgs,
) -> Result<RunOutcome, String> {
    let ChdMigrateArgs {
        input_path,
        output,
        codecs,
        level,
        hunk_size,
        output_template,
        report,
        verify_after,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("chd-migrate");
    let dry_run = common.dry_run.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input_path.clone(), &["chd"]).await?;
    let basis = resolved.output_basis().to_path_buf();
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &basis,
        "chd",
        None,
        || rom_converto_lib::chd::migrated_chd_path(&basis),
        dry_run,
    )?;
    let in_bytes = input_size(resolved.path());
    let opts = resolve_chd_opts(hunk_size, codecs, level)?;
    let source = resolved.path().to_path_buf();
    let runner = progress.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key,
            operation: "migrate",
            input: &input_path,
            desired,
            on_conflict: common.on_conflict.as_deref(),
            verify: rom_converto_lib::util::OutputVerify::Chd,
            media: None,
            missing_keys: None,
            input_bytes: in_bytes,
            required_bytes: in_bytes,
            skip_space_check: common.skip_space_check,
            report: report.unwrap_or(false),
            verify_after: verify_after.unwrap_or(false),
            dry_run,
            output_size: input_size,
        },
        move |output, token| async move {
            migrate_chd_to_v5(runner.as_ref(), source, output, opts, token)
                .await
                .map_err(err_to_string)?;
            Ok(None)
        },
    )
    .await
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CsoCompressArgs {
    input_path: PathBuf,
    output: Option<PathBuf>,
    format: String,
    block_size: Option<u32>,
    output_template: Option<String>,
    report: Option<bool>,
    verify_after: Option<bool>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_cso_compress(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: CsoCompressArgs,
) -> Result<RunOutcome, String> {
    let CsoCompressArgs {
        input_path,
        output,
        format,
        block_size,
        output_template,
        report,
        verify_after,
        common,
    } = args;
    let format = match format.as_str() {
        "zso" => CsoFormat::Zso,
        _ => CsoFormat::Cso,
    };
    let key = common.task_id.as_deref().unwrap_or("cso-compress");
    let dry_run = common.dry_run.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input_path.clone(), &["iso"]).await?;
    let basis = resolved.output_basis().to_path_buf();
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &basis,
        format.extension(),
        None,
        || basis.with_extension(format.extension()),
        dry_run,
    )?;
    let opts = CsoCompressOptions {
        format,
        block_size,
        force: true,
    };
    let source = resolved.path().to_path_buf();
    let runner = progress.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key,
            operation: "compress",
            input: &input_path,
            desired,
            on_conflict: common.on_conflict.as_deref(),
            verify: rom_converto_lib::util::OutputVerify::Cso,
            media: Some(format.name().to_string()),
            missing_keys: None,
            input_bytes: input_size(&input_path),
            required_bytes: input_size(&source),
            skip_space_check: common.skip_space_check,
            report: report.unwrap_or(false),
            verify_after: verify_after.unwrap_or(false),
            dry_run,
            output_size: input_size,
        },
        move |output, token| async move {
            compress_to_cso(runner.as_ref(), source, output, opts, token)
                .await
                .map_err(err_to_string)?;
            Ok(None)
        },
    )
    .await
}

/// Compress a CSO/ZSO straight to a CHD through a temporary ISO, mirroring
/// `cmd_chd_compress` but calling the pipeline's chained conversion so the
/// temporary ISO is always cleaned up. DAX input is rejected by the pipeline
/// itself before anything is written.
#[tauri::command]
pub async fn cmd_cso_to_chd(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: ChdCompressArgs,
) -> Result<RunOutcome, String> {
    let ChdCompressArgs {
        input_path,
        output,
        codecs,
        level,
        hunk_size,
        mode,
        output_template,
        report,
        verify_after,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("cso-to-chd");
    let dry_run = common.dry_run.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input_path.clone(), &["cso", "zso", "dax"]).await?;
    let basis = resolved.output_basis().to_path_buf();
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &basis,
        "chd",
        None,
        || basis.with_extension("chd"),
        dry_run,
    )?;
    // The temporary ISO the pipeline stages is the real space requirement.
    let required = rom_converto_lib::cso::info::read_info(resolved.path())
        .map(|info| info.uncompressed_size)
        .unwrap_or_else(|_| input_size(resolved.path()));
    let mode = match mode.as_deref() {
        Some("cd") => Some(DiscMode::Cd),
        Some("dvd") => Some(DiscMode::Dvd),
        _ => None,
    };
    let opts = resolve_chd_opts(hunk_size, codecs, level)?;
    let source = resolved.path().to_path_buf();
    let runner = progress.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key,
            operation: "compress",
            input: &input_path,
            desired,
            on_conflict: common.on_conflict.as_deref(),
            verify: rom_converto_lib::util::OutputVerify::Chd,
            media: None,
            missing_keys: None,
            input_bytes: input_size(&input_path),
            required_bytes: required,
            skip_space_check: common.skip_space_check,
            report: report.unwrap_or(false),
            verify_after: verify_after.unwrap_or(false),
            dry_run,
            output_size: input_size,
        },
        move |output, token| async move {
            cso_to_chd(runner.as_ref(), source, output, mode, opts, token)
                .await
                .map_err(err_to_string)?;
            Ok(None)
        },
    )
    .await
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CsoDecompressArgs {
    input_path: PathBuf,
    output: Option<PathBuf>,
    output_template: Option<String>,
    report: Option<bool>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_cso_decompress(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: CsoDecompressArgs,
) -> Result<RunOutcome, String> {
    let CsoDecompressArgs {
        input_path,
        output,
        output_template,
        report,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("cso-decompress");
    let dry_run = common.dry_run.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input_path.clone(), &["cso", "zso", "dax"]).await?;
    let basis = resolved.output_basis().to_path_buf();
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &basis,
        "iso",
        None,
        || basis.with_extension("iso"),
        dry_run,
    )?;
    let source = resolved.path().to_path_buf();
    let runner = progress.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key,
            operation: "decompress",
            input: &input_path,
            desired,
            on_conflict: common.on_conflict.as_deref(),
            verify: rom_converto_lib::util::OutputVerify::None,
            media: None,
            missing_keys: None,
            input_bytes: input_size(&input_path),
            required_bytes: input_size(&source),
            skip_space_check: common.skip_space_check,
            report: report.unwrap_or(false),
            verify_after: false,
            dry_run,
            output_size: input_size,
        },
        move |output, token| async move {
            decompress_from_cso(runner.as_ref(), source, output, true, token)
                .await
                .map_err(err_to_string)?;
            Ok(None)
        },
    )
    .await
}

#[tauri::command]
pub async fn cmd_cso_verify(
    app: AppHandle,
    input_path: PathBuf,
    full: bool,
    task_id: Option<String>,
) -> Result<String, String> {
    let key = task_id.as_deref().unwrap_or("cso-verify");
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input_path, &["cso", "zso", "dax"]).await?;
    let resolved_path = resolved.path().to_path_buf();
    tokio::spawn(async move {
        verify_cso(progress.as_ref(), resolved_path, full, CancelToken::new()).await
    })
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
            PlanInput {
                operation: "merge",
                input: &cue_path,
                desired: &output,
                on_conflict: on_conflict.as_deref(),
                media: Some(format!("+ {}", output.with_extension("bin").display())),
                verify: rom_converto_lib::util::OutputVerify::None,
                missing_keys: None,
            },
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
    let resolved = resolve_archive_input(cue_path, &["cue"]).await?;
    let cue_path = resolved.path().to_path_buf();
    let required = rom_converto_lib::cue::referenced_files_size(&cue_path)
        .await
        .unwrap_or_else(|_| input_size(&cue_path));
    preflight_space(
        output.parent().unwrap_or(&output),
        required,
        skip_space_check,
    )?;
    tokio::spawn(async move {
        merge_bin(
            progress.as_ref(),
            cue_path,
            output,
            true,
            CancelToken::new(),
        )
        .await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)?;
    Ok(format!("Wrote {out_display}"))
}

#[tauri::command]
pub async fn cmd_cue_to_iso(
    app: AppHandle,
    cue_path: PathBuf,
    output: PathBuf,
    on_conflict: Option<String>,
    skip_space_check: bool,
    dry_run: Option<bool>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "cue-to-iso"));
    if dry_run.unwrap_or(false) {
        let line = plan_line(
            progress.as_ref(),
            PlanInput {
                operation: "to-iso",
                input: &cue_path,
                desired: &output,
                on_conflict: on_conflict.as_deref(),
                media: None,
                verify: rom_converto_lib::util::OutputVerify::None,
                missing_keys: None,
            },
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
    let resolved = resolve_archive_input(cue_path, &["cue"]).await?;
    let cue_path = resolved.path().to_path_buf();
    let required = rom_converto_lib::cue::referenced_files_size(&cue_path)
        .await
        .unwrap_or_else(|_| input_size(&cue_path));
    preflight_space(
        output.parent().unwrap_or(&output),
        required,
        skip_space_check,
    )?;
    tokio::spawn(async move { cue_to_iso(progress.as_ref(), cue_path, output, true).await })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?;
    Ok(format!("Wrote {out_display}"))
}

#[tauri::command]
pub async fn cmd_cue_to_cso(
    app: AppHandle,
    cue_path: PathBuf,
    output: PathBuf,
    format: String,
    on_conflict: Option<String>,
    skip_space_check: bool,
    dry_run: Option<bool>,
) -> Result<String, String> {
    let format = match format.as_str() {
        "cso" => CsoFormat::Cso,
        _ => CsoFormat::Zso,
    };
    let progress = Arc::new(TauriProgress::new(app, "cue-to-cso"));
    if dry_run.unwrap_or(false) {
        let line = plan_line(
            progress.as_ref(),
            PlanInput {
                operation: "to-cso",
                input: &cue_path,
                desired: &output,
                on_conflict: on_conflict.as_deref(),
                media: None,
                verify: rom_converto_lib::util::OutputVerify::None,
                missing_keys: None,
            },
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
    let resolved = resolve_archive_input(cue_path, &["cue"]).await?;
    let cue_path = resolved.path().to_path_buf();
    let required = rom_converto_lib::cue::referenced_files_size(&cue_path)
        .await
        .unwrap_or_else(|_| input_size(&cue_path));
    preflight_space(
        output.parent().unwrap_or(&output),
        required,
        skip_space_check,
    )?;
    tokio::spawn(
        async move { cue_to_cso(progress.as_ref(), cue_path, output, format, true).await },
    )
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)?;
    Ok(format!("Wrote {out_display}"))
}

// CHD extract and verify use deeply nested async types from ChdReader
// that exceed the compiler's recursion limit for Send inference. They run
// on a dedicated thread with its own tokio runtime to sidestep the issue.

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChdExtractArgs {
    input: PathBuf,
    output: Option<PathBuf>,
    parent: Option<PathBuf>,
    skip_space_check: bool,
    output_template: Option<String>,
    report: Option<bool>,
    dry_run: Option<bool>,
    task_id: Option<String>,
}

#[tauri::command]
pub async fn cmd_chd_extract(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: ChdExtractArgs,
) -> Result<RunOutcome, String> {
    let ChdExtractArgs {
        input,
        output,
        parent,
        skip_space_check,
        output_template,
        report,
        dry_run,
        task_id,
    } = args;
    let key = task_id.as_deref().unwrap_or("chd-extract");
    let dry_run = dry_run.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input.clone(), &["chd"]).await?;
    let basis = resolved.output_basis().to_path_buf();
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &basis,
        "cue",
        None,
        || basis.with_extension("cue"),
        dry_run,
    )?;
    let source = resolved.path().to_path_buf();
    let runner = progress.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key,
            operation: "extract",
            input: &input,
            desired,
            on_conflict: None,
            verify: rom_converto_lib::util::OutputVerify::None,
            media: None,
            missing_keys: None,
            input_bytes: input_size(&input),
            required_bytes: input_size(&source),
            skip_space_check,
            report: report.unwrap_or(false),
            verify_after: false,
            dry_run,
            output_size: extracted_output_size,
        },
        move |output, token| async move {
            on_dedicated_runtime(move |rt| {
                rt.block_on(extract_from_chd(
                    runner.as_ref(),
                    source,
                    output,
                    parent,
                    token,
                ))
                .map_err(err_to_string)
            })?;
            Ok(None)
        },
    )
    .await
}

#[tauri::command]
pub async fn cmd_chd_verify(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    parent: Option<PathBuf>,
    fix: bool,
    task_id: Option<String>,
) -> Result<String, String> {
    let key = task_id.as_deref().unwrap_or("chd-verify");
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input, &["chd"]).await?;
    let resolved_path = resolved.path().to_path_buf();
    let token = begin(&state, key).await;
    let result = std::thread::spawn(move || -> Result<(), String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(err_to_string)?;
        rt.block_on(verify_chd(
            progress.as_ref(),
            resolved_path,
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
    finish(&state, key).await;
    result??;
    Ok("CHD verification passed".to_string())
}

/// Extract a CHD straight to a CSO/ZSO through a temporary ISO, mirroring
/// `cmd_cso_compress` but calling the pipeline's chained conversion. Only
/// DVD-mode CHDs qualify; the pipeline rejects a CD-mode CHD before anything
/// is written.
#[tauri::command]
pub async fn cmd_chd_to_cso(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: CsoCompressArgs,
) -> Result<RunOutcome, String> {
    let CsoCompressArgs {
        input_path,
        output,
        format,
        block_size,
        output_template,
        report,
        verify_after,
        common,
    } = args;
    let format = match format.as_str() {
        "zso" => CsoFormat::Zso,
        _ => CsoFormat::Cso,
    };
    let key = common.task_id.as_deref().unwrap_or("chd-to-cso");
    let dry_run = common.dry_run.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input_path.clone(), &["chd"]).await?;
    let basis = resolved.output_basis().to_path_buf();
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &basis,
        format.extension(),
        None,
        || basis.with_extension(format.extension()),
        dry_run,
    )?;
    // The temporary ISO the pipeline stages is the real space requirement.
    let required = rom_converto_lib::chd::info::read_info(resolved.path())
        .map(|info| info.logical_bytes)
        .unwrap_or_else(|_| input_size(resolved.path()));
    let opts = CsoCompressOptions {
        format,
        block_size,
        force: true,
    };
    let source = resolved.path().to_path_buf();
    let runner = progress.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key,
            operation: "compress",
            input: &input_path,
            desired,
            on_conflict: common.on_conflict.as_deref(),
            verify: rom_converto_lib::util::OutputVerify::Cso,
            media: None,
            missing_keys: None,
            input_bytes: input_size(&input_path),
            required_bytes: required,
            skip_space_check: common.skip_space_check,
            report: report.unwrap_or(false),
            verify_after: verify_after.unwrap_or(false),
            dry_run,
            output_size: input_size,
        },
        move |output, token| async move {
            on_dedicated_runtime(move |rt| {
                rt.block_on(chd_to_cso(runner.as_ref(), source, output, opts, token))
                    .map_err(err_to_string)
            })?;
            Ok(None)
        },
    )
    .await
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompressDiscArgs {
    input: PathBuf,
    output: Option<PathBuf>,
    level: Option<i32>,
    chunk_size: Option<u32>,
    output_template: Option<String>,
    report: Option<bool>,
    verify_after: Option<bool>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_compress_disc(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: CompressDiscArgs,
) -> Result<RunOutcome, String> {
    let CompressDiscArgs {
        input,
        output,
        level,
        chunk_size,
        output_template,
        report,
        verify_after,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("compress-disc");
    let dry_run = common.dry_run.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved =
        resolve_archive_input(input.clone(), &["iso", "gcm", "gcz", "wbfs", "wia"]).await?;
    let basis = resolved.output_basis().to_path_buf();
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &basis,
        "rvz",
        None,
        || derive_rvz_path(&basis),
        dry_run,
    )?;
    let opts = RvzCompressOptions {
        compression_level: level.unwrap_or(RvzCompressOptions::default().compression_level),
        chunk_size: chunk_size.unwrap_or(RvzCompressOptions::default().chunk_size),
        ..RvzCompressOptions::default()
    };
    if !dry_run && let Some(msg) = oversized_rvz_chunk(opts.chunk_size) {
        progress.warn(msg);
    }
    let source = resolved.path().to_path_buf();
    let runner = progress.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key,
            operation: "compress",
            input: &input,
            desired,
            on_conflict: common.on_conflict.as_deref(),
            verify: rom_converto_lib::util::OutputVerify::Rvz,
            media: Some("RVZ".to_string()),
            missing_keys: None,
            input_bytes: input_size(&input),
            required_bytes: input_size(&source),
            skip_space_check: common.skip_space_check,
            report: report.unwrap_or(false),
            verify_after: verify_after.unwrap_or(false),
            dry_run,
            output_size: input_size,
        },
        move |output, token| async move {
            compress_disc(&source, &output, opts, runner.as_ref(), token)
                .await
                .map_err(err_to_string)?;
            Ok(None)
        },
    )
    .await
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecompressDiscArgs {
    input: PathBuf,
    output: Option<PathBuf>,
    output_template: Option<String>,
    report: Option<bool>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_decompress_disc(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: DecompressDiscArgs,
) -> Result<RunOutcome, String> {
    let DecompressDiscArgs {
        input,
        output,
        output_template,
        report,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("decompress-disc");
    let dry_run = common.dry_run.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input.clone(), &["rvz"]).await?;
    let basis = resolved.output_basis().to_path_buf();
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &basis,
        "iso",
        None,
        || derive_disc_path(&basis),
        dry_run,
    )?;
    let source = resolved.path().to_path_buf();
    let runner = progress.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key,
            operation: "decompress",
            input: &input,
            desired,
            on_conflict: common.on_conflict.as_deref(),
            verify: rom_converto_lib::util::OutputVerify::None,
            media: None,
            missing_keys: None,
            input_bytes: input_size(&input),
            required_bytes: input_size(&source),
            skip_space_check: common.skip_space_check,
            report: report.unwrap_or(false),
            verify_after: false,
            dry_run,
            output_size: input_size,
        },
        move |output, token| async move {
            let to_wbfs = ext_of(&output).eq_ignore_ascii_case("wbfs");
            if to_wbfs {
                decompress_disc_to_wbfs(&source, &output, runner.as_ref(), token).await
            } else {
                decompress_disc(&source, &output, runner.as_ref(), token).await
            }
            .map_err(err_to_string)?;
            Ok(None)
        },
    )
    .await
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WupCompressArgs {
    inputs: Vec<PathBuf>,
    output: PathBuf,
    level: Option<i32>,
    keys: Option<Vec<PathBuf>>,
    on_conflict: Option<String>,
    skip_space_check: bool,
    dry_run: Option<bool>,
}

#[tauri::command]
pub async fn cmd_wup_compress(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: WupCompressArgs,
) -> Result<String, String> {
    let WupCompressArgs {
        inputs,
        output,
        level,
        keys,
        on_conflict,
        skip_space_check,
        dry_run,
    } = args;
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
            PlanInput {
                operation: "compress",
                input: &input,
                desired: &output,
                on_conflict: on_conflict.as_deref(),
                media: media.map(str::to_string),
                verify: rom_converto_lib::util::OutputVerify::None,
                missing_keys: None,
            },
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
    let token = begin(&state, "wup-compress").await;
    let result = tokio::spawn(async move {
        compress_titles_async(titles, output, opts, progress.as_ref(), token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state, "wup-compress").await;
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
            PlanInput {
                operation: "decrypt",
                input: &input,
                desired: &output,
                on_conflict: on_conflict.as_deref(),
                media: None,
                verify: rom_converto_lib::util::OutputVerify::None,
                missing_keys: None,
            },
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
    let token = begin(&state, "wup-decrypt").await;
    let result = tokio::spawn(async move {
        decrypt_nus_title_async(input, output, progress.as_ref(), token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state, "wup-decrypt").await;
    result?;
    Ok(format!("Wrote {out_display}"))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ps3DecryptArgs {
    input: PathBuf,
    output: Option<PathBuf>,
    key: Option<PathBuf>,
    #[serde(default)]
    skip_probe: bool,
    output_template: Option<String>,
    report: Option<bool>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_ps3_decrypt(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: Ps3DecryptArgs,
) -> Result<RunOutcome, String> {
    let Ps3DecryptArgs {
        input,
        output,
        key,
        skip_probe,
        output_template,
        report,
        common,
    } = args;
    let task_key = common.task_id.as_deref().unwrap_or("ps3-decrypt");
    let dry_run = common.dry_run.unwrap_or(false);
    let report = report.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, task_key));
    let resolved = resolve_archive_input(input.clone(), &["iso"]).await?;
    let basis = resolved.output_basis().to_path_buf();
    let ext = ext_of(&derive_ps3_decrypted_path(&basis));
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &basis,
        &ext,
        key.as_deref(),
        || derive_ps3_decrypted_path(&basis),
        dry_run,
    )?;
    let source = resolved.path().to_path_buf();
    let ps3_key = resolve_ps3_key(&source, &basis, key.as_deref()).map_err(err_to_string)?;
    let record_input = input.clone();
    let runner = progress.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key: task_key,
            operation: "decrypt",
            input: &input,
            desired,
            on_conflict: common.on_conflict.as_deref(),
            verify: rom_converto_lib::util::OutputVerify::None,
            media: None,
            missing_keys: None,
            input_bytes: input_size(&input),
            required_bytes: input_size(&source),
            skip_space_check: common.skip_space_check,
            report,
            verify_after: false,
            dry_run,
            output_size: input_size,
        },
        move |output, token| async move {
            match decrypt_ps3_iso(
                runner.as_ref(),
                source,
                output,
                ps3_key,
                true,
                skip_probe,
                token,
            )
            .await
            {
                Ok(()) => Ok(None),
                Err(e @ Ps3Error::AlreadyDecrypted) => Ok(Some(RunOutcome::skipped_already_done(
                    report,
                    &record_input,
                    "decrypt",
                    "already decrypted",
                    &e,
                ))),
                Err(e) => Err(err_to_string(e)),
            }
        },
    )
    .await
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NdsCryptArgs {
    input: PathBuf,
    output: Option<PathBuf>,
    output_template: Option<String>,
    report: Option<bool>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_nds_encrypt(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: NdsCryptArgs,
) -> Result<RunOutcome, String> {
    let NdsCryptArgs {
        input,
        output,
        output_template,
        report,
        common,
    } = args;
    let task_key = common.task_id.as_deref().unwrap_or("nds-encrypt");
    let dry_run = common.dry_run.unwrap_or(false);
    let report = report.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, task_key));
    let resolved = resolve_archive_input(input.clone(), &["nds"]).await?;
    let basis = resolved.output_basis().to_path_buf();
    let ext = ext_of(&derive_nds_encrypted_path(&basis));
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &basis,
        &ext,
        None,
        || derive_nds_encrypted_path(&basis),
        dry_run,
    )?;
    let source = resolved.path().to_path_buf();
    let record_input = input.clone();
    let runner = progress.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key: task_key,
            operation: "encrypt",
            input: &input,
            desired,
            on_conflict: common.on_conflict.as_deref(),
            verify: rom_converto_lib::util::OutputVerify::None,
            media: None,
            missing_keys: None,
            input_bytes: input_size(&input),
            required_bytes: input_size(&source),
            skip_space_check: common.skip_space_check,
            report,
            verify_after: false,
            dry_run,
            output_size: input_size,
        },
        move |output, token| async move {
            let Err(err) = encrypt_nds_rom(runner.as_ref(), source, output, true, token).await
            else {
                return Ok(None);
            };
            // A ROM that is already in the target state, or has no secure area
            // to work on, is a skip rather than a failure.
            let reason = match &err {
                NdsError::AlreadyEncrypted => "already encrypted",
                NdsError::NoSecureArea => "no secure area",
                NdsError::TooSmall => "too small for a secure area",
                _ => return Err(err_to_string(err)),
            };
            Ok(Some(RunOutcome::skipped_already_done(
                report,
                &record_input,
                "encrypt",
                reason,
                &err,
            )))
        },
    )
    .await
}

#[tauri::command]
pub async fn cmd_nds_decrypt(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: NdsCryptArgs,
) -> Result<RunOutcome, String> {
    let NdsCryptArgs {
        input,
        output,
        output_template,
        report,
        common,
    } = args;
    let task_key = common.task_id.as_deref().unwrap_or("nds-decrypt");
    let dry_run = common.dry_run.unwrap_or(false);
    let report = report.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, task_key));
    let resolved = resolve_archive_input(input.clone(), &["nds"]).await?;
    let basis = resolved.output_basis().to_path_buf();
    let ext = ext_of(&derive_nds_decrypted_path(&basis));
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &basis,
        &ext,
        None,
        || derive_nds_decrypted_path(&basis),
        dry_run,
    )?;
    let source = resolved.path().to_path_buf();
    let record_input = input.clone();
    let runner = progress.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key: task_key,
            operation: "decrypt",
            input: &input,
            desired,
            on_conflict: common.on_conflict.as_deref(),
            verify: rom_converto_lib::util::OutputVerify::None,
            media: None,
            missing_keys: None,
            input_bytes: input_size(&input),
            required_bytes: input_size(&source),
            skip_space_check: common.skip_space_check,
            report,
            verify_after: false,
            dry_run,
            output_size: input_size,
        },
        move |output, token| async move {
            let Err(err) = decrypt_nds_rom(runner.as_ref(), source, output, true, token).await
            else {
                return Ok(None);
            };
            // A ROM that is already in the target state, or has no secure area
            // to work on, is a skip rather than a failure.
            let reason = match &err {
                NdsError::AlreadyDecrypted => "already decrypted",
                NdsError::NoSecureArea => "no secure area",
                NdsError::TooSmall => "too small for a secure area",
                _ => return Err(err_to_string(err)),
            };
            Ok(Some(RunOutcome::skipped_already_done(
                report,
                &record_input,
                "decrypt",
                reason,
                &err,
            )))
        },
    )
    .await
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NxCompressArgs {
    input: PathBuf,
    output: Option<PathBuf>,
    keys: Option<PathBuf>,
    level: Option<i32>,
    mode: Option<String>,
    block_size_exp: Option<u8>,
    output_template: Option<String>,
    report: Option<bool>,
    verify_after: Option<bool>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_nx_compress(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: NxCompressArgs,
) -> Result<RunOutcome, String> {
    let NxCompressArgs {
        input,
        output,
        keys,
        level,
        mode,
        block_size_exp,
        output_template,
        report,
        verify_after,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("nx-compress");
    let dry_run = common.dry_run.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input.clone(), &["nsp", "xci", "nca"]).await?;
    let basis = resolved.output_basis().to_path_buf();
    let kind = detect_container(resolved.path()).map_err(err_to_string)?;
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
    let ext = ext_of(&nx_derive_compressed_path(&basis));
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &basis,
        &ext,
        keys.as_deref(),
        || nx_derive_compressed_path(&basis),
        dry_run,
    )?;
    let (keyset, missing_keys) = keyset_for_run(keys.as_deref(), dry_run)?;
    let source = resolved.path().to_path_buf();
    let runner = progress.clone();
    let run_keyset = keyset.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key,
            operation: "compress",
            input: &input,
            desired,
            on_conflict: common.on_conflict.as_deref(),
            verify: rom_converto_lib::util::OutputVerify::Nx(Box::new(keyset)),
            media: Some(format!("{kind:?}")),
            missing_keys,
            input_bytes: input_size(&input),
            required_bytes: input_size(&source),
            skip_space_check: common.skip_space_check,
            report: report.unwrap_or(false),
            verify_after: verify_after.unwrap_or(false),
            dry_run,
            output_size: input_size,
        },
        move |output, token| async move {
            compress_container_async(source, output, opts, run_keyset, runner.as_ref(), token)
                .await
                .map_err(err_to_string)?;
            Ok(None)
        },
    )
    .await
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NxDecompressArgs {
    input: PathBuf,
    output: Option<PathBuf>,
    keys: Option<PathBuf>,
    output_template: Option<String>,
    report: Option<bool>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_nx_decompress(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: NxDecompressArgs,
) -> Result<RunOutcome, String> {
    let NxDecompressArgs {
        input,
        output,
        keys,
        output_template,
        report,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("nx-decompress");
    let dry_run = common.dry_run.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input.clone(), &["nsz", "xcz", "ncz"]).await?;
    let basis = resolved.output_basis().to_path_buf();
    let ext = ext_of(&nx_derive_decompressed_path(&basis));
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &basis,
        &ext,
        keys.as_deref(),
        || nx_derive_decompressed_path(&basis),
        dry_run,
    )?;
    let source = resolved.path().to_path_buf();
    let runner = progress.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key,
            operation: "decompress",
            input: &input,
            desired,
            on_conflict: common.on_conflict.as_deref(),
            verify: rom_converto_lib::util::OutputVerify::None,
            media: None,
            missing_keys: None,
            input_bytes: input_size(&input),
            required_bytes: input_size(&source),
            skip_space_check: common.skip_space_check,
            report: report.unwrap_or(false),
            verify_after: false,
            dry_run,
            output_size: input_size,
        },
        move |output, token| async move {
            let keyset = load_keyset(keys.as_deref()).map_err(err_to_string)?;
            decompress_container_async(source, output, keyset, runner.as_ref(), token)
                .await
                .map_err(err_to_string)?;
            Ok(None)
        },
    )
    .await
}

#[tauri::command]
pub async fn cmd_nx_verify(
    app: AppHandle,
    input: PathBuf,
    keys: Option<PathBuf>,
    task_id: Option<String>,
) -> Result<String, String> {
    let key = task_id.as_deref().unwrap_or("nx-verify");
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved =
        resolve_archive_input(input, &["nsp", "xci", "nca", "nsz", "xcz", "ncz"]).await?;
    let resolved_path = resolved.path().to_path_buf();
    let keys = load_keyset(keys.as_deref()).map_err(err_to_string)?;
    let result = tokio::spawn(async move {
        verify_container_async(resolved_path, keys, progress.as_ref(), CancelToken::new()).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)?;
    serde_json::to_string(&result).map_err(err_to_string)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NxMergeArgs {
    inputs: Vec<PathBuf>,
    output: PathBuf,
    format: Option<String>,
    keys: Option<PathBuf>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_nx_merge(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: NxMergeArgs,
) -> Result<RunOutcome, String> {
    let NxMergeArgs {
        inputs,
        output,
        format,
        keys,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("nx-merge");
    let dry_run = common.dry_run.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, key));
    let format = match format.as_deref() {
        Some("xci") => NxMergeFormat::Xci,
        _ => NxMergeFormat::Nsp,
    };
    let media = match format {
        NxMergeFormat::Nsp => "NSP",
        NxMergeFormat::Xci => "XCI",
    };
    let (keyset, missing_keys) = keyset_for_run(keys.as_deref(), dry_run)?;
    // The merge has many inputs but one record; the first one names it.
    let record_input = inputs.first().cloned().unwrap_or_else(|| output.clone());
    let in_bytes: u64 = inputs.iter().map(|p| input_size(p)).sum();
    let runner = progress.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key,
            operation: "merge",
            input: &record_input,
            desired: output,
            on_conflict: common.on_conflict.as_deref(),
            verify: rom_converto_lib::util::OutputVerify::None,
            media: Some(media.to_string()),
            missing_keys,
            input_bytes: in_bytes,
            required_bytes: in_bytes,
            skip_space_check: common.skip_space_check,
            report: false,
            verify_after: false,
            dry_run,
            output_size: input_size,
        },
        move |output, token| async move {
            merge_containers_async(inputs, output, format, keyset, runner.as_ref(), token)
                .await
                .map_err(err_to_string)?;
            Ok(None)
        },
    )
    .await
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NxSplitArgs {
    input: PathBuf,
    output_dir: PathBuf,
    keys: Option<PathBuf>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_nx_split(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: NxSplitArgs,
) -> Result<String, String> {
    let NxSplitArgs {
        input,
        output_dir,
        keys,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("nx-split");
    let progress = Arc::new(TauriProgress::new(app, key));
    if common.dry_run.unwrap_or(false) {
        // The output is a directory, so the plan uses the same
        // directory-aware decision the real run does rather than
        // `plan_line`'s per-file conflict resolution.
        let occupied = output_dir_occupied(&output_dir)?;
        let decision = match resolve_output_dir(&output_dir, common.on_conflict.as_deref())? {
            None => rom_converto_lib::util::PlanDecision::Skip,
            Some(_) if occupied => rom_converto_lib::util::PlanDecision::Overwrite,
            Some(_) => rom_converto_lib::util::PlanDecision::New,
        };
        let line = rom_converto_lib::util::PlanLine {
            operation: "split".to_string(),
            input: input.clone(),
            output: output_dir.clone(),
            decision,
            media: None,
            missing_keys: load_keyset(keys.as_deref()).err().map(|e| e.to_string()),
        };
        return Ok(line.display_text());
    }
    let output_dir = match resolve_output_dir(&output_dir, common.on_conflict.as_deref())? {
        Some(p) => p,
        None => return Ok(format!("Skipped existing {}", output_dir.display())),
    };
    let out_display = output_dir.display().to_string();
    let resolved = resolve_archive_input(input, &["nsp", "xci"]).await?;
    let resolved_path = resolved.path().to_path_buf();
    let keys = load_keyset(keys.as_deref()).map_err(err_to_string)?;
    preflight_space(
        &output_dir,
        input_size(&resolved_path),
        common.skip_space_check,
    )?;
    let token = begin(&state, key).await;
    let result = tokio::spawn(async move {
        split_container_async(resolved_path, output_dir, keys, progress.as_ref(), token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state, key).await;
    let files = result?;
    let names: Vec<String> = files
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    Ok(format!(
        "Wrote {} files to {out_display}: {}",
        files.len(),
        names.join(", ")
    ))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvertCtrArgs {
    input: PathBuf,
    output: Option<PathBuf>,
    output_template: Option<String>,
    verify_after: Option<bool>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_convert_ctr(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: ConvertCtrArgs,
) -> Result<RunOutcome, String> {
    let ConvertCtrArgs {
        input,
        output,
        output_template,
        verify_after,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("ctr-convert");
    let dry_run = common.dry_run.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input.clone(), CTR_CONVERT_EXTS).await?;
    let basis = resolved.output_basis().to_path_buf();
    let ext = ext_of(&derive_converted_path(&basis));
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &basis,
        &ext,
        None,
        || derive_converted_path(&basis),
        dry_run,
    )?;
    let source = resolved.path().to_path_buf();
    let runner = progress.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key,
            operation: "convert",
            input: &input,
            desired,
            on_conflict: common.on_conflict.as_deref(),
            verify: rom_converto_lib::util::OutputVerify::None,
            media: None,
            missing_keys: None,
            input_bytes: input_size(&input),
            required_bytes: input_size(&source),
            skip_space_check: common.skip_space_check,
            report: false,
            verify_after: verify_after.unwrap_or(false),
            dry_run,
            output_size: input_size,
        },
        move |output, token| async move {
            convert_rom(&source, &output, runner.as_ref(), token)
                .await
                .map_err(err_to_string)?;
            Ok(None)
        },
    )
    .await
}

#[tauri::command]
pub async fn cmd_verify_ctr(
    app: AppHandle,
    input: PathBuf,
    verify_content: bool,
    task_id: Option<String>,
) -> Result<String, String> {
    let key = task_id.as_deref().unwrap_or("ctr-verify");
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input, CTR_DECRYPT_EXTS).await?;
    let resolved_path = resolved.path().to_path_buf();
    let opts = CtrVerifyOptions {
        verify_content_hashes: verify_content,
    };
    let result = tokio::spawn(async move {
        verify_ctr(
            &resolved_path,
            &opts,
            progress.as_ref(),
            &CancelToken::new(),
        )
        .await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)?;

    serde_json::to_string(&result).map_err(err_to_string)
}

#[tauri::command]
pub async fn cmd_verify_dol(
    app: AppHandle,
    input: PathBuf,
    full: bool,
    task_id: Option<String>,
) -> Result<String, String> {
    let key = task_id.as_deref().unwrap_or("dol-verify");
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input, &["iso", "gcm", "gcz", "rvz"]).await?;
    let resolved_path = resolved.path().to_path_buf();
    let result = tokio::task::spawn_blocking(move || {
        let opts = DolVerifyOptions { full };
        verify_dol(
            &resolved_path,
            &opts,
            progress.as_ref(),
            &CancelToken::new(),
        )
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)?;

    serde_json::to_string(&result).map_err(err_to_string)
}

#[tauri::command]
pub async fn cmd_verify_rvl(
    app: AppHandle,
    input: PathBuf,
    full: bool,
    task_id: Option<String>,
) -> Result<String, String> {
    let key = task_id.as_deref().unwrap_or("rvl-verify");
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input, &["iso", "wbfs", "gcz", "wia", "rvz"]).await?;
    let resolved_path = resolved.path().to_path_buf();
    let result = tokio::task::spawn_blocking(move || {
        let opts = RvlVerifyOptions { full };
        verify_rvl(
            &resolved_path,
            &opts,
            progress.as_ref(),
            &CancelToken::new(),
        )
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
    task_id: Option<String>,
) -> Result<String, String> {
    let key = task_id.as_deref().unwrap_or("wup-verify");
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input, &["wud", "wux"]).await?;
    let resolved_path = resolved.path().to_path_buf();
    let result = tokio::spawn(async move {
        verify_wup_async(resolved_path, keys, progress.as_ref(), CancelToken::new()).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)?;

    serde_json::to_string(&result).map_err(err_to_string)
}

/// Which `prod.keys` file nx operations would use right now, for the GUI
/// status row. Home-relative results are shortened to `~` for display.
#[tauri::command]
pub fn cmd_nx_keys_resolve(keys: Option<PathBuf>) -> Option<String> {
    find_keys_file(keys.as_deref()).map(|p| rom_converto_lib::util::contract_tilde(&p))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XboxConvertArgs {
    input: PathBuf,
    output: Option<PathBuf>,
    media_patch: Option<bool>,
    output_template: Option<String>,
    report: Option<bool>,
    verify_after: Option<bool>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_xbox_convert(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: XboxConvertArgs,
) -> Result<RunOutcome, String> {
    let XboxConvertArgs {
        input,
        output,
        media_patch,
        output_template,
        report,
        verify_after,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("xbox-convert");
    let dry_run = common.dry_run.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input.clone(), &["iso"]).await?;
    let basis = resolved.output_basis().to_path_buf();
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &basis,
        "xiso",
        None,
        || basis.with_extension("xiso"),
        dry_run,
    )?;
    // A directory input is rebuilt from its files, so the payload is their total.
    let in_bytes = if resolved.path().is_dir() {
        rom_converto_lib::microsoft::xbox::input_total_bytes(resolved.path())
            .unwrap_or_else(|_| input_size(resolved.path()))
    } else {
        input_size(resolved.path())
    };
    let opts = XisoCreateOptions {
        media_patch: media_patch.unwrap_or(XisoCreateOptions::default().media_patch),
    };
    let source = resolved.path().to_path_buf();
    let runner = progress.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key,
            operation: "convert",
            input: &input,
            desired,
            on_conflict: common.on_conflict.as_deref(),
            verify: rom_converto_lib::util::OutputVerify::None,
            media: Some("XISO".to_string()),
            missing_keys: None,
            input_bytes: in_bytes,
            required_bytes: in_bytes,
            skip_space_check: common.skip_space_check,
            report: report.unwrap_or(false),
            verify_after: verify_after.unwrap_or(false),
            dry_run,
            output_size: input_size,
        },
        move |output, token| async move {
            convert_to_xiso(&source, &output, opts, runner.as_ref(), token)
                .await
                .map_err(err_to_string)?;
            Ok(None)
        },
    )
    .await
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XboxExtractArgs {
    input: PathBuf,
    output_dir: PathBuf,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_xbox_extract(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: XboxExtractArgs,
) -> Result<String, String> {
    let XboxExtractArgs {
        input,
        output_dir,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("xbox-extract");
    let progress = Arc::new(TauriProgress::new(app, key));
    if common.dry_run.unwrap_or(false) {
        let line = plan_line(
            progress.as_ref(),
            PlanInput {
                operation: "extract",
                input: &input,
                desired: &output_dir,
                on_conflict: common.on_conflict.as_deref(),
                media: None,
                verify: rom_converto_lib::util::OutputVerify::None,
                missing_keys: None,
            },
        )
        .await?;
        return Ok(line.display_text());
    }
    let output_dir = match resolve_output(
        progress.as_ref(),
        &output_dir,
        common.on_conflict.as_deref(),
        rom_converto_lib::util::OutputVerify::None,
    )
    .await?
    {
        Some(p) => p,
        None => return Ok(format!("Skipped existing {}", output_dir.display())),
    };
    let out_display = output_dir.display().to_string();
    let resolved = resolve_archive_input(input, &["xiso", "iso"]).await?;
    let resolved_path = resolved.path().to_path_buf();
    let required_space = {
        let resolved_path = resolved_path.clone();
        tokio::task::spawn_blocking(move || {
            xbox_read_info(&resolved_path)
                .map(|info| info.total_file_bytes)
                .unwrap_or_else(|_| input_size(&resolved_path))
        })
        .await
        .map_err(err_to_string)?
    };
    preflight_space(&output_dir, required_space, common.skip_space_check)?;
    let token = begin(&state, key).await;
    let result = tokio::spawn(async move {
        extract_xiso(&resolved_path, &output_dir, progress.as_ref(), token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state, key).await;
    result?;
    Ok(format!("Wrote {out_display}"))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XenonCompressArgs {
    input: PathBuf,
    output: Option<PathBuf>,
    output_template: Option<String>,
    report: Option<bool>,
    verify_after: Option<bool>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_xenon_compress(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: XenonCompressArgs,
) -> Result<RunOutcome, String> {
    let XenonCompressArgs {
        input,
        output,
        output_template,
        report,
        verify_after,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("xenon-compress");
    let dry_run = common.dry_run.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input.clone(), &["iso"]).await?;
    let basis = resolved.output_basis().to_path_buf();
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &basis,
        "zar",
        None,
        || basis.with_extension("zar"),
        dry_run,
    )?;
    // A directory input is packed from its files, so the payload is their total.
    let in_bytes = if resolved.path().is_dir() {
        rom_converto_lib::microsoft::xenon::total_input_bytes(resolved.path())
            .unwrap_or_else(|_| input_size(resolved.path()))
    } else {
        input_size(resolved.path())
    };
    let source = resolved.path().to_path_buf();
    let runner = progress.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key,
            operation: "compress",
            input: &input,
            desired,
            on_conflict: common.on_conflict.as_deref(),
            verify: rom_converto_lib::util::OutputVerify::None,
            media: Some("ZAR".to_string()),
            missing_keys: None,
            input_bytes: in_bytes,
            required_bytes: in_bytes,
            skip_space_check: common.skip_space_check,
            report: report.unwrap_or(false),
            verify_after: verify_after.unwrap_or(false),
            dry_run,
            output_size: input_size,
        },
        move |output, token| async move {
            pack_zar(&source, &output, runner.as_ref(), token)
                .await
                .map_err(err_to_string)?;
            Ok(None)
        },
    )
    .await
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XenonExtractArgs {
    input: PathBuf,
    output_dir: PathBuf,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_xenon_extract(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: XenonExtractArgs,
) -> Result<String, String> {
    let XenonExtractArgs {
        input,
        output_dir,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("xenon-extract");
    let progress = Arc::new(TauriProgress::new(app, key));
    if common.dry_run.unwrap_or(false) {
        let line = plan_line(
            progress.as_ref(),
            PlanInput {
                operation: "extract",
                input: &input,
                desired: &output_dir,
                on_conflict: common.on_conflict.as_deref(),
                media: None,
                verify: rom_converto_lib::util::OutputVerify::None,
                missing_keys: None,
            },
        )
        .await?;
        return Ok(line.display_text());
    }
    let output_dir = match resolve_output(
        progress.as_ref(),
        &output_dir,
        common.on_conflict.as_deref(),
        rom_converto_lib::util::OutputVerify::None,
    )
    .await?
    {
        Some(p) => p,
        None => return Ok(format!("Skipped existing {}", output_dir.display())),
    };
    let out_display = output_dir.display().to_string();
    let resolved = resolve_archive_input(input, &["zar"]).await?;
    let resolved_path = resolved.path().to_path_buf();
    let required_space = {
        let resolved_path = resolved_path.clone();
        tokio::task::spawn_blocking(move || {
            xenon_read_info(&resolved_path)
                .map(|info| info.logical_size)
                .unwrap_or_else(|_| input_size(&resolved_path))
        })
        .await
        .map_err(err_to_string)?
    };
    preflight_space(&output_dir, required_space, common.skip_space_check)?;
    let token = begin(&state, key).await;
    let result = tokio::spawn(async move {
        extract_zar(&resolved_path, &output_dir, progress.as_ref(), token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state, key).await;
    result?;
    Ok(format!("Wrote {out_display}"))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PspToIsoArgs {
    input: PathBuf,
    output: Option<PathBuf>,
    output_template: Option<String>,
    report: Option<bool>,
    verify_after: Option<bool>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_psp_to_iso(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: PspToIsoArgs,
) -> Result<RunOutcome, String> {
    let PspToIsoArgs {
        input,
        output,
        output_template,
        report,
        verify_after,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("psp-to-iso");
    let dry_run = common.dry_run.unwrap_or(false);
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input.clone(), &["pbp", "pkg"]).await?;
    let basis = resolved.output_basis().to_path_buf();
    let desired = pick_output(
        output,
        output_template.as_deref(),
        &basis,
        "iso",
        None,
        || basis.with_extension("iso"),
        dry_run,
    )?;
    let source = resolved.path().to_path_buf();
    let runner = progress.clone();
    run_single_file_op(
        SingleFileOp {
            state: &state,
            progress,
            key,
            operation: "convert",
            input: &input,
            desired,
            on_conflict: common.on_conflict.as_deref(),
            verify: rom_converto_lib::util::OutputVerify::None,
            media: None,
            missing_keys: None,
            input_bytes: input_size(&input),
            required_bytes: input_size(&source),
            skip_space_check: common.skip_space_check,
            report: report.unwrap_or(false),
            verify_after: verify_after.unwrap_or(false),
            dry_run,
            output_size: input_size,
        },
        move |output, _token| async move {
            tokio::task::spawn_blocking(move || psp_to_iso(runner.as_ref(), &source, &output))
                .await
                .map_err(err_to_string)?
                .map_err(err_to_string)?;
            Ok(None)
        },
    )
    .await
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PspExtractArgs {
    input: PathBuf,
    output_dir: PathBuf,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_psp_extract(app: AppHandle, args: PspExtractArgs) -> Result<String, String> {
    let PspExtractArgs {
        input,
        output_dir,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("psp-extract");
    let progress = Arc::new(TauriProgress::new(app, key));
    if common.dry_run.unwrap_or(false) {
        let line = plan_line(
            progress.as_ref(),
            PlanInput {
                operation: "extract",
                input: &input,
                desired: &output_dir,
                on_conflict: common.on_conflict.as_deref(),
                media: None,
                verify: rom_converto_lib::util::OutputVerify::None,
                missing_keys: None,
            },
        )
        .await?;
        return Ok(line.display_text());
    }
    let output_dir = match resolve_output(
        progress.as_ref(),
        &output_dir,
        common.on_conflict.as_deref(),
        rom_converto_lib::util::OutputVerify::None,
    )
    .await?
    {
        Some(p) => p,
        None => return Ok(format!("Skipped existing {}", output_dir.display())),
    };
    let out_display = output_dir.display().to_string();
    let resolved = resolve_archive_input(input, &["pbp"]).await?;
    let resolved_path = resolved.path().to_path_buf();
    preflight_space(
        &output_dir,
        input_size(&resolved_path),
        common.skip_space_check,
    )?;
    tokio::task::spawn_blocking(move || {
        extract_segments(progress.as_ref(), &resolved_path, &output_dir)
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)?;
    Ok(format!("Wrote {out_display}"))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VitaExtractArgs {
    input: PathBuf,
    output_dir: PathBuf,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_vita_extract(app: AppHandle, args: VitaExtractArgs) -> Result<String, String> {
    let VitaExtractArgs {
        input,
        output_dir,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("vita-extract");
    let progress = Arc::new(TauriProgress::new(app, key));
    if common.dry_run.unwrap_or(false) {
        let line = plan_line(
            progress.as_ref(),
            PlanInput {
                operation: "extract",
                input: &input,
                desired: &output_dir,
                on_conflict: common.on_conflict.as_deref(),
                media: None,
                verify: rom_converto_lib::util::OutputVerify::None,
                missing_keys: None,
            },
        )
        .await?;
        return Ok(line.display_text());
    }
    let output_dir = match resolve_output(
        progress.as_ref(),
        &output_dir,
        common.on_conflict.as_deref(),
        rom_converto_lib::util::OutputVerify::None,
    )
    .await?
    {
        Some(p) => p,
        None => return Ok(format!("Skipped existing {}", output_dir.display())),
    };
    let out_display = output_dir.display().to_string();
    let resolved = resolve_archive_input(input, &["pkg"]).await?;
    let resolved_path = resolved.path().to_path_buf();
    preflight_space(
        &output_dir,
        input_size(&resolved_path),
        common.skip_space_check,
    )?;
    tokio::task::spawn_blocking(move || {
        vita_pkg_extract(&resolved_path, &output_dir, progress.as_ref())
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)?;
    Ok(format!("Wrote {out_display}"))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XenonConvertArgs {
    input: PathBuf,
    output_dir: PathBuf,
    title: Option<String>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[tauri::command]
pub async fn cmd_xenon_convert(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    args: XenonConvertArgs,
) -> Result<String, String> {
    let XenonConvertArgs {
        input,
        output_dir,
        title,
        common,
    } = args;
    let key = common.task_id.as_deref().unwrap_or("xenon-convert");
    let progress = Arc::new(TauriProgress::new(app, key));
    if common.dry_run.unwrap_or(false) {
        let line = plan_line(
            progress.as_ref(),
            PlanInput {
                operation: "convert",
                input: &input,
                desired: &output_dir,
                on_conflict: common.on_conflict.as_deref(),
                media: None,
                verify: rom_converto_lib::util::OutputVerify::None,
                missing_keys: None,
            },
        )
        .await?;
        return Ok(line.display_text());
    }
    let output_dir = match resolve_output(
        progress.as_ref(),
        &output_dir,
        common.on_conflict.as_deref(),
        rom_converto_lib::util::OutputVerify::None,
    )
    .await?
    {
        Some(p) => p,
        None => return Ok(format!("Skipped existing {}", output_dir.display())),
    };
    let out_display = output_dir.display().to_string();
    let resolved = resolve_archive_input(input, &["iso"]).await?;
    let resolved_path = resolved.path().to_path_buf();
    // On top of the payload: one hash block per 0xCC data blocks, plus
    // the container header.
    let len = input_size(&resolved_path);
    preflight_space(
        &output_dir,
        len + len / 0xCC + 0xB000,
        common.skip_space_check,
    )?;
    let token = begin(&state, key).await;
    let result = tokio::spawn(async move {
        convert_to_god(
            &resolved_path,
            &output_dir,
            title.as_deref(),
            progress.as_ref(),
            token,
        )
        .await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state, key).await;
    result?;
    Ok(format!("Wrote {out_display}"))
}

#[tauri::command]
pub async fn cmd_xenon_verify(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    task_id: Option<String>,
) -> Result<String, String> {
    let key = task_id.as_deref().unwrap_or("xenon-verify");
    let progress = Arc::new(TauriProgress::new(app, key));
    let resolved = resolve_archive_input(input, &["zar"]).await?;
    let resolved_path = resolved.path().to_path_buf();
    let token = begin(&state, key).await;
    let result =
        tokio::spawn(async move { verify_zar(&resolved_path, progress.as_ref(), token).await })
            .await
            .map_err(err_to_string)?
            .map_err(err_to_string);
    finish(&state, key).await;
    let verify = result?;
    serde_json::to_string(&serde_json::json!({
        "blocks": verify.blocks,
        "logical_bytes": verify.logical_bytes,
        "hash_ok": verify.hash_ok,
    }))
    .map_err(err_to_string)
}

#[tauri::command]
pub async fn cmd_read_info(
    cache: State<'_, Arc<InfoCache>>,
    input: PathBuf,
    keys: Option<PathBuf>,
) -> Result<String, String> {
    let cache_inner = cache.inner().clone();
    let result = tokio::task::spawn_blocking(move || -> Result<Arc<InfoResult>, anyhow::Error> {
        if let Some(key) = InfoCache::key_for(&input, keys.as_deref())
            && let Some(hit) = cache_inner.get(&key)
        {
            return Ok(hit);
        }
        let resolved = rom_converto_lib::util::resolve_input(&input, ALL_IMAGE_EXTS)?;
        let opts = InfoOptions {
            keys_path: keys.clone(),
            parent_path: None,
        };
        let info = read_info(resolved.path(), &opts)?;
        let arc = Arc::new(info);
        if let Some(key) = InfoCache::key_for(&input, keys.as_deref()) {
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
    cache: State<'_, Arc<HashCache>>,
    input: PathBuf,
    algos: Vec<String>,
    recursive: bool,
    max_depth: Option<usize>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "hash"));
    let token = begin(&state, "hash").await;
    let cache = cache.inner().clone();
    let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let parsed = parse_algos(&algos.join(","))?;
        let hash_one = |file: &Path| -> Result<_, String> {
            if let Some(hit) = cache.lookup_raw(file, &parsed) {
                return Ok(hit);
            }
            let digests =
                hash_file(file, &parsed, progress.as_ref(), &token).map_err(err_to_string)?;
            cache.store_raw(file, &digests);
            Ok(digests)
        };
        let mut lines = Vec::new();
        // A staged directory with the recursive toggle off hashes its top-level
        // files instead of failing to open the directory as a file.
        if recursive || input.is_dir() {
            let depth = if recursive { max_depth } else { Some(1) };
            let files =
                collect_all_files(&input, depth, &CancelToken::new()).map_err(err_to_string)?;
            if files.is_empty() {
                return Ok(format!("no files found in {}", input.display()));
            }
            for file in files {
                if token.is_cancelled() {
                    return Err(Cancelled.to_string());
                }
                let digests = hash_one(&file)?;
                lines.push(render_hash_row(&file, &digests, &parsed));
            }
        } else {
            let digests = hash_one(&input)?;
            lines.push(render_hash_row(&input, &digests, &parsed));
        }
        cache.save();
        Ok(lines.join("\n"))
    })
    .await
    .map_err(err_to_string);
    finish(&state, "hash").await;
    result?
}

#[tauri::command]
pub async fn cmd_playlist(
    app: AppHandle,
    scan_dir: PathBuf,
    output_dir: Option<PathBuf>,
    mode: String,
    extensions: String,
    max_depth: Option<usize>,
    on_conflict: Option<String>,
) -> Result<String, String> {
    let progress = TauriProgress::new(app, "playlist");
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
        let plans = plan_playlists(
            &PlaylistOptions {
                scan_dir: &scan_dir,
                output_dir: output_dir.as_deref(),
                extensions: &ext_refs,
                mode: pmode,
                max_depth,
            },
            &CancelToken::new(),
        )
        .map_err(err_to_string)?;
        if let Some(dir) = output_dir.as_deref() {
            std::fs::create_dir_all(dir).map_err(err_to_string)?;
        }
        let policy = conflict_policy(on_conflict.as_deref());
        let mut written = 0usize;
        let mut skipped = 0usize;
        for plan in &plans {
            let entry_exts = plan
                .contents
                .lines()
                .filter_map(|line| Path::new(line).extension())
                .filter_map(|ext| ext.to_str());
            if let Some(mixed) = mixed_playlist_extensions(entry_exts) {
                progress.warn(&format!(
                    "Mixed track formats ({mixed}) in set {}; emulators expect every disc in a \
                     playlist to use the same format",
                    plan.base_title
                ));
            }
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

// Dat verify/scan/rename: digesting and Playmatch calls are both async-native
// (digest_inner_async already runs the blocking decode in spawn_blocking;
// PlaymatchClient is plain reqwest), so these commands run on the Tauri async
// runtime directly, unlike the CHD extract/verify commands above.

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
    #[serde(rename = "datFile")]
    dat_file: Option<String>,
    #[serde(rename = "datFileId")]
    dat_file_id: Option<String>,
    #[serde(rename = "datVersion")]
    dat_version: Option<String>,
    #[serde(rename = "externalIds")]
    external_ids: Vec<ExternalIdJson>,
    tracks: Option<Vec<DatTrackCheckJson>>,
    error: Option<String>,
}

impl DatVerifyResult {
    /// A verify outcome with no match data: only the verdict and the error
    /// that produced it.
    fn errored(input: &Path, verdict: &'static str, error: impl std::fmt::Display) -> Self {
        Self {
            kind: "verify",
            path: input.display().to_string(),
            verdict,
            match_algo: None,
            game_name: None,
            platform: None,
            signature_group: None,
            dat_file: None,
            dat_file_id: None,
            dat_version: None,
            external_ids: Vec::new(),
            tracks: None,
            error: Some(error.to_string()),
        }
    }
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
    cache: State<'_, Arc<HashCache>>,
    input: PathBuf,
    quick: Option<bool>,
    task_id: Option<String>,
) -> Result<String, String> {
    let key = task_id.as_deref().unwrap_or("dat-verify");
    let progress = Arc::new(TauriProgress::new(app, key));
    let token = begin(&state, key).await;
    let cache = cache.inner().clone();
    let result = run_dat_verify(
        progress.clone(),
        cache.clone(),
        input.clone(),
        quick.unwrap_or(false),
        token,
    )
    .await;
    finish(&state, key).await;
    cache.save();
    // Cancellation propagates as an Err carrying "operation cancelled",
    // matching the other verify commands and scan/rename. Any other failure
    // still renders as a single Failed card so genuine digest/API errors do
    // not abort the whole invoke.
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(rom_converto_lib::dat::DatError::Cancelled(_)) => {
            return Err(Cancelled.to_string());
        }
        Err(e @ rom_converto_lib::dat::DatError::UnsupportedInnerHash { .. }) => {
            progress.warn(NX_DAT_UNSUPPORTED_HINT);
            DatVerifyResult::errored(&input, DatVerdict::Unsupported.as_str(), e)
        }
        Err(e) => DatVerifyResult::errored(&input, DatVerdict::Failed.as_str(), e),
    };
    serde_json::to_string(&outcome).map_err(err_to_string)
}

/// Checksum escalation bounds from the user config's [dat] section, with
/// the CLI defaults (crc32 floor, sha256 ceiling) when unset or invalid.
fn dat_checksum_bounds() -> ChecksumBounds {
    let dat = crate::config_cmds::cmd_load_config()
        .ok()
        .and_then(|c| c.dat)
        .unwrap_or_default();
    let tier = |spec: Option<&str>, default| {
        spec.and_then(|s| parse_checksum_bound(s).ok())
            .unwrap_or(default)
    };
    let min = tier(dat.input_checksum_min.as_deref(), HashAlgo::Crc32);
    let max = tier(dat.input_checksum_max.as_deref(), HashAlgo::Sha256);
    ChecksumBounds::new(min, max).unwrap_or(ChecksumBounds {
        min: HashAlgo::Crc32,
        max: HashAlgo::Sha256,
    })
}

/// Digest through the persistent hash cache: an unchanged file (same
/// canonical path, size, mtime) reuses its stored digests instead of being
/// decoded and hashed again. Track sets stay uncached; the store holds
/// single-stream digests only.
async fn digest_cached(
    cache: &HashCache,
    input: PathBuf,
    algos: Vec<HashAlgo>,
    progress: &TauriProgress,
    token: CancelToken,
) -> Result<RomDigests, rom_converto_lib::dat::DatError> {
    if let Some(hit) = cache.lookup_decoded(&input, &algos) {
        return Ok(RomDigests::Single(hit));
    }
    let digests = digest_inner_async(input.clone(), algos, progress, token).await?;
    if let RomDigests::Single(d) = &digests {
        cache.store_decoded(&input, d);
    }
    Ok(digests)
}

async fn run_dat_verify(
    progress: Arc<TauriProgress>,
    cache: Arc<HashCache>,
    input: PathBuf,
    quick: bool,
    token: CancelToken,
) -> Result<DatVerifyResult, rom_converto_lib::dat::DatError> {
    let client = PlaymatchClient::new(Some(DEFAULT_API_BASE));

    // Quick mode: trust the zip's stored CRC32 for an eligible single-member
    // archive. Anything short of a verified match falls back to full hashing.
    if quick {
        let probe = input.clone();
        let quick_hit = tokio::task::spawn_blocking(move || quick_crc_digest(&probe))
            .await
            .ok()
            .flatten();
        if let Some(q) = quick_hit {
            progress.set_phase("Querying matches");
            let search = GameFileMatchSearch::from_digests(&q.member_name, &q.digests);
            let matched = client.identify_relations(&search, &token).await?;
            if match_strength(matched.game_match_type).is_verified() {
                return Ok(verify_result_from_single(
                    input.display().to_string(),
                    &matched,
                ));
            }
        }
    }

    // Tiered digests: compute the config floor first and escalate to the
    // stronger tier only when the floor alone does not verify, mirroring
    // the CLI's --input-checksum-min/--input-checksum-max policy.
    let bounds = dat_checksum_bounds();
    let (floor, escalation) = bounds.split(&[HashAlgo::Crc32, HashAlgo::Sha1]);
    let first = verify_pass(&client, &cache, &progress, &input, floor.clone(), &token).await?;
    if escalation.is_empty() || first.verdict == DatVerdict::Verified.as_str() {
        return Ok(first);
    }
    let mut full = floor;
    full.extend(escalation);
    verify_pass(&client, &cache, &progress, &input, full, &token).await
}

async fn verify_pass(
    client: &PlaymatchClient,
    cache: &HashCache,
    progress: &Arc<TauriProgress>,
    input: &Path,
    algos: Vec<HashAlgo>,
    token: &CancelToken,
) -> Result<DatVerifyResult, rom_converto_lib::dat::DatError> {
    let digests = digest_cached(
        cache,
        input.to_path_buf(),
        algos,
        progress.as_ref(),
        token.clone(),
    )
    .await?;
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
            let matched = client.identify_relations(&search, token).await?;
            Ok(verify_result_from_single(path_str, &matched))
        }
        RomDigests::Tracks { tracks, whole } => {
            let stem = input.file_stem().and_then(|n| n.to_str()).unwrap_or("file");
            let whole_name = format!("{stem}.bin");
            let whole_search = GameFileMatchSearch::from_digests(&whole_name, &whole);
            let whole_match = client.identify_relations(&whole_search, token).await?;
            if match_strength(whole_match.game_match_type).is_verified() {
                return Ok(verify_result_from_single(path_str, &whole_match));
            }

            let first_name = format!("{stem} (Track 1).bin");
            let first_digests = &tracks[0].digests;
            let first_search = GameFileMatchSearch::from_digests(&first_name, first_digests);
            let track_match = client.identify_relations(&first_search, token).await?;
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
        dat_file: matched
            .dat_file
            .as_ref()
            .map(|d| d.name.clone())
            .or_else(|| matched.dat_file_import.as_ref().map(|i| i.name.clone())),
        dat_file_id: matched.dat_file.as_ref().map(|d| d.id.clone()).or_else(|| {
            matched
                .dat_file_import
                .as_ref()
                .map(|i| i.dat_file_id.clone())
        }),
        dat_version: matched
            .dat_file_import
            .as_ref()
            .map(|i| i.version.clone())
            .or_else(|| matched.dat_file.as_ref().map(|d| d.current_version.clone())),
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
        dat_file: matched
            .dat_file
            .as_ref()
            .map(|d| d.name.clone())
            .or_else(|| matched.dat_file_import.as_ref().map(|i| i.name.clone())),
        dat_file_id: matched.dat_file.as_ref().map(|d| d.id.clone()).or_else(|| {
            matched
                .dat_file_import
                .as_ref()
                .map(|i| i.dat_file_id.clone())
        }),
        dat_version: matched
            .dat_file_import
            .as_ref()
            .map(|i| i.version.clone())
            .or_else(|| matched.dat_file.as_ref().map(|d| d.current_version.clone())),
        external_ids: external_ids_from(matched),
        tracks: Some(track_checks),
        error: None,
    }
}

#[derive(Clone, serde::Serialize)]
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
    Ok {
        path: PathBuf,
        digests: RomDigests,
        /// Digests taken from the zip's own central directory rather than a
        /// real read; scan redoes these in full when they fail to verify.
        quick: bool,
    },
    Unsupported {
        path: PathBuf,
    },
    Failed {
        path: PathBuf,
        error: String,
    },
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
    cache: &HashCache,
    input_dir: &Path,
    max_depth: Option<usize>,
    algos: &[HashAlgo],
    quick: bool,
    token: &CancelToken,
) -> Result<Vec<DigestedUnit>, String> {
    // Cue sheets and playlists are set descriptors, not hashable images: they
    // are handled via cue grouping (rename) and would otherwise digest to an
    // InvalidInput failure and surface as a spurious Failed row in scan.
    progress.set_phase("Collecting files");
    let walk_root = input_dir.to_path_buf();
    let walk_token = token.clone();
    let files: Vec<PathBuf> =
        tokio::task::spawn_blocking(move || collect_all_files(&walk_root, max_depth, &walk_token))
            .await
            .map_err(err_to_string)?
            .map_err(err_to_string)?
            .into_iter()
            .filter(|f| {
                !f.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("cue") || e.eq_ignore_ascii_case("m3u"))
            })
            .collect();
    // The outer channel counts files; per-file byte progress from the hasher
    // goes to a side channel so the bar never resets on every small file.
    progress.start(files.len() as u64, "Hashing files");
    let file_progress = progress.child("file");
    let mut units = Vec::with_capacity(files.len());
    for file in files {
        if token.is_cancelled() {
            return Err(Cancelled.to_string());
        }
        units.push(digest_one(progress, &file_progress, cache, file, algos, quick, token).await?);
        progress.inc(1);
    }
    Ok(units)
}

async fn digest_one(
    progress: &TauriProgress,
    file_progress: &TauriProgress,
    cache: &HashCache,
    file: PathBuf,
    algos: &[HashAlgo],
    quick: bool,
    token: &CancelToken,
) -> Result<DigestedUnit, String> {
    let pending = |path: &Path| DatScanRow {
        path: path.display().to_string(),
        status: "pending",
        game_name: None,
        canonical_stem: None,
        error: None,
    };
    if let Some(hit) = cache.lookup_decoded(&file, algos) {
        progress.emit_row(pending(&file));
        return Ok(DigestedUnit::Ok {
            path: file,
            digests: RomDigests::Single(hit),
            quick: false,
        });
    }
    // Quick digests come from the zip's own central directory, not a
    // real read of the content, so they are used as is and never stored
    // in the cache.
    if quick {
        let probe = file.clone();
        if let Ok(Some(q)) = tokio::task::spawn_blocking(move || quick_crc_digest(&probe)).await {
            progress.emit_row(pending(&file));
            return Ok(DigestedUnit::Ok {
                path: file,
                digests: RomDigests::Single(q.digests),
                quick: true,
            });
        }
    }
    match digest_inner_async(file.clone(), algos.to_vec(), file_progress, token.clone()).await {
        Ok(digests) => {
            if let RomDigests::Single(d) = &digests {
                cache.store_decoded(&file, d);
            }
            progress.emit_row(pending(&file));
            Ok(DigestedUnit::Ok {
                path: file,
                digests,
                quick: false,
            })
        }
        Err(rom_converto_lib::dat::DatError::UnsupportedInnerHash { .. }) => {
            progress.emit_row(DatScanRow {
                path: file.display().to_string(),
                status: DatVerdict::Unsupported.as_str(),
                game_name: None,
                canonical_stem: None,
                error: None,
            });
            Ok(DigestedUnit::Unsupported { path: file })
        }
        Err(rom_converto_lib::dat::DatError::Cancelled(_)) => Err(Cancelled.to_string()),
        Err(e) => {
            let error = e.to_string();
            progress.emit_row(DatScanRow {
                path: file.display().to_string(),
                status: DatVerdict::Failed.as_str(),
                game_name: None,
                canonical_stem: None,
                error: Some(error.clone()),
            });
            Ok(DigestedUnit::Failed { path: file, error })
        }
    }
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
    cache: State<'_, Arc<HashCache>>,
    input: PathBuf,
    max_depth: Option<usize>,
    algos: Option<Vec<String>>,
    quick: Option<bool>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "dat-scan"));
    // Scan level from the frontend selector; size plus CRC32 is the default
    // tier, matching the CLI's `dat scan --algo` default.
    let algos = match algos {
        Some(list) if !list.is_empty() => parse_algos(&list.join(","))?,
        _ => vec![HashAlgo::Crc32],
    };
    let token = begin(&state, "dat-scan").await;
    let cache = cache.inner().clone();
    let result = run_dat_scan(
        progress.clone(),
        cache.clone(),
        input,
        max_depth,
        algos,
        quick.unwrap_or(false),
        token,
    )
    .await;
    progress.finish();
    finish(&state, "dat-scan").await;
    cache.save();
    let outcome = result?;
    serde_json::to_string(&outcome).map_err(err_to_string)
}

async fn run_dat_scan(
    progress: Arc<TauriProgress>,
    cache: Arc<HashCache>,
    input: PathBuf,
    max_depth: Option<usize>,
    algos: Vec<HashAlgo>,
    quick: bool,
    token: CancelToken,
) -> Result<DatScanResult, String> {
    let units = digest_all(
        progress.as_ref(),
        &cache,
        &input,
        max_depth,
        &algos,
        quick,
        &token,
    )
    .await?;

    // Slot per unit, filled in either immediately (unsupported/failed) or
    // after the bulk query resolves (queryable). Keeps output row order
    // matching the walk order regardless of query completion order.
    let mut slots: Vec<Option<DatScanRow>> = Vec::with_capacity(units.len());
    let mut queryable = Vec::new();
    for (i, unit) in units.iter().enumerate() {
        match unit {
            DigestedUnit::Ok {
                path,
                digests,
                quick,
            } => {
                let file_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file")
                    .to_string();
                queryable.push((
                    i,
                    path.clone(),
                    file_name,
                    primary_digests(digests).clone(),
                    *quick,
                ));
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
    // Zero total marks the network phases as indeterminate for the frontend.
    if !queryable.is_empty() {
        progress.start(0, &format!("Matching {} files", queryable.len()));
    }
    let items: Vec<BulkIdentifyItem> = queryable
        .iter()
        .map(|(_, _, name, digests, _)| BulkIdentifyItem {
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

    for (queryable_idx, (unit_idx, path, _, _, _)) in queryable.iter().enumerate() {
        let result = bulk_results.iter().find(|r| r.index == queryable_idx);
        let row = scan_row_for(path, result, &name_for_id);
        progress.emit_row(row.clone());
        slots[*unit_idx] = Some(row);
    }

    // Quick digests come from the zip's central directory; when that alone
    // did not hash-verify, redo the unit with a full decode at the requested
    // scan level and query again, mirroring the CLI's quick redo pass.
    let redo: Vec<(usize, PathBuf, String)> = queryable
        .iter()
        .filter(|(unit_idx, _, _, _, was_quick)| {
            *was_quick
                && !matches!(
                    slots[*unit_idx].as_ref().map(|r| r.status),
                    Some("matched") | Some("misnamed")
                )
        })
        .map(|(unit_idx, path, name, _, _)| (*unit_idx, path.clone(), name.clone()))
        .collect();
    if !redo.is_empty() {
        progress.start(redo.len() as u64, "Rehashing quick misses");
        let file_progress = progress.child("file");
        let mut redo_items: Vec<BulkIdentifyItem> = Vec::new();
        let mut redo_owner: Vec<(usize, PathBuf)> = Vec::new();
        for (unit_idx, path, name) in redo {
            if token.is_cancelled() {
                return Err(Cancelled.to_string());
            }
            let digested =
                digest_inner_async(path.clone(), algos.clone(), &file_progress, token.clone())
                    .await;
            progress.inc(1);
            match digested {
                Ok(digests) => {
                    if let RomDigests::Single(d) = &digests {
                        cache.store_decoded(&path, d);
                    }
                    redo_items.push(BulkIdentifyItem {
                        search: GameFileMatchSearch::from_digests(&name, primary_digests(&digests)),
                        key: None,
                    });
                    redo_owner.push((unit_idx, path));
                }
                Err(rom_converto_lib::dat::DatError::Cancelled(_)) => {
                    return Err(Cancelled.to_string());
                }
                Err(e) => {
                    let row = DatScanRow {
                        path: path.display().to_string(),
                        status: DatVerdict::Failed.as_str(),
                        game_name: None,
                        canonical_stem: None,
                        error: Some(e.to_string()),
                    };
                    progress.emit_row(row.clone());
                    slots[unit_idx] = Some(row);
                }
            }
        }
        if !redo_items.is_empty() {
            progress.start(0, &format!("Matching {} rehashed files", redo_items.len()));
            let redo_results = client
                .identify_bulk_ids(redo_items, &token)
                .await
                .map_err(err_to_string)?;
            let mut redo_ids: Vec<String> = Vec::new();
            for r in &redo_results {
                if r.status == BulkItemStatus::Ok
                    && let Some(m) = &r.matched
                    && match_strength(m.game_match_type).is_verified()
                    && let Some(id) = &m.id
                {
                    redo_ids.push(id.clone());
                }
            }
            redo_ids.sort();
            redo_ids.dedup();
            let redo_games = if redo_ids.is_empty() {
                Vec::new()
            } else {
                client
                    .games_bulk(redo_ids, &token)
                    .await
                    .map_err(err_to_string)?
            };
            let redo_name_for_id = |id: &str| -> Option<String> {
                redo_games
                    .iter()
                    .find(|g| g.id == id)
                    .and_then(|g| g.data.as_ref())
                    .map(|d| d.name.clone())
            };
            for (redo_idx, (unit_idx, path)) in redo_owner.iter().enumerate() {
                let result = redo_results.iter().find(|r| r.index == redo_idx);
                let row = scan_row_for(path, result, &redo_name_for_id);
                progress.emit_row(row.clone());
                slots[*unit_idx] = Some(row);
            }
        }
    }

    let mut tally = ScanTally::default();
    let rows: Vec<DatScanRow> = slots
        .into_iter()
        .map(|slot| slot.expect("every unit gets exactly one row"))
        .inspect(|row| tally.count(row.status))
        .collect();

    if tally.unsupported > 0 {
        progress.warn(NX_DAT_UNSUPPORTED_HINT);
    }

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
    cache: State<'_, Arc<HashCache>>,
    input: PathBuf,
    max_depth: Option<usize>,
    dry_run: bool,
    on_conflict: String,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "dat-rename"));
    let token = begin(&state, "dat-rename").await;
    let cache = cache.inner().clone();
    let result = run_dat_rename(
        progress.clone(),
        cache.clone(),
        input,
        max_depth,
        dry_run,
        on_conflict,
        token,
    )
    .await;
    progress.finish();
    finish(&state, "dat-rename").await;
    cache.save();
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
    let files =
        collect_all_files(input_dir, max_depth, &CancelToken::new()).map_err(err_to_string)?;
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
    cache: Arc<HashCache>,
    input: PathBuf,
    max_depth: Option<usize>,
    dry_run: bool,
    on_conflict: String,
    token: CancelToken,
) -> Result<DatRenameResult, String> {
    if !input.is_dir() {
        return Err(format!("input is not a folder: {}", input.display()));
    }
    let policy = conflict_policy(Some(on_conflict.as_str()));
    let cue_sets = cue_sets_under(&input, max_depth).await?;
    let cue_covered: std::collections::HashSet<PathBuf> = cue_sets
        .iter()
        .flat_map(|(cue, bins)| std::iter::once(cue.clone()).chain(bins.iter().cloned()))
        .collect();
    let units = digest_all(
        progress.as_ref(),
        &cache,
        &input,
        max_depth,
        &[HashAlgo::Crc32, HashAlgo::Sha1],
        false,
        &token,
    )
    .await?;

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
            DigestedUnit::Ok { path, digests, .. } => {
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
/// filter and sort as the CLI batch walk. A `"*"` entry in `exts` matches
/// every file; an empty `exts` matches nothing. A non-directory path surfaces
/// as an error so the caller can fall back to treating it as a single file.
#[tauri::command]
pub async fn cmd_scan_dir(
    dir: PathBuf,
    exts: Vec<String>,
    max_depth: Option<usize>,
) -> Result<Vec<PathBuf>, String> {
    tokio::task::spawn_blocking(move || {
        // "*" lists every file; an empty list still matches nothing so folder-input ops stay unexpanded.
        if exts.iter().any(|e| e == "*") {
            return collect_all_files(&dir, max_depth, &CancelToken::new()).map_err(err_to_string);
        }
        let ext_refs: Vec<&str> = exts.iter().map(String::as_str).collect();
        collect_files_with_exts(&dir, &ext_refs, max_depth, &CancelToken::new())
            .map_err(err_to_string)
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
        InfoResult::Chd(c) => c.content.as_ref().and_then(disc_content_icon_png),
        InfoResult::Cso(c) => c.content.as_ref().and_then(disc_content_icon_png),
        InfoResult::Xbox(x) => x
            .xbe
            .as_ref()
            .and_then(|b| b.icon.as_ref())
            .or_else(|| x.xex.as_ref().and_then(|x| x.icon.as_ref()))
            .map(|i| i.png_bytes.clone()),
        InfoResult::Xenon(z) => z
            .xex
            .as_ref()
            .and_then(|x| x.icon.as_ref())
            .map(|i| i.png_bytes.clone()),
        InfoResult::Ps3(p) => p.icon.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::Psx(_) => None,
        InfoResult::Psp(p) => p.icon.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::LaserDisc(_) => None,
        InfoResult::Nds(n) => n.banner.as_ref().map(|b| b.icon.png_bytes.clone()),
        InfoResult::Retro(_) => None,
        InfoResult::Pbp(p) => p.icon.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::Vpk(v) => v.icon.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::Pkg(p) => p.icon.as_ref().map(|i| i.png_bytes.clone()),
    }
}

fn disc_content_icon_png(content: &DiscContent) -> Option<Vec<u8>> {
    match content {
        DiscContent::Psp(p) => p.icon.as_ref().map(|i| i.png_bytes.clone()),
        DiscContent::Psx(_) => None,
    }
}

#[cfg(test)]
mod args_serde_tests {
    use super::*;

    /// `CommonArgs` is flattened, so the wire payload stays flat camelCase.
    #[test]
    fn common_args_stay_flat_on_the_wire() {
        let args: ChdCompressArgs = serde_json::from_str(
            r#"{"inputPath":"/in.iso","output":null,"codecs":null,"level":null,
                "hunkSize":null,"mode":"cd","onConflict":"skip","skipSpaceCheck":true,
                "outputTemplate":null,"report":true,"verifyAfter":false,
                "dryRun":true,"taskId":"job-1"}"#,
        )
        .expect("flat payload deserializes");
        assert_eq!(args.common.on_conflict.as_deref(), Some("skip"));
        assert!(args.common.skip_space_check);
        assert_eq!(args.common.dry_run, Some(true));
        assert_eq!(args.common.task_id.as_deref(), Some("job-1"));
        assert_eq!(args.report, Some(true));
    }

    /// Optional members of the tail may be absent, as they are for ops whose
    /// UI never sets them.
    #[test]
    fn common_args_tolerate_absent_optionals() {
        let args: XboxExtractArgs = serde_json::from_str(
            r#"{"input":"/in.iso","outputDir":"/out","skipSpaceCheck":false}"#,
        )
        .expect("flat payload deserializes");
        assert!(args.common.on_conflict.is_none());
        assert!(args.common.dry_run.is_none());
        assert!(args.common.task_id.is_none());
    }

    /// `task_id` moved from a required `String` to `Option<String>` so it can
    /// flatten into `CommonArgs`; the wire shape (always sent, camelCase) is
    /// unchanged.
    #[test]
    fn compress_disc_args_stay_flat_on_the_wire() {
        let args: CompressDiscArgs = serde_json::from_str(
            r#"{"input":"/in.iso","output":null,"level":5,"chunkSize":null,
                "onConflict":"skip","skipSpaceCheck":true,"outputTemplate":null,
                "report":true,"verifyAfter":false,"taskId":"job-1"}"#,
        )
        .expect("flat payload deserializes");
        assert_eq!(args.common.on_conflict.as_deref(), Some("skip"));
        assert!(args.common.skip_space_check);
        assert_eq!(args.common.task_id.as_deref(), Some("job-1"));
        assert_eq!(args.level, Some(5));
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
            dat_file: Some("Some DAT".into()),
            dat_file_id: Some("d-1".into()),
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
                "datFile",
                "datFileId",
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
            &CancelToken::new(),
            ComparisonInput {
                input: &input,
                output: &output,
                input_bytes: 1024,
                output_bytes: 256,
                target: rom_converto_lib::util::OutputVerify::Rvz,
                verify_after: false,
            },
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
            &CancelToken::new(),
            ComparisonInput {
                input: &input,
                output: &output,
                input_bytes: 256,
                output_bytes: 1024,
                target: rom_converto_lib::util::OutputVerify::None,
                verify_after: false,
            },
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
            &CancelToken::new(),
            ComparisonInput {
                input: &input,
                output: &output,
                input_bytes: 8,
                output_bytes: 8,
                target: rom_converto_lib::util::OutputVerify::Chd,
                verify_after: false,
            },
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
            &CancelToken::new(),
            ComparisonInput {
                input: &input,
                output: &output,
                input_bytes: 22,
                output_bytes: 25,
                target: rom_converto_lib::util::OutputVerify::Chd,
                verify_after: true,
            },
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
            &CancelToken::new(),
            ComparisonInput {
                input: &input,
                output: &output,
                input_bytes: 22,
                output_bytes: 24,
                target: rom_converto_lib::util::OutputVerify::Rvz,
                verify_after: true,
            },
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
            &CancelToken::new(),
            ComparisonInput {
                input: &input,
                output: &output,
                input_bytes: 4,
                output_bytes: 4,
                target: rom_converto_lib::util::OutputVerify::Nx(Box::default()),
                verify_after: true,
            },
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
            &CancelToken::new(),
            ComparisonInput {
                input: &input,
                output: &output,
                input_bytes: 4,
                output_bytes: 4,
                target: rom_converto_lib::util::OutputVerify::None,
                verify_after: true,
            },
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
