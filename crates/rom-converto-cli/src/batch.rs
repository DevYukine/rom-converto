use crate::util::{WriteDecision, resolve_output};
use anyhow::Result;
use log::{info, warn};
use rom_converto_lib::cue::CueParser;
use rom_converto_lib::dat::client::DatFileFilter;
use rom_converto_lib::dat::digest::{
    QuickDigest, RomDigests, TrackDigests, digest_inner_async, quick_crc_digest,
};
use rom_converto_lib::dat::fixdat::{LocalHashIndex, diff_library, write_fixdat_xml};
use rom_converto_lib::dat::model::{
    BulkIdentifyIdsResult, BulkIdentifyItem, BulkItemStatus, DatFileSummary,
    GameAndRelationMatchResult, GameFileMatchSearch,
};
use rom_converto_lib::dat::rename::{RenameAction, RenameCandidate, RenamePlan, plan_renames};
use rom_converto_lib::dat::verdict::{DatVerdict, MatchStrength, match_strength, reconcile_tracks};
use rom_converto_lib::dat::{DatError, DatResult, PlaymatchClient};
use rom_converto_lib::util::fs::{collect_all_files, collect_files_with_exts, is_os_junk_dir};
use rom_converto_lib::util::hash::MultiHasher;
use rom_converto_lib::util::report::{DatReportRecord, write_dat_report};
use rom_converto_lib::util::{
    CachedTrack, CancelToken, ChecksumBounds, ConflictPolicy, ConflictResolution, FileDigests,
    FileStatus, HashAlgo, HashCache, HashReportRecord, NX_DAT_UNSUPPORTED_HINT, ProgressReporter,
    ReportFormat, ReportRecord, ReportTotals, Tally, TallyDirection, hash_file,
    hash_file_cancellable, resolve_conflict, resolve_input, write_hash_report, write_report,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Sum of on-disk sizes for the aggregate progress bar's total byte length.
fn files_bytes(files: &[PathBuf]) -> u64 {
    files.iter().map(|p| file_len(p)).sum()
}

/// On-disk size of one DAT unit: the file itself, or every member bin for a
/// cue set (the cue sheet text itself is not counted).
fn unit_bytes(unit: &DatUnit) -> u64 {
    match unit {
        DatUnit::File(p) => file_len(p),
        DatUnit::CueSet(set) => set.bins.iter().map(|p| file_len(p)).sum(),
    }
}

fn units_bytes(units: &[DatUnit]) -> u64 {
    units.iter().map(unit_bytes).sum()
}

pub(crate) fn space_preflight(files: &[PathBuf], check_dir: &Path) -> Result<()> {
    let required: u64 = files.iter().map(|p| file_len(p)).sum();
    space_preflight_for_size(required, check_dir)
}

pub(crate) fn space_preflight_for_size(required: u64, check_dir: &Path) -> Result<()> {
    // The output dir may not exist yet (it is created after this check), so
    // probe the nearest existing ancestor, which sits on the same filesystem.
    let probe = check_dir
        .ancestors()
        .find(|p| p.exists())
        .unwrap_or(check_dir);
    match rom_converto_lib::util::available_space(probe) {
        Ok(available) => {
            if rom_converto_lib::util::space_shortfall(
                available,
                required,
                rom_converto_lib::util::DEFAULT_SPACE_HEADROOM,
            )
            .is_some()
            {
                anyhow::bail!(
                    "not enough free space at {}: need about {}, only {} available. Re-run with --skip-space-check to proceed anyway.",
                    check_dir.display(),
                    rom_converto_lib::util::format_bytes(
                        required.saturating_add(rom_converto_lib::util::DEFAULT_SPACE_HEADROOM)
                    ),
                    rom_converto_lib::util::format_bytes(available)
                );
            }
        }
        Err(e) => log::debug!(
            "Free-space check unavailable at {}: {e}",
            check_dir.display()
        ),
    }
    Ok(())
}

pub(crate) fn totals_from(tally: &Tally) -> ReportTotals {
    ReportTotals {
        total_files: tally.count(),
        ok: tally.ok_count(),
        skipped: tally.skipped_count(),
        failed: tally.failed_count(),
        total_input_bytes: tally.total_input_bytes(),
        total_output_bytes: tally.total_output_bytes(),
        elapsed_ms: tally.elapsed().as_millis().min(u64::MAX as u128) as u64,
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

fn ok_record(
    input: &Path,
    output: &Path,
    operation: &str,
    input_bytes: u64,
    output_bytes: u64,
    started: Instant,
) -> ReportRecord {
    ReportRecord::new(
        input.display().to_string(),
        output.display().to_string(),
        operation,
        FileStatus::Ok,
        input_bytes,
        output_bytes,
        elapsed_ms(started),
        None,
    )
}

fn failed_record(
    input: &Path,
    operation: &str,
    input_bytes: u64,
    started: Instant,
    error: impl std::fmt::Display,
) -> ReportRecord {
    ReportRecord::new(
        input.display().to_string(),
        String::new(),
        operation,
        FileStatus::Failed,
        input_bytes,
        0,
        elapsed_ms(started),
        Some(error.to_string()),
    )
}

fn skipped_record(input: &Path, operation: &str, error: Option<String>) -> ReportRecord {
    ReportRecord::new(
        input.display().to_string(),
        String::new(),
        operation,
        FileStatus::Skipped,
        0,
        0,
        0,
        error,
    )
}

struct VerifyTally {
    total: usize,
    ok: usize,
    failed: usize,
}

/// Dry-run plan entry for a batch arm that may verify. For `overwrite-invalid`
/// with an existing output the read-only verify runs to choose the keep or
/// rewrite label; every other case falls through to the existing plan path so
/// the tally and report counts stay identical to a real run.
#[allow(clippy::too_many_arguments)]
async fn dry_run_verify_record(
    progress: &dyn ProgressReporter,
    operation: &str,
    input: &Path,
    desired: &Path,
    decision: &WriteDecision,
    policy: ConflictPolicy,
    target: crate::util::OutputVerify,
    media: Option<&str>,
    tally: &mut Tally,
    records: &mut Vec<ReportRecord>,
) {
    if policy == ConflictPolicy::OverwriteInvalid && desired.exists() {
        let (synth, outcome) =
            match crate::util::verify_existing_output(progress, desired, target).await {
                crate::util::VerifyOutcome::Valid => (
                    WriteDecision::Skip,
                    rom_converto_lib::util::PlanDecision::KeepValid,
                ),
                crate::util::VerifyOutcome::Invalid => (
                    WriteDecision::Write(desired.to_path_buf()),
                    rom_converto_lib::util::PlanDecision::RewriteInvalid,
                ),
            };
        crate::dry_run::log_plan_decision(operation, input, desired, &synth, outcome, media, None);
        crate::dry_run::record(tally, input, &synth);
        records.push(crate::dry_run::report_record(
            operation, input, desired, &synth,
        ));
        return;
    }
    crate::dry_run::log_plan(operation, input, desired, decision, media, None);
    crate::dry_run::record(tally, input, decision);
    records.push(crate::dry_run::report_record(
        operation, input, desired, decision,
    ));
}

fn collect_or_warn(
    input_dir: &Path,
    exts: &[&str],
    max_depth: Option<usize>,
) -> Result<Vec<PathBuf>> {
    let files = collect_files_with_exts(input_dir, exts, max_depth)?;
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

fn finish_tally(
    tally: &Tally,
    direction: TallyDirection,
    records: &[ReportRecord],
    dry_run: bool,
    report_path: Option<&Path>,
) -> Result<()> {
    let direction = if dry_run {
        TallyDirection::DryRun
    } else {
        direction
    };
    info!("{}", tally.summary_line(direction));
    // Write the report before the failed-count bail so failed-only runs still
    // leave a report on disk even though the command exits with an error.
    if let Some(path) = report_path {
        write_report(
            path,
            records,
            &totals_from(tally),
            ReportFormat::from_path(path),
        )?;
    }
    let failed = tally.failed_count();
    if failed > 0 {
        anyhow::bail!("{failed} of {} files failed", tally.count());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn cso_decompress(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    policy: ConflictPolicy,
    output_dir: Option<&Path>,
    output_template: Option<&str>,
    max_depth: Option<usize>,
    dry_run: bool,
    skip_space_check: bool,
    report_path: Option<&Path>,
    cancel: CancelToken,
) -> Result<()> {
    use rom_converto_lib::cso::decompress_from_cso_cancellable;

    let files = collect_or_warn(input_dir, &["cso", "zso", "dax"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if !dry_run && !skip_space_check {
        space_preflight(&files, output_dir.unwrap_or(input_dir))?;
    }
    if !dry_run && let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }
    let total = files.len();
    total_progress.begin(total as u64, files_bytes(&files));
    let mut tally = Tally::new();
    let mut records: Vec<ReportRecord> = Vec::new();
    for path in files {
        if cancel.is_cancelled() {
            break;
        }
        let output = match crate::util::batch_output(
            &path,
            &path.with_extension("iso"),
            input_dir,
            output_dir,
            output_template,
            "iso",
            None,
            dry_run,
        ) {
            Ok(p) => p,
            Err(e) => {
                warn!("{e}");
                tally.record_skipped();
                records.push(skipped_record(&path, "decompress", Some(e.to_string())));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        if !dry_run && let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let decision = match resolve_output(&output, policy) {
            Ok(d) => d,
            Err(e) => {
                warn!("{e}");
                tally.record_skipped();
                records.push(skipped_record(&path, "decompress", Some(e.to_string())));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        if dry_run {
            crate::dry_run::log_plan("decompress", &path, &output, &decision, None, None);
            crate::dry_run::record(&mut tally, &path, &decision);
            records.push(crate::dry_run::report_record(
                "decompress",
                &path,
                &output,
                &decision,
            ));
            total_progress.advance(file_len(&path));
            continue;
        }
        let output = match decision {
            WriteDecision::Write(p) => p,
            WriteDecision::Skip => {
                info!("Skipped, output exists: {}", output.display());
                tally.record_skipped();
                records.push(skipped_record(&path, "decompress", None));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        let input_bytes = file_len(&path);
        let out_path = output.clone();
        let started = Instant::now();
        if let Err(e) =
            decompress_from_cso_cancellable(progress, path.clone(), output, true, cancel.clone())
                .await
        {
            if matches!(e, rom_converto_lib::cso::CsoError::Cancelled) {
                break;
            }
            warn!("Failed to decompress {}: {e}", path.display());
            tally.record_failed();
            records.push(failed_record(&path, "decompress", input_bytes, started, e));
        } else {
            let out_bytes = file_len(&out_path);
            tally.record_ok(input_bytes, out_bytes, started.elapsed());
            records.push(ok_record(
                &path,
                &out_path,
                "decompress",
                input_bytes,
                out_bytes,
                started,
            ));
        }
        total_progress.advance(input_bytes);
    }
    total_progress.finish();
    if cancel.is_cancelled() {
        return Ok(());
    }
    finish_tally(
        &tally,
        TallyDirection::Decompress,
        &records,
        dry_run,
        report_path,
    )
}

pub async fn cso_verify(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    full: bool,
    max_depth: Option<usize>,
) -> Result<()> {
    use rom_converto_lib::cso::verify_cso;

    let files = collect_or_warn(input_dir, &["cso", "zso", "dax"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    let total = files.len();
    total_progress.begin(total as u64, files_bytes(&files));
    let mut ok = 0usize;
    let mut failed = 0usize;
    for path in files {
        let bytes = file_len(&path);
        match verify_cso(progress, path.clone(), full).await {
            Ok(()) => {
                ok += 1;
                info!("[OK] {}", path.display());
            }
            Err(e) => {
                failed += 1;
                warn!("[FAIL] {}: {e}", path.display());
            }
        }
        total_progress.advance(bytes);
    }
    total_progress.finish();
    finish_verify(VerifyTally { total, ok, failed })
}

#[allow(clippy::too_many_arguments)]
pub async fn rvz_compress(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    exts: &[&str],
    opts: rom_converto_lib::nintendo::rvz::RvzCompressOptions,
    policy: ConflictPolicy,
    output_dir: Option<&Path>,
    output_template: Option<&str>,
    max_depth: Option<usize>,
    dry_run: bool,
    skip_space_check: bool,
    report_path: Option<&Path>,
    cancel: CancelToken,
    cache: &HashCache,
) -> Result<()> {
    use rom_converto_lib::nintendo::rvz::{compress_disc_cancellable, derive_rvz_path};

    let files = collect_or_warn(input_dir, exts, max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if !dry_run && !skip_space_check {
        space_preflight(&files, output_dir.unwrap_or(input_dir))?;
    }
    if !dry_run && let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }
    let total = files.len();
    total_progress.begin(total as u64, files_bytes(&files));
    let mut tally = Tally::new();
    let mut records: Vec<ReportRecord> = Vec::new();
    for path in files {
        if cancel.is_cancelled() {
            break;
        }
        let output = match crate::util::batch_output(
            &path,
            &derive_rvz_path(&path),
            input_dir,
            output_dir,
            output_template,
            "rvz",
            None,
            dry_run,
        ) {
            Ok(p) => p,
            Err(e) => {
                warn!("{e}");
                tally.record_skipped();
                records.push(skipped_record(&path, "compress", Some(e.to_string())));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        if !dry_run && let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let decision = match resolve_output(&output, policy) {
            Ok(d) => d,
            Err(e) => {
                warn!("{e}");
                tally.record_skipped();
                records.push(skipped_record(&path, "compress", Some(e.to_string())));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        if dry_run {
            dry_run_verify_record(
                progress,
                "compress",
                &path,
                &output,
                &decision,
                policy,
                crate::util::OutputVerify::Rvz,
                Some("RVZ"),
                &mut tally,
                &mut records,
            )
            .await;
            total_progress.advance(file_len(&path));
            continue;
        }
        let output = match decision {
            WriteDecision::Write(p) => p,
            WriteDecision::Skip if policy == ConflictPolicy::OverwriteInvalid => {
                match crate::util::verify_existing_cached(
                    cache,
                    progress,
                    &output,
                    crate::util::OutputVerify::Rvz,
                )
                .await
                {
                    crate::util::VerifyOutcome::Valid => {
                        info!("Kept, output verified valid: {}", output.display());
                        tally.record_skipped();
                        records.push(skipped_record(
                            &path,
                            "compress",
                            Some("output verified valid".into()),
                        ));
                        total_progress.advance(file_len(&path));
                        continue;
                    }
                    crate::util::VerifyOutcome::Invalid => {
                        info!(
                            "Rewriting, output failed verification: {}",
                            output.display()
                        );
                        output
                    }
                }
            }
            WriteDecision::Skip => {
                info!("Skipped, output exists: {}", output.display());
                tally.record_skipped();
                records.push(skipped_record(&path, "compress", None));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        let input_bytes = file_len(&path);
        let started = Instant::now();
        if let Err(e) =
            compress_disc_cancellable(&path, &output, opts, progress, cancel.clone()).await
        {
            if matches!(e, rom_converto_lib::nintendo::rvz::RvzError::Cancelled) {
                break;
            }
            warn!("Failed to compress {}: {e}", path.display());
            tally.record_failed();
            records.push(failed_record(&path, "compress", input_bytes, started, e));
        } else {
            let out_bytes = file_len(&output);
            tally.record_ok(input_bytes, out_bytes, started.elapsed());
            records.push(ok_record(
                &path,
                &output,
                "compress",
                input_bytes,
                out_bytes,
                started,
            ));
        }
        total_progress.advance(input_bytes);
    }
    total_progress.finish();
    if cancel.is_cancelled() {
        return Ok(());
    }
    finish_tally(
        &tally,
        TallyDirection::Compress,
        &records,
        dry_run,
        report_path,
    )
}

#[allow(clippy::too_many_arguments)]
pub async fn rvz_decompress(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    policy: ConflictPolicy,
    output_dir: Option<&Path>,
    output_template: Option<&str>,
    max_depth: Option<usize>,
    dry_run: bool,
    skip_space_check: bool,
    report_path: Option<&Path>,
    cancel: CancelToken,
) -> Result<()> {
    use rom_converto_lib::nintendo::rvz::{decompress_disc_cancellable, derive_disc_path};

    let files = collect_or_warn(input_dir, &["rvz"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if !dry_run && !skip_space_check {
        space_preflight(&files, output_dir.unwrap_or(input_dir))?;
    }
    if !dry_run && let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }
    let total = files.len();
    total_progress.begin(total as u64, files_bytes(&files));
    let mut tally = Tally::new();
    let mut records: Vec<ReportRecord> = Vec::new();
    for path in files {
        if cancel.is_cancelled() {
            break;
        }
        let output = match crate::util::batch_output(
            &path,
            &derive_disc_path(&path),
            input_dir,
            output_dir,
            output_template,
            "iso",
            None,
            dry_run,
        ) {
            Ok(p) => p,
            Err(e) => {
                warn!("{e}");
                tally.record_skipped();
                records.push(skipped_record(&path, "decompress", Some(e.to_string())));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        if !dry_run && let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let decision = match resolve_output(&output, policy) {
            Ok(d) => d,
            Err(e) => {
                warn!("{e}");
                tally.record_skipped();
                records.push(skipped_record(&path, "decompress", Some(e.to_string())));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        if dry_run {
            crate::dry_run::log_plan("decompress", &path, &output, &decision, None, None);
            crate::dry_run::record(&mut tally, &path, &decision);
            records.push(crate::dry_run::report_record(
                "decompress",
                &path,
                &output,
                &decision,
            ));
            total_progress.advance(file_len(&path));
            continue;
        }
        let output = match decision {
            WriteDecision::Write(p) => p,
            WriteDecision::Skip => {
                info!("Skipped, output exists: {}", output.display());
                tally.record_skipped();
                records.push(skipped_record(&path, "decompress", None));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        let input_bytes = file_len(&path);
        let started = Instant::now();
        if let Err(e) = decompress_disc_cancellable(&path, &output, progress, cancel.clone()).await
        {
            if matches!(e, rom_converto_lib::nintendo::rvz::RvzError::Cancelled) {
                break;
            }
            warn!("Failed to decompress {}: {e}", path.display());
            tally.record_failed();
            records.push(failed_record(&path, "decompress", input_bytes, started, e));
        } else {
            let out_bytes = file_len(&output);
            tally.record_ok(input_bytes, out_bytes, started.elapsed());
            records.push(ok_record(
                &path,
                &output,
                "decompress",
                input_bytes,
                out_bytes,
                started,
            ));
        }
        total_progress.advance(input_bytes);
    }
    total_progress.finish();
    if cancel.is_cancelled() {
        return Ok(());
    }
    finish_tally(
        &tally,
        TallyDirection::Decompress,
        &records,
        dry_run,
        report_path,
    )
}

pub async fn dol_verify(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    full: bool,
    max_depth: Option<usize>,
) -> Result<()> {
    use rom_converto_lib::nintendo::dol::verify::{DolVerifyOptions, verify_dol};

    let files = collect_or_warn(input_dir, &["iso", "gcm", "rvz"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    let total = files.len();
    let opts = DolVerifyOptions { full };
    total_progress.begin(total as u64, files_bytes(&files));
    let mut ok = 0usize;
    let mut failed = 0usize;
    for path in files {
        let bytes = file_len(&path);
        match verify_dol(&path, &opts, progress) {
            Ok(result) if result.ok => {
                ok += 1;
                info!("[OK] {}", path.display());
            }
            Ok(_) => {
                failed += 1;
                warn!("[FAIL] {}", path.display());
            }
            Err(e) => {
                failed += 1;
                warn!("[FAIL] {}: {e}", path.display());
            }
        }
        total_progress.advance(bytes);
    }
    total_progress.finish();
    finish_verify(VerifyTally { total, ok, failed })
}

pub async fn rvl_verify(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    full: bool,
    max_depth: Option<usize>,
) -> Result<()> {
    use rom_converto_lib::nintendo::rvl::verify::{RvlVerifyOptions, verify_rvl};

    let files = collect_or_warn(input_dir, &["iso", "wbfs", "rvz"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    let total = files.len();
    let opts = RvlVerifyOptions { full };
    total_progress.begin(total as u64, files_bytes(&files));
    let mut ok = 0usize;
    let mut failed = 0usize;
    for path in files {
        let bytes = file_len(&path);
        match verify_rvl(&path, &opts, progress) {
            Ok(result) if result.ok => {
                ok += 1;
                info!("[OK] {}", path.display());
            }
            Ok(_) => {
                failed += 1;
                warn!("[FAIL] {}", path.display());
            }
            Err(e) => {
                failed += 1;
                warn!("[FAIL] {}: {e}", path.display());
            }
        }
        total_progress.advance(bytes);
    }
    total_progress.finish();
    finish_verify(VerifyTally { total, ok, failed })
}

pub struct NxCompressTuning {
    pub level: Option<i32>,
    pub mode: Option<String>,
    pub block_size_exp: Option<u8>,
    pub policy: ConflictPolicy,
    pub output_dir: Option<PathBuf>,
    pub output_template: Option<String>,
    pub max_depth: Option<usize>,
    pub dry_run: bool,
    pub skip_space_check: bool,
    pub report: Option<PathBuf>,
}

pub async fn nx_compress(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    keys: rom_converto_lib::nintendo::nx::KeySet,
    tuning: NxCompressTuning,
    cancel: CancelToken,
    cache: &HashCache,
) -> Result<()> {
    use rom_converto_lib::nintendo::nx::{
        NczMode, NxCompressOptions, compress_container_async_cancellable, derive_compressed_path,
        detect_container,
    };

    let dry_run = tuning.dry_run;
    let files = collect_or_warn(input_dir, &["nsp", "xci"], tuning.max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if !dry_run && !tuning.skip_space_check {
        space_preflight(&files, tuning.output_dir.as_deref().unwrap_or(input_dir))?;
    }
    if !dry_run && let Some(dir) = tuning.output_dir.as_deref() {
        std::fs::create_dir_all(dir)?;
    }
    let total = files.len();
    total_progress.begin(total as u64, files_bytes(&files));
    let mut tally = Tally::new();
    let mut records: Vec<ReportRecord> = Vec::new();
    for path in files {
        if cancel.is_cancelled() {
            break;
        }
        let kind = match detect_container(&path) {
            Ok(kind) => kind,
            Err(e) => {
                warn!("Failed to compress {}: {e}", path.display());
                tally.record_failed();
                records.push(failed_record(
                    &path,
                    "compress",
                    file_len(&path),
                    Instant::now(),
                    e,
                ));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        let mut opts = NxCompressOptions::for_kind(kind);
        if let Some(level) = tuning.level {
            opts.level = level;
        }
        if let Some(mode) = tuning.mode.as_deref() {
            opts.mode = match mode {
                "solid" => NczMode::Solid,
                "block" => NczMode::Block {
                    size_exp: tuning.block_size_exp.unwrap_or(20),
                },
                _ => unreachable!("clap value_parser already validated"),
            };
        } else if let Some(exp) = tuning.block_size_exp {
            opts.mode = NczMode::Block { size_exp: exp };
        }
        let output = match crate::util::batch_output(
            &path,
            &derive_compressed_path(&path),
            input_dir,
            tuning.output_dir.as_deref(),
            tuning.output_template.as_deref(),
            derive_compressed_path(&path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or(""),
            None,
            dry_run,
        ) {
            Ok(p) => p,
            Err(e) => {
                warn!("{e}");
                tally.record_skipped();
                records.push(skipped_record(&path, "compress", Some(e.to_string())));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        if !dry_run && let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let decision = match resolve_output(&output, tuning.policy) {
            Ok(d) => d,
            Err(e) => {
                warn!("{e}");
                tally.record_skipped();
                records.push(skipped_record(&path, "compress", Some(e.to_string())));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        if dry_run {
            let media = format!("{kind:?}");
            dry_run_verify_record(
                progress,
                "compress",
                &path,
                &output,
                &decision,
                tuning.policy,
                crate::util::OutputVerify::Nx(Box::new(keys.clone())),
                Some(&media),
                &mut tally,
                &mut records,
            )
            .await;
            total_progress.advance(file_len(&path));
            continue;
        }
        let output = match decision {
            WriteDecision::Write(p) => p,
            WriteDecision::Skip if tuning.policy == ConflictPolicy::OverwriteInvalid => {
                match crate::util::verify_existing_cached(
                    cache,
                    progress,
                    &output,
                    crate::util::OutputVerify::Nx(Box::new(keys.clone())),
                )
                .await
                {
                    crate::util::VerifyOutcome::Valid => {
                        info!("Kept, output verified valid: {}", output.display());
                        tally.record_skipped();
                        records.push(skipped_record(
                            &path,
                            "compress",
                            Some("output verified valid".into()),
                        ));
                        total_progress.advance(file_len(&path));
                        continue;
                    }
                    crate::util::VerifyOutcome::Invalid => {
                        info!(
                            "Rewriting, output failed verification: {}",
                            output.display()
                        );
                        output
                    }
                }
            }
            WriteDecision::Skip => {
                info!("Skipped, output exists: {}", output.display());
                tally.record_skipped();
                records.push(skipped_record(&path, "compress", None));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        let input_bytes = file_len(&path);
        let out_path = output.clone();
        let started = Instant::now();
        if let Err(e) = compress_container_async_cancellable(
            path.clone(),
            output,
            opts,
            keys.clone(),
            progress,
            cancel.clone(),
        )
        .await
        {
            if matches!(e, rom_converto_lib::nintendo::nx::NxError::Cancelled) {
                break;
            }
            warn!("Failed to compress {}: {e}", path.display());
            tally.record_failed();
            records.push(failed_record(&path, "compress", input_bytes, started, e));
        } else {
            let out_bytes = file_len(&out_path);
            tally.record_ok(input_bytes, out_bytes, started.elapsed());
            records.push(ok_record(
                &path,
                &out_path,
                "compress",
                input_bytes,
                out_bytes,
                started,
            ));
        }
        total_progress.advance(input_bytes);
    }
    total_progress.finish();
    if cancel.is_cancelled() {
        return Ok(());
    }
    finish_tally(
        &tally,
        TallyDirection::Compress,
        &records,
        dry_run,
        tuning.report.as_deref(),
    )
}

#[allow(clippy::too_many_arguments)]
pub async fn nx_decompress(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    keys: rom_converto_lib::nintendo::nx::KeySet,
    policy: ConflictPolicy,
    output_dir: Option<&Path>,
    output_template: Option<&str>,
    max_depth: Option<usize>,
    dry_run: bool,
    skip_space_check: bool,
    report_path: Option<&Path>,
    cancel: CancelToken,
) -> Result<()> {
    use rom_converto_lib::nintendo::nx::{
        decompress_container_async_cancellable, derive_decompressed_path,
    };

    let files = collect_or_warn(input_dir, &["nsz", "xcz"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if !dry_run && !skip_space_check {
        space_preflight(&files, output_dir.unwrap_or(input_dir))?;
    }
    if !dry_run && let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }
    let total = files.len();
    total_progress.begin(total as u64, files_bytes(&files));
    let mut tally = Tally::new();
    let mut records: Vec<ReportRecord> = Vec::new();
    for path in files {
        if cancel.is_cancelled() {
            break;
        }
        let output = match crate::util::batch_output(
            &path,
            &derive_decompressed_path(&path),
            input_dir,
            output_dir,
            output_template,
            derive_decompressed_path(&path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or(""),
            None,
            dry_run,
        ) {
            Ok(p) => p,
            Err(e) => {
                warn!("{e}");
                tally.record_skipped();
                records.push(skipped_record(&path, "decompress", Some(e.to_string())));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        if !dry_run && let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let decision = match resolve_output(&output, policy) {
            Ok(d) => d,
            Err(e) => {
                warn!("{e}");
                tally.record_skipped();
                records.push(skipped_record(&path, "decompress", Some(e.to_string())));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        if dry_run {
            crate::dry_run::log_plan("decompress", &path, &output, &decision, None, None);
            crate::dry_run::record(&mut tally, &path, &decision);
            records.push(crate::dry_run::report_record(
                "decompress",
                &path,
                &output,
                &decision,
            ));
            total_progress.advance(file_len(&path));
            continue;
        }
        let output = match decision {
            WriteDecision::Write(p) => p,
            WriteDecision::Skip => {
                info!("Skipped, output exists: {}", output.display());
                tally.record_skipped();
                records.push(skipped_record(&path, "decompress", None));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        let input_bytes = file_len(&path);
        let out_path = output.clone();
        let started = Instant::now();
        if let Err(e) = decompress_container_async_cancellable(
            path.clone(),
            output,
            keys.clone(),
            progress,
            cancel.clone(),
        )
        .await
        {
            if matches!(e, rom_converto_lib::nintendo::nx::NxError::Cancelled) {
                break;
            }
            warn!("Failed to decompress {}: {e}", path.display());
            tally.record_failed();
            records.push(failed_record(&path, "decompress", input_bytes, started, e));
        } else {
            let out_bytes = file_len(&out_path);
            tally.record_ok(input_bytes, out_bytes, started.elapsed());
            records.push(ok_record(
                &path,
                &out_path,
                "decompress",
                input_bytes,
                out_bytes,
                started,
            ));
        }
        total_progress.advance(input_bytes);
    }
    total_progress.finish();
    if cancel.is_cancelled() {
        return Ok(());
    }
    finish_tally(
        &tally,
        TallyDirection::Decompress,
        &records,
        dry_run,
        report_path,
    )
}

pub async fn nx_verify(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    keys: rom_converto_lib::nintendo::nx::KeySet,
    max_depth: Option<usize>,
) -> Result<()> {
    use rom_converto_lib::nintendo::nx::verify_container_async;

    let files = collect_or_warn(input_dir, &["nsp", "xci", "nsz", "xcz"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    let total = files.len();
    total_progress.begin(total as u64, files_bytes(&files));
    let mut ok = 0usize;
    let mut failed = 0usize;
    for path in files {
        let bytes = file_len(&path);
        match verify_container_async(path.clone(), keys.clone(), progress).await {
            Ok(result) if result.ok => {
                ok += 1;
                info!("[OK] {}", path.display());
            }
            Ok(_) => {
                failed += 1;
                warn!("[FAIL] {}", path.display());
            }
            Err(e) => {
                failed += 1;
                warn!("[FAIL] {}: {e}", path.display());
            }
        }
        total_progress.advance(bytes);
    }
    total_progress.finish();
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
) -> Result<()> {
    use rom_converto_lib::nintendo::wup::verify_wup_async;

    let mut inputs = collect_files_with_exts(input_dir, &["wud", "wux"], max_depth)?;
    if let Ok(entries) = std::fs::read_dir(input_dir) {
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_none_or(|n| !is_os_junk_dir(n))
                    && is_nus_title_dir(p)
            })
            .collect();
        dirs.sort();
        inputs.extend(dirs);
    }
    inputs.sort();

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
    for path in inputs {
        let bytes = file_len(&path);
        match verify_wup_async(path.clone(), None, progress).await {
            Ok(result) if result.ok => {
                ok += 1;
                info!("[OK] {}", path.display());
            }
            Ok(_) => {
                failed += 1;
                warn!("[FAIL] {}", path.display());
            }
            Err(e) => {
                failed += 1;
                warn!("[FAIL] {}: {e}", path.display());
            }
        }
        total_progress.advance(bytes);
    }
    total_progress.finish();
    finish_verify(VerifyTally { total, ok, failed })
}

#[allow(clippy::too_many_arguments)]
pub async fn chd_compress(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    mut opts: rom_converto_lib::chd::ChdDvdOptions,
    mode: Option<rom_converto_lib::chd::DiscMode>,
    policy: ConflictPolicy,
    output_dir: Option<&Path>,
    output_template: Option<&str>,
    max_depth: Option<usize>,
    dry_run: bool,
    skip_space_check: bool,
    report_path: Option<&Path>,
    cancel: CancelToken,
    cache: &HashCache,
) -> Result<()> {
    use rom_converto_lib::chd::convert_disc_to_chd_cancellable;

    let files = collect_or_warn(input_dir, &["cue", "iso"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if !dry_run && !skip_space_check {
        space_preflight(&files, output_dir.unwrap_or(input_dir))?;
    }
    if !dry_run && let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }
    opts.force = true;
    let total = files.len();
    total_progress.begin(total as u64, files_bytes(&files));
    let mut tally = Tally::new();
    let mut records: Vec<ReportRecord> = Vec::new();
    for path in files {
        if cancel.is_cancelled() {
            break;
        }
        let output = match crate::util::batch_output(
            &path,
            &path.with_extension("chd"),
            input_dir,
            output_dir,
            output_template,
            "chd",
            None,
            dry_run,
        ) {
            Ok(p) => p,
            Err(e) => {
                warn!("{e}");
                tally.record_skipped();
                records.push(skipped_record(&path, "compress", Some(e.to_string())));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        if !dry_run && let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let decision = match resolve_output(&output, policy) {
            Ok(d) => d,
            Err(e) => {
                warn!("{e}");
                tally.record_skipped();
                records.push(skipped_record(&path, "compress", Some(e.to_string())));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        if dry_run {
            let media = crate::chd_media_label(&path);
            dry_run_verify_record(
                progress,
                "compress",
                &path,
                &output,
                &decision,
                policy,
                crate::util::OutputVerify::Chd,
                media.as_deref(),
                &mut tally,
                &mut records,
            )
            .await;
            total_progress.advance(file_len(&path));
            continue;
        }
        let output = match decision {
            WriteDecision::Write(p) => p,
            WriteDecision::Skip if policy == ConflictPolicy::OverwriteInvalid => {
                match crate::util::verify_existing_cached(
                    cache,
                    progress,
                    &output,
                    crate::util::OutputVerify::Chd,
                )
                .await
                {
                    crate::util::VerifyOutcome::Valid => {
                        info!("Kept, output verified valid: {}", output.display());
                        tally.record_skipped();
                        records.push(skipped_record(
                            &path,
                            "compress",
                            Some("output verified valid".into()),
                        ));
                        total_progress.advance(file_len(&path));
                        continue;
                    }
                    crate::util::VerifyOutcome::Invalid => {
                        info!(
                            "Rewriting, output failed verification: {}",
                            output.display()
                        );
                        output
                    }
                }
            }
            WriteDecision::Skip => {
                info!("Skipped, output exists: {}", output.display());
                tally.record_skipped();
                records.push(skipped_record(&path, "compress", None));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        let input_bytes = file_len(&path);
        let out_path = output.clone();
        let started = Instant::now();
        if let Err(e) = convert_disc_to_chd_cancellable(
            progress,
            path.clone(),
            output,
            mode,
            opts.clone(),
            cancel.clone(),
        )
        .await
        {
            if matches!(e, rom_converto_lib::chd::error::ChdError::Cancelled) {
                break;
            }
            warn!("Failed to compress {}: {e}", path.display());
            tally.record_failed();
            records.push(failed_record(&path, "compress", input_bytes, started, e));
        } else {
            let out_bytes = file_len(&out_path);
            tally.record_ok(input_bytes, out_bytes, started.elapsed());
            records.push(ok_record(
                &path,
                &out_path,
                "compress",
                input_bytes,
                out_bytes,
                started,
            ));
        }
        total_progress.advance(input_bytes);
    }
    total_progress.finish();
    if cancel.is_cancelled() {
        return Ok(());
    }
    finish_tally(
        &tally,
        TallyDirection::Compress,
        &records,
        dry_run,
        report_path,
    )
}

#[allow(clippy::too_many_arguments)]
pub async fn chd_extract(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    parent: Option<PathBuf>,
    policy: ConflictPolicy,
    output_dir: Option<&Path>,
    output_template: Option<&str>,
    max_depth: Option<usize>,
    dry_run: bool,
    skip_space_check: bool,
    report_path: Option<&Path>,
    cancel: CancelToken,
) -> Result<()> {
    use rom_converto_lib::chd::extract_from_chd_cancellable;

    let files = collect_or_warn(input_dir, &["chd"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if !dry_run && !skip_space_check {
        space_preflight(&files, output_dir.unwrap_or(input_dir))?;
    }
    if !dry_run && let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }
    let total = files.len();
    total_progress.begin(total as u64, files_bytes(&files));
    let mut tally = Tally::new();
    let mut records: Vec<ReportRecord> = Vec::new();
    for path in files {
        if cancel.is_cancelled() {
            break;
        }
        let output = match crate::util::batch_output(
            &path,
            &path.with_extension(""),
            input_dir,
            output_dir,
            output_template,
            "iso",
            None,
            dry_run,
        ) {
            Ok(p) => p,
            Err(e) => {
                warn!("{e}");
                tally.record_skipped();
                records.push(skipped_record(&path, "extract", Some(e.to_string())));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        if !dry_run && let Some(p) = output.parent() {
            std::fs::create_dir_all(p)?;
        }
        let decision = match resolve_output(&output, policy) {
            Ok(d) => d,
            Err(e) => {
                warn!("{e}");
                tally.record_skipped();
                records.push(skipped_record(&path, "extract", Some(e.to_string())));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        if dry_run {
            crate::dry_run::log_plan("extract", &path, &output, &decision, None, None);
            crate::dry_run::record(&mut tally, &path, &decision);
            records.push(crate::dry_run::report_record(
                "extract", &path, &output, &decision,
            ));
            total_progress.advance(file_len(&path));
            continue;
        }
        let output = match decision {
            WriteDecision::Write(p) => p,
            WriteDecision::Skip => {
                if policy == ConflictPolicy::OverwriteInvalid {
                    crate::util::verify_existing_output(
                        progress,
                        &output,
                        crate::util::OutputVerify::None,
                    )
                    .await;
                }
                info!("Skipped, output exists: {}", output.display());
                tally.record_skipped();
                records.push(skipped_record(&path, "extract", None));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        let input_bytes = file_len(&path);
        let out_path = output.clone();
        let started = Instant::now();
        if let Err(e) = extract_from_chd_cancellable(
            progress,
            path.clone(),
            output,
            parent.clone(),
            cancel.clone(),
        )
        .await
        {
            if matches!(e, rom_converto_lib::chd::error::ChdError::Cancelled) {
                break;
            }
            warn!("Failed to extract {}: {e}", path.display());
            tally.record_failed();
            records.push(failed_record(&path, "extract", 0, started, e));
        } else {
            tally.record_ok(0, 0, started.elapsed());
            records.push(ok_record(&path, &out_path, "extract", 0, 0, started));
        }
        total_progress.advance(input_bytes);
    }
    total_progress.finish();
    if cancel.is_cancelled() {
        return Ok(());
    }
    finish_tally(
        &tally,
        TallyDirection::CountOnly,
        &records,
        dry_run,
        report_path,
    )
}

#[allow(clippy::too_many_arguments)]
pub async fn cso_compress(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    mut opts: rom_converto_lib::cso::CsoCompressOptions,
    policy: ConflictPolicy,
    output_dir: Option<&Path>,
    output_template: Option<&str>,
    max_depth: Option<usize>,
    dry_run: bool,
    skip_space_check: bool,
    report_path: Option<&Path>,
    cancel: CancelToken,
    cache: &HashCache,
) -> Result<()> {
    use rom_converto_lib::cso::compress_to_cso_cancellable;

    let ext = opts.format.extension();
    let media = opts.format.name();
    let files = collect_or_warn(input_dir, &["iso"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if !dry_run && !skip_space_check {
        space_preflight(&files, output_dir.unwrap_or(input_dir))?;
    }
    if !dry_run && let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }
    opts.force = true;
    let total = files.len();
    total_progress.begin(total as u64, files_bytes(&files));
    let mut tally = Tally::new();
    let mut records: Vec<ReportRecord> = Vec::new();
    for path in files {
        if cancel.is_cancelled() {
            break;
        }
        let output = match crate::util::batch_output(
            &path,
            &path.with_extension(ext),
            input_dir,
            output_dir,
            output_template,
            ext,
            None,
            dry_run,
        ) {
            Ok(p) => p,
            Err(e) => {
                warn!("{e}");
                tally.record_skipped();
                records.push(skipped_record(&path, "compress", Some(e.to_string())));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        if !dry_run && let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let decision = match resolve_output(&output, policy) {
            Ok(d) => d,
            Err(e) => {
                warn!("{e}");
                tally.record_skipped();
                records.push(skipped_record(&path, "compress", Some(e.to_string())));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        if dry_run {
            dry_run_verify_record(
                progress,
                "compress",
                &path,
                &output,
                &decision,
                policy,
                crate::util::OutputVerify::Cso,
                Some(media),
                &mut tally,
                &mut records,
            )
            .await;
            total_progress.advance(file_len(&path));
            continue;
        }
        let output = match decision {
            WriteDecision::Write(p) => p,
            WriteDecision::Skip if policy == ConflictPolicy::OverwriteInvalid => {
                match crate::util::verify_existing_cached(
                    cache,
                    progress,
                    &output,
                    crate::util::OutputVerify::Cso,
                )
                .await
                {
                    crate::util::VerifyOutcome::Valid => {
                        info!("Kept, output verified valid: {}", output.display());
                        tally.record_skipped();
                        records.push(skipped_record(
                            &path,
                            "compress",
                            Some("output verified valid".into()),
                        ));
                        total_progress.advance(file_len(&path));
                        continue;
                    }
                    crate::util::VerifyOutcome::Invalid => {
                        info!(
                            "Rewriting, output failed verification: {}",
                            output.display()
                        );
                        output
                    }
                }
            }
            WriteDecision::Skip => {
                info!("Skipped, output exists: {}", output.display());
                tally.record_skipped();
                records.push(skipped_record(&path, "compress", None));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        let input_bytes = file_len(&path);
        let out_path = output.clone();
        let started = Instant::now();
        if let Err(e) = compress_to_cso_cancellable(
            progress,
            path.clone(),
            output,
            opts.clone(),
            cancel.clone(),
        )
        .await
        {
            if matches!(e, rom_converto_lib::cso::CsoError::Cancelled) {
                break;
            }
            warn!("Failed to compress {}: {e}", path.display());
            tally.record_failed();
            records.push(failed_record(&path, "compress", input_bytes, started, e));
        } else {
            let out_bytes = file_len(&out_path);
            tally.record_ok(input_bytes, out_bytes, started.elapsed());
            records.push(ok_record(
                &path,
                &out_path,
                "compress",
                input_bytes,
                out_bytes,
                started,
            ));
        }
        total_progress.advance(input_bytes);
    }
    total_progress.finish();
    if cancel.is_cancelled() {
        return Ok(());
    }
    finish_tally(
        &tally,
        TallyDirection::Compress,
        &records,
        dry_run,
        report_path,
    )
}

/// Best-effort uncompressed size read from a CSO/ZSO header, for the
/// space preflight: the temporary ISO the pipeline decodes to needs
/// room for the full disc, not just the compressed file's own size.
fn cso_uncompressed_size(path: &Path) -> u64 {
    rom_converto_lib::cso::info::read_info(path)
        .map(|info| info.uncompressed_size)
        .unwrap_or(0)
}

/// Best-effort flat-ISO size read from a CHD header, for the space
/// preflight on the extract-then-compress direction.
fn chd_logical_bytes(path: &Path) -> u64 {
    rom_converto_lib::chd::info::read_info(path)
        .map(|info| info.logical_bytes)
        .unwrap_or(0)
}

#[allow(clippy::too_many_arguments)]
pub async fn cso_to_chd(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    mode: Option<rom_converto_lib::chd::DiscMode>,
    mut opts: rom_converto_lib::chd::ChdDvdOptions,
    policy: ConflictPolicy,
    output_dir: Option<&Path>,
    output_template: Option<&str>,
    max_depth: Option<usize>,
    dry_run: bool,
    skip_space_check: bool,
    report_path: Option<&Path>,
    cancel: CancelToken,
    cache: &HashCache,
) -> Result<()> {
    use rom_converto_lib::pipeline::cso_to_chd_cancellable;

    let files = collect_or_warn(input_dir, &["cso", "zso", "dax"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if !dry_run && !skip_space_check {
        let required: u64 = files.iter().map(|p| cso_uncompressed_size(p)).sum();
        space_preflight_for_size(required, output_dir.unwrap_or(input_dir))?;
    }
    if !dry_run && let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }
    opts.force = true;
    let total = files.len();
    total_progress.begin(total as u64, files_bytes(&files));
    let mut tally = Tally::new();
    let mut records: Vec<ReportRecord> = Vec::new();
    for path in files {
        if cancel.is_cancelled() {
            break;
        }
        let output = match crate::util::batch_output(
            &path,
            &path.with_extension("chd"),
            input_dir,
            output_dir,
            output_template,
            "chd",
            None,
            dry_run,
        ) {
            Ok(p) => p,
            Err(e) => {
                warn!("{e}");
                tally.record_skipped();
                records.push(skipped_record(&path, "compress", Some(e.to_string())));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        if !dry_run && let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let decision = match resolve_output(&output, policy) {
            Ok(d) => d,
            Err(e) => {
                warn!("{e}");
                tally.record_skipped();
                records.push(skipped_record(&path, "compress", Some(e.to_string())));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        if dry_run {
            dry_run_verify_record(
                progress,
                "compress",
                &path,
                &output,
                &decision,
                policy,
                crate::util::OutputVerify::Chd,
                None,
                &mut tally,
                &mut records,
            )
            .await;
            total_progress.advance(file_len(&path));
            continue;
        }
        let output = match decision {
            WriteDecision::Write(p) => p,
            WriteDecision::Skip if policy == ConflictPolicy::OverwriteInvalid => {
                match crate::util::verify_existing_cached(
                    cache,
                    progress,
                    &output,
                    crate::util::OutputVerify::Chd,
                )
                .await
                {
                    crate::util::VerifyOutcome::Valid => {
                        info!("Kept, output verified valid: {}", output.display());
                        tally.record_skipped();
                        records.push(skipped_record(
                            &path,
                            "compress",
                            Some("output verified valid".into()),
                        ));
                        total_progress.advance(file_len(&path));
                        continue;
                    }
                    crate::util::VerifyOutcome::Invalid => {
                        info!(
                            "Rewriting, output failed verification: {}",
                            output.display()
                        );
                        output
                    }
                }
            }
            WriteDecision::Skip => {
                info!("Skipped, output exists: {}", output.display());
                tally.record_skipped();
                records.push(skipped_record(&path, "compress", None));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        let input_bytes = file_len(&path);
        let out_path = output.clone();
        let started = Instant::now();
        if let Err(e) = cso_to_chd_cancellable(
            progress,
            path.clone(),
            output,
            mode,
            opts.clone(),
            cancel.clone(),
        )
        .await
        {
            if crate::is_cancelled_error(&e) {
                break;
            }
            warn!("Failed to compress {}: {e}", path.display());
            tally.record_failed();
            records.push(failed_record(&path, "compress", input_bytes, started, e));
        } else {
            let out_bytes = file_len(&out_path);
            tally.record_ok(input_bytes, out_bytes, started.elapsed());
            records.push(ok_record(
                &path,
                &out_path,
                "compress",
                input_bytes,
                out_bytes,
                started,
            ));
        }
        total_progress.advance(input_bytes);
    }
    total_progress.finish();
    if cancel.is_cancelled() {
        return Ok(());
    }
    finish_tally(
        &tally,
        TallyDirection::Compress,
        &records,
        dry_run,
        report_path,
    )
}

#[allow(clippy::too_many_arguments)]
pub async fn chd_to_cso(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    mut opts: rom_converto_lib::cso::CsoCompressOptions,
    policy: ConflictPolicy,
    output_dir: Option<&Path>,
    output_template: Option<&str>,
    max_depth: Option<usize>,
    dry_run: bool,
    skip_space_check: bool,
    report_path: Option<&Path>,
    cancel: CancelToken,
    cache: &HashCache,
) -> Result<()> {
    use rom_converto_lib::pipeline::chd_to_cso_cancellable;

    let ext = opts.format.extension();
    let files = collect_or_warn(input_dir, &["chd"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if !dry_run && !skip_space_check {
        let required: u64 = files.iter().map(|p| chd_logical_bytes(p)).sum();
        space_preflight_for_size(required, output_dir.unwrap_or(input_dir))?;
    }
    if !dry_run && let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }
    opts.force = true;
    let total = files.len();
    total_progress.begin(total as u64, files_bytes(&files));
    let mut tally = Tally::new();
    let mut records: Vec<ReportRecord> = Vec::new();
    for path in files {
        if cancel.is_cancelled() {
            break;
        }
        let output = match crate::util::batch_output(
            &path,
            &path.with_extension(ext),
            input_dir,
            output_dir,
            output_template,
            ext,
            None,
            dry_run,
        ) {
            Ok(p) => p,
            Err(e) => {
                warn!("{e}");
                tally.record_skipped();
                records.push(skipped_record(&path, "compress", Some(e.to_string())));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        if !dry_run && let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let decision = match resolve_output(&output, policy) {
            Ok(d) => d,
            Err(e) => {
                warn!("{e}");
                tally.record_skipped();
                records.push(skipped_record(&path, "compress", Some(e.to_string())));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        if dry_run {
            dry_run_verify_record(
                progress,
                "compress",
                &path,
                &output,
                &decision,
                policy,
                crate::util::OutputVerify::Cso,
                None,
                &mut tally,
                &mut records,
            )
            .await;
            total_progress.advance(file_len(&path));
            continue;
        }
        let output = match decision {
            WriteDecision::Write(p) => p,
            WriteDecision::Skip if policy == ConflictPolicy::OverwriteInvalid => {
                match crate::util::verify_existing_cached(
                    cache,
                    progress,
                    &output,
                    crate::util::OutputVerify::Cso,
                )
                .await
                {
                    crate::util::VerifyOutcome::Valid => {
                        info!("Kept, output verified valid: {}", output.display());
                        tally.record_skipped();
                        records.push(skipped_record(
                            &path,
                            "compress",
                            Some("output verified valid".into()),
                        ));
                        total_progress.advance(file_len(&path));
                        continue;
                    }
                    crate::util::VerifyOutcome::Invalid => {
                        info!(
                            "Rewriting, output failed verification: {}",
                            output.display()
                        );
                        output
                    }
                }
            }
            WriteDecision::Skip => {
                info!("Skipped, output exists: {}", output.display());
                tally.record_skipped();
                records.push(skipped_record(&path, "compress", None));
                total_progress.advance(file_len(&path));
                continue;
            }
        };
        let input_bytes = file_len(&path);
        let out_path = output.clone();
        let started = Instant::now();
        if let Err(e) =
            chd_to_cso_cancellable(progress, path.clone(), output, opts.clone(), cancel.clone())
                .await
        {
            if crate::is_cancelled_error(&e) {
                break;
            }
            warn!("Failed to compress {}: {e}", path.display());
            tally.record_failed();
            records.push(failed_record(&path, "compress", input_bytes, started, e));
        } else {
            let out_bytes = file_len(&out_path);
            tally.record_ok(input_bytes, out_bytes, started.elapsed());
            records.push(ok_record(
                &path,
                &out_path,
                "compress",
                input_bytes,
                out_bytes,
                started,
            ));
        }
        total_progress.advance(input_bytes);
    }
    total_progress.finish();
    if cancel.is_cancelled() {
        return Ok(());
    }
    finish_tally(
        &tally,
        TallyDirection::Compress,
        &records,
        dry_run,
        report_path,
    )
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

pub async fn hash_batch(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    algos: &[HashAlgo],
    max_depth: Option<usize>,
    report_path: Option<&Path>,
    cache: &HashCache,
) -> Result<()> {
    let files = collect_all_files(input_dir, max_depth)?;
    if files.is_empty() {
        warn!("No files found in {}", input_dir.display());
        return Ok(());
    }
    let total = files.len();
    total_progress.begin(total as u64, files_bytes(&files));
    let mut tally = Tally::new();
    let mut records: Vec<HashReportRecord> = Vec::new();
    for path in files {
        let bytes = file_len(&path);
        let started = Instant::now();
        let hashed = match cache.lookup_raw(&path, algos) {
            Some(d) => Ok(d),
            None => {
                let computed = hash_file(&path, algos, progress);
                if let Ok(d) = &computed {
                    cache.store_raw(&path, d);
                }
                computed
            }
        };
        match hashed {
            Ok(d) => {
                crate::print_hash_row(&path, &d, algos);
                tally.record_ok(d.size_bytes, 0, started.elapsed());
                records.push(hash_ok_record(&path, &d, started));
            }
            Err(e) => {
                warn!("Failed to hash {}: {e}", path.display());
                tally.record_failed();
                records.push(hash_failed_record(&path, started, e));
            }
        }
        total_progress.advance(bytes);
    }
    total_progress.finish();
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
        )?;
    }
    Ok(())
}

/// Resolved fixdat command fields, decoupled from the clap struct so the
/// driver takes owned values without borrowing the parsed command.
pub struct DatFixdatArgs {
    pub input: PathBuf,
    pub output: PathBuf,
    pub platform: Option<String>,
    pub dat_id: Option<String>,
    pub dat_name: Option<String>,
    pub subset: Option<String>,
    pub max_depth: Option<usize>,
    pub api_base: Option<String>,
}

/// A member .bin set behind one .cue sheet. The bins are hashed raw in cue
/// order; the concatenation is the single-bin whole-image stream.
struct CueSet {
    cue: PathBuf,
    bins: Vec<PathBuf>,
}

/// One walkable input: a standalone file digested by its container decoder, or
/// a cue set whose member bins are hashed raw.
enum DatUnit {
    File(PathBuf),
    CueSet(CueSet),
}

impl DatUnit {
    fn display_path(&self) -> &Path {
        match self {
            DatUnit::File(p) => p,
            DatUnit::CueSet(s) => &s.cue,
        }
    }
}

/// Walk `input_dir`, group each .cue with its member .bin files, and drop the
/// grouped bins plus report-ish sidecar files from the flat list. Bins with no
/// owning cue stay as standalone File units.
async fn dat_collect(input_dir: &Path, max_depth: Option<usize>) -> Result<Vec<DatUnit>> {
    let files = collect_all_files(input_dir, max_depth)?;
    let mut cues: Vec<PathBuf> = Vec::new();
    let mut others: Vec<PathBuf> = Vec::new();
    for f in files {
        match f
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref()
        {
            Some("cue") => cues.push(f),
            Some("m3u") => {}
            _ => others.push(f),
        }
    }

    let mut covered: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut sets: Vec<CueSet> = Vec::new();
    for cue in cues {
        let parent = cue.parent().unwrap_or_else(|| Path::new("."));
        let sheet = match CueParser::new(&cue).parse().await {
            Ok(s) => s,
            Err(e) => {
                warn!("Skipping unreadable cue {}: {e}", cue.display());
                continue;
            }
        };
        let mut bins: Vec<PathBuf> = Vec::new();
        for file in &sheet.files {
            let bin = parent.join(&file.filename);
            covered.insert(bin.clone());
            bins.push(bin);
        }
        if !bins.is_empty() {
            sets.push(CueSet { cue, bins });
        }
    }

    let mut units: Vec<DatUnit> = Vec::new();
    for set in sets {
        units.push(DatUnit::CueSet(set));
    }
    for f in others {
        if !covered.contains(&f) {
            units.push(DatUnit::File(f));
        }
    }
    Ok(units)
}

/// Digest one unit. A File goes through the container decoder; a CueSet hashes
/// each member bin raw in cue order, folding each into its own track digest and
/// the concatenated whole-image digest.
async fn digest_unit(
    unit: &DatUnit,
    algos: &[HashAlgo],
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
    cache: &HashCache,
) -> DatResult<RomDigests> {
    match unit {
        DatUnit::File(path) => {
            if let Some(d) = cache.lookup_decoded(path, algos) {
                return Ok(RomDigests::Single(d));
            }
            let result =
                digest_inner_async(path.clone(), algos.to_vec(), progress, cancel.clone()).await?;
            // Only single-stream decodes are cached; a CHD's per-track result
            // is left uncached (no whole-set fingerprint is tracked here).
            if let RomDigests::Single(d) = &result {
                cache.store_decoded(path, d);
            }
            Ok(result)
        }
        DatUnit::CueSet(set) => {
            if let Some(hit) = cache.lookup_cue_set(&set.cue, &set.bins, algos) {
                let tracks = hit
                    .tracks
                    .into_iter()
                    .map(|t| TrackDigests {
                        track_number: t.number,
                        track_type: t.kind,
                        digests: t.digests,
                    })
                    .collect();
                return Ok(RomDigests::Tracks {
                    tracks,
                    whole: hit.whole,
                });
            }
            let result = digest_cue_set(set, algos, progress, cancel)?;
            if let RomDigests::Tracks { tracks, whole } = &result {
                let cached: Vec<CachedTrack> = tracks
                    .iter()
                    .map(|t| CachedTrack {
                        number: t.track_number,
                        kind: t.track_type.clone(),
                        digests: t.digests.clone(),
                    })
                    .collect();
                cache.store_cue_set(&set.cue, &set.bins, whole, &cached);
            }
            Ok(result)
        }
    }
}

/// Hash each member bin raw in cue order into a per-track digest and fold the
/// same bytes into a whole-image hasher, yielding `RomDigests::Tracks`.
fn digest_cue_set(
    set: &CueSet,
    algos: &[HashAlgo],
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> DatResult<RomDigests> {
    let mut tracks: Vec<TrackDigests> = Vec::with_capacity(set.bins.len());
    let mut whole = MultiHasher::new(algos);
    let mut whole_size: u64 = 0;
    for (i, bin) in set.bins.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(DatError::Cancelled);
        }
        let digests = hash_file_cancellable(bin, algos, progress, cancel).map_err(|e| {
            if e.kind() == std::io::ErrorKind::Interrupted {
                DatError::Cancelled
            } else {
                DatError::IoError(e)
            }
        })?;
        // Re-read the bin to fold it into the whole-image hasher. Bins are
        // hashed twice (once per-track, once whole) because the DAT may use
        // either the single-bin or the multi-bin convention.
        fold_file(bin, &mut whole, cancel)?;
        whole_size += digests.size_bytes;
        tracks.push(TrackDigests {
            track_number: (i + 1) as u32,
            track_type: String::new(),
            digests,
        });
    }
    Ok(RomDigests::Tracks {
        tracks,
        whole: whole.finalize(whole_size),
    })
}

/// Stream a file into an existing multi-hasher, honoring cancellation.
fn fold_file(path: &Path, hasher: &mut MultiHasher, cancel: &CancelToken) -> DatResult<()> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        if cancel.is_cancelled() {
            return Err(DatError::Cancelled);
        }
        hasher.update(&buf[..n]);
    }
    Ok(())
}

/// Resolved verify/identify outcome for one unit: the verdict plus the display
/// fields and per-track detail the CLI prints and the report row records.
struct DatOutcome {
    verdict: DatVerdict,
    match_algo: Option<HashAlgo>,
    game_name: Option<String>,
    game_id: Option<String>,
    platform: Option<String>,
    signature_group: Option<String>,
    dat_file_name: Option<String>,
    dat_file_id: Option<String>,
    dat_version: Option<String>,
    detail: Option<String>,
    size_bytes: u64,
}

/// The display fields lifted from a relations match.
struct DisplayFields {
    game_name: Option<String>,
    game_id: Option<String>,
    platform: Option<String>,
    signature_group: Option<String>,
    dat_file_name: Option<String>,
    dat_file_id: Option<String>,
    dat_version: Option<String>,
}

fn display_fields(m: &GameAndRelationMatchResult) -> DisplayFields {
    DisplayFields {
        game_name: m.game.as_ref().map(|g| g.name.clone()),
        game_id: m.game.as_ref().map(|g| g.id.clone()),
        platform: m.platform.as_ref().map(|p| p.name.clone()),
        signature_group: m.signature_group.as_ref().map(|g| g.name.clone()),
        dat_file_name: m
            .dat_file
            .as_ref()
            .map(|d| d.name.clone())
            .or_else(|| m.dat_file_import.as_ref().map(|i| i.name.clone())),
        dat_file_id: m
            .dat_file
            .as_ref()
            .map(|d| d.id.clone())
            .or_else(|| m.dat_file_import.as_ref().map(|i| i.dat_file_id.clone())),
        dat_version: m
            .dat_file_import
            .as_ref()
            .map(|i| i.version.clone())
            .or_else(|| m.dat_file.as_ref().map(|d| d.current_version.clone())),
    }
}

/// Whole-image decoded size for a digested unit: the sole size for a single
/// stream, the concatenated whole size for a track set.
fn unit_size(digests: &RomDigests) -> u64 {
    match digests {
        RomDigests::Single(d) => d.size_bytes,
        RomDigests::Tracks { whole, .. } => whole.size_bytes,
    }
}

/// Resolve the verify verdict for one unit against the database. Single
/// streams take one relations call; track sets try the whole-image query
/// first (single-bin DATs) and fall back to a per-track reconciliation
/// (multi-bin DATs), keeping the stronger of the two results.
async fn resolve_verify(
    client: &PlaymatchClient,
    unit: &DatUnit,
    digests: &RomDigests,
    cancel: &CancelToken,
) -> DatResult<DatOutcome> {
    let size = unit_size(digests);
    match digests {
        RomDigests::Single(d) => {
            let file_name = file_name_of(unit.display_path());
            let search = GameFileMatchSearch::from_digests(&file_name, d);
            let m = client.identify_relations(&search, cancel).await?;
            Ok(outcome_from_single(&m, size))
        }
        RomDigests::Tracks { tracks, whole } => {
            let stem = stem_of(unit.display_path());
            let whole_name = format!("{stem}.bin");
            let whole_search = GameFileMatchSearch::from_digests(&whole_name, whole);
            let whole_match = client.identify_relations(&whole_search, cancel).await?;
            if match_strength(whole_match.game_match_type).is_verified() {
                return Ok(outcome_from_single(&whole_match, size));
            }

            let first_name = match unit {
                DatUnit::CueSet(set) => file_name_of(&set.bins[0]),
                _ => format!("{stem} (Track 1).bin"),
            };
            let first_search = GameFileMatchSearch::from_digests(&first_name, &tracks[0].digests);
            let track_match = client.identify_relations(&first_search, cancel).await?;
            Ok(outcome_from_tracks(
                &track_match,
                tracks,
                &whole_match,
                size,
            ))
        }
    }
}

fn outcome_from_single(m: &GameAndRelationMatchResult, size: u64) -> DatOutcome {
    let strength = match_strength(m.game_match_type);
    let verdict = match strength {
        MatchStrength::Verified(_) => DatVerdict::Verified,
        MatchStrength::NameSizeHint => DatVerdict::Hint,
        MatchStrength::NoMatch => DatVerdict::Unknown,
    };
    let f = display_fields(m);
    DatOutcome {
        verdict,
        match_algo: match strength {
            MatchStrength::Verified(a) => Some(a),
            _ => None,
        },
        game_name: f.game_name,
        game_id: f.game_id,
        platform: f.platform,
        signature_group: f.signature_group,
        dat_file_name: f.dat_file_name,
        dat_file_id: f.dat_file_id,
        dat_version: f.dat_version,
        detail: None,
        size_bytes: size,
    }
}

fn outcome_from_tracks(
    track_match: &GameAndRelationMatchResult,
    tracks: &[TrackDigests],
    whole_match: &GameAndRelationMatchResult,
    size: u64,
) -> DatOutcome {
    let recon = reconcile_tracks(tracks, &track_match.game_files);
    // Prefer the response that actually carried the match for display: the
    // per-track query when it reconciled or resolved a game, else the whole
    // query. A lone track-1 hash hit is display-only; it does not verify the set.
    let track_resolved = recon.all_ok || match_strength(track_match.game_match_type).is_verified();
    let display = if track_resolved {
        track_match
    } else {
        whole_match
    };
    let f = display_fields(display);

    // A track set is Verified only when every local track reconciles by a real
    // hash (recon.all_ok). A hash-verified track 1 with an unreconciled other
    // track is not whole-set verification.
    let verdict = if recon.all_ok {
        DatVerdict::Verified
    } else if match_strength(track_match.game_match_type) == MatchStrength::NameSizeHint
        || match_strength(whole_match.game_match_type) == MatchStrength::NameSizeHint
    {
        DatVerdict::Hint
    } else {
        DatVerdict::Unknown
    };

    let ok = recon.tracks.iter().filter(|t| t.ok).count();
    let detail = if recon.tracks.is_empty() {
        None
    } else {
        Some(format!("{ok}/{} tracks matched", recon.tracks.len()))
    };

    DatOutcome {
        verdict,
        match_algo: None,
        game_name: f.game_name,
        game_id: f.game_id,
        platform: f.platform,
        signature_group: f.signature_group,
        dat_file_name: f.dat_file_name,
        dat_file_id: f.dat_file_id,
        dat_version: f.dat_version,
        detail,
        size_bytes: size,
    }
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string()
}

fn stem_of(path: &Path) -> String {
    path.file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string()
}

/// Build a report row from a resolved outcome.
fn dat_record(
    path: &Path,
    outcome: &DatOutcome,
    status: FileStatus,
    started: Instant,
) -> DatReportRecord {
    DatReportRecord {
        path: path.display().to_string(),
        verdict: outcome.verdict.as_str().to_string(),
        game_name: outcome.game_name.clone(),
        game_id: outcome.game_id.clone(),
        platform: outcome.platform.clone(),
        signature_group: outcome.signature_group.clone(),
        dat_file_name: outcome.dat_file_name.clone(),
        dat_file_id: outcome.dat_file_id.clone(),
        dat_version: outcome.dat_version.clone(),
        match_algo: outcome.match_algo.map(|a| a.label().to_string()),
        detail: outcome.detail.clone(),
        size_bytes: outcome.size_bytes,
        status,
        elapsed_ms: elapsed_ms(started),
        error: None,
    }
}

/// A failed or unsupported unit as a report row (no database lookup happened).
fn dat_error_record(
    path: &Path,
    verdict: DatVerdict,
    status: FileStatus,
    error: Option<String>,
    started: Instant,
) -> DatReportRecord {
    DatReportRecord {
        path: path.display().to_string(),
        verdict: verdict.as_str().to_string(),
        game_name: None,
        game_id: None,
        platform: None,
        signature_group: None,
        dat_file_name: None,
        dat_file_id: None,
        dat_version: None,
        match_algo: None,
        detail: None,
        size_bytes: 0,
        status,
        elapsed_ms: elapsed_ms(started),
        error,
    }
}

/// Totals for a DAT report driven purely by outcome counts (no byte flow).
fn dat_totals(records: &[DatReportRecord], elapsed: std::time::Duration) -> ReportTotals {
    ReportTotals {
        total_files: records.len(),
        ok: records
            .iter()
            .filter(|r| r.status == FileStatus::Ok)
            .count(),
        failed: records
            .iter()
            .filter(|r| r.status == FileStatus::Failed)
            .count(),
        total_input_bytes: records.iter().map(|r| r.size_bytes).sum(),
        elapsed_ms: elapsed.as_millis().min(u64::MAX as u128) as u64,
        ..ReportTotals::default()
    }
}

/// Print one verify/identify verdict line, with a per-track summary when the
/// unit is a track set.
fn print_verdict(path: &Path, outcome: &DatOutcome) {
    let name = path.display();
    match outcome.verdict {
        DatVerdict::Verified => {
            let algo = outcome
                .match_algo
                .map(|a| format!(" [{}]", a.label()))
                .unwrap_or_default();
            let game = outcome.game_name.as_deref().unwrap_or("");
            let mut extra = Vec::new();
            if let Some(p) = &outcome.platform {
                extra.push(format!("platform: {p}"));
            }
            if let Some(g) = &outcome.signature_group {
                extra.push(format!("group: {g}"));
            }
            if let Some(d) = &outcome.dat_file_name {
                extra.push(format!("DAT: {d}"));
            }
            if let Some(v) = &outcome.dat_version {
                extra.push(format!("version: {v}"));
            }
            let suffix = if extra.is_empty() {
                String::new()
            } else {
                format!("  ({})", extra.join(", "))
            };
            info!("{name}: verified{algo} -> {game}{suffix}");
            if let Some(d) = &outcome.detail {
                info!("  {d}");
            }
        }
        DatVerdict::Hint => {
            let game = outcome.game_name.as_deref().unwrap_or("?");
            info!("{name}: not verified  (name+size hint only: \"{game}\")");
        }
        DatVerdict::Unsupported => {
            info!("{name}: unsupported (decompress the file first)");
        }
        DatVerdict::Failed => {
            info!("{name}: failed");
        }
        _ => {
            info!("{name}: no match");
        }
    }
}

/// Map a digest error to a bucket: unsupported inner formats and transport or
/// cancellation errors are distinguished from a plain per-file failure.
fn digest_bucket(e: DatError) -> DatResult<(DatVerdict, String)> {
    match e {
        DatError::Cancelled => Err(DatError::Cancelled),
        DatError::Transport(_) => Err(e),
        DatError::UnsupportedInnerHash { .. } => Ok((DatVerdict::Unsupported, e.to_string())),
        other => Ok((DatVerdict::Failed, other.to_string())),
    }
}

/// True when tiered checksum escalation is worth attempting for `unit`: a
/// second full read is cheap for a raw file or a cue set's raw bin members,
/// but not for a container whose first decode already dominates cost.
fn unit_is_tierable(unit: &DatUnit) -> bool {
    match unit {
        DatUnit::File(path) => rom_converto_lib::dat::is_raw_reread_cheap(path),
        DatUnit::CueSet(_) => true,
    }
}

/// Digest and resolve one unit's verify verdict, escalating past the cheap
/// floor tier to the full ceiling tier only when the floor tier alone does
/// not resolve a `DatVerdict::Verified` match. The hash cache makes any
/// escalation happen at most once per file: a later lookup at the floor
/// tier alone is already satisfied by the merged, escalated digest.
async fn digest_and_resolve_verify(
    client: &PlaymatchClient,
    unit: &DatUnit,
    algos: &[HashAlgo],
    bounds: &ChecksumBounds,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
    cache: &HashCache,
) -> DatResult<(RomDigests, DatOutcome)> {
    let (floor, escalation) = if unit_is_tierable(unit) {
        bounds.split(algos)
    } else {
        (algos.to_vec(), Vec::new())
    };
    let digests = digest_unit(unit, &floor, progress, cancel, cache).await?;
    let outcome = resolve_verify(client, unit, &digests, cancel).await?;
    if escalation.is_empty() || outcome.verdict != DatVerdict::Hint {
        return Ok((digests, outcome));
    }
    let full: Vec<HashAlgo> = floor.iter().chain(escalation.iter()).copied().collect();
    let digests = digest_unit(unit, &full, progress, cancel, cache).await?;
    let outcome = resolve_verify(client, unit, &digests, cancel).await?;
    Ok((digests, outcome))
}

/// `--quick` mode's zip-central-directory CRC shortcut for one unit. Only a
/// `DatUnit::File` qualifies; a cue set spans multiple bins, not a single
/// zip central directory. A hash cache hit already gives the real digest
/// for free, so the CRC probe only runs on a cache miss. The CRC-only
/// digest is never stored in the hash cache and is only returned when it
/// resolves an authoritative `Verified` match; anything weaker is
/// discarded here so the caller falls back to the normal full extraction
/// and hashing path.
async fn quick_verify(
    client: &PlaymatchClient,
    unit: &DatUnit,
    cache: &HashCache,
    cancel: &CancelToken,
) -> DatResult<Option<(RomDigests, DatOutcome)>> {
    let DatUnit::File(path) = unit else {
        return Ok(None);
    };
    if cache.lookup_decoded(path, &[HashAlgo::Crc32]).is_some() {
        return Ok(None);
    }
    let Some(QuickDigest {
        digests: crc_digest,
        member_name,
    }) = quick_crc_digest(path)
    else {
        return Ok(None);
    };
    // Search under the inner member's name, matching the full path, which
    // digests the extracted member rather than the archive.
    let search = GameFileMatchSearch::from_digests(&member_name, &crc_digest);
    let m = client.identify_relations(&search, cancel).await?;
    let outcome = outcome_from_single(&m, crc_digest.size_bytes);
    if outcome.verdict == DatVerdict::Verified {
        Ok(Some((RomDigests::Single(crc_digest), outcome)))
    } else {
        Ok(None)
    }
}

/// `--quick` mode composed with the existing tiered digest-and-verify path:
/// try the CRC-only shortcut first, falling back to the unmodified
/// escalation path when quick mode is off, the unit is not eligible, or the
/// CRC-only search does not verify.
#[allow(clippy::too_many_arguments)]
async fn digest_and_resolve_verify_quick(
    client: &PlaymatchClient,
    unit: &DatUnit,
    algos: &[HashAlgo],
    bounds: &ChecksumBounds,
    quick: bool,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
    cache: &HashCache,
) -> DatResult<(RomDigests, DatOutcome)> {
    if quick && let Some(hit) = quick_verify(client, unit, cache, cancel).await? {
        return Ok(hit);
    }
    digest_and_resolve_verify(client, unit, algos, bounds, progress, cancel, cache).await
}

#[allow(clippy::too_many_arguments)]
pub async fn dat_verify_single(
    progress: &dyn ProgressReporter,
    input: &Path,
    algos: &[HashAlgo],
    bounds: &ChecksumBounds,
    quick: bool,
    api_base: Option<&str>,
    report: Option<&Path>,
    cancel: &CancelToken,
    cache: &HashCache,
) -> Result<()> {
    let started = Instant::now();
    let client = PlaymatchClient::new(api_base);

    // Quick mode inspects the original path's zip central directory, so it
    // must run before resolve_input below would extract a member: extraction
    // is exactly the cost quick mode exists to skip.
    let quick_hit = if quick {
        let unit = DatUnit::File(input.to_path_buf());
        quick_verify(&client, &unit, cache, cancel).await?
    } else {
        None
    };

    let record = if let Some((_, outcome)) = quick_hit {
        print_verdict(input, &outcome);
        dat_record(input, &outcome, FileStatus::Ok, started)
    } else {
        let resolved_input = resolve_input(input, crate::ALL_IMAGE_EXTS)?;
        let unit = DatUnit::File(resolved_input.path().to_path_buf());
        let resolved =
            digest_and_resolve_verify(&client, &unit, algos, bounds, progress, cancel, cache).await;
        match resolved {
            Ok((_, outcome)) => {
                print_verdict(input, &outcome);
                dat_record(input, &outcome, FileStatus::Ok, started)
            }
            Err(e) => {
                let (verdict, msg) = digest_bucket(e)?;
                if verdict == DatVerdict::Unsupported {
                    warn!("{NX_DAT_UNSUPPORTED_HINT}");
                }
                let outcome = DatOutcome {
                    verdict,
                    match_algo: None,
                    game_name: None,
                    game_id: None,
                    platform: None,
                    signature_group: None,
                    dat_file_name: None,
                    dat_file_id: None,
                    dat_version: None,
                    detail: None,
                    size_bytes: 0,
                };
                print_verdict(input, &outcome);
                dat_error_record(
                    input,
                    verdict,
                    if verdict == DatVerdict::Failed {
                        FileStatus::Failed
                    } else {
                        FileStatus::Ok
                    },
                    Some(msg),
                    started,
                )
            }
        }
    };
    if let Some(path) = report {
        let records = [record];
        write_dat_report(
            path,
            &records,
            &dat_totals(&records, started.elapsed()),
            ReportFormat::from_path(path),
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn dat_verify_batch(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    algos: &[HashAlgo],
    bounds: &ChecksumBounds,
    quick: bool,
    max_depth: Option<usize>,
    api_base: Option<&str>,
    report: Option<&Path>,
    cancel: &CancelToken,
    cache: &HashCache,
) -> Result<()> {
    let units = dat_collect(input_dir, max_depth).await?;
    if units.is_empty() {
        warn!("No files found under {}", input_dir.display());
        return Ok(());
    }
    let client = PlaymatchClient::new(api_base);
    let total = units.len();
    total_progress.begin(total as u64, units_bytes(&units));
    let started = Instant::now();
    let mut records: Vec<DatReportRecord> = Vec::new();
    let mut dedup: HashMap<String, DatVerdict> = HashMap::new();
    let mut verified = 0usize;
    let mut hints = 0usize;
    let mut unsupported = 0usize;
    for unit in &units {
        let unit_started = Instant::now();
        let path = unit.display_path().to_path_buf();
        match digest_and_resolve_verify_quick(
            &client, unit, algos, bounds, quick, progress, cancel, cache,
        )
        .await
        {
            Ok((digests, outcome)) => {
                dedup_note(&mut dedup, &digests, outcome.verdict);
                match outcome.verdict {
                    DatVerdict::Verified => verified += 1,
                    DatVerdict::Hint => hints += 1,
                    _ => {}
                }
                print_verdict(&path, &outcome);
                records.push(dat_record(&path, &outcome, FileStatus::Ok, unit_started));
            }
            Err(e) => {
                let (verdict, msg) = digest_bucket(e)?;
                if verdict == DatVerdict::Unsupported {
                    unsupported += 1;
                }
                let status = if verdict == DatVerdict::Failed {
                    FileStatus::Failed
                } else {
                    FileStatus::Ok
                };
                info!("{}: {}", path.display(), verdict.as_str());
                records.push(dat_error_record(
                    &path,
                    verdict,
                    status,
                    Some(msg),
                    unit_started,
                ));
            }
        }
        total_progress.advance(unit_bytes(unit));
    }
    total_progress.finish();
    info!("{verified} verified, {hints} hint");
    if unsupported > 0 {
        warn!("{NX_DAT_UNSUPPORTED_HINT}");
    }
    if let Some(path) = report {
        write_dat_report(
            path,
            &records,
            &dat_totals(&records, started.elapsed()),
            ReportFormat::from_path(path),
        )?;
    }
    Ok(())
}

/// Record a unit's verdict in the per-run dedup cache keyed on its strongest
/// available digest, so an identical file need not be re-reported. The cache is
/// advisory in v1, kept for the API round-trip savings.
fn dedup_note(cache: &mut HashMap<String, DatVerdict>, digests: &RomDigests, verdict: DatVerdict) {
    let key = match digests {
        RomDigests::Single(d) => strongest_key(d),
        RomDigests::Tracks { whole, .. } => strongest_key(whole),
    };
    if let Some(k) = key {
        cache.entry(k).or_insert(verdict);
    }
}

fn strongest_key(d: &FileDigests) -> Option<String> {
    d.sha256
        .clone()
        .or_else(|| d.sha1.clone())
        .or_else(|| d.md5.clone())
        .or_else(|| d.crc32.clone())
}

async fn identify_one(
    client: &PlaymatchClient,
    file_name: &str,
    digests: &RomDigests,
    cancel: &CancelToken,
) -> DatResult<GameAndRelationMatchResult> {
    let d = match digests {
        RomDigests::Single(d) => d,
        RomDigests::Tracks { whole, .. } => whole,
    };
    let search = GameFileMatchSearch::from_digests(file_name, d);
    client.identify_relations(&search, cancel).await
}

pub async fn dat_identify(
    progress: &dyn ProgressReporter,
    input: &Path,
    algos: &[HashAlgo],
    bounds: &ChecksumBounds,
    api_base: Option<&str>,
    cancel: &CancelToken,
    cache: &HashCache,
) -> Result<()> {
    let client = PlaymatchClient::new(api_base);
    let unit = DatUnit::File(input.to_path_buf());
    let file_name = file_name_of(input);
    let (floor, escalation) = if unit_is_tierable(&unit) {
        bounds.split(algos)
    } else {
        (algos.to_vec(), Vec::new())
    };
    let digests = digest_unit(&unit, &floor, progress, cancel, cache).await?;
    let mut m = identify_one(&client, &file_name, &digests, cancel).await?;
    if !escalation.is_empty() && match_strength(m.game_match_type) == MatchStrength::NameSizeHint {
        let full: Vec<HashAlgo> = floor.iter().chain(escalation.iter()).copied().collect();
        let digests = digest_unit(&unit, &full, progress, cancel, cache).await?;
        m = identify_one(&client, &file_name, &digests, cancel).await?;
    }
    let strength = match_strength(m.game_match_type);
    match strength {
        MatchStrength::Verified(a) => info!("Match: {} (verified)", a.label().to_uppercase()),
        MatchStrength::NameSizeHint => info!("Match: name+size (weak)"),
        MatchStrength::NoMatch => {
            info!("No match");
            return Ok(());
        }
    }
    if let Some(g) = &m.game {
        let platform = m.platform.as_ref().map(|p| p.name.as_str()).unwrap_or("?");
        let group = m
            .signature_group
            .as_ref()
            .map(|g| g.name.as_str())
            .unwrap_or("?");
        info!(
            "Game:  {}      platform: {platform}   group: {group}",
            g.name
        );
    }
    if let Some(i) = &m.dat_file_import {
        info!("DAT:   version {}", i.version);
    }
    let ids: Vec<String> = m
        .external_metadata
        .iter()
        .filter(|e| matches!(e.match_type.as_str(), "Automatic" | "Manual"))
        .filter_map(|e| {
            e.provider_id
                .as_ref()
                .map(|id| format!("{} {id}", e.provider_name))
        })
        .collect();
    if !ids.is_empty() {
        info!("IDs:   {}", ids.join(", "));
    }
    Ok(())
}

/// One digested unit carried through scan/rename: a decoded result, an
/// unsupported format, or a per-file digest failure. Buckets a digest error
/// rather than aborting the batch (read-only semantics).
enum ScanUnit {
    Ok {
        unit_index: usize,
        digests: RomDigests,
        /// Digested via the `--quick` CRC-only shortcut rather than a full
        /// extraction and hash; if the first bulk lookup below does not
        /// verify it, the unit must be redone with a full digest.
        quick: bool,
    },
    Unsupported {
        unit_index: usize,
    },
    Failed {
        unit_index: usize,
        error: String,
    },
}

/// `--quick` mode's CRC-only digest for one scan unit: `None` when quick is
/// off, the unit is a cue set (no single zip central directory to trust),
/// the hash cache already holds a real decoded digest for it (the CRC probe
/// would just be extra work for the same answer), or the file does not
/// qualify (see [`quick_crc_digest`]).
fn quick_scan_digest(unit: &DatUnit, quick: bool, cache: &HashCache) -> Option<FileDigests> {
    if !quick {
        return None;
    }
    let DatUnit::File(path) = unit else {
        return None;
    };
    if cache.lookup_decoded(path, &[HashAlgo::Crc32]).is_some() {
        return None;
    }
    quick_crc_digest(path).map(|q| q.digests)
}

/// Digest every unit under `input_dir`, bucketing unsupported and failed units.
async fn digest_scan_units(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    units: &[DatUnit],
    algos: &[HashAlgo],
    quick: bool,
    cancel: &CancelToken,
    cache: &HashCache,
) -> DatResult<Vec<ScanUnit>> {
    total_progress.begin(units.len() as u64, units_bytes(units));
    let mut out = Vec::with_capacity(units.len());
    for (i, unit) in units.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(DatError::Cancelled);
        }
        let quick_digest = quick_scan_digest(unit, quick, cache);
        let is_quick = quick_digest.is_some();
        let result = match quick_digest {
            Some(d) => Ok(RomDigests::Single(d)),
            None => digest_unit(unit, algos, progress, cancel, cache).await,
        };
        match result {
            Ok(digests) => out.push(ScanUnit::Ok {
                unit_index: i,
                digests,
                quick: is_quick,
            }),
            Err(e) => match digest_bucket_dat(e)? {
                Some(msg) => out.push(ScanUnit::Failed {
                    unit_index: i,
                    error: msg,
                }),
                None => out.push(ScanUnit::Unsupported { unit_index: i }),
            },
        }
        total_progress.advance(unit_bytes(unit));
    }
    total_progress.finish();
    Ok(out)
}

/// Like `digest_bucket` but as a DatResult of Option: None means unsupported,
/// Some(msg) means a plain per-file failure; transport/cancel propagate.
fn digest_bucket_dat(e: DatError) -> DatResult<Option<String>> {
    match e {
        DatError::Cancelled => Err(DatError::Cancelled),
        DatError::Transport(_) => Err(e),
        DatError::UnsupportedInnerHash { .. } => Ok(None),
        other => Ok(Some(other.to_string())),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn dat_scan(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input_dir: &Path,
    max_depth: Option<usize>,
    algos: &[HashAlgo],
    quick: bool,
    api_base: Option<&str>,
    report: Option<&Path>,
    cancel: &CancelToken,
    cache: &HashCache,
) -> Result<()> {
    let units = dat_collect(input_dir, max_depth).await?;
    if units.is_empty() {
        warn!("No files found under {}", input_dir.display());
        return Ok(());
    }
    let started = Instant::now();
    let mut scanned = digest_scan_units(
        progress,
        total_progress,
        &units,
        algos,
        quick,
        cancel,
        cache,
    )
    .await?;

    let client = PlaymatchClient::new(api_base);
    progress.set_phase("Querying Playmatch");

    let mut items: Vec<BulkIdentifyItem> = Vec::new();
    let mut item_owner: Vec<usize> = Vec::new();
    for su in &scanned {
        if let ScanUnit::Ok {
            unit_index,
            digests,
            ..
        } = su
        {
            let name = file_name_of(units[*unit_index].display_path());
            items.push(BulkIdentifyItem {
                search: GameFileMatchSearch::from_digests(&name, primary_digests(digests)),
                key: None,
            });
            item_owner.push(*unit_index);
        }
    }

    let bulk = client.identify_bulk_ids(items, cancel).await?;

    // `--quick` mode: a CRC-only unit whose first bulk lookup did not verify
    // must be redone with a full digest so the match stays authoritative.
    // Redo is expected to be rare (quick mode is meant to resolve almost
    // everything), so a second small bulk pass over just the stragglers
    // keeps this one extra round trip rather than one per file.
    let mut redo_results: HashMap<usize, BulkIdentifyIdsResult> = HashMap::new();
    if quick {
        let mut redo_owner: Vec<usize> = Vec::new();
        let mut redo_items: Vec<BulkIdentifyItem> = Vec::new();
        for (item_pos, &unit_index) in item_owner.iter().enumerate() {
            if !matches!(&scanned[unit_index], ScanUnit::Ok { quick: true, .. }) {
                continue;
            }
            let verified = bulk.iter().find(|r| r.index == item_pos).is_some_and(|r| {
                r.status == BulkItemStatus::Ok
                    && r.matched
                        .as_ref()
                        .is_some_and(|m| match_strength(m.game_match_type).is_verified())
            });
            if verified {
                continue;
            }
            if cancel.is_cancelled() {
                return Err(DatError::Cancelled.into());
            }
            match digest_unit(&units[unit_index], algos, progress, cancel, cache).await {
                Ok(full_digests) => {
                    let name = file_name_of(units[unit_index].display_path());
                    redo_items.push(BulkIdentifyItem {
                        search: GameFileMatchSearch::from_digests(
                            &name,
                            primary_digests(&full_digests),
                        ),
                        key: None,
                    });
                    redo_owner.push(unit_index);
                    scanned[unit_index] = ScanUnit::Ok {
                        unit_index,
                        digests: full_digests,
                        quick: false,
                    };
                }
                Err(e) => {
                    scanned[unit_index] = match digest_bucket_dat(e)? {
                        Some(error) => ScanUnit::Failed { unit_index, error },
                        None => ScanUnit::Unsupported { unit_index },
                    };
                }
            }
        }
        if !redo_owner.is_empty() {
            let redo_bulk = client.identify_bulk_ids(redo_items, cancel).await?;
            for (i, &unit_index) in redo_owner.iter().enumerate() {
                if let Some(r) = redo_bulk.iter().find(|r| r.index == i) {
                    redo_results.insert(unit_index, r.clone());
                }
            }
        }
    }

    // Resolve canonical names for hash-verified matches via one games_bulk pass.
    let mut matched_ids: Vec<String> = bulk
        .iter()
        .filter(|r| r.status == BulkItemStatus::Ok)
        .filter_map(|r| r.matched.as_ref())
        .filter(|m| match_strength(m.game_match_type).is_verified())
        .filter_map(|m| m.id.clone())
        .collect();
    matched_ids.extend(
        redo_results
            .values()
            .filter(|r| r.status == BulkItemStatus::Ok)
            .filter_map(|r| r.matched.as_ref())
            .filter(|m| match_strength(m.game_match_type).is_verified())
            .filter_map(|m| m.id.clone()),
    );
    matched_ids.sort();
    matched_ids.dedup();
    let games = if matched_ids.is_empty() {
        Vec::new()
    } else {
        client.games_bulk(matched_ids, cancel).await?
    };
    let name_for_id = |id: &str| -> Option<String> {
        games
            .iter()
            .find(|g| g.id == id)
            .and_then(|g| g.data.as_ref())
            .map(|d| d.name.clone())
    };

    let mut records: Vec<DatReportRecord> = Vec::new();
    let mut counts = ScanCounts::default();
    for su in &scanned {
        let unit_index = match su {
            ScanUnit::Ok { unit_index, .. }
            | ScanUnit::Unsupported { unit_index }
            | ScanUnit::Failed { unit_index, .. } => *unit_index,
        };
        let path = units[unit_index].display_path();
        let record = match su {
            ScanUnit::Unsupported { .. } => {
                counts.unsupported += 1;
                dat_error_record(path, DatVerdict::Unsupported, FileStatus::Ok, None, started)
            }
            ScanUnit::Failed { error, .. } => {
                counts.failed += 1;
                dat_error_record(
                    path,
                    DatVerdict::Failed,
                    FileStatus::Failed,
                    Some(error.clone()),
                    started,
                )
            }
            ScanUnit::Ok { .. } => {
                let result = redo_results.get(&unit_index).or_else(|| {
                    let item_pos = item_owner.iter().position(|&u| u == unit_index);
                    item_pos.and_then(|p| bulk.iter().find(|r| r.index == p))
                });
                scan_record(path, result, &name_for_id, &mut counts, started)
            }
        };
        records.push(record);
    }

    info!(
        "{} matched, {} misnamed, {} hint, {} unknown, {} unsupported, {} failed",
        counts.matched,
        counts.misnamed,
        counts.hint,
        counts.unknown,
        counts.unsupported,
        counts.failed
    );
    if counts.unsupported > 0 {
        warn!("{NX_DAT_UNSUPPORTED_HINT}");
    }
    if let Some(path) = report {
        write_dat_report(
            path,
            &records,
            &dat_totals(&records, started.elapsed()),
            ReportFormat::from_path(path),
        )?;
    }
    Ok(())
}

fn algos_scan() -> &'static [HashAlgo] {
    &[HashAlgo::Crc32, HashAlgo::Sha1]
}

fn primary_digests(digests: &RomDigests) -> &FileDigests {
    match digests {
        RomDigests::Single(d) => d,
        RomDigests::Tracks { whole, .. } => whole,
    }
}

#[derive(Default)]
struct ScanCounts {
    matched: usize,
    misnamed: usize,
    hint: usize,
    unknown: usize,
    unsupported: usize,
    failed: usize,
}

/// Classify one bulk-ids result into a scan report row and bump the running
/// counts. A non-ok status is a failure (never dropped), NoMatch is unknown, a
/// FileNameAndSize match is a hint, and a hash-verified match is matched unless
/// the local stem differs from the canonical name (misnamed).
fn scan_record(
    path: &Path,
    result: Option<&BulkIdentifyIdsResult>,
    name_for_id: &impl Fn(&str) -> Option<String>,
    counts: &mut ScanCounts,
    started: Instant,
) -> DatReportRecord {
    let mut rec = dat_error_record(path, DatVerdict::Unknown, FileStatus::Ok, None, started);
    let Some(result) = result else {
        counts.failed += 1;
        rec.verdict = DatVerdict::Failed.as_str().to_string();
        rec.status = FileStatus::Failed;
        rec.error = Some("no result returned for this file".to_string());
        return rec;
    };
    if result.status != BulkItemStatus::Ok {
        counts.failed += 1;
        rec.verdict = DatVerdict::Failed.as_str().to_string();
        rec.status = FileStatus::Failed;
        rec.error = Some(
            result
                .error
                .as_ref()
                .map(|e| e.message.clone())
                .unwrap_or_else(|| "bulk identify item failed".to_string()),
        );
        return rec;
    }
    let Some(matched) = &result.matched else {
        counts.unknown += 1;
        return rec;
    };
    match match_strength(matched.game_match_type) {
        MatchStrength::NoMatch => {
            counts.unknown += 1;
            rec
        }
        MatchStrength::NameSizeHint => {
            counts.hint += 1;
            rec.verdict = DatVerdict::Hint.as_str().to_string();
            rec
        }
        MatchStrength::Verified(a) => {
            let game_name = matched.id.as_deref().and_then(name_for_id);
            let local_stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let misnamed = game_name
                .as_deref()
                .is_some_and(|n| !n.eq_ignore_ascii_case(local_stem));
            rec.game_name = game_name.clone();
            rec.game_id = matched.id.clone();
            rec.match_algo = Some(a.label().to_string());
            if misnamed {
                counts.misnamed += 1;
                rec.verdict = DatVerdict::Misnamed.as_str().to_string();
            } else {
                counts.matched += 1;
                rec.verdict = "matched".to_string();
            }
            rec
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn dat_rename(
    progress: &dyn ProgressReporter,
    total_progress: &crate::util::TotalProgress,
    input: &Path,
    recursive: bool,
    max_depth: Option<usize>,
    api_base: Option<&str>,
    policy: ConflictPolicy,
    dry_run: bool,
    report: Option<&Path>,
    cancel: &CancelToken,
    cache: &HashCache,
) -> Result<()> {
    // Group cue members so a set is one unit; renaming a member .bin in
    // isolation would leave the cue's FILE line dangling. Cue sets are recorded
    // as a single skipped row (member renaming with FILE-line rewrite is not
    // performed), so their bins and cue are left untouched on disk.
    let units: Vec<DatUnit> = if recursive {
        dat_collect(input, max_depth).await?
    } else {
        vec![DatUnit::File(input.to_path_buf())]
    };
    if units.is_empty() {
        warn!("No files found under {}", input.display());
        return Ok(());
    }
    let client = PlaymatchClient::new(api_base);
    let started = Instant::now();
    total_progress.begin(units.len() as u64, units_bytes(&units));

    let mut queryable: Vec<PathBuf> = Vec::new();
    let mut items: Vec<BulkIdentifyItem> = Vec::new();
    let mut records: Vec<DatReportRecord> = Vec::new();
    for unit in &units {
        if cancel.is_cancelled() {
            return Err(DatError::Cancelled.into());
        }
        let path = unit.display_path();
        if let DatUnit::CueSet(_) = unit {
            records.push(dat_error_record(
                path,
                DatVerdict::Skipped,
                FileStatus::Ok,
                Some("cue set: rename skipped to keep FILE lines consistent".to_string()),
                started,
            ));
            total_progress.advance(unit_bytes(unit));
            continue;
        }
        match digest_unit(unit, algos_scan(), progress, cancel, cache).await {
            Ok(digests) => {
                items.push(BulkIdentifyItem {
                    search: GameFileMatchSearch::from_digests(
                        &file_name_of(path),
                        primary_digests(&digests),
                    ),
                    key: None,
                });
                queryable.push(path.to_path_buf());
            }
            Err(e) => match digest_bucket_dat(e)? {
                None => records.push(dat_error_record(
                    path,
                    DatVerdict::Unsupported,
                    FileStatus::Ok,
                    None,
                    started,
                )),
                Some(msg) => records.push(dat_error_record(
                    path,
                    DatVerdict::Failed,
                    FileStatus::Failed,
                    Some(msg),
                    started,
                )),
            },
        }
        total_progress.advance(unit_bytes(unit));
    }
    total_progress.finish();

    progress.set_phase("Querying Playmatch");
    let bulk = client.identify_bulk_relations(items, cancel).await?;

    let candidates: Vec<RenameCandidate> = queryable
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let matched = bulk
                .iter()
                .find(|r| r.index == i)
                .filter(|r| r.status == BulkItemStatus::Ok)
                .and_then(|r| r.matched.as_ref());
            candidate_from_match(path, matched)
        })
        .collect();

    let plans = plan_renames(&candidates);
    let mut renamed = 0usize;
    let mut already = 0usize;
    let mut skipped = 0usize;
    for plan in &plans {
        records.push(execute_rename(
            plan,
            dry_run,
            policy,
            &mut renamed,
            &mut already,
            &mut skipped,
            started,
        ));
    }

    info!("{renamed} renamed, {already} already canonical, {skipped} skipped");
    if let Some(path) = report {
        write_dat_report(
            path,
            &records,
            &dat_totals(&records, started.elapsed()),
            ReportFormat::from_path(path),
        )?;
    }
    Ok(())
}

/// Build a rename candidate from a file's relations match. `verified` is set
/// only for a hash-rung match (hints never rename); the file-level canonical
/// name is taken from the sole matching gameFiles entry when present.
fn candidate_from_match(
    path: &Path,
    matched: Option<&GameAndRelationMatchResult>,
) -> RenameCandidate {
    let Some(m) = matched else {
        return RenameCandidate {
            path: path.to_path_buf(),
            game_id: None,
            game_name: None,
            file_name: None,
            verified: false,
        };
    };
    RenameCandidate {
        path: path.to_path_buf(),
        game_id: m.game.as_ref().map(|g| g.id.clone()),
        game_name: m.game.as_ref().map(|g| g.name.clone()),
        file_name: if m.game_files.len() == 1 {
            Some(m.game_files[0].file_name.clone())
        } else {
            None
        },
        verified: match_strength(m.game_match_type).is_verified(),
    }
}

/// Execute one rename plan (or preview it under dry-run), record the row, and
/// bump the running counts. std::fs::rename replaces an existing destination on
/// Windows, so an overwrite decision needs no separate delete.
#[allow(clippy::too_many_arguments)]
fn execute_rename(
    plan: &RenamePlan,
    dry_run: bool,
    policy: ConflictPolicy,
    renamed: &mut usize,
    already: &mut usize,
    skipped: &mut usize,
    started: Instant,
) -> DatReportRecord {
    let mut rec = dat_error_record(
        &plan.from,
        DatVerdict::Skipped,
        FileStatus::Ok,
        None,
        started,
    );
    rec.detail = plan.detail.clone();
    match plan.action {
        RenameAction::AlreadyCanonical => {
            *already += 1;
            rec.verdict = DatVerdict::Skipped.as_str().to_string();
            rec.detail = Some("already canonical".to_string());
        }
        RenameAction::SkipUnmatched
        | RenameAction::SkipWeakMatch
        | RenameAction::SkipCollision
        | RenameAction::SkipDiscSetConflict => {
            *skipped += 1;
        }
        RenameAction::Rename => {
            let Some(target) = &plan.to else {
                *skipped += 1;
                rec.verdict = DatVerdict::Failed.as_str().to_string();
                rec.status = FileStatus::Failed;
                rec.error = Some("rename plan missing target".to_string());
                return rec;
            };
            if dry_run {
                *renamed += 1;
                info!(
                    "Would rename {} -> {}",
                    plan.from.display(),
                    target.display()
                );
                rec.verdict = DatVerdict::Renamed.as_str().to_string();
                rec.detail = Some(target.display().to_string());
                return rec;
            }
            match resolve_conflict(target, policy) {
                Ok(ConflictResolution::Skip) => {
                    *skipped += 1;
                    rec.detail = Some(format!("target exists: {}", target.display()));
                }
                Ok(ConflictResolution::Write(dest)) => match std::fs::rename(&plan.from, &dest) {
                    Ok(()) => {
                        *renamed += 1;
                        info!("{} -> {}", plan.from.display(), dest.display());
                        rec.verdict = DatVerdict::Renamed.as_str().to_string();
                        rec.detail = Some(dest.display().to_string());
                    }
                    Err(e) => {
                        *skipped += 1;
                        rec.verdict = DatVerdict::Failed.as_str().to_string();
                        rec.status = FileStatus::Failed;
                        rec.error = Some(e.to_string());
                    }
                },
                Err(e) => {
                    *skipped += 1;
                    rec.verdict = DatVerdict::Failed.as_str().to_string();
                    rec.status = FileStatus::Failed;
                    rec.error = Some(e.to_string());
                }
            }
        }
    }
    rec
}

pub async fn dat_fixdat(
    progress: &dyn ProgressReporter,
    args: &DatFixdatArgs,
    dry_run: bool,
    policy: ConflictPolicy,
    cancel: &CancelToken,
    cache: &HashCache,
) -> Result<()> {
    let client = PlaymatchClient::new(args.api_base.as_deref());
    let dat = resolve_fixdat_dat(&client, args, cancel).await?;
    info!("Using DAT {} (version {})", dat.name, dat.current_version);

    let games = client
        .dat_file_games(&dat.id, true, progress, cancel)
        .await?;
    let total_games = games.len();

    let units = dat_collect(&args.input, args.max_depth).await?;
    total_progress_hash(progress, units.len());
    let mut index = LocalHashIndex::default();
    for unit in &units {
        if cancel.is_cancelled() {
            return Err(DatError::Cancelled.into());
        }
        match digest_unit(unit, algos_index(), progress, cancel, cache).await {
            Ok(RomDigests::Single(d)) => index.insert(&d),
            Ok(RomDigests::Tracks { tracks, .. }) => index.insert_tracks(&tracks),
            Err(e) => match digest_bucket_dat(e)? {
                None => {}
                Some(msg) => warn!("Skipping {}: {msg}", unit.display_path().display()),
            },
        }
    }

    let entries = diff_library(&games, &index);
    let missing_files: usize = entries.iter().map(|e| e.missing.len()).sum();

    if dry_run {
        info!(
            "Dry run: missing {} of {} games ({} files); would write {}",
            entries.len(),
            total_games,
            missing_files,
            args.output.display()
        );
        return Ok(());
    }

    let out_path = match resolve_conflict(&args.output, policy)? {
        ConflictResolution::Skip => {
            info!("Skipped, output exists: {}", args.output.display());
            return Ok(());
        }
        ConflictResolution::Write(p) => p,
    };
    let file = std::fs::File::create(&out_path)?;
    let mut w = std::io::BufWriter::new(file);
    write_fixdat_xml(&mut w, &dat, &entries)?;
    use std::io::Write;
    w.flush()?;
    info!(
        "Missing {} of {} games ({} files); wrote {}",
        entries.len(),
        total_games,
        missing_files,
        out_path.display()
    );
    Ok(())
}

fn algos_index() -> &'static [HashAlgo] {
    &[
        HashAlgo::Crc32,
        HashAlgo::Sha1,
        HashAlgo::Md5,
        HashAlgo::Sha256,
    ]
}

fn total_progress_hash(progress: &dyn ProgressReporter, count: usize) {
    progress.set_phase(&format!("Hashing {count} files"));
}

/// Resolve the one DAT to diff against: an explicit `--dat-id` is found by
/// scanning the DAT list; otherwise the platform is resolved by name and its
/// DATs are filtered by name/subset. An ambiguous result lists candidates and
/// bails so the user can narrow it.
async fn resolve_fixdat_dat(
    client: &PlaymatchClient,
    args: &DatFixdatArgs,
    cancel: &CancelToken,
) -> Result<DatFileSummary> {
    if let Some(id) = &args.dat_id {
        let all = client
            .list_dat_files(&DatFileFilter::default(), cancel)
            .await?;
        return all
            .into_iter()
            .find(|d| &d.id == id)
            .ok_or_else(|| anyhow::anyhow!("no DAT with id {id}"));
    }

    let platform_name = args
        .platform
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("either --platform or --dat-id is required"))?;
    let platforms = client.platforms_search(platform_name, cancel).await?;
    let platform = platforms
        .into_iter()
        .find(|p| p.name.eq_ignore_ascii_case(platform_name))
        .ok_or_else(|| anyhow::anyhow!("no platform matching \"{platform_name}\""))?;

    let filter = DatFileFilter {
        platform_id: Some(platform.id.clone()),
        name: args.dat_name.clone(),
        subset: args.subset.clone(),
        ..DatFileFilter::default()
    };
    let mut candidates = client.list_dat_files(&filter, cancel).await?;
    match candidates.len() {
        0 => Err(anyhow::anyhow!(
            "no DAT found for platform \"{platform_name}\" with the given filters"
        )),
        1 => Ok(candidates.remove(0)),
        _ => {
            warn!("Multiple DATs match; narrow with --dat-name or --subset:");
            for d in &candidates {
                let subset = d.subset.as_deref().unwrap_or("-");
                info!(
                    "  {}  {}  subset={subset}  version={}",
                    d.id, d.name, d.current_version
                );
            }
            Err(anyhow::anyhow!("ambiguous DAT selection"))
        }
    }
}
