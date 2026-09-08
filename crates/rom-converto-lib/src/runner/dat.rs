//! Request parsing for the `dat.*` operations: each handler reads its
//! options off the request and drives the matching, scan, rename and fixdat
//! workflows in [`crate::dat`] over [`DatUnit`]s.

use super::models::{
    DatMatchData, DatRenameData, DatRenameRowData, DatVerifyData, FixdatPlanData,
    FixdatWrittenData, RunData, RunRequest, RunResponse, RunRow, RunStatus,
};
use super::ops::{conflict_policy, elapsed_ms, required_input, skipped, totals_for_records};
use super::{RUN_SCHEMA, invalid_arg, record_verb};
use crate::dat::fixdat::{LocalHashIndex, diff_library, write_fixdat_xml};
use crate::dat::model::{BulkIdentifyItem, BulkItemStatus, GameFileMatchSearch};
use crate::dat::rename::{RenameAction, RollbackFailed, plan_renames, rename_transaction};
use crate::dat::run::{
    MatchPolicy, error_report_record, match_unit, primary_digests, quick_match, rename_candidate,
    report_record,
};
use crate::dat::scan::scan_units;
use crate::dat::units::{DatUnit, DigestBucket, bucket, collect_units, digest_unit};
use crate::dat::verdict::DatVerdict;
use crate::dat::{DatFileFilter, PlaymatchClient, RomDigests};
use crate::util::fs::file_len;
use crate::util::report::{DatReportRecord, write_dat_report};
use crate::util::{
    CancelToken, Cancelled, ChecksumBounds, ConflictPolicy, ConflictResolution, FileStatus,
    HashAlgo, NX_DAT_UNSUPPORTED_HINT, ProgressReporter, ReportFormat, ReportRecord,
    ReportRecordInput, ReportTotals, parse_algos, parse_checksum_bound, resolve_conflict,
};
use anyhow::{Context, Result, anyhow};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Units under `input` for a batch, or none (with a warning) when the
/// directory holds nothing to match.
async fn batch_units(
    input: &Path,
    req: &RunRequest,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<Vec<DatUnit>> {
    progress.set_phase("Collecting files");
    let units = collect_units(input, req.options.max_depth, cancel).await?;
    if units.is_empty() {
        progress.warn(&format!("No files found under {}", input.display()));
    }
    progress.batch_start(
        units.len() as u64,
        units.iter().map(DatUnit::size_bytes).sum(),
    );
    Ok(units)
}

pub(crate) async fn dat_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let algos = dat_algos(&req, "crc32,sha1")?;
    let bounds = dat_checksum_bounds(&req, &algos)?;
    let quick = req.options.quick.unwrap_or(false);
    let cache = req.ctx.hash_cache.as_deref();
    let policy = MatchPolicy {
        algos: &algos,
        bounds: &bounds,
        quick,
        cache,
    };
    let client = PlaymatchClient::new(req.options.api_base.as_deref());
    let started = Instant::now();

    if input.is_dir() {
        let units = batch_units(&input, &req, progress, &cancel).await?;
        // The outer bar counts files; per-file byte progress goes to a child
        // channel so it never resets the outer counter.
        progress.start(units.len() as u64, "Verifying files");
        let file_progress = progress.child("file");
        let mut data = DatVerifyData::default();
        let mut records = Vec::new();
        let mut dat_records = Vec::new();
        for unit in &units {
            let unit_started = Instant::now();
            let row = match match_unit(
                "verify",
                &client,
                unit,
                policy,
                file_progress.as_ref(),
                &cancel,
            )
            .await
            {
                Ok(row) => row,
                Err(e) => {
                    let (kind, msg) = bucket(e)?;
                    DatMatchData::errored("verify", unit.display_path(), kind.verdict(), msg)
                }
            };
            progress.row(&RunRow::DatMatch(Box::new(row.clone())));
            progress.batch_advance(unit.size_bytes());
            progress.inc(1);
            records.push(dat_run_record(&row.path, &row.verdict, row.error.clone()));
            dat_records.push(report_record(&row, elapsed_ms(unit_started)));
            data.push(row);
        }
        if data.unsupported > 0 {
            progress.warn(NX_DAT_UNSUPPORTED_HINT);
        }
        dat_write_report(&req, &dat_records, elapsed_ms(started), &cancel)?;
        return Ok(dat_batch_response(
            "DAT verification complete.",
            records,
            started,
            Some(RunData::DatVerify(data)),
        ));
    }

    // Quick mode inspects the archive's own central directory, so it runs
    // before staging would extract the member: extraction is exactly the
    // cost quick mode exists to skip.
    let quick_hit = if quick {
        quick_match(
            "verify",
            &client,
            &DatUnit::File(input.clone()),
            cache,
            &cancel,
        )
        .await?
    } else {
        None
    };
    let data = match quick_hit {
        Some(data) => data,
        None => {
            let unit = DatUnit::File(input.clone());
            let policy = MatchPolicy {
                quick: false,
                ..policy
            };
            match match_unit("verify", &client, &unit, policy, progress, &cancel).await {
                Ok(data) => data,
                Err(e) => {
                    let (kind, msg) = bucket(e)?;
                    if kind == DigestBucket::Unsupported {
                        progress.warn(NX_DAT_UNSUPPORTED_HINT);
                    }
                    DatMatchData::errored("verify", &input, kind.verdict(), msg)
                }
            }
        }
    };
    let record = dat_run_record(&input, &data.verdict, data.error.clone());
    let elapsed = elapsed_ms(started);
    dat_write_report(&req, &[report_record(&data, elapsed)], elapsed, &cancel)?;
    Ok(dat_batch_response(
        "DAT verification complete.",
        vec![record],
        started,
        Some(RunData::DatMatch(data)),
    ))
}

