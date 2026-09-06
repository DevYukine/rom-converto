//! Request parsing for the `dat.*` operations: each handler reads its
//! options off the request and drives the matching, rename and fixdat
//! workflows in [`crate::dat`].

use super::models::{
    DatMatchData, DatRenameData, DatRenameRowData, DatScanData, FixdatPlanData, FixdatWrittenData,
    RunData, RunRequest, RunResponse, RunStatus,
};
use super::ops::{conflict_policy, elapsed_ms, required_input, skipped, totals_for_records};
use super::{RUN_SCHEMA, invalid_arg, is_cancelled_error};
use crate::dat::fixdat::{LocalHashIndex, diff_library, write_fixdat_xml};
use crate::dat::rename::{RenameAction, plan_renames, rename_transaction};
use crate::dat::run::{
    error_report_record, match_data, match_file, match_tiered, rename_candidate, report_record,
};
use crate::dat::verdict::DatVerdict;
use crate::dat::{PlaymatchClient, RomDigests};
use crate::util::fs::file_len;
use crate::util::report::{DatReportRecord, write_dat_report};
use crate::util::{
    CancelToken, Cancelled, ChecksumBounds, ConflictPolicy, ConflictResolution, FileStatus,
    HashAlgo, ProgressReporter, ReportFormat, ReportRecord, ReportRecordInput, ReportTotals,
    parse_algos, parse_checksum_bound, resolve_conflict,
};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub(crate) async fn dat_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let algos = dat_algos(&req, "crc32,sha1")?;
    let bounds = dat_checksum_bounds(&req, &algos)?;
    let matched = match_tiered(
        &input,
        &algos,
        &bounds,
        progress,
        cancel.clone(),
        req.options.api_base.as_deref(),
    )
    .await?;
    let data = match_data("verify", &input, &matched);
    let record = dat_run_record(&input, &data.verdict, None);
    dat_write_report(&req, &[report_record(&input, &matched, None)], &cancel)?;
    Ok(
        RunResponse::ok("DAT verification complete.", Some(RunData::DatMatch(data)))
            .with_record(record),
    )
}

pub(crate) async fn dat_identify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let algos = dat_algos(&req, "crc32,sha1")?;
    let bounds = dat_checksum_bounds(&req, &algos)?;
    let matched = match_tiered(
        &input,
        &algos,
        &bounds,
        progress,
        cancel,
        req.options.api_base.as_deref(),
    )
    .await?;
    Ok(RunResponse::ok(
        "DAT identify complete.",
        Some(RunData::DatMatch(match_data("identify", &input, &matched))),
    ))
}

pub(crate) async fn dat_scan(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    if !input.is_dir() {
        return Err(invalid_arg(format!(
            "dat.scan input must be a directory: {}",
            input.display()
        )));
    }
    let algos = dat_algos(&req, "crc32")?;
    let files = crate::util::fs::collect_all_files(&input, req.options.max_depth, &cancel)?;
    let started = Instant::now();
    let mut rows = Vec::new();
    let mut records = Vec::new();
    let mut dat_records = Vec::new();
    for file in files {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        match match_file(
            &file,
            &algos,
            progress,
            cancel.clone(),
            req.options.api_base.as_deref(),
        )
        .await
        {
            Ok(matched) => {
                let row = match_data("scan", &file, &matched);
                records.push(dat_run_record(&file, &row.verdict, None));
                dat_records.push(report_record(&file, &matched, None));
                rows.push(row);
            }
            Err(err) => {
                if cancel.is_cancelled() || is_cancelled_error(&err) {
                    return Err(err);
                }
                let error = err.to_string();
                records.push(dat_run_record(&file, "failed", Some(error.clone())));
                dat_records.push(error_report_record(&file, error.clone()));
                rows.push(DatMatchData {
                    kind: "scan",
                    path: file,
                    verdict: DatVerdict::Failed.as_str().to_string(),
                    match_algo: None,
                    game_name: None,
                    platform: None,
                    signature_group: None,
                    dat_file: None,
                    dat_file_id: None,
                    dat_version: None,
                    matched: None,
                    error: Some(error),
                });
            }
        }
    }
    dat_write_report(&req, &dat_records, &cancel)?;
    Ok(dat_batch_response(
        "DAT scan complete.",
        records,
        started,
        Some(RunData::DatScan(DatScanData { rows })),
    ))
}

pub(crate) enum RenameResolution {
    Write(PathBuf),
    Skip(PathBuf),
    Failed(PathBuf, String),
}

