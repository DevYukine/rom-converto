use crate::util::{WriteDecision, resolve_output};
use anyhow::Result;
use log::{info, warn};
use rom_converto_lib::util::fs::{collect_all_files, collect_files_with_exts};
use rom_converto_lib::util::{
    ConflictPolicy, FileDigests, FileStatus, HashAlgo, HashReportRecord, ProgressReporter,
    ReportFormat, ReportRecord, ReportTotals, Tally, TallyDirection, hash_file, write_hash_report,
    write_report,
};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
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
    let direction = if dry_run { TallyDirection::DryRun } else { direction };
    info!("{}", tally.summary_line(direction));
    // Write the report before the failed-count bail so failed-only runs still
    // leave a report on disk even though the command exits with an error.
    if let Some(path) = report_path {
        write_report(path, records, &totals_from(tally), ReportFormat::from_path(path))?;
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
    total_progress: &dyn ProgressReporter,
    input_dir: &Path,
    policy: ConflictPolicy,
    output_dir: Option<&Path>,
    output_template: Option<&str>,
    max_depth: Option<usize>,
    dry_run: bool,
    report_path: Option<&Path>,
) -> Result<()> {
    use rom_converto_lib::cso::decompress_from_cso;

    let files = collect_or_warn(input_dir, &["cso", "zso"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if !dry_run && let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }
    let total = files.len();
    total_progress.start(total as u64, &format!("Decompressing {total} files..."));
    let mut tally = Tally::new();
    let mut records: Vec<ReportRecord> = Vec::new();
    for path in files {
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
                total_progress.inc(1);
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
                total_progress.inc(1);
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
            total_progress.inc(1);
            continue;
        }
        let output = match decision {
            WriteDecision::Write(p) => p,
            WriteDecision::Skip => {
                info!("skipped, output exists: {}", output.display());
                tally.record_skipped();
                records.push(skipped_record(&path, "decompress", None));
                total_progress.inc(1);
                continue;
            }
        };
        let input_bytes = file_len(&path);
        let out_path = output.clone();
        let started = Instant::now();
        if let Err(e) = decompress_from_cso(progress, path.clone(), output, true).await {
            warn!("Failed to decompress {}: {e}", path.display());
            tally.record_failed();
            records.push(failed_record(&path, "decompress", input_bytes, started, e));
        } else {
            let out_bytes = file_len(&out_path);
            tally.record_ok(input_bytes, out_bytes, started.elapsed());
            records.push(ok_record(
                &path, &out_path, "decompress", input_bytes, out_bytes, started,
            ));
        }
        total_progress.inc(1);
    }
    total_progress.finish();
    finish_tally(&tally, TallyDirection::Decompress, &records, dry_run, report_path)
}

pub async fn cso_verify(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &Path,
    full: bool,
    max_depth: Option<usize>,
) -> Result<()> {
    use rom_converto_lib::cso::verify_cso;

    let files = collect_or_warn(input_dir, &["cso", "zso"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    let total = files.len();
    total_progress.start(total as u64, &format!("Verifying {total} files..."));
    let mut ok = 0usize;
    let mut failed = 0usize;
    for path in files {
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
        total_progress.inc(1);
    }
    total_progress.finish();
    finish_verify(VerifyTally { total, ok, failed })
}

#[allow(clippy::too_many_arguments)]
pub async fn rvz_compress(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &Path,
    exts: &[&str],
    opts: rom_converto_lib::nintendo::rvz::RvzCompressOptions,
    policy: ConflictPolicy,
    output_dir: Option<&Path>,
    output_template: Option<&str>,
    max_depth: Option<usize>,
    dry_run: bool,
    report_path: Option<&Path>,
) -> Result<()> {
    use rom_converto_lib::nintendo::rvz::{compress_disc, derive_rvz_path};

    let files = collect_or_warn(input_dir, exts, max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if !dry_run && let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }
    let total = files.len();
    total_progress.start(total as u64, &format!("Compressing {total} files..."));
    let mut tally = Tally::new();
    let mut records: Vec<ReportRecord> = Vec::new();
    for path in files {
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
                total_progress.inc(1);
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
                total_progress.inc(1);
                continue;
            }
        };
        if dry_run {
            crate::dry_run::log_plan("compress", &path, &output, &decision, Some("RVZ"), None);
            crate::dry_run::record(&mut tally, &path, &decision);
            records.push(crate::dry_run::report_record(
                "compress", &path, &output, &decision,
            ));
            total_progress.inc(1);
            continue;
        }
        let output = match decision {
            WriteDecision::Write(p) => p,
            WriteDecision::Skip => {
                info!("skipped, output exists: {}", output.display());
                tally.record_skipped();
                records.push(skipped_record(&path, "compress", None));
                total_progress.inc(1);
                continue;
            }
        };
        let input_bytes = file_len(&path);
        let started = Instant::now();
        if let Err(e) = compress_disc(&path, &output, opts, progress).await {
            warn!("Failed to compress {}: {e}", path.display());
            tally.record_failed();
            records.push(failed_record(&path, "compress", input_bytes, started, e));
        } else {
            let out_bytes = file_len(&output);
            tally.record_ok(input_bytes, out_bytes, started.elapsed());
            records.push(ok_record(
                &path, &output, "compress", input_bytes, out_bytes, started,
            ));
        }
        total_progress.inc(1);
    }
    total_progress.finish();
    finish_tally(&tally, TallyDirection::Compress, &records, dry_run, report_path)
}