pub(crate) async fn dat_identify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let algos = dat_algos(&req, "crc32,sha1")?;
    let bounds = dat_checksum_bounds(&req, &algos)?;
    let client = PlaymatchClient::new(req.options.api_base.as_deref());
    let unit = DatUnit::File(input);
    let policy = MatchPolicy {
        algos: &algos,
        bounds: &bounds,
        quick: false,
        cache: req.ctx.hash_cache.as_deref(),
    };
    let data = match_unit("identify", &client, &unit, policy, progress, &cancel).await?;
    Ok(RunResponse::ok(
        "DAT identify complete.",
        Some(RunData::DatMatch(data)),
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
    let started = Instant::now();
    let units = batch_units(&input, &req, progress, &cancel).await?;
    let client = PlaymatchClient::new(req.options.api_base.as_deref());
    let data = scan_units(
        &units,
        &algos,
        req.options.quick.unwrap_or(false),
        req.ctx.hash_cache.as_deref(),
        &client,
        progress,
        &cancel,
    )
    .await?;
    let elapsed = elapsed_ms(started);
    let mut records = Vec::with_capacity(units.len());
    let mut dat_records = Vec::with_capacity(units.len());
    for (unit, row) in units.iter().zip(&data.rows) {
        records.push(dat_run_record(&row.path, row.status, row.error.clone()));
        let mut record =
            error_report_record(&row.path, DatVerdict::Failed, row.error.clone(), elapsed);
        record.verdict = row.status.to_string();
        record.status = if row.status == DatVerdict::Failed.as_str() {
            FileStatus::Failed
        } else {
            FileStatus::Ok
        };
        record.game_name = row.game_name.clone();
        record.game_id = row.game_id.clone();
        record.match_algo = row.match_algo.clone();
        record.size_bytes = unit.size_bytes();
        dat_records.push(record);
    }
    dat_write_report(&req, &dat_records, elapsed, &cancel)?;
    Ok(dat_batch_response(
        "DAT scan complete.",
        records,
        started,
        Some(RunData::DatScan(data)),
    ))
}

enum RenameResolution {
    Write(PathBuf),
    Skip(PathBuf),
    Failed(PathBuf, String),
}

struct RenameRun {
    data: DatRenameData,
    records: Vec<ReportRecord>,
    dat_records: Vec<DatReportRecord>,
    started: Instant,
}

impl RenameRun {
    fn push(&mut self, progress: &dyn ProgressReporter, row: DatRenameRowData) {
        let status = match row.action {
            "renamed" | "would_rename" => FileStatus::Ok,
            "failed" => FileStatus::Failed,
            _ => FileStatus::Skipped,
        };
        match status {
            FileStatus::Ok => self.data.renamed += 1,
            FileStatus::Failed => self.data.failed += 1,
            FileStatus::Skipped => self.data.skipped += 1,
        }
        let error = (status == FileStatus::Failed)
            .then(|| row.detail.clone())
            .flatten();
        self.records.push(ReportRecord::new(ReportRecordInput {
            input_path: row.from.display().to_string(),
            output_path: row
                .to
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            operation: record_verb("dat.rename"),
            status,
            input_bytes: file_len(&row.from),
            output_bytes: row.to.as_deref().map(file_len).unwrap_or(0),
            elapsed_ms: 0,
            error: error.clone(),
        }));
        let mut record = error_report_record(
            &row.from,
            DatVerdict::Skipped,
            error,
            elapsed_ms(self.started),
        );
        record.status = status;
        record.verdict = match status {
            FileStatus::Ok => DatVerdict::Renamed,
            FileStatus::Failed => DatVerdict::Failed,
            FileStatus::Skipped => DatVerdict::Skipped,
        }
        .as_str()
        .to_string();
        record.detail = match status {
            FileStatus::Ok => row.to.as_ref().map(|p| p.display().to_string()),
            _ => row.detail.clone(),
        };
        self.dat_records.push(record);
        progress.row(&RunRow::DatRename(row.clone()));
        self.data.rows.push(row);
    }
}

pub(crate) async fn dat_rename(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let policy = conflict_policy(&req)?;
    let dry_run = req.dry_run;
    let cache = req.ctx.hash_cache.as_deref();
    let started = Instant::now();
    // Cue members are grouped so a set is one unit; renaming a member .bin in
    // isolation would leave the cue's FILE line dangling. A set is recorded as
    // one skipped row and left untouched on disk.
    let units = if input.is_dir() {
        batch_units(&input, &req, progress, &cancel).await?
    } else {
        vec![DatUnit::File(input.clone())]
    };
    let mut run = RenameRun {
        data: DatRenameData {
            dry_run,
            ..Default::default()
        },
        records: Vec::new(),
        dat_records: Vec::new(),
        started,
    };
    // The outer bar counts files; per-file byte progress goes to a child
    // channel so it never resets the outer counter.
    progress.start(units.len() as u64, "Hashing files");
    let file_progress = progress.child("file");
    let mut queryable = Vec::new();
    let mut items = Vec::new();
    for unit in &units {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let path = unit.display_path();
        let DatUnit::File(_) = unit else {
            run.push(
                progress,
                DatRenameRowData {
                    from: path.to_path_buf(),
                    to: None,
                    action: "skipped",
                    detail: Some(
                        "cue set: rename skipped to keep FILE lines consistent".to_string(),
                    ),
                },
            );
            progress.batch_advance(unit.size_bytes());
            progress.inc(1);
            continue;
        };
        match digest_unit(
            unit,
            &[HashAlgo::Crc32, HashAlgo::Sha1],
            cache,
            file_progress.as_ref(),
            &cancel,
        )
        .await
        {
            Ok(digests) => {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
                items.push(BulkIdentifyItem {
                    search: GameFileMatchSearch::from_digests(name, primary_digests(&digests)),
                    key: None,
                });
                queryable.push(path.to_path_buf());
            }
            Err(e) => {
                let (kind, msg) = bucket(e)?;
                let (action, detail) = match kind {
                    DigestBucket::Unsupported => ("skipped", "unsupported format".to_string()),
                    DigestBucket::Failed => ("failed", msg),
                };
                run.push(
                    progress,
                    DatRenameRowData {
                        from: path.to_path_buf(),
                        to: None,
                        action,
                        detail: Some(detail),
                    },
                );
            }
        }
        progress.batch_advance(unit.size_bytes());
        progress.inc(1);
    }

    // A zero total marks the one-shot network phase as indeterminate.
    let bulk = if items.is_empty() {
        Vec::new()
    } else {
        progress.start(0, &format!("Matching {} files", items.len()));
        let client = PlaymatchClient::new(req.options.api_base.as_deref());
        client.identify_bulk_relations(items, &cancel).await?
    };
    let by_index: HashMap<usize, _> = bulk.iter().map(|r| (r.index, r)).collect();
    let candidates: Vec<_> = queryable
        .into_iter()
        .enumerate()
        .map(|(i, path)| {
            let matched = by_index
                .get(&i)
                .filter(|r| r.status == BulkItemStatus::Ok)
                .and_then(|r| r.matched.as_ref());
            rename_candidate(path, matched)
        })
        .collect();
    let plans = plan_renames(&candidates);
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    let mut resolutions = if dry_run {
        HashMap::new()
    } else {
        apply_renames(&plans, policy, &cancel)?
    };
    for plan in plans {
        let (output, action, error) = match plan.action {
            RenameAction::Rename => {
                let target = plan.to.clone().context("rename plan missing target")?;
                if dry_run {
                    (Some(target), "would_rename", None)
                } else {
                    match resolutions.remove(&plan.from).expect("resolved rename") {
                        RenameResolution::Write(dest) => (Some(dest), "renamed", None),
                        RenameResolution::Skip(target) => {
                            (Some(target), "skipped", Some("target exists".to_string()))
                        }
                        RenameResolution::Failed(target, error) => {
                            (Some(target), "failed", Some(error))
                        }
                    }
                }
            }
            RenameAction::AlreadyCanonical => (None, "already_canonical", None),
            RenameAction::SkipUnmatched => (None, "skip_unmatched", None),
            RenameAction::SkipWeakMatch => (None, "skip_weak", None),
            RenameAction::SkipCollision => (None, "skip_collision", None),
            RenameAction::SkipDiscSetConflict => (None, "skip_disc_set", None),
        };
        run.push(
            progress,
            DatRenameRowData {
                from: plan.from,
                to: output,
                action,
                detail: error.or(plan.detail),
            },
        );
    }
    dat_write_report(&req, &run.dat_records, elapsed_ms(started), &cancel)?;
    Ok(dat_batch_response(
        "DAT rename complete.",
        run.records,
        started,
        Some(RunData::DatRename(run.data)),
    ))
}

/// Resolve each planned rename's target under `policy` and apply the
/// writable ones as one transaction. A transaction failure rolls every
/// rename back and reports each affected pair as a failed row instead of
/// failing the run.
fn apply_renames(
    plans: &[crate::dat::rename::RenamePlan],
    policy: ConflictPolicy,
    cancel: &CancelToken,
) -> Result<HashMap<PathBuf, RenameResolution>> {
    let mut resolutions = HashMap::new();
    let mut pairs = Vec::new();
    for plan in plans {
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
    if let Err(err) = rename_transaction(&pairs, policy == ConflictPolicy::Overwrite, cancel) {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        // A rollback that failed leaves the sources moved away, so calling
        // the rows rolled back would send the user after files that are no
        // longer where they were.
        let detail = if RollbackFailed::is_in(&err) {
            format!("rename failed; {err}")
        } else {
            format!("rolled back: {err}")
        };
        for (from, to) in pairs {
            resolutions.insert(from, RenameResolution::Failed(to, detail.clone()));
        }
    }
    Ok(resolutions)
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
    let client = PlaymatchClient::new(req.options.api_base.as_deref());
    let dat = resolve_dat_file(&req, &client, &cancel).await?;
    let games = client
        .dat_file_games(&dat.id, true, progress, &cancel)
        .await?;
    let total_games = games.len();

    let units = collect_units(&input, req.options.max_depth, &cancel).await?;
    progress.set_phase(&format!("Hashing {} files", units.len()));
    let cache = req.ctx.hash_cache.as_deref();
    let mut index = LocalHashIndex::default();
    let mut unsupported = false;
    for unit in &units {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let algos = [
            HashAlgo::Crc32,
            HashAlgo::Sha1,
            HashAlgo::Md5,
            HashAlgo::Sha256,
        ];
        match digest_unit(unit, &algos, cache, progress, &cancel).await {
            Ok(RomDigests::Single(d)) => index.insert(&d),
            Ok(RomDigests::Tracks { tracks, whole }) => {
                index.insert_tracks(&tracks);
                index.insert(&whole);
            }
            Err(e) => match bucket(e)? {
                (DigestBucket::Unsupported, _) => unsupported = true,
                (DigestBucket::Failed, msg) => progress.warn(&format!(
                    "Skipping {}: {msg}",
                    unit.display_path().display()
                )),
            },
        }
    }
    if unsupported {
        progress.warn(NX_DAT_UNSUPPORTED_HINT);
    }
    let missing = diff_library(&games, &index);
    let missing_files = missing.iter().map(|e| e.missing.len()).sum();
    if req.dry_run {
        return Ok(RunResponse::ok(
            "Fixdat planned.",
            Some(RunData::FixdatPlan(FixdatPlanData {
                dat_file: dat,
                total_games,
                missing_count: missing.len(),
                missing_files,
                output,
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
            total_games,
            missing_count: missing.len(),
            missing_files,
            output: output.clone(),
        })),
    )
    .with_record(ReportRecord::new(ReportRecordInput {
        input_path: input.display().to_string(),
        output_path: output.display().to_string(),
        operation: record_verb("dat.fixdat"),
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
    let failed = verdict == DatVerdict::Failed.as_str();
    ReportRecord::new(ReportRecordInput {
        input_path: path.display().to_string(),
        output_path: String::new(),
        operation: record_verb("dat"),
        status: if failed {
            FileStatus::Failed
        } else {
            FileStatus::Ok
        },
        input_bytes: file_len(path),
        output_bytes: 0,
        elapsed_ms: 0,
        error: error.or_else(|| failed.then(|| "DAT operation failed".to_string())),
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
    elapsed_ms: u64,
    cancel: &CancelToken,
) -> Result<()> {
    let Some(path) = req.options.report.clone() else {
        return Ok(());
    };
    let count = |status| records.iter().filter(|r| r.status == status).count();
    let totals = ReportTotals {
        total_files: records.len(),
        ok: count(FileStatus::Ok),
        skipped: count(FileStatus::Skipped),
        failed: count(FileStatus::Failed),
        total_input_bytes: records.iter().map(|r| r.size_bytes).sum(),
        total_output_bytes: 0,
        elapsed_ms,
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

/// Resolve the one DAT to diff against: an explicit `dat_id` is found by
/// scanning the DAT list; otherwise the platform is resolved by exact name
/// and its DATs are filtered by `dat_name`/`subset`. An ambiguous result
/// lists the candidates in the error so the caller can narrow it.
pub(crate) async fn resolve_dat_file(
    req: &RunRequest,
    client: &PlaymatchClient,
    cancel: &CancelToken,
) -> Result<crate::dat::model::DatFileSummary> {
    if let Some(id) = req.options.dat_id.as_deref() {
        let all = client
            .list_dat_files(&DatFileFilter::default(), cancel)
            .await?;
        return all
            .into_iter()
            .find(|d| d.id == id)
            .ok_or_else(|| invalid_arg(format!("no DAT with id {id}")));
    }
    let platform_name = req
        .options
        .platform
        .as_deref()
        .ok_or_else(|| invalid_arg("either platform or dat_id is required"))?;
    let platform = client
        .platforms_search(platform_name, cancel)
        .await?
        .into_iter()
        .find(|p| p.name.eq_ignore_ascii_case(platform_name))
        .ok_or_else(|| invalid_arg(format!("no platform matching \"{platform_name}\"")))?;
    let filter = DatFileFilter {
        platform_id: Some(platform.id),
        name: req.options.dat_name.clone(),
        subset: req.options.subset.clone(),
        ..Default::default()
    };
    let mut candidates = client.list_dat_files(&filter, cancel).await?;
    match candidates.len() {
        0 => Err(invalid_arg(format!(
            "no DAT found for platform \"{platform_name}\" with the given filters"
        ))),
        1 => Ok(candidates.remove(0)),
        _ => {
            let listing: Vec<String> = candidates
                .iter()
                .map(|d| {
                    format!(
                        "  {}  {}  subset={}  version={}",
                        d.id,
                        d.name,
                        d.subset.as_deref().unwrap_or("-"),
                        d.current_version
                    )
                })
                .collect();
            Err(anyhow!(
                "ambiguous DAT selection; multiple DATs match, narrow with dat_name or subset:\n{}",
                listing.join("\n")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dat::rename::RenamePlan;
    use crate::runner::models::RunOptions;
    use crate::util::NoProgress;

    #[tokio::test]
    async fn single_verify_caches_digest_under_the_archive_path() {
        use crate::util::HashCache;
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("game.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("game.iso", opts).unwrap();
        zip.write_all(b"cartridge").unwrap();
        zip.finish().unwrap();
        // The cache refuses fingerprints of files modified in the last seconds.
        std::fs::File::options()
            .write(true)
            .open(&zip_path)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(60))
            .unwrap();
        let cache = std::sync::Arc::new(HashCache::open_at(
            Some(dir.path().join("hash-cache.json.gz")),
            false,
        ));

        let req = RunRequest {
            schema: None,
            operation: "dat.verify".to_string(),
            input: Some(zip_path.clone()),
            output: None,
            config: None,
            preset: None,
            options: RunOptions {
                // Refused connection: the digest lands in the cache before
                // the lookup fails.
                api_base: Some("http://127.0.0.1:1".to_string()),
                ..RunOptions::default()
            },
            ctx: crate::runner::models::RunContext {
                hash_cache: Some(cache.clone()),
                ..Default::default()
            },
            dry_run: false,
        };
        let _ = dat_verify(req, &NoProgress, CancelToken::new()).await;
        let cached = cache
            .lookup_decoded(&zip_path, &[HashAlgo::Crc32])
            .expect("digest cached under the archive path");
        // The member's digest, not the zip container's.
        assert_eq!(cached.size_bytes, b"cartridge".len() as u64);
        assert_eq!(cached.crc32.as_deref(), Some("e9648752"));
    }

    #[test]
    fn failed_rename_transaction_reports_rolled_back_rows() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        std::fs::write(&a, b"a").unwrap();
        let ghost = dir.path().join("ghost.bin");
        let plan = |from: &Path, to: &str| RenamePlan {
            from: from.to_path_buf(),
            to: Some(dir.path().join(to)),
            action: RenameAction::Rename,
            detail: None,
        };
        let plans = [plan(&a, "x.bin"), plan(&ghost, "y.bin")];
        let resolutions =
            apply_renames(&plans, ConflictPolicy::Error, &CancelToken::new()).unwrap();
        assert_eq!(resolutions.len(), 2);
        for from in [&a, &ghost] {
            let RenameResolution::Failed(_, detail) = &resolutions[from.as_path()] else {
                panic!("{} should have failed", from.display());
            };
            assert!(detail.starts_with("rolled back: "), "{detail}");
        }
        assert!(a.exists());
        assert!(!dir.path().join("x.bin").exists());
    }

    #[tokio::test]
    async fn rename_dry_run_counts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("game.cue"),
            "FILE \"game.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("game.bin"), b"1").unwrap();
        std::fs::write(dir.path().join("switch.nsz"), b"1").unwrap();
        std::fs::write(dir.path().join("broken.chd"), b"not a chd").unwrap();

        let req = RunRequest {
            schema: None,
            operation: "dat.rename".to_string(),
            input: Some(dir.path().to_path_buf()),
            output: None,
            config: None,
            preset: None,
            options: RunOptions::default(),
            ctx: Default::default(),
            dry_run: true,
        };
        let res = dat_rename(req, &NoProgress, CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::DatRename(data)) = res.data else {
            panic!("expected rename data");
        };
        assert!(data.dry_run);
        assert_eq!(data.renamed, 0);
        assert_eq!(data.skipped, 2);
        assert_eq!(data.failed, 1);
        assert_eq!(data.rows.len(), 3);
        // Rows follow walk order: the broken CHD sorts before the cue set.
        assert!(data.rows[0].from.ends_with("broken.chd"));
        assert_eq!(data.rows[1].action, "skipped");
        assert!(data.rows[1].from.ends_with("game.cue"));
        let totals = res.totals.unwrap();
        assert_eq!((totals.skipped, totals.failed), (2, 1));
        assert_eq!(res.status, RunStatus::PartialFailure.as_i32());
    }
}