pub(crate) async fn dat_rename(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let files = if input.is_dir() {
        crate::util::fs::collect_all_files(&input, req.options.max_depth, &cancel)?
    } else {
        vec![input.clone()]
    };
    let algos = dat_algos(&req, "crc32,sha1")?;
    let started = Instant::now();
    let mut candidates = Vec::new();
    let mut records = Vec::new();
    let mut rows = Vec::new();
    for file in files {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        match match_file(
            &file,
            &algos,
            progress,
            cancel.clone(),
            req.options.api_base.as_deref(),
        )
        .await
        {
            Ok(matched) => candidates.push(rename_candidate(file, &matched)),
            Err(err) => {
                if cancel.is_cancelled() || is_cancelled_error(&err) {
                    return Err(err);
                }
                let detail = err.to_string();
                records.push(dat_run_record(&file, "failed", Some(detail.clone())));
                rows.push(DatRenameRowData {
                    from: file,
                    to: None,
                    action: "failed",
                    detail: Some(detail),
                });
            }
        }
    }
    let policy = conflict_policy(&req)?;
    let dry_run = req.dry_run;
    let plans = plan_renames(&candidates);
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    let mut resolutions = HashMap::new();
    if !dry_run {
        let mut pairs = Vec::new();
        for plan in &plans {
            if plan.action != RenameAction::Rename {
                continue;
            }
            let target = plan.to.clone().context("rename plan missing target")?;
            let resolution = match resolve_conflict(&target, policy) {
                Ok(ConflictResolution::Write(dest)) => {
                    pairs.push((plan.from.clone(), dest.clone()));
                    RenameResolution::Write(dest)
                }
                Ok(ConflictResolution::Skip) => RenameResolution::Skip(target),
                Err(err) => RenameResolution::Failed(target, err.to_string()),
            };
            resolutions.insert(plan.from.clone(), resolution);
        }
        rename_transaction(&pairs, policy == ConflictPolicy::Overwrite, &cancel)?;
    }
    for plan in plans {
        if dry_run && cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let (status, output, action, error) = match plan.action {
            RenameAction::Rename => {
                let target = plan.to.clone().context("rename plan missing target")?;
                if dry_run {
                    (FileStatus::Ok, Some(target), "would_rename", None)
                } else {
                    match resolutions.remove(&plan.from).expect("resolved rename") {
                        RenameResolution::Write(dest) => {
                            (FileStatus::Ok, Some(dest), "renamed", None)
                        }
                        RenameResolution::Skip(target) => (
                            FileStatus::Skipped,
                            Some(target),
                            "skipped",
                            Some("target exists".to_string()),
                        ),
                        RenameResolution::Failed(target, error) => {
                            (FileStatus::Failed, Some(target), "failed", Some(error))
                        }
                    }
                }
            }
            RenameAction::AlreadyCanonical => (
                FileStatus::Skipped,
                None,
                "already_canonical",
                plan.detail.clone(),
            ),
            RenameAction::SkipUnmatched => (
                FileStatus::Skipped,
                None,
                "skip_unmatched",
                plan.detail.clone(),
            ),
            RenameAction::SkipWeakMatch => {
                (FileStatus::Skipped, None, "skip_weak", plan.detail.clone())
            }
            RenameAction::SkipCollision => (
                FileStatus::Skipped,
                None,
                "skip_collision",
                plan.detail.clone(),
            ),
            RenameAction::SkipDiscSetConflict => (
                FileStatus::Skipped,
                None,
                "skip_disc_set",
                plan.detail.clone(),
            ),
        };
        records.push(ReportRecord::new(ReportRecordInput {
            input_path: plan.from.display().to_string(),
            output_path: output
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            operation: ("dat.rename").into(),
            status,
            input_bytes: file_len(&plan.from),
            output_bytes: output.as_deref().map(file_len).unwrap_or(0),
            elapsed_ms: 0,
            error: error.clone(),
        }));
        rows.push(DatRenameRowData {
            from: plan.from,
            to: output,
            action,
            detail: error.or(plan.detail),
        });
    }
    Ok(dat_batch_response(
        "DAT rename complete.",
        records,
        started,
        Some(RunData::DatRename(DatRenameData { rows, dry_run })),
    ))
}