#[allow(clippy::too_many_arguments)]
pub async fn rvz_decompress(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &Path,
    policy: ConflictPolicy,
    output_dir: Option<&Path>,
    output_template: Option<&str>,
    max_depth: Option<usize>,
    dry_run: bool,
    report_path: Option<&Path>,
) -> Result<()> {
    use rom_converto_lib::nintendo::rvz::{decompress_disc, derive_disc_path};

    let files = collect_or_warn(input_dir, &["rvz"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if !dry_run && let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }
    let total = files.len();
    total_progress.start(total as u64, &format!("Decompressing {total} files..."));
    let mut tally = Tally::new();
    let mut records: Vec<ReportRecord> = Vec::new();
    for path in files {
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
                total_progress.inc(1);
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
                total_progress.inc(1);
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
            total_progress.inc(1);
            continue;
        }
        let output = match decision {
            WriteDecision::Write(p) => p,
            WriteDecision::Skip => {
                info!("skipped, output exists: {}", output.display());
                tally.record_skipped();
                records.push(skipped_record(&path, "decompress", None));
                total_progress.inc(1);
                continue;
            }
        };
        let input_bytes = file_len(&path);
        let started = Instant::now();
        if let Err(e) = decompress_disc(&path, &output, progress).await {
            warn!("Failed to decompress {}: {e}", path.display());
            tally.record_failed();
            records.push(failed_record(&path, "decompress", input_bytes, started, e));
        } else {
            let out_bytes = file_len(&output);
            tally.record_ok(input_bytes, out_bytes, started.elapsed());
            records.push(ok_record(
                &path, &output, "decompress", input_bytes, out_bytes, started,
            ));
        }
        total_progress.inc(1);
    }
    total_progress.finish();
    finish_tally(&tally, TallyDirection::Decompress, &records, dry_run, report_path)
}

pub async fn dol_verify(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
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
    total_progress.start(total as u64, &format!("Verifying {total} files..."));
    let mut ok = 0usize;
    let mut failed = 0usize;
    for path in files {
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
        total_progress.inc(1);
    }
    total_progress.finish();
    finish_verify(VerifyTally { total, ok, failed })
}

pub async fn rvl_verify(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
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
    total_progress.start(total as u64, &format!("Verifying {total} files..."));
    let mut ok = 0usize;
    let mut failed = 0usize;
    for path in files {
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
        total_progress.inc(1);
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
    pub report: Option<PathBuf>,
}

pub async fn nx_compress(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &Path,
    keys: rom_converto_lib::nintendo::nx::KeySet,
    tuning: NxCompressTuning,
) -> Result<()> {
    use rom_converto_lib::nintendo::nx::{
        NczMode, NxCompressOptions, compress_container_async, derive_compressed_path,
        detect_container,
    };

    let dry_run = tuning.dry_run;
    let files = collect_or_warn(input_dir, &["nsp", "xci"], tuning.max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if !dry_run && let Some(dir) = tuning.output_dir.as_deref() {
        std::fs::create_dir_all(dir)?;
    }
    let total = files.len();
    total_progress.start(total as u64, &format!("Compressing {total} files..."));
    let mut tally = Tally::new();
    let mut records: Vec<ReportRecord> = Vec::new();
    for path in files {
        let kind = match detect_container(&path) {
            Ok(kind) => kind,
            Err(e) => {
                warn!("Failed to compress {}: {e}", path.display());
                tally.record_failed();
                records.push(failed_record(&path, "compress", file_len(&path), Instant::now(), e));
                total_progress.inc(1);
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
                total_progress.inc(1);
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
                total_progress.inc(1);
                continue;
            }
        };
        if dry_run {
            let media = format!("{kind:?}");
            crate::dry_run::log_plan("compress", &path, &output, &decision, Some(&media), None);
            crate::dry_run::record(&mut tally, &path, &decision);
            records.push(crate::dry_run::report_record(
                "compress", &path, &output, &decision,
            ));
            total_progress.inc(1);
            continue;
        }
        let output = match decision {
            WriteDecision::Write(p) => p,
            WriteDecision::Skip => {
                info!("skipped, output exists: {}", output.display());
                tally.record_skipped();
                records.push(skipped_record(&path, "compress", None));
                total_progress.inc(1);
                continue;
            }
        };
        let input_bytes = file_len(&path);
        let out_path = output.clone();
        let started = Instant::now();
        if let Err(e) =
            compress_container_async(path.clone(), output, opts, keys.clone(), progress).await
        {
            warn!("Failed to compress {}: {e}", path.display());
            tally.record_failed();
            records.push(failed_record(&path, "compress", input_bytes, started, e));
        } else {
            let out_bytes = file_len(&out_path);
            tally.record_ok(input_bytes, out_bytes, started.elapsed());
            records.push(ok_record(
                &path, &out_path, "compress", input_bytes, out_bytes, started,
            ));
        }
        total_progress.inc(1);
    }
    total_progress.finish();
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
    total_progress: &dyn ProgressReporter,
    input_dir: &Path,
    keys: rom_converto_lib::nintendo::nx::KeySet,
    policy: ConflictPolicy,
    output_dir: Option<&Path>,
    output_template: Option<&str>,
    max_depth: Option<usize>,
    dry_run: bool,
    report_path: Option<&Path>,
) -> Result<()> {
    use rom_converto_lib::nintendo::nx::{decompress_container_async, derive_decompressed_path};

    let files = collect_or_warn(input_dir, &["nsz", "xcz"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if !dry_run && let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }
    let total = files.len();
    total_progress.start(total as u64, &format!("Decompressing {total} files..."));
    let mut tally = Tally::new();
    let mut records: Vec<ReportRecord> = Vec::new();
    for path in files {
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
                total_progress.inc(1);
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
                total_progress.inc(1);
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
            total_progress.inc(1);
            continue;
        }
        let output = match decision {
            WriteDecision::Write(p) => p,
            WriteDecision::Skip => {
                info!("skipped, output exists: {}", output.display());
                tally.record_skipped();
                records.push(skipped_record(&path, "decompress", None));
                total_progress.inc(1);
                continue;
            }
        };
        let input_bytes = file_len(&path);
        let out_path = output.clone();
        let started = Instant::now();
        if let Err(e) =
            decompress_container_async(path.clone(), output, keys.clone(), progress).await
        {
            warn!("Failed to decompress {}: {e}", path.display());
            tally.record_failed();
            records.push(failed_record(&path, "decompress", input_bytes, started, e));
        } else {
            let out_bytes = file_len(&out_path);
            tally.record_ok(input_bytes, out_bytes, started.elapsed());
            records.push(ok_record(
                &path, &out_path, "decompress", input_bytes, out_bytes, started,
            ));
        }
        total_progress.inc(1);
    }
    total_progress.finish();
    finish_tally(&tally, TallyDirection::Decompress, &records, dry_run, report_path)
}

pub async fn nx_verify(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
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
    total_progress.start(total as u64, &format!("Verifying {total} files..."));
    let mut ok = 0usize;
    let mut failed = 0usize;
    for path in files {
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
        total_progress.inc(1);
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
    total_progress: &dyn ProgressReporter,
    input_dir: &Path,
    max_depth: Option<usize>,
) -> Result<()> {
    use rom_converto_lib::nintendo::wup::verify_wup_async;

    let mut inputs = collect_files_with_exts(input_dir, &["wud", "wux"], max_depth)?;
    if let Ok(entries) = std::fs::read_dir(input_dir) {
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && is_nus_title_dir(p))
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
    total_progress.start(total as u64, &format!("Verifying {total} titles..."));
    let mut ok = 0usize;
    let mut failed = 0usize;
    for path in inputs {
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
        total_progress.inc(1);
    }
    total_progress.finish();
    finish_verify(VerifyTally { total, ok, failed })
}

#[allow(clippy::too_many_arguments)]
pub async fn chd_compress(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &Path,
    mut opts: rom_converto_lib::chd::ChdDvdOptions,
    mode: Option<rom_converto_lib::chd::DiscMode>,
    policy: ConflictPolicy,
    output_dir: Option<&Path>,
    output_template: Option<&str>,
    max_depth: Option<usize>,
    dry_run: bool,
    report_path: Option<&Path>,
) -> Result<()> {
    use rom_converto_lib::chd::convert_disc_to_chd;

    let files = collect_or_warn(input_dir, &["cue", "iso"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if !dry_run && let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }
    opts.force = true;
    let total = files.len();
    total_progress.start(total as u64, &format!("Compressing {total} files..."));
    let mut tally = Tally::new();
    let mut records: Vec<ReportRecord> = Vec::new();
    for path in files {
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
                total_progress.inc(1);
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
                total_progress.inc(1);
                continue;
            }
        };
        if dry_run {
            let media = crate::chd_media_label(&path);
            crate::dry_run::log_plan(
                "compress",
                &path,
                &output,
                &decision,
                media.as_deref(),
                None,
            );
            crate::dry_run::record(&mut tally, &path, &decision);
            records.push(crate::dry_run::report_record(
                "compress", &path, &output, &decision,
            ));
            total_progress.inc(1);
            continue;
        }
        let output = match decision {
            WriteDecision::Write(p) => p,
            WriteDecision::Skip => {
                info!("skipped, output exists: {}", output.display());
                tally.record_skipped();
                records.push(skipped_record(&path, "compress", None));
                total_progress.inc(1);
                continue;
            }
        };
        let input_bytes = file_len(&path);
        let out_path = output.clone();
        let started = Instant::now();
        if let Err(e) = convert_disc_to_chd(progress, path.clone(), output, mode, opts.clone()).await {
            warn!("Failed to compress {}: {e}", path.display());
            tally.record_failed();
            records.push(failed_record(&path, "compress", input_bytes, started, e));
        } else {
            let out_bytes = file_len(&out_path);
            tally.record_ok(input_bytes, out_bytes, started.elapsed());
            records.push(ok_record(
                &path, &out_path, "compress", input_bytes, out_bytes, started,
            ));
        }
        total_progress.inc(1);
    }
    total_progress.finish();
    finish_tally(&tally, TallyDirection::Compress, &records, dry_run, report_path)
}

#[allow(clippy::too_many_arguments)]
pub async fn chd_extract(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &Path,
    parent: Option<PathBuf>,
    policy: ConflictPolicy,
    output_dir: Option<&Path>,
    output_template: Option<&str>,
    max_depth: Option<usize>,
    dry_run: bool,
    report_path: Option<&Path>,
) -> Result<()> {
    use rom_converto_lib::chd::extract_from_chd;

    let files = collect_or_warn(input_dir, &["chd"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if !dry_run && let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }
    let total = files.len();
    total_progress.start(total as u64, &format!("Extracting {total} files..."));
    let mut tally = Tally::new();
    let mut records: Vec<ReportRecord> = Vec::new();
    for path in files {
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
                total_progress.inc(1);
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
                total_progress.inc(1);
                continue;
            }
        };
        if dry_run {
            crate::dry_run::log_plan("extract", &path, &output, &decision, None, None);
            crate::dry_run::record(&mut tally, &path, &decision);
            records.push(crate::dry_run::report_record(
                "extract", &path, &output, &decision,
            ));
            total_progress.inc(1);
            continue;
        }
        let output = match decision {
            WriteDecision::Write(p) => p,
            WriteDecision::Skip => {
                info!("skipped, output exists: {}", output.display());
                tally.record_skipped();
                records.push(skipped_record(&path, "extract", None));
                total_progress.inc(1);
                continue;
            }
        };
        let out_path = output.clone();
        let started = Instant::now();
        if let Err(e) = extract_from_chd(progress, path.clone(), output, parent.clone()).await {
            warn!("Failed to extract {}: {e}", path.display());
            tally.record_failed();
            records.push(failed_record(&path, "extract", 0, started, e));
        } else {
            tally.record_ok(0, 0, started.elapsed());
            records.push(ok_record(&path, &out_path, "extract", 0, 0, started));
        }
        total_progress.inc(1);
    }
    total_progress.finish();
    finish_tally(&tally, TallyDirection::CountOnly, &records, dry_run, report_path)
}

#[allow(clippy::too_many_arguments)]
pub async fn cso_compress(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &Path,
    mut opts: rom_converto_lib::cso::CsoCompressOptions,
    policy: ConflictPolicy,
    output_dir: Option<&Path>,
    output_template: Option<&str>,
    max_depth: Option<usize>,
    dry_run: bool,
    report_path: Option<&Path>,
) -> Result<()> {
    use rom_converto_lib::cso::{CsoFormat, compress_to_cso};

    let ext = opts.format.extension();
    let media = match opts.format {
        CsoFormat::Cso => "CSO",
        CsoFormat::Zso => "ZSO",
    };
    let files = collect_or_warn(input_dir, &["iso"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if !dry_run && let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }
    opts.force = true;
    let total = files.len();
    total_progress.start(total as u64, &format!("Compressing {total} files..."));
    let mut tally = Tally::new();
    let mut records: Vec<ReportRecord> = Vec::new();
    for path in files {
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
                total_progress.inc(1);
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
                total_progress.inc(1);
                continue;
            }
        };
        if dry_run {
            crate::dry_run::log_plan("compress", &path, &output, &decision, Some(media), None);
            crate::dry_run::record(&mut tally, &path, &decision);
            records.push(crate::dry_run::report_record(
                "compress", &path, &output, &decision,
            ));
            total_progress.inc(1);
            continue;
        }
        let output = match decision {
            WriteDecision::Write(p) => p,
            WriteDecision::Skip => {
                info!("skipped, output exists: {}", output.display());
                tally.record_skipped();
                records.push(skipped_record(&path, "compress", None));
                total_progress.inc(1);
                continue;
            }
        };
        let input_bytes = file_len(&path);
        let out_path = output.clone();
        let started = Instant::now();
        if let Err(e) = compress_to_cso(progress, path.clone(), output, opts.clone()).await {
            warn!("Failed to compress {}: {e}", path.display());
            tally.record_failed();
            records.push(failed_record(&path, "compress", input_bytes, started, e));
        } else {
            let out_bytes = file_len(&out_path);
            tally.record_ok(input_bytes, out_bytes, started.elapsed());
            records.push(ok_record(
                &path, &out_path, "compress", input_bytes, out_bytes, started,
            ));
        }
        total_progress.inc(1);
    }
    total_progress.finish();
    finish_tally(&tally, TallyDirection::Compress, &records, dry_run, report_path)
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
    total_progress: &dyn ProgressReporter,
    input_dir: &Path,
    algos: &[HashAlgo],
    max_depth: Option<usize>,
    report_path: Option<&Path>,
) -> Result<()> {
    let files = collect_all_files(input_dir, max_depth)?;
    if files.is_empty() {
        warn!("No files found in {}", input_dir.display());
        return Ok(());
    }
    let total = files.len();
    total_progress.start(total as u64, &format!("Hashing {total} files..."));
    let mut tally = Tally::new();
    let mut records: Vec<HashReportRecord> = Vec::new();
    for path in files {
        let started = Instant::now();
        match hash_file(&path, algos, progress) {
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
        total_progress.inc(1);
    }
    total_progress.finish();
    info!("{}", tally.summary_line(TallyDirection::CountOnly));
    // Hashing is read-only diagnostics, so a per-file read failure must not
    // abort the run: it is recorded and we move on. The report is still
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