pub(crate) async fn dat_fixdat(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = req
        .output
        .clone()
        .ok_or_else(|| invalid_arg("output path is required"))?;
    let policy = conflict_policy(&req)?;
    let output = match resolve_conflict(&desired, policy)? {
        ConflictResolution::Write(path) => path,
        ConflictResolution::Skip if !req.dry_run => {
            return Ok(skipped(&input, &desired, "dat.fixdat"));
        }
        ConflictResolution::Skip => desired,
    };
    let mut index = LocalHashIndex::default();
    for file in crate::util::fs::collect_all_files(&input, req.options.max_depth, &cancel)? {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        match crate::dat::digest_inner_async(
            file,
            vec![
                HashAlgo::Crc32,
                HashAlgo::Md5,
                HashAlgo::Sha1,
                HashAlgo::Sha256,
            ],
            progress,
            cancel.clone(),
        )
        .await?
        {
            RomDigests::Single(d) => index.insert(&d),
            RomDigests::Tracks { tracks, .. } => index.insert_tracks(&tracks),
        }
    }
    let client = PlaymatchClient::new(req.options.api_base.as_deref());
    let dat = resolve_dat_file(&req, &client, &cancel).await?;
    let games = client
        .dat_file_games(&dat.id, true, progress, &cancel)
        .await?;
    let missing = diff_library(&games, &index);
    if req.dry_run {
        return Ok(RunResponse::ok(
            "Fixdat planned.",
            Some(RunData::FixdatPlan(FixdatPlanData {
                dat_file: dat,
                missing_count: missing.len(),
            })),
        ));
    }
    crate::util::atomic_write(
        &output,
        policy == ConflictPolicy::Overwrite,
        |file| -> anyhow::Result<()> {
            write_fixdat_xml(file, &dat, &missing, &cancel)?;
            file.sync_all()?;
            if cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            Ok(())
        },
    )?;
    Ok(RunResponse::ok(
        "Fixdat written.",
        Some(RunData::FixdatWritten(FixdatWrittenData {
            dat_file: dat,
            missing_count: missing.len(),
        })),
    )
    .with_record(ReportRecord::new(ReportRecordInput {
        input_path: input.display().to_string(),
        output_path: output.display().to_string(),
        operation: ("dat.fixdat").into(),
        status: FileStatus::Ok,
        input_bytes: 0,
        output_bytes: file_len(&output),
        elapsed_ms: 0,
        error: None,
    })))
}

pub(crate) fn dat_algos(req: &RunRequest, default: &str) -> Result<Vec<HashAlgo>> {
    let value = req.options.algo.as_deref().unwrap_or(default);
    parse_algos(value).map_err(invalid_arg)
}

pub(crate) fn dat_checksum_bounds(req: &RunRequest, algos: &[HashAlgo]) -> Result<ChecksumBounds> {
    let min = parse_checksum_bound(req.options.input_checksum_min.as_deref().unwrap_or("crc32"))
        .map_err(invalid_arg)?;
    let max = parse_checksum_bound(
        req.options
            .input_checksum_max
            .as_deref()
            .unwrap_or("sha256"),
    )
    .map_err(invalid_arg)?;
    let bounds = ChecksumBounds::new(min, max).map_err(invalid_arg)?;
    bounds.validate_requested(algos).map_err(invalid_arg)?;
    Ok(bounds)
}

pub(crate) fn dat_run_record(path: &Path, verdict: &str, error: Option<String>) -> ReportRecord {
    ReportRecord::new(ReportRecordInput {
        input_path: path.display().to_string(),
        output_path: String::new(),
        operation: ("dat").into(),
        status: if error.is_some() {
            FileStatus::Failed
        } else {
            FileStatus::Ok
        },
        input_bytes: file_len(path),
        output_bytes: 0,
        elapsed_ms: 0,
        error: error.or_else(|| (verdict == "failed").then(|| "DAT operation failed".to_string())),
    })
}

pub(crate) fn dat_batch_response(
    message: &str,
    records: Vec<ReportRecord>,
    started: Instant,
    data: Option<RunData>,
) -> RunResponse {
    let totals = totals_for_records(&records, elapsed_ms(started));
    let status = if totals.failed == 0 {
        RunStatus::Ok
    } else if totals.ok == 0 && totals.skipped == 0 {
        RunStatus::Failed
    } else {
        RunStatus::PartialFailure
    };
    RunResponse {
        schema: RUN_SCHEMA,
        ok: status == RunStatus::Ok,
        status: status.as_i32(),
        code: status.code().to_string(),
        message: message.to_string(),
        details: None,
        totals: Some(totals),
        records,
        events: Vec::new(),
        data,
    }
}

pub(crate) fn dat_write_report(
    req: &RunRequest,
    records: &[DatReportRecord],
    cancel: &CancelToken,
) -> Result<()> {
    let Some(path) = req.options.report.clone() else {
        return Ok(());
    };
    let totals = ReportTotals {
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
        ..ReportTotals::default()
    };
    write_dat_report(
        &path,
        records,
        &totals,
        ReportFormat::from_path(&path),
        cancel,
    )?;
    Ok(())
}

pub(crate) async fn resolve_dat_file(
    req: &RunRequest,
    client: &PlaymatchClient,
    cancel: &CancelToken,
) -> Result<crate::dat::model::DatFileSummary> {
    let mut filter = crate::dat::DatFileFilter {
        name: req.options.dat_name.as_deref().map(str::to_string),
        subset: req.options.subset.as_deref().map(str::to_string),
        ..Default::default()
    };
    if let Some(platform) = req.options.platform.as_deref() {
        let matches = client.platforms_search(platform, cancel).await?;
        filter.platform_id = matches.first().map(|p| p.id.clone());
    }
    let dat_id = req.options.dat_id.as_deref();
    let files = client.list_dat_files(&filter, cancel).await?;
    if let Some(dat_id) = dat_id
        && let Some(found) = files.iter().find(|d| d.id == dat_id)
    {
        return Ok(found.clone());
    }
    files
        .into_iter()
        .next()
        .with_context(|| "no matching DAT file found")
}
