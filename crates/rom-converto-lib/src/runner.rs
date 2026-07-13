//! Shared JSON command runner used by embedding frontends.
//!
//! The C ABI passes JSON through this module so the ABI stays small while
//! Rust-side request and response schemas can evolve behind a versioned
//! payload.

use crate::chd::{ChdDvdOptions, DiscMode};
use crate::cso::{CsoCompressOptions, CsoFormat};
use crate::dat::fixdat::{LocalHashIndex, diff_library, write_fixdat_xml_cancellable};
use crate::dat::model::{GameAndRelationMatchResult, GameFileMatchSearch};
use crate::dat::rename::{RenameAction, RenameCandidate, plan_renames};
use crate::dat::verdict::{DatVerdict, MatchStrength, match_strength};
use crate::dat::{PlaymatchClient, RomDigests};
use crate::nintendo::legacy_input::{
    ALL_MIGRATE_FORMATS, DOL_MIGRATE_FORMATS, MigrateOptions, migrate_disc_cancellable,
};
use crate::nintendo::rvz::RvzCompressOptions;
use crate::util::report::{DatReportRecord, write_dat_report_cancellable};
use crate::util::{
    CancelToken, ChecksumBounds, ConflictPolicy, ConflictResolution, FileStatus, HashAlgo,
    NoProgress, OutputVerify, PlanLine, ProgressReporter, ReportFormat, ReportRecord, ReportTotals,
    VerifyOutcome, hash_file_cancellable, parse_algos, parse_checksum_bound, resolve_conflict,
    verify_existing_output_cancellable, write_report_cancellable,
};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

pub const RUN_SCHEMA: &str = "rom-converto.run.v1";

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct InvalidArgument(String);

fn invalid_arg(message: impl Into<String>) -> anyhow::Error {
    InvalidArgument(message.into()).into()
}

pub fn schema_json() -> Value {
    serde_json::to_value(RunSchemaManifest::current()).expect("runner schema serializes")
}

pub mod models;

use models::{
    BasicPlanData, ComparisonData, DatMatchData, DatRenameData, DatRenameRowData, DatScanData,
    FixdatPlanData, FixdatWrittenData, PlaylistPlanData, PlaylistsData, ProgressEvent,
    RunComparisonData, RunData, RunOptions, RunPlansData, RunRequest, RunResponse,
    RunSchemaManifest, RunStatus, WupTitleInputOption,
};
#[derive(Default)]
pub struct RecordingProgress {
    events: Mutex<Vec<ProgressEvent>>,
}

impl RecordingProgress {
    pub fn take_events(&self) -> Vec<ProgressEvent> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }

    fn push(&self, event: ProgressEvent) {
        self.events.lock().unwrap().push(event);
    }
}

impl ProgressReporter for RecordingProgress {
    fn start(&self, total: u64, msg: &str) {
        self.push(ProgressEvent::Start {
            total,
            message: msg.to_string(),
        });
    }

    fn inc(&self, delta: u64) {
        self.push(ProgressEvent::Advance { delta });
    }

    fn finish(&self) {
        self.push(ProgressEvent::Finish);
    }

    fn set_phase(&self, label: &str) {
        self.push(ProgressEvent::Phase {
            message: label.to_string(),
        });
    }

    fn warn(&self, message: &str) {
        self.push(ProgressEvent::Warn {
            message: message.to_string(),
        });
    }
}

pub async fn run_json(request_json: &str, cancel: CancelToken) -> RunResponse {
    let progress = RecordingProgress::default();
    let mut response = run_json_with_progress(request_json, &progress, cancel).await;
    response.events = progress.take_events();
    response
}

pub async fn run_json_with_progress(
    request_json: &str,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> RunResponse {
    let req = match serde_json::from_str::<RunRequest>(request_json) {
        Ok(req) => req,
        Err(err) => {
            return RunResponse::error(
                RunStatus::InvalidArgument,
                "Request JSON is invalid.",
                Some(err.to_string()),
            );
        }
    };

    match run_request(req, progress, cancel).await {
        Ok(response) => response,
        Err(err) if is_cancelled_error(&err) => RunResponse::error(
            RunStatus::Cancelled,
            "Operation cancelled.",
            Some(err.to_string()),
        ),
        Err(err) if err.downcast_ref::<InvalidArgument>().is_some() => RunResponse::error(
            RunStatus::InvalidArgument,
            err.to_string(),
            error_chain(&err),
        ),
        Err(err) => RunResponse::error(RunStatus::Failed, err.to_string(), error_chain(&err)),
    }
}

pub async fn run_request(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    if cancel.is_cancelled() {
        bail!("cancelled");
    }
    if let Some(schema) = req.schema.as_deref()
        && schema != RUN_SCHEMA
    {
        return Err(invalid_arg(format!(
            "unsupported schema {schema:?}; expected {RUN_SCHEMA}"
        )));
    }

    let req = apply_config_defaults(req)?;
    let report = opt_path(&req, "report");
    let response = if opt_bool(&req, "recursive").unwrap_or(false) {
        run_batch_request(req.clone(), progress, cancel.clone()).await?
    } else {
        run_single_request(req.clone(), progress, cancel.clone()).await?
    };
    if !req.operation.starts_with("dat.")
        && let (Some(path), Some(totals)) = (report.as_deref(), response.totals.as_ref())
        && !response.records.is_empty()
    {
        write_report_cancellable(
            path,
            &response.records,
            totals,
            ReportFormat::from_path(path),
            &cancel,
        )?;
    }
    Ok(response)
}

fn apply_config_defaults(mut req: RunRequest) -> Result<RunRequest> {
    let config_path = req.config.clone().or_else(|| req.options.config.clone());
    let preset_name = req.preset.clone().or_else(|| req.options.preset.clone());
    let config = crate::config::load_config(config_path.as_deref())
        .map_err(|err| invalid_arg(err.to_string()))?;
    let preset = crate::config::resolve_preset(&config, preset_name.as_deref())
        .map_err(|err| invalid_arg(err.to_string()))?;

    let operation = req.operation.clone();
    match operation.split_once('.').map(|(family, _)| family) {
        Some("dol") => apply_disc_defaults(
            &mut req.options,
            preset.as_ref().and_then(|p| p.dol.as_ref()),
            config.dol.as_ref(),
        ),
        Some("rvl") => apply_disc_defaults(
            &mut req.options,
            preset.as_ref().and_then(|p| p.rvl.as_ref()),
            config.rvl.as_ref(),
        ),
        Some("nx") => apply_nx_defaults(
            &mut req.options,
            preset.as_ref().and_then(|p| p.nx.as_ref()),
            config.nx.as_ref(),
        ),
        Some("chd") => apply_chd_defaults(
            &mut req.options,
            preset.as_ref().and_then(|p| p.chd.as_ref()),
            config.chd.as_ref(),
        ),
        Some("cso") => apply_cso_defaults(
            &mut req.options,
            preset.as_ref().and_then(|p| p.cso.as_ref()),
            config.cso.as_ref(),
        ),
        Some("wup") => apply_wup_defaults(
            &mut req.options,
            preset.as_ref().and_then(|p| p.wup.as_ref()),
            config.wup.as_ref(),
        ),
        Some("dat") => apply_dat_defaults(
            &mut req.options,
            preset.as_ref().and_then(|p| p.dat.as_ref()),
            config.dat.as_ref(),
        ),
        _ => {}
    }
    Ok(req)
}

fn apply_disc_defaults(
    options: &mut RunOptions,
    top: Option<&crate::config::DiscDefaults>,
    base: Option<&crate::config::DiscDefaults>,
) {
    fill(
        &mut options.level,
        pick(top.and_then(|v| v.level), base.and_then(|v| v.level)),
    );
    fill(
        &mut options.chunk_size,
        pick(
            top.and_then(|v| v.chunk_size),
            base.and_then(|v| v.chunk_size),
        ),
    );
    fill(
        &mut options.on_conflict,
        pick(
            top.and_then(|v| v.on_conflict.clone()),
            base.and_then(|v| v.on_conflict.clone()),
        ),
    );
    fill(
        &mut options.output_dir,
        pick(
            top.and_then(|v| v.output_dir.clone()),
            base.and_then(|v| v.output_dir.clone()),
        ),
    );
    fill(
        &mut options.report,
        pick(
            top.and_then(|v| v.report.clone()),
            base.and_then(|v| v.report.clone()),
        ),
    );
}

fn apply_nx_defaults(
    options: &mut RunOptions,
    top: Option<&crate::config::NxDefaults>,
    base: Option<&crate::config::NxDefaults>,
) {
    fill(
        &mut options.level,
        pick(top.and_then(|v| v.level), base.and_then(|v| v.level)),
    );
    fill(
        &mut options.mode,
        pick(
            top.and_then(|v| v.mode.clone()),
            base.and_then(|v| v.mode.clone()),
        ),
    );
    fill(
        &mut options.block_size_exp,
        pick(
            top.and_then(|v| v.block_size_exp).map(u32::from),
            base.and_then(|v| v.block_size_exp).map(u32::from),
        ),
    );
    fill(
        &mut options.on_conflict,
        pick(
            top.and_then(|v| v.on_conflict.clone()),
            base.and_then(|v| v.on_conflict.clone()),
        ),
    );
    fill(
        &mut options.output_dir,
        pick(
            top.and_then(|v| v.output_dir.clone()),
            base.and_then(|v| v.output_dir.clone()),
        ),
    );
    fill(
        &mut options.report,
        pick(
            top.and_then(|v| v.report.clone()),
            base.and_then(|v| v.report.clone()),
        ),
    );
}

fn apply_chd_defaults(
    options: &mut RunOptions,
    top: Option<&crate::config::ChdDefaults>,
    base: Option<&crate::config::ChdDefaults>,
) {
    fill(
        &mut options.hunk_size,
        pick(
            top.and_then(|v| v.hunk_size),
            base.and_then(|v| v.hunk_size),
        ),
    );
    fill(
        &mut options.on_conflict,
        pick(
            top.and_then(|v| v.on_conflict.clone()),
            base.and_then(|v| v.on_conflict.clone()),
        ),
    );
    fill(
        &mut options.output_dir,
        pick(
            top.and_then(|v| v.output_dir.clone()),
            base.and_then(|v| v.output_dir.clone()),
        ),
    );
    fill(
        &mut options.report,
        pick(
            top.and_then(|v| v.report.clone()),
            base.and_then(|v| v.report.clone()),
        ),
    );
}

fn apply_cso_defaults(
    options: &mut RunOptions,
    top: Option<&crate::config::CsoDefaults>,
    base: Option<&crate::config::CsoDefaults>,
) {
    fill(
        &mut options.block_size,
        pick(
            top.and_then(|v| v.block_size),
            base.and_then(|v| v.block_size),
        ),
    );
    fill(
        &mut options.on_conflict,
        pick(
            top.and_then(|v| v.on_conflict.clone()),
            base.and_then(|v| v.on_conflict.clone()),
        ),
    );
    fill(
        &mut options.output_dir,
        pick(
            top.and_then(|v| v.output_dir.clone()),
            base.and_then(|v| v.output_dir.clone()),
        ),
    );
    fill(
        &mut options.report,
        pick(
            top.and_then(|v| v.report.clone()),
            base.and_then(|v| v.report.clone()),
        ),
    );
}

fn apply_wup_defaults(
    options: &mut RunOptions,
    top: Option<&crate::config::WupDefaults>,
    base: Option<&crate::config::WupDefaults>,
) {
    fill(
        &mut options.level,
        pick(top.and_then(|v| v.level), base.and_then(|v| v.level)),
    );
    fill(
        &mut options.on_conflict,
        pick(
            top.and_then(|v| v.on_conflict.clone()),
            base.and_then(|v| v.on_conflict.clone()),
        ),
    );
}

fn apply_dat_defaults(
    options: &mut RunOptions,
    top: Option<&crate::config::DatDefaults>,
    base: Option<&crate::config::DatDefaults>,
) {
    fill(
        &mut options.api_base,
        pick(
            top.and_then(|v| v.api_base.clone()),
            base.and_then(|v| v.api_base.clone()),
        ),
    );
    fill(
        &mut options.report,
        pick(
            top.and_then(|v| v.report.clone()),
            base.and_then(|v| v.report.clone()),
        ),
    );
    fill(
        &mut options.input_checksum_min,
        pick(
            top.and_then(|v| v.input_checksum_min.clone()),
            base.and_then(|v| v.input_checksum_min.clone()),
        ),
    );
    fill(
        &mut options.input_checksum_max,
        pick(
            top.and_then(|v| v.input_checksum_max.clone()),
            base.and_then(|v| v.input_checksum_max.clone()),
        ),
    );
}

fn pick<T>(top: Option<T>, base: Option<T>) -> Option<T> {
    top.or(base)
}

fn fill<T>(slot: &mut Option<T>, value: Option<T>) {
    if slot.is_none() {
        *slot = value;
    }
}

async fn run_single_request(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    match req.operation.as_str() {
        "cso.compress" => cso_compress(req, progress, cancel).await,
        "cso.decompress" => cso_decompress(req, progress, cancel).await,
        "cso.verify" => cso_verify(req, progress, cancel).await,
        "chd.compress" => chd_compress(req, progress, cancel).await,
        "chd.extract" => chd_extract(req, progress, cancel).await,
        "chd.verify" => chd_verify(req, progress, cancel).await,
        "cso.to_chd" | "cso.to-chd" => cso_to_chd(req, progress, cancel).await,
        "chd.to_cso" | "chd.to-cso" => chd_to_cso(req, progress, cancel).await,
        "rvz.compress" | "dol.compress" | "rvl.compress" => {
            rvz_compress(req, progress, cancel).await
        }
        "rvz.decompress" | "dol.decompress" | "rvl.decompress" => {
            rvz_decompress(req, progress, cancel).await
        }
        "dol.migrate" => migrate_disc(req, progress, cancel, DOL_MIGRATE_FORMATS).await,
        "rvl.migrate" | "rvz.migrate" => {
            migrate_disc(req, progress, cancel, ALL_MIGRATE_FORMATS).await
        }
        "ctr.decrypt" => ctr_decrypt(req, progress, cancel).await,
        "ctr.encrypt" => ctr_encrypt(req, progress, cancel).await,
        "ctr.compress" => ctr_compress(req, progress, cancel).await,
        "ctr.decompress" => ctr_decompress(req, progress, cancel).await,
        "ctr.convert" => ctr_convert(req, progress, cancel).await,
        "ctr.verify" => ctr_verify(req, progress, cancel).await,
        "ctr.cdn_to_cia" | "ctr.cdn-to-cia" => ctr_cdn_to_cia(req, progress, cancel).await,
        "ctr.generate_cdn_ticket" | "ctr.generate-cdn-ticket" => {
            ctr_generate_cdn_ticket(req, cancel).await
        }
        "dol.verify" => dol_verify(req, progress, cancel).await,
        "rvl.verify" => rvl_verify(req, progress, cancel).await,
        "nx.compress" => nx_compress(req, progress, cancel).await,
        "nx.decompress" => nx_decompress(req, progress, cancel).await,
        "nx.verify" => nx_verify(req, progress, cancel).await,
        "wup.compress" => wup_compress(req, progress, cancel).await,
        "wup.decrypt" => wup_decrypt(req, progress, cancel).await,
        "wup.verify" => wup_verify(req, progress, cancel).await,
        "cue.merge" => cue_merge(req, progress, cancel).await,
        "playlist.write" | "playlist" => playlist_write(req, cancel).await,
        "dat.verify" => dat_verify(req, progress, cancel).await,
        "dat.identify" => dat_identify(req, progress, cancel).await,
        "dat.scan" => dat_scan(req, progress, cancel).await,
        "dat.rename" => dat_rename(req, progress, cancel).await,
        "dat.fixdat" => dat_fixdat(req, progress, cancel).await,
        "hash" => hash(req, progress, cancel).await,
        "info.read" | "info" => info(req),
        other => Err(invalid_arg(format!("unknown operation {other:?}"))),
    }
}

async fn run_batch_request(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let root = required_input(&req)?;
    if !root.is_dir() {
        return Err(invalid_arg(format!(
            "recursive input must be a directory: {}",
            root.display()
        )));
    }
    let exts = batch_exts(&req.operation)?;
    let files = crate::util::fs::collect_files_with_exts_cancellable(
        &root,
        exts,
        opt_usize(&req, "max_depth")?,
        &cancel,
    )
    .with_context(|| format!("scanning {}", root.display()))?;
    if files.is_empty() {
        return Err(invalid_arg(format!(
            "no matching files found in {}",
            root.display()
        )));
    }

    let started = Instant::now();
    let mut records = Vec::new();
    let mut plans = Vec::new();
    let child_options = child_options(&req.options);
    for input in files {
        if cancel.is_cancelled() {
            bail!("cancelled");
        }
        let mut child = req.clone();
        child.input = Some(input.clone());
        child.output = None;
        child.options = child_options.clone();
        match run_single_request(child, progress, cancel.clone()).await {
            Ok(mut response) => {
                if let Some(RunData::Plan(line)) = response.data.take()
                    && req.dry_run
                {
                    plans.push(line);
                }
                if response.records.is_empty() {
                    records.push(ReportRecord::new(
                        input.display().to_string(),
                        String::new(),
                        &req.operation,
                        FileStatus::Ok,
                        file_len(&input),
                        0,
                        0,
                        None,
                    ));
                } else {
                    records.append(&mut response.records);
                }
            }
            Err(err) if cancel.is_cancelled() || is_cancelled_error(&err) => return Err(err),
            Err(err) => records.push(ReportRecord::new(
                input.display().to_string(),
                String::new(),
                &req.operation,
                FileStatus::Failed,
                file_len(&input),
                0,
                0,
                Some(err.to_string()),
            )),
        }
    }

    let totals = totals_for_records(&records, elapsed_ms(started));
    let status = if totals.failed == 0 {
        RunStatus::Ok
    } else if totals.ok == 0 && totals.skipped == 0 {
        RunStatus::Failed
    } else {
        RunStatus::PartialFailure
    };
    Ok(RunResponse {
        schema: RUN_SCHEMA,
        ok: status == RunStatus::Ok,
        status: status.as_i32(),
        code: status.code().to_string(),
        message: batch_message(&totals),
        details: None,
        totals: Some(totals),
        records,
        events: Vec::new(),
        data: (!plans.is_empty()).then_some(RunData::Plans(RunPlansData { plans })),
    })
}

async fn cso_compress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let format = cso_format(opt_str(&req, "format").unwrap_or("cso"))?;
    let desired = output_or(&req, || input.with_extension(format.extension()))?;
    let plan = prepare_output(
        progress,
        &req,
        &input,
        &desired,
        "cso.compress",
        OutputVerify::Cso,
        &cancel,
    )
    .await?;
    let Some(output) = plan.output else {
        return Ok(skipped(&input, &desired, "cso.compress"));
    };
    if let Some(line) = plan.line {
        return Ok(RunResponse::ok(
            "Dry run planned.",
            Some(RunData::Plan(line)),
        ));
    }
    let opts = CsoCompressOptions {
        format,
        block_size: opt_u32(&req, "block_size")?,
        force: true,
    };
    run_file_op(&input, &output, "cso.compress", || async {
        crate::cso::compress_to_cso_cancellable(
            progress,
            input.clone(),
            output.clone(),
            opts,
            cancel,
        )
        .await
        .map_err(anyhow::Error::from)
    })
    .await
}

async fn cso_decompress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = output_or(&req, || input.with_extension("iso"))?;
    let plan = prepare_output(
        progress,
        &req,
        &input,
        &desired,
        "cso.decompress",
        OutputVerify::None,
        &cancel,
    )
    .await?;
    let Some(output) = plan.output else {
        return Ok(skipped(&input, &desired, "cso.decompress"));
    };
    if let Some(line) = plan.line {
        return Ok(RunResponse::ok(
            "Dry run planned.",
            Some(RunData::Plan(line)),
        ));
    }
    run_file_op(&input, &output, "cso.decompress", || async {
        crate::cso::decompress_from_cso_cancellable(
            progress,
            input.clone(),
            output.clone(),
            true,
            cancel,
        )
        .await
        .map_err(anyhow::Error::from)
    })
    .await
}

async fn cso_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    crate::cso::verify_cso_cancellable(
        progress,
        input.clone(),
        opt_bool(&req, "full").unwrap_or(true),
        cancel,
    )
    .await?;
    Ok(RunResponse::ok("CSO verification passed.", None))
}

async fn chd_compress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = output_or(&req, || input.with_extension("chd"))?;
    let plan = prepare_output(
        progress,
        &req,
        &input,
        &desired,
        "chd.compress",
        OutputVerify::Chd,
        &cancel,
    )
    .await?;
    let Some(output) = plan.output else {
        return Ok(skipped(&input, &desired, "chd.compress"));
    };
    if let Some(line) = plan.line {
        return Ok(RunResponse::ok(
            "Dry run planned.",
            Some(RunData::Plan(line)),
        ));
    }
    let opts = ChdDvdOptions {
        hunk_size: opt_u32(&req, "hunk_size")?,
        allow_zstd: opt_bool(&req, "allow_zstd").unwrap_or(false),
        force: true,
    };
    let mode = disc_mode(opt_str(&req, "mode"))?;
    run_file_op(&input, &output, "chd.compress", || async {
        crate::chd::convert_disc_to_chd_cancellable(
            progress,
            input.clone(),
            output.clone(),
            mode,
            opts,
            cancel,
        )
        .await
        .map_err(anyhow::Error::from)
    })
    .await
}

async fn chd_extract(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = output_or(&req, || input.with_extension("iso"))?;
    let plan = prepare_output(
        progress,
        &req,
        &input,
        &desired,
        "chd.extract",
        OutputVerify::None,
        &cancel,
    )
    .await?;
    let Some(output) = plan.output else {
        return Ok(skipped(&input, &desired, "chd.extract"));
    };
    if let Some(line) = plan.line {
        return Ok(RunResponse::ok(
            "Dry run planned.",
            Some(RunData::Plan(line)),
        ));
    }
    run_file_op(&input, &output, "chd.extract", || async {
        crate::chd::extract_from_chd_cancellable(
            progress,
            input.clone(),
            output.clone(),
            opt_path(&req, "parent"),
            cancel,
        )
        .await
        .map_err(anyhow::Error::from)
    })
    .await
}

async fn chd_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    crate::chd::verify_chd_cancellable(
        progress,
        input,
        opt_path(&req, "parent"),
        opt_bool(&req, "fix").unwrap_or(false),
        cancel,
    )
    .await?;
    Ok(RunResponse::ok("CHD verification passed.", None))
}

async fn cso_to_chd(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = output_or(&req, || input.with_extension("chd"))?;
    let plan = prepare_output(
        progress,
        &req,
        &input,
        &desired,
        "cso.to_chd",
        OutputVerify::Chd,
        &cancel,
    )
    .await?;
    let Some(output) = plan.output else {
        return Ok(skipped(&input, &desired, "cso.to_chd"));
    };
    if let Some(line) = plan.line {
        return Ok(RunResponse::ok(
            "Dry run planned.",
            Some(RunData::Plan(line)),
        ));
    }
    let opts = ChdDvdOptions {
        hunk_size: opt_u32(&req, "hunk_size")?,
        allow_zstd: opt_bool(&req, "allow_zstd").unwrap_or(false),
        force: true,
    };
    run_file_op(&input, &output, "cso.to_chd", || async {
        crate::pipeline::cso_to_chd_cancellable(
            progress,
            input.clone(),
            output.clone(),
            disc_mode(opt_str(&req, "mode"))?,
            opts,
            cancel,
        )
        .await
    })
    .await
}

async fn chd_to_cso(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let format = cso_format(opt_str(&req, "format").unwrap_or("cso"))?;
    let desired = output_or(&req, || input.with_extension(format.extension()))?;
    let plan = prepare_output(
        progress,
        &req,
        &input,
        &desired,
        "chd.to_cso",
        OutputVerify::Cso,
        &cancel,
    )
    .await?;
    let Some(output) = plan.output else {
        return Ok(skipped(&input, &desired, "chd.to_cso"));
    };
    if let Some(line) = plan.line {
        return Ok(RunResponse::ok(
            "Dry run planned.",
            Some(RunData::Plan(line)),
        ));
    }
    let opts = CsoCompressOptions {
        format,
        block_size: opt_u32(&req, "block_size")?,
        force: true,
    };
    run_file_op(&input, &output, "chd.to_cso", || async {
        crate::pipeline::chd_to_cso_cancellable(
            progress,
            input.clone(),
            output.clone(),
            opts,
            cancel,
        )
        .await
    })
    .await
}

async fn rvz_compress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let operation = req.operation.clone();
    let desired = output_or(&req, || crate::nintendo::rvz::derive_rvz_path(&input))?;
    let plan = prepare_output(
        progress,
        &req,
        &input,
        &desired,
        &operation,
        OutputVerify::Rvz,
        &cancel,
    )
    .await?;
    let Some(output) = plan.output else {
        return Ok(skipped(&input, &desired, &operation));
    };
    if let Some(line) = plan.line {
        return Ok(RunResponse::ok(
            "Dry run planned.",
            Some(RunData::Plan(line)),
        ));
    }
    let mut opts = RvzCompressOptions::default();
    if let Some(level) = opt_i32(&req, "level")? {
        opts.compression_level = level;
    }
    if let Some(chunk_size) = opt_u32(&req, "chunk_size")? {
        opts.chunk_size = chunk_size;
    }
    run_file_op(&input, &output, &operation, || async {
        crate::nintendo::rvz::compress_disc_cancellable(&input, &output, opts, progress, cancel)
            .await
            .map_err(anyhow::Error::from)
    })
    .await
}

async fn rvz_decompress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let operation = req.operation.clone();
    let desired = output_or(&req, || crate::nintendo::rvz::derive_disc_path(&input))?;
    let plan = prepare_output(
        progress,
        &req,
        &input,
        &desired,
        &operation,
        OutputVerify::None,
        &cancel,
    )
    .await?;
    let Some(output) = plan.output else {
        return Ok(skipped(&input, &desired, &operation));
    };
    if let Some(line) = plan.line {
        return Ok(RunResponse::ok(
            "Dry run planned.",
            Some(RunData::Plan(line)),
        ));
    }
    run_file_op(&input, &output, &operation, || async {
        if has_ext(&output, "wbfs") {
            crate::nintendo::rvz::decompress_disc_to_wbfs_cancellable(
                &input, &output, progress, cancel,
            )
            .await
            .map_err(anyhow::Error::from)
        } else {
            crate::nintendo::rvz::decompress_disc_cancellable(&input, &output, progress, cancel)
                .await
                .map_err(anyhow::Error::from)
        }
    })
    .await
}

async fn migrate_disc(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
    allowed: &'static [crate::nintendo::legacy_input::LegacyFormat],
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = output_or(&req, || crate::nintendo::rvz::derive_rvz_path(&input))?;
    let plan = prepare_output(
        progress,
        &req,
        &input,
        &desired,
        &req.operation,
        OutputVerify::Rvz,
        &cancel,
    )
    .await?;
    let Some(output) = plan.output else {
        return Ok(skipped(&input, &desired, &req.operation));
    };
    if let Some(line) = plan.line {
        return Ok(RunResponse::ok(
            "Dry run planned.",
            Some(RunData::Plan(line)),
        ));
    }
    let mut opts = RvzCompressOptions::default();
    if let Some(level) = opt_i32(&req, "level")? {
        opts.compression_level = level;
    }
    if let Some(chunk_size) = opt_u32(&req, "chunk_size")? {
        opts.chunk_size = chunk_size;
    }
    let migrate = MigrateOptions {
        skip_verify: opt_bool(&req, "skip_verify").unwrap_or(false),
        deep_verify: opt_bool(&req, "deep").unwrap_or(false)
            || opt_bool(&req, "deep_verify").unwrap_or(false),
    };
    run_file_op(&input, &output, &req.operation, || async {
        migrate_disc_cancellable(&input, &output, opts, migrate, allowed, progress, cancel)
            .await
            .map_err(anyhow::Error::from)
    })
    .await
}

async fn hash(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let algos = opt_str(&req, "algo")
        .map(|s| parse_algos(s).map_err(invalid_arg))
        .transpose()?
        .unwrap_or_else(|| vec![HashAlgo::Crc32, HashAlgo::Sha1]);
    let digest = hash_file_cancellable(&input, &algos, progress, &cancel)?;
    Ok(RunResponse::ok(
        "Hash complete.",
        Some(RunData::Hash(digest)),
    ))
}

async fn ctr_decrypt(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = output_or(&req, || crate::nintendo::ctr::derive_decrypted_path(&input))?;
    let plan = prepare_output(
        progress,
        &req,
        &input,
        &desired,
        "ctr.decrypt",
        OutputVerify::None,
        &cancel,
    )
    .await?;
    let Some(output) = plan.output else {
        return Ok(skipped(&input, &desired, "ctr.decrypt"));
    };
    if let Some(line) = plan.line {
        return Ok(RunResponse::ok(
            "Dry run planned.",
            Some(RunData::Plan(line)),
        ));
    }
    run_file_op(&input, &output, "ctr.decrypt", || async {
        crate::nintendo::ctr::decrypt_rom_cancellable(&input, &output, progress, cancel).await
    })
    .await
}

async fn ctr_encrypt(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = output_or(&req, || crate::nintendo::ctr::derive_encrypted_path(&input))?;
    let plan = prepare_output(
        progress,
        &req,
        &input,
        &desired,
        "ctr.encrypt",
        OutputVerify::None,
        &cancel,
    )
    .await?;
    let Some(output) = plan.output else {
        return Ok(skipped(&input, &desired, "ctr.encrypt"));
    };
    if let Some(line) = plan.line {
        return Ok(RunResponse::ok(
            "Dry run planned.",
            Some(RunData::Plan(line)),
        ));
    }
    run_file_op(&input, &output, "ctr.encrypt", || async {
        crate::nintendo::ctr::encrypt_rom_cancellable(&input, &output, progress, cancel).await
    })
    .await
}

async fn ctr_compress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = output_or(&req, || {
        crate::nintendo::ctr::z3ds::derive_compressed_path(&input)
    })?;
    let plan = prepare_output(
        progress,
        &req,
        &input,
        &desired,
        "ctr.compress",
        OutputVerify::None,
        &cancel,
    )
    .await?;
    let Some(output) = plan.output else {
        return Ok(skipped(&input, &desired, "ctr.compress"));
    };
    if let Some(line) = plan.line {
        return Ok(RunResponse::ok(
            "Dry run planned.",
            Some(RunData::Plan(line)),
        ));
    }
    run_file_op(&input, &output, "ctr.compress", || async {
        crate::nintendo::ctr::z3ds::compress_rom_cancellable(
            &input,
            &output,
            opt_i32(&req, "level")?,
            opt_bool(&req, "allow_encrypted").unwrap_or(false),
            progress,
            cancel,
        )
        .await
        .map_err(anyhow::Error::from)
    })
    .await
}

async fn ctr_decompress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = output_or(&req, || {
        crate::nintendo::ctr::z3ds::derive_decompressed_path(&input)
    })?;
    let plan = prepare_output(
        progress,
        &req,
        &input,
        &desired,
        "ctr.decompress",
        OutputVerify::None,
        &cancel,
    )
    .await?;
    let Some(output) = plan.output else {
        return Ok(skipped(&input, &desired, "ctr.decompress"));
    };
    if let Some(line) = plan.line {
        return Ok(RunResponse::ok(
            "Dry run planned.",
            Some(RunData::Plan(line)),
        ));
    }
    run_file_op(&input, &output, "ctr.decompress", || async {
        crate::nintendo::ctr::z3ds::decompress_rom_cancellable(&input, &output, progress, cancel)
            .await
            .map_err(anyhow::Error::from)
    })
    .await
}

async fn ctr_convert(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = output_or(&req, || {
        crate::nintendo::ctr::convert::derive_converted_path(&input)
    })?;
    let plan = prepare_output(
        progress,
        &req,
        &input,
        &desired,
        "ctr.convert",
        OutputVerify::None,
        &cancel,
    )
    .await?;
    let Some(output) = plan.output else {
        return Ok(skipped(&input, &desired, "ctr.convert"));
    };
    if let Some(line) = plan.line {
        return Ok(RunResponse::ok(
            "Dry run planned.",
            Some(RunData::Plan(line)),
        ));
    }
    run_file_op(&input, &output, "ctr.convert", || async {
        crate::nintendo::ctr::convert::convert_rom_cancellable(&input, &output, progress, cancel)
            .await
    })
    .await
}

async fn ctr_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let result = crate::nintendo::ctr::verify::verify_ctr_cancellable(
        &input,
        &crate::nintendo::ctr::verify::CtrVerifyOptions {
            verify_content_hashes: opt_bool(&req, "content_hashes").unwrap_or(false),
        },
        progress,
        &cancel,
    )
    .await?;
    Ok(RunResponse::ok(
        "CTR verification complete.",
        Some(RunData::CtrVerify(result)),
    ))
}

async fn ctr_cdn_to_cia(
    mut req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    if req.options.output_dir.is_none() {
        req.options.output_dir = req.options.output_dir_cia.clone();
    }
    let input = required_input(&req)?;
    let cia_output = output_or(&req, || {
        let name = input
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("title");
        input.with_file_name(format!("{name}.cia"))
    })?;
    let compress = opt_bool(&req, "compress").unwrap_or(false);
    let output = if compress {
        crate::nintendo::ctr::z3ds::derive_compressed_path(&cia_output)
    } else {
        cia_output.clone()
    };
    if req.dry_run {
        return Ok(RunResponse::ok(
            "Dry run planned.",
            Some(RunData::BasicPlan(BasicPlanData {
                operation: "ctr.cdn_to_cia",
                input,
                output,
            })),
        ));
    }
    let opts = crate::nintendo::ctr::CdnToCiaOptions {
        cdn_dir: input.clone(),
        output: Some(cia_output),
        cleanup: opt_bool(&req, "cleanup").unwrap_or(false),
        recursive: false,
        ensure_ticket_exists: opt_bool(&req, "ensure_ticket_exists").unwrap_or(false),
        decrypt: opt_bool(&req, "decrypt").unwrap_or(false),
        compress,
        output_dir: opt_path(&req, "output_dir"),
        on_conflict: conflict_policy(&req)?,
    };
    run_file_op(&input, &output, "ctr.cdn_to_cia", || async {
        crate::nintendo::ctr::convert_cdn_to_cia_cancellable(opts, progress, progress, cancel).await
    })
    .await
}

async fn ctr_generate_cdn_ticket(req: RunRequest, cancel: CancelToken) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = output_or(&req, || input.join("ticket.tik"))?;
    let policy = conflict_policy(&req)?;
    let output = match resolve_conflict(&desired, policy)? {
        ConflictResolution::Write(path) => path,
        ConflictResolution::Skip => {
            return Ok(skipped(&input, &desired, "ctr.generate_cdn_ticket"));
        }
    };
    if req.dry_run {
        return Ok(RunResponse::ok(
            "Dry run planned.",
            Some(RunData::BasicPlan(BasicPlanData {
                operation: "ctr.generate_cdn_ticket",
                input,
                output,
            })),
        ));
    }
    crate::nintendo::ctr::generate_ticket_from_cdn_with_publish(
        &input,
        &output,
        &cancel,
        policy == ConflictPolicy::Overwrite,
    )
    .await?;
    Ok(
        RunResponse::ok("CDN ticket generated.", None).with_record(ReportRecord::new(
            input.display().to_string(),
            output.display().to_string(),
            "ctr.generate_cdn_ticket",
            FileStatus::Ok,
            file_len(&input),
            file_len(&output),
            0,
            None,
        )),
    )
}

async fn dol_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let result = crate::nintendo::dol::verify::verify_dol_cancellable(
        &input,
        &crate::nintendo::dol::verify::DolVerifyOptions {
            full: opt_bool(&req, "full").unwrap_or(false),
        },
        progress,
        &cancel,
    )?;
    Ok(RunResponse::ok(
        "DOL verification complete.",
        Some(RunData::DolVerify(result)),
    ))
}

async fn rvl_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let result = crate::nintendo::rvl::verify::verify_rvl_cancellable(
        &input,
        &crate::nintendo::rvl::verify::RvlVerifyOptions {
            full: opt_bool(&req, "full").unwrap_or(false),
        },
        progress,
        &cancel,
    )?;
    Ok(RunResponse::ok(
        "RVL verification complete.",
        Some(RunData::RvlVerify(result)),
    ))
}

async fn nx_compress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let keys = crate::nintendo::nx::load_keyset(opt_path(&req, "keys").as_deref())?;
    let desired = output_or(&req, || crate::nintendo::nx::derive_compressed_path(&input))?;
    let plan = prepare_output(
        progress,
        &req,
        &input,
        &desired,
        "nx.compress",
        OutputVerify::Nx(Box::new(keys.clone())),
        &cancel,
    )
    .await?;
    let Some(output) = plan.output else {
        return Ok(skipped(&input, &desired, "nx.compress"));
    };
    if let Some(line) = plan.line {
        return Ok(RunResponse::ok(
            "Dry run planned.",
            Some(RunData::Plan(line)),
        ));
    }
    let kind = crate::nintendo::nx::detect_container(&input)?;
    let mut opts = crate::nintendo::nx::NxCompressOptions::for_kind(kind);
    if let Some(level) = opt_i32(&req, "level")? {
        opts.level = level;
    }
    if let Some(mode) = opt_str(&req, "mode") {
        opts.mode = nx_mode(mode, opt_u32(&req, "block_size_exp")?)?;
    }
    run_file_op(&input, &output, "nx.compress", || async {
        crate::nintendo::nx::compress_container_async_cancellable(
            input.clone(),
            output.clone(),
            opts,
            keys,
            progress,
            cancel,
        )
        .await
        .map_err(anyhow::Error::from)
    })
    .await
}

async fn nx_decompress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let keys = crate::nintendo::nx::load_keyset(opt_path(&req, "keys").as_deref())?;
    let desired = output_or(&req, || {
        crate::nintendo::nx::derive_decompressed_path(&input)
    })?;
    let plan = prepare_output(
        progress,
        &req,
        &input,
        &desired,
        "nx.decompress",
        OutputVerify::None,
        &cancel,
    )
    .await?;
    let Some(output) = plan.output else {
        return Ok(skipped(&input, &desired, "nx.decompress"));
    };
    if let Some(line) = plan.line {
        return Ok(RunResponse::ok(
            "Dry run planned.",
            Some(RunData::Plan(line)),
        ));
    }
    run_file_op(&input, &output, "nx.decompress", || async {
        crate::nintendo::nx::decompress_container_async_cancellable(
            input.clone(),
            output.clone(),
            keys,
            progress,
            cancel,
        )
        .await
        .map_err(anyhow::Error::from)
    })
    .await
}

async fn nx_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let keys = crate::nintendo::nx::load_keyset(opt_path(&req, "keys").as_deref())?;
    let result =
        crate::nintendo::nx::verify_container_async_cancellable(input, keys, progress, cancel)
            .await?;
    Ok(RunResponse::ok(
        "NX verification complete.",
        Some(RunData::NxVerify(result)),
    ))
}

async fn wup_compress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = req
        .input
        .clone()
        .or_else(|| first_wup_input(&req))
        .ok_or_else(|| invalid_arg("input path is required"))?;
    let desired = output_or(&req, || crate::nintendo::wup::derive_wua_path(&input))?;
    let plan = prepare_output(
        progress,
        &req,
        &input,
        &desired,
        "wup.compress",
        OutputVerify::None,
        &cancel,
    )
    .await?;
    let Some(output) = plan.output else {
        return Ok(skipped(&input, &desired, "wup.compress"));
    };
    if let Some(line) = plan.line {
        return Ok(RunResponse::ok(
            "Dry run planned.",
            Some(RunData::Plan(line)),
        ));
    }
    let mut opts = crate::nintendo::wup::WupCompressOptions::default();
    if let Some(level) = opt_i32(&req, "level")? {
        opts.zstd_level = level;
    }
    let titles = wup_titles(&req, &input)?;
    run_file_op(&input, &output, "wup.compress", || async {
        crate::nintendo::wup::compress_titles_async_cancellable(
            titles,
            output.clone(),
            opts,
            progress,
            cancel,
        )
        .await
        .map_err(anyhow::Error::from)
    })
    .await
}

async fn wup_decrypt(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let output = req
        .output
        .clone()
        .ok_or_else(|| invalid_arg("output path is required"))?;
    if req.dry_run {
        return Ok(RunResponse::ok(
            "Dry run planned.",
            Some(RunData::BasicPlan(BasicPlanData {
                operation: "wup.decrypt",
                input,
                output,
            })),
        ));
    }
    crate::nintendo::wup::decrypt_nus_title_async_cancellable(
        input.clone(),
        output.clone(),
        progress,
        cancel,
    )
    .await?;
    Ok(
        RunResponse::ok("WUP decrypt complete.", None).with_record(ReportRecord::new(
            input.display().to_string(),
            output.display().to_string(),
            "wup.decrypt",
            FileStatus::Ok,
            file_len(&input),
            0,
            0,
            None,
        )),
    )
}

async fn wup_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let result = crate::nintendo::wup::verify_wup_async_cancellable(
        input,
        opt_path(&req, "key"),
        progress,
        cancel,
    )
    .await?;
    Ok(RunResponse::ok(
        "WUP verification complete.",
        Some(RunData::WupVerify(result)),
    ))
}

async fn cue_merge(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let output = req
        .output
        .clone()
        .ok_or_else(|| invalid_arg("output path is required"))?;
    let plan = prepare_output(
        progress,
        &req,
        &input,
        &output,
        "cue.merge",
        OutputVerify::None,
        &cancel,
    )
    .await?;
    let Some(output) = plan.output else {
        return Ok(skipped(&input, &output, "cue.merge"));
    };
    if let Some(line) = plan.line {
        return Ok(RunResponse::ok(
            "Dry run planned.",
            Some(RunData::Plan(line)),
        ));
    }
    run_file_op(&input, &output, "cue.merge", || async {
        if cancel.is_cancelled() {
            bail!("cancelled");
        }
        crate::cue::merge::merge_bin_cancellable(
            progress,
            input.clone(),
            output.clone(),
            true,
            cancel,
        )
        .await
        .map_err(anyhow::Error::from)
    })
    .await
}

async fn playlist_write(req: RunRequest, cancel: CancelToken) -> Result<RunResponse> {
    let input = required_input(&req)?;
    if !input.is_dir() {
        return Err(invalid_arg(format!(
            "playlist input must be a directory: {}",
            input.display()
        )));
    }
    let extensions = opt_str(&req, "extensions")
        .unwrap_or("cue,chd,iso,cso,zso")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let extension_refs = extensions.iter().map(String::as_str).collect::<Vec<_>>();
    let mode = match opt_str(&req, "playlist_mode").unwrap_or("multiple") {
        "multiple" => crate::playlist::PlaylistMode::Multiple,
        "always" => crate::playlist::PlaylistMode::Always,
        other => return Err(invalid_arg(format!("invalid playlist_mode {other:?}"))),
    };
    let plans = crate::playlist::plan_playlists_cancellable(
        &crate::playlist::PlaylistOptions {
            scan_dir: &input,
            output_dir: opt_path(&req, "output_dir").as_deref(),
            extensions: &extension_refs,
            mode,
            max_depth: opt_usize(&req, "max_depth")?,
        },
        &cancel,
    )?;
    let started = Instant::now();
    let policy = conflict_policy(&req)?;
    let mut records = Vec::new();
    let mut playlists = Vec::new();
    for plan in plans {
        if cancel.is_cancelled() {
            bail!("cancelled");
        }
        let resolution = resolve_conflict(&plan.m3u_path, policy)?;
        let output = match &resolution {
            ConflictResolution::Write(path) => path.clone(),
            ConflictResolution::Skip => plan.m3u_path.clone(),
        };
        playlists.push(PlaylistPlanData {
            base_title: plan.base_title.clone(),
            output: output.clone(),
            contents: plan.contents.clone(),
            disc_count: plan.disc_count,
            has_duplicate_numbers: plan.has_duplicate_numbers,
        });
        if req.dry_run {
            records.push(ReportRecord::new(
                input.display().to_string(),
                output.display().to_string(),
                "playlist.write",
                if matches!(resolution, ConflictResolution::Skip) {
                    FileStatus::Skipped
                } else {
                    FileStatus::Ok
                },
                0,
                0,
                0,
                None,
            ));
            continue;
        }
        match resolution {
            ConflictResolution::Write(path) => {
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&path, plan.contents).await?;
                records.push(ReportRecord::new(
                    input.display().to_string(),
                    path.display().to_string(),
                    "playlist.write",
                    FileStatus::Ok,
                    0,
                    file_len(&path),
                    0,
                    None,
                ));
            }
            ConflictResolution::Skip => records.push(ReportRecord::new(
                input.display().to_string(),
                plan.m3u_path.display().to_string(),
                "playlist.write",
                FileStatus::Skipped,
                0,
                0,
                0,
                None,
            )),
        }
    }
    let totals = totals_for_records(&records, elapsed_ms(started));
    Ok(RunResponse {
        schema: RUN_SCHEMA,
        ok: true,
        status: RunStatus::Ok.as_i32(),
        code: RunStatus::Ok.code().to_string(),
        message: batch_message(&totals),
        details: None,
        totals: Some(totals),
        records,
        events: Vec::new(),
        data: Some(RunData::Playlists(PlaylistsData { playlists })),
    })
}

async fn dat_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let algos = dat_algos(&req, "crc32,sha1")?;
    let bounds = dat_checksum_bounds(&req, &algos)?;
    let matched = dat_match_tiered(
        &input,
        &algos,
        &bounds,
        progress,
        cancel.clone(),
        opt_str(&req, "api_base"),
    )
    .await?;
    let data = dat_match_data("verify", &input, &matched);
    let record = dat_run_record(&input, &data.verdict, None);
    dat_write_report(&req, &[dat_report_record(&input, &matched, None)], &cancel)?;
    Ok(
        RunResponse::ok("DAT verification complete.", Some(RunData::DatMatch(data)))
            .with_record(record),
    )
}

async fn dat_identify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let algos = dat_algos(&req, "crc32,sha1")?;
    let bounds = dat_checksum_bounds(&req, &algos)?;
    let matched = dat_match_tiered(
        &input,
        &algos,
        &bounds,
        progress,
        cancel,
        opt_str(&req, "api_base"),
    )
    .await?;
    Ok(RunResponse::ok(
        "DAT identify complete.",
        Some(RunData::DatMatch(dat_match_data(
            "identify", &input, &matched,
        ))),
    ))
}

async fn dat_scan(
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
    let files = crate::util::fs::collect_all_files_cancellable(
        &input,
        opt_usize(&req, "max_depth")?,
        &cancel,
    )?;
    let started = Instant::now();
    let mut rows = Vec::new();
    let mut records = Vec::new();
    let mut dat_records = Vec::new();
    for file in files {
        if cancel.is_cancelled() {
            bail!("cancelled");
        }
        match dat_match(
            &file,
            &algos,
            progress,
            cancel.clone(),
            opt_str(&req, "api_base"),
        )
        .await
        {
            Ok(matched) => {
                let row = dat_match_data("scan", &file, &matched);
                records.push(dat_run_record(&file, &row.verdict, None));
                dat_records.push(dat_report_record(&file, &matched, None));
                rows.push(row);
            }
            Err(err) => {
                if cancel.is_cancelled() || is_cancelled_error(&err) {
                    return Err(err);
                }
                let error = err.to_string();
                records.push(dat_run_record(&file, "failed", Some(error.clone())));
                dat_records.push(dat_error_report_record(&file, error.clone()));
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

enum RenameResolution {
    Write(PathBuf),
    Skip(PathBuf),
    Failed(PathBuf, String),
}

struct StagedRename {
    from: PathBuf,
    to: PathBuf,
    temp: Option<tempfile::TempPath>,
    backup: Option<tempfile::TempPath>,
    published: bool,
}

fn cancelled_io() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled")
}

fn rollback_renames(moves: &mut [StagedRename]) -> std::io::Result<()> {
    let mut first_error = None;
    for item in moves.iter_mut().filter(|item| item.published) {
        let result = crate::util::scratch_output_path(&item.to).and_then(|temp| {
            std::fs::remove_file(&temp)?;
            std::fs::rename(&item.to, &temp)?;
            item.temp = Some(temp);
            item.published = false;
            Ok(())
        });
        if let Err(err) = result {
            if let Some(backup) = item.backup.take() {
                let _ = backup.keep();
            }
            first_error.get_or_insert(err);
        }
    }
    for item in moves.iter_mut().filter(|item| !item.published) {
        if let Some(temp) = item.temp.take()
            && let Err(err) = crate::util::restore_temp(temp, &item.from)
        {
            first_error.get_or_insert(err);
        }
        if let Some(backup) = item.backup.take()
            && let Err(err) = crate::util::restore_temp(backup, &item.to)
        {
            first_error.get_or_insert(err);
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn rename_transaction(
    pairs: &[(PathBuf, PathBuf)],
    overwrite: bool,
    cancel: &CancelToken,
) -> std::io::Result<()> {
    let mut moves = Vec::with_capacity(pairs.len());
    for (from, to) in pairs {
        if cancel.is_cancelled() {
            rollback_renames(&mut moves)?;
            return Err(cancelled_io());
        }
        let temp = match crate::util::scratch_output_path(from) {
            Ok(temp) => temp,
            Err(err) => {
                rollback_renames(&mut moves)?;
                return Err(err);
            }
        };
        if let Err(err) = std::fs::remove_file(&temp) {
            rollback_renames(&mut moves)?;
            return Err(err);
        }
        if let Err(err) = std::fs::rename(from, &temp) {
            rollback_renames(&mut moves)?;
            return Err(err);
        }
        moves.push(StagedRename {
            from: from.clone(),
            to: to.clone(),
            temp: Some(temp),
            backup: None,
            published: false,
        });
    }

    if cancel.is_cancelled() {
        rollback_renames(&mut moves)?;
        return Err(cancelled_io());
    }
    if overwrite {
        for index in 0..moves.len() {
            match crate::util::backup_existing(&moves[index].to) {
                Ok(backup) => moves[index].backup = backup,
                Err(err) => {
                    rollback_renames(&mut moves)?;
                    return Err(err);
                }
            }
        }
    }

    for index in 0..moves.len() {
        if cancel.is_cancelled() {
            rollback_renames(&mut moves)?;
            return Err(cancelled_io());
        }
        let temp = moves[index].temp.take().expect("staged source");
        let result = if overwrite {
            temp.persist(&moves[index].to)
        } else {
            temp.persist_noclobber(&moves[index].to)
        };
        match result {
            Ok(_) => moves[index].published = true,
            Err(err) => {
                let error = err.error;
                moves[index].temp = Some(err.path);
                rollback_renames(&mut moves)?;
                return Err(error);
            }
        }
    }
    Ok(())
}

async fn dat_rename(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let files = if input.is_dir() {
        crate::util::fs::collect_all_files_cancellable(
            &input,
            opt_usize(&req, "max_depth")?,
            &cancel,
        )?
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
            bail!("cancelled");
        }
        match dat_match(
            &file,
            &algos,
            progress,
            cancel.clone(),
            opt_str(&req, "api_base"),
        )
        .await
        {
            Ok(matched) => candidates.push(dat_rename_candidate(file, &matched)),
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
        bail!("cancelled");
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
            bail!("cancelled");
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
        records.push(ReportRecord::new(
            plan.from.display().to_string(),
            output
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            "dat.rename",
            status,
            file_len(&plan.from),
            output.as_deref().map(file_len).unwrap_or(0),
            0,
            error.clone(),
        ));
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

async fn dat_fixdat(
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
    for file in crate::util::fs::collect_all_files_cancellable(
        &input,
        opt_usize(&req, "max_depth")?,
        &cancel,
    )? {
        if cancel.is_cancelled() {
            bail!("cancelled");
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
    let client = PlaymatchClient::new(opt_str(&req, "api_base"));
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
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    tokio::fs::create_dir_all(parent).await?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    write_fixdat_xml_cancellable(&mut temp, &dat, &missing, &cancel)?;
    std::io::Write::flush(&mut temp)?;
    temp.as_file().sync_all()?;
    if cancel.is_cancelled() {
        bail!("cancelled");
    }
    if policy == ConflictPolicy::Overwrite {
        temp.persist(&output).map_err(|err| err.error)?;
    } else {
        temp.persist_noclobber(&output).map_err(|err| err.error)?;
    }
    Ok(RunResponse::ok(
        "Fixdat written.",
        Some(RunData::FixdatWritten(FixdatWrittenData {
            dat_file: dat,
            missing_count: missing.len(),
        })),
    )
    .with_record(ReportRecord::new(
        input.display().to_string(),
        output.display().to_string(),
        "dat.fixdat",
        FileStatus::Ok,
        0,
        file_len(&output),
        0,
        None,
    )))
}

fn info(req: RunRequest) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let keys_path = opt_path(&req, "keys");
    let info = crate::info::read_info(
        &input,
        &crate::info::InfoOptions {
            keys_path,
            parent_path: None,
        },
    )?;
    Ok(RunResponse::ok("Info read.", Some(RunData::Info(info))))
}

struct PreparedOutput {
    output: Option<PathBuf>,
    line: Option<PlanLine>,
}

async fn prepare_output(
    progress: &dyn ProgressReporter,
    req: &RunRequest,
    input: &Path,
    desired: &Path,
    operation: &str,
    verify: OutputVerify,
    cancel: &CancelToken,
) -> Result<PreparedOutput> {
    let policy = conflict_policy(req)?;
    let resolution = resolve_conflict(desired, policy)?;
    if req.dry_run {
        let output = match &resolution {
            ConflictResolution::Write(p) => p.clone(),
            ConflictResolution::Skip => desired.to_path_buf(),
        };
        let decision = if policy == ConflictPolicy::OverwriteInvalid && desired.exists() {
            match verify_existing_output_cancellable(progress, desired, verify, cancel.clone())
                .await?
            {
                VerifyOutcome::Valid => crate::util::PlanDecision::KeepValid,
                VerifyOutcome::Invalid => crate::util::PlanDecision::RewriteInvalid,
            }
        } else {
            crate::util::classify(desired, &resolution)
        };
        return Ok(PreparedOutput {
            output: Some(output.clone()),
            line: Some(PlanLine {
                operation: operation.to_string(),
                input: input.to_path_buf(),
                output,
                decision,
                media: None,
                missing_keys: None,
            }),
        });
    }

    match resolution {
        ConflictResolution::Write(path) => Ok(PreparedOutput {
            output: Some(path),
            line: None,
        }),
        ConflictResolution::Skip
            if policy == ConflictPolicy::OverwriteInvalid && desired.exists() =>
        {
            match verify_existing_output_cancellable(progress, desired, verify, cancel.clone())
                .await?
            {
                VerifyOutcome::Valid => Ok(PreparedOutput {
                    output: None,
                    line: None,
                }),
                VerifyOutcome::Invalid => Ok(PreparedOutput {
                    output: Some(desired.to_path_buf()),
                    line: None,
                }),
            }
        }
        ConflictResolution::Skip => Ok(PreparedOutput {
            output: None,
            line: None,
        }),
    }
}

async fn run_file_op<F, Fut>(
    input: &Path,
    output: &Path,
    operation: &str,
    run: F,
) -> Result<RunResponse>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    if let Some(parent) = output.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let started = Instant::now();
    let input_bytes = file_len(input);
    run().await?;
    let output_bytes = file_len(output);
    let record = ReportRecord::new(
        input.display().to_string(),
        output.display().to_string(),
        operation,
        FileStatus::Ok,
        input_bytes,
        output_bytes,
        elapsed_ms(started),
        None,
    );
    let mut response = RunResponse::ok(
        "Operation complete.",
        Some(RunData::Comparison(RunComparisonData {
            comparison: comparison_data(input, output, input_bytes, output_bytes),
        })),
    )
    .with_record(record);
    if input_bytes == 0 && output_bytes == 0 {
        response.data = None;
    }
    Ok(response)
}

fn skipped(input: &Path, desired: &Path, operation: &str) -> RunResponse {
    RunResponse::ok("Skipped existing output.", None).with_record(ReportRecord::new(
        input.display().to_string(),
        desired.display().to_string(),
        operation,
        FileStatus::Skipped,
        0,
        0,
        0,
        None,
    ))
}

fn totals_for(record: &ReportRecord) -> ReportTotals {
    totals_for_records(std::slice::from_ref(record), record.elapsed_ms)
}

fn totals_for_records(records: &[ReportRecord], elapsed_ms: u64) -> ReportTotals {
    let mut totals = ReportTotals {
        total_files: records.len(),
        elapsed_ms,
        ..ReportTotals::default()
    };
    for record in records {
        match record.status {
            FileStatus::Ok => totals.ok += 1,
            FileStatus::Skipped => totals.skipped += 1,
            FileStatus::Failed => totals.failed += 1,
        }
        totals.total_input_bytes += record.input_bytes;
        totals.total_output_bytes += record.output_bytes;
    }
    totals
}

fn batch_message(totals: &ReportTotals) -> String {
    if totals.failed == 0 {
        format!(
            "{} files completed ({} ok, {} skipped).",
            totals.total_files, totals.ok, totals.skipped
        )
    } else {
        format!("{} of {} files failed.", totals.failed, totals.total_files)
    }
}

fn child_options(options: &RunOptions) -> RunOptions {
    let mut options = options.clone();
    options.recursive = None;
    options.report = None;
    options
}

fn batch_exts(operation: &str) -> Result<&'static [&'static str]> {
    match operation {
        "cso.compress" => Ok(&["iso"]),
        "cso.decompress" | "cso.verify" | "cso.to_chd" | "cso.to-chd" => Ok(&["cso", "zso", "dax"]),
        "chd.compress" => Ok(&["iso", "cue"]),
        "chd.extract" | "chd.verify" | "chd.to_cso" | "chd.to-cso" => Ok(&["chd"]),
        "dol.compress" => Ok(&["iso", "gcm"]),
        "rvl.compress" => Ok(&["iso", "wbfs"]),
        "rvz.compress" => Ok(&["iso", "gcm", "wbfs"]),
        "rvz.decompress" | "dol.decompress" | "rvl.decompress" => Ok(&["rvz"]),
        "dol.migrate" => Ok(&["gcz", "iso"]),
        "rvl.migrate" | "rvz.migrate" => Ok(&["wia", "gcz", "iso"]),
        "ctr.decrypt" | "ctr.encrypt" => Ok(&["cia", "3ds", "cci", "cxi"]),
        "ctr.compress" => Ok(&["cia", "cci", "3ds", "cxi", "3dsx"]),
        "ctr.decompress" => Ok(&["zcia", "zcci", "zcxi", "z3dsx"]),
        "ctr.convert" => Ok(&["cia", "3ds", "cci"]),
        "ctr.verify" => Ok(&["cia", "3ds", "cci", "cxi", "zcia", "zcci", "zcxi"]),
        "dol.verify" => Ok(&["iso", "gcm", "rvz", "gcz"]),
        "rvl.verify" => Ok(&["iso", "wbfs", "rvz", "wia", "gcz"]),
        "nx.compress" => Ok(&["nsp", "xci", "nca"]),
        "nx.decompress" => Ok(&["nsz", "xcz", "ncz"]),
        "nx.verify" => Ok(&["nsp", "xci", "nca", "nsz", "xcz", "ncz"]),
        "wup.compress" => Ok(&["wud", "wux"]),
        "wup.verify" => Ok(&["wud", "wux", "wua"]),
        "cue.merge" => Ok(&["cue"]),
        "hash" => Ok(&[
            "iso", "gcm", "wbfs", "rvz", "gcz", "wia", "nkit", "chd", "cso", "zso", "dax", "cue",
            "cia", "3ds", "cci", "cxi", "3dsx", "zcia", "zcci", "zcxi", "z3dsx", "nsp", "xci",
            "nca", "nsz", "xcz", "ncz", "wud", "wux",
        ]),
        other => Err(invalid_arg(format!(
            "operation {other:?} does not support recursive runs"
        ))),
    }
}

fn wup_titles(req: &RunRequest, fallback: &Path) -> Result<Vec<crate::nintendo::wup::TitleInput>> {
    let Some(inputs) = req.options.inputs.as_ref() else {
        return Ok(vec![crate::nintendo::wup::TitleInput::auto(
            fallback.to_path_buf(),
        )]);
    };
    let mut titles = Vec::with_capacity(inputs.len());
    for input in inputs {
        match input {
            WupTitleInputOption::Path(path) => {
                titles.push(crate::nintendo::wup::TitleInput::auto(path))
            }
            WupTitleInputOption::Object {
                path,
                format,
                key,
                key_path,
            } => {
                let format = format.as_deref().map(wup_format).transpose()?;
                titles.push(crate::nintendo::wup::TitleInput {
                    dir: path.clone(),
                    format,
                    key_path: key.clone().or_else(|| key_path.clone()),
                });
            }
        }
    }
    if titles.is_empty() {
        return Err(invalid_arg("options.inputs must not be empty"));
    }
    Ok(titles)
}

fn dat_algos(req: &RunRequest, default: &str) -> Result<Vec<HashAlgo>> {
    let value = opt_str(req, "algo").unwrap_or(default);
    parse_algos(value).map_err(invalid_arg)
}

fn dat_checksum_bounds(req: &RunRequest, algos: &[HashAlgo]) -> Result<ChecksumBounds> {
    let min = parse_checksum_bound(opt_str(req, "input_checksum_min").unwrap_or("crc32"))
        .map_err(invalid_arg)?;
    let max = parse_checksum_bound(opt_str(req, "input_checksum_max").unwrap_or("sha256"))
        .map_err(invalid_arg)?;
    let bounds = ChecksumBounds::new(min, max).map_err(invalid_arg)?;
    bounds.validate_requested(algos).map_err(invalid_arg)?;
    Ok(bounds)
}

async fn dat_match_tiered(
    input: &Path,
    algos: &[HashAlgo],
    bounds: &ChecksumBounds,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
    api_base: Option<&str>,
) -> Result<GameAndRelationMatchResult> {
    let tierable = crate::dat::is_raw_reread_cheap(input) || has_ext(input, "cue");
    let (floor, escalation) = if tierable {
        bounds.split(algos)
    } else {
        (algos.to_vec(), Vec::new())
    };
    let mut matched = dat_match(input, &floor, progress, cancel.clone(), api_base).await?;
    if !escalation.is_empty()
        && match_strength(matched.game_match_type) == MatchStrength::NameSizeHint
    {
        let full = floor.into_iter().chain(escalation).collect::<Vec<_>>();
        matched = dat_match(input, &full, progress, cancel, api_base).await?;
    }
    Ok(matched)
}

async fn dat_match(
    input: &Path,
    algos: &[HashAlgo],
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
    api_base: Option<&str>,
) -> Result<GameAndRelationMatchResult> {
    let digests = crate::dat::digest_inner_async(
        input.to_path_buf(),
        algos.to_vec(),
        progress,
        cancel.clone(),
    )
    .await?;
    let file_name = input
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let search = GameFileMatchSearch::from_digests(file_name, dat_primary_digests(&digests));
    PlaymatchClient::new(api_base)
        .identify_relations(&search, &cancel)
        .await
        .map_err(anyhow::Error::from)
}

fn dat_primary_digests(digests: &RomDigests) -> &crate::util::FileDigests {
    match digests {
        RomDigests::Single(d) => d,
        RomDigests::Tracks { whole, .. } => whole,
    }
}

fn dat_match_data(
    kind: &'static str,
    path: &Path,
    matched: &GameAndRelationMatchResult,
) -> DatMatchData {
    let (verdict, match_algo) = match match_strength(matched.game_match_type) {
        MatchStrength::Verified(algo) => (DatVerdict::Verified.as_str(), Some(algo.label())),
        MatchStrength::NameSizeHint => (DatVerdict::Hint.as_str(), None),
        MatchStrength::NoMatch => (DatVerdict::Unknown.as_str(), None),
    };
    DatMatchData {
        kind,
        path: path.to_path_buf(),
        verdict: verdict.to_string(),
        match_algo: match_algo.map(str::to_string),
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
        matched: Some(matched.clone()),
        error: None,
    }
}

fn dat_report_record(
    path: &Path,
    matched: &GameAndRelationMatchResult,
    error: Option<String>,
) -> DatReportRecord {
    let data = dat_match_data("dat", path, matched);
    DatReportRecord {
        path: path.display().to_string(),
        verdict: data.verdict,
        game_name: data.game_name,
        game_id: matched.game.as_ref().map(|g| g.id.clone()),
        platform: data.platform,
        signature_group: data.signature_group,
        dat_file_name: data.dat_file,
        dat_file_id: data.dat_file_id,
        dat_version: data.dat_version,
        match_algo: data.match_algo,
        detail: None,
        size_bytes: file_len(path),
        status: if error.is_some() {
            FileStatus::Failed
        } else {
            FileStatus::Ok
        },
        elapsed_ms: 0,
        error,
    }
}

fn dat_error_report_record(path: &Path, error: String) -> DatReportRecord {
    DatReportRecord {
        path: path.display().to_string(),
        verdict: DatVerdict::Failed.as_str().to_string(),
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
        status: FileStatus::Failed,
        elapsed_ms: 0,
        error: Some(error),
    }
}

fn dat_run_record(path: &Path, verdict: &str, error: Option<String>) -> ReportRecord {
    ReportRecord::new(
        path.display().to_string(),
        String::new(),
        "dat",
        if error.is_some() {
            FileStatus::Failed
        } else {
            FileStatus::Ok
        },
        file_len(path),
        0,
        0,
        error.or_else(|| (verdict == "failed").then(|| "DAT operation failed".to_string())),
    )
}

fn dat_batch_response(
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

fn dat_write_report(
    req: &RunRequest,
    records: &[DatReportRecord],
    cancel: &CancelToken,
) -> Result<()> {
    let Some(path) = opt_path(req, "report") else {
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
    write_dat_report_cancellable(
        &path,
        records,
        &totals,
        ReportFormat::from_path(&path),
        cancel,
    )?;
    Ok(())
}

fn dat_rename_candidate(path: PathBuf, matched: &GameAndRelationMatchResult) -> RenameCandidate {
    let verified = match_strength(matched.game_match_type).is_verified();
    RenameCandidate {
        path,
        game_id: matched.game.as_ref().map(|g| g.id.clone()),
        game_name: matched.game.as_ref().map(|g| g.name.clone()),
        file_name: (matched.game_files.len() == 1).then(|| matched.game_files[0].file_name.clone()),
        verified,
    }
}

async fn resolve_dat_file(
    req: &RunRequest,
    client: &PlaymatchClient,
    cancel: &CancelToken,
) -> Result<crate::dat::model::DatFileSummary> {
    let mut filter = crate::dat::DatFileFilter {
        name: opt_str(req, "dat_name").map(str::to_string),
        subset: opt_str(req, "subset").map(str::to_string),
        ..Default::default()
    };
    if let Some(platform) = opt_str(req, "platform") {
        let matches = client.platforms_search(platform, cancel).await?;
        filter.platform_id = matches.first().map(|p| p.id.clone());
    }
    let dat_id = opt_str(req, "dat_id");
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

fn first_wup_input(req: &RunRequest) -> Option<PathBuf> {
    let first = req.options.inputs.as_ref()?.first()?;
    match first {
        WupTitleInputOption::Path(path) => Some(path.clone()),
        WupTitleInputOption::Object { path, .. } => Some(path.clone()),
    }
}

fn wup_format(value: &str) -> Result<crate::nintendo::wup::TitleInputFormat> {
    match value {
        "loadiine" => Ok(crate::nintendo::wup::TitleInputFormat::Loadiine),
        "nus" => Ok(crate::nintendo::wup::TitleInputFormat::Nus),
        "disc" => Ok(crate::nintendo::wup::TitleInputFormat::Disc),
        other => Err(invalid_arg(format!("invalid WUP input format {other:?}"))),
    }
}

fn required_input(req: &RunRequest) -> Result<PathBuf> {
    req.input
        .clone()
        .ok_or_else(|| invalid_arg("input path is required"))
}

fn output_or(req: &RunRequest, default: impl FnOnce() -> PathBuf) -> Result<PathBuf> {
    if req.output.is_some() && opt_str(req, "output_template").is_some() {
        return Err(invalid_arg(
            "output_template conflicts with an explicit output path",
        ));
    }
    if let Some(output) = req.output.clone() {
        return Ok(output);
    }
    let derived = default();
    if let (Some(template), Some(input)) = (opt_str(req, "output_template"), req.input.as_deref()) {
        let ext = derived.extension().and_then(|s| s.to_str()).unwrap_or("");
        let keys_path = opt_path(req, "keys");
        let info = crate::info::read_info(
            input,
            &crate::info::InfoOptions {
                keys_path,
                parent_path: None,
            },
        )
        .ok();
        let tokens = crate::util::TemplateTokens::new(info.as_ref(), input, ext);
        let rel = crate::util::apply_template(template, &tokens)?;
        let base = opt_path(req, "output_dir")
            .or_else(|| input.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        return Ok(base.join(rel));
    }
    Ok(crate::util::place_in_dir(
        &derived,
        opt_path(req, "output_dir").as_deref(),
    ))
}

fn opt_str<'a>(req: &'a RunRequest, key: &str) -> Option<&'a str> {
    match key {
        "format" => req.options.format.as_deref(),
        "mode" => req.options.mode.as_deref(),
        "algo" => req.options.algo.as_deref(),
        "extensions" => req.options.extensions.as_deref(),
        "playlist_mode" => req.options.playlist_mode.as_deref(),
        "api_base" => req.options.api_base.as_deref(),
        "input_checksum_min" => req.options.input_checksum_min.as_deref(),
        "input_checksum_max" => req.options.input_checksum_max.as_deref(),
        "on_conflict" => req.options.on_conflict.as_deref(),
        "output_template" => req.options.output_template.as_deref(),
        "preset" => req.options.preset.as_deref(),
        "platform" => req.options.platform.as_deref(),
        "dat_id" => req.options.dat_id.as_deref(),
        "dat_name" => req.options.dat_name.as_deref(),
        "subset" => req.options.subset.as_deref(),
        _ => None,
    }
}

fn opt_bool(req: &RunRequest, key: &str) -> Option<bool> {
    match key {
        "recursive" => req.options.recursive,
        "full" => req.options.full,
        "allow_zstd" => req.options.allow_zstd,
        "fix" => req.options.fix,
        "skip_verify" => req.options.skip_verify,
        "deep" => req.options.deep,
        "deep_verify" => req.options.deep_verify,
        "allow_encrypted" => req.options.allow_encrypted,
        "content_hashes" => req.options.content_hashes,
        "compress" => req.options.compress,
        "cleanup" => req.options.cleanup,
        "ensure_ticket_exists" => req.options.ensure_ticket_exists,
        "decrypt" => req.options.decrypt,
        _ => None,
    }
}

fn opt_i32(req: &RunRequest, key: &str) -> Result<Option<i32>> {
    Ok(match key {
        "level" => req.options.level,
        _ => None,
    })
}

fn opt_u32(req: &RunRequest, key: &str) -> Result<Option<u32>> {
    Ok(match key {
        "block_size" => req.options.block_size,
        "hunk_size" => req.options.hunk_size,
        "chunk_size" => req.options.chunk_size,
        "block_size_exp" => req.options.block_size_exp,
        _ => None,
    })
}

fn opt_usize(req: &RunRequest, key: &str) -> Result<Option<usize>> {
    Ok(match key {
        "max_depth" => req.options.max_depth,
        _ => None,
    })
}

fn opt_path(req: &RunRequest, key: &str) -> Option<PathBuf> {
    match key {
        "config" => req.options.config.clone(),
        "report" => req.options.report.clone(),
        "output_dir" => req.options.output_dir.clone(),
        "parent" => req.options.parent.clone(),
        "keys" => req.options.keys.clone(),
        "key" => req.options.key.clone(),
        _ => None,
    }
}

fn conflict_policy(req: &RunRequest) -> Result<ConflictPolicy> {
    match opt_str(req, "on_conflict").unwrap_or("error") {
        "error" => Ok(ConflictPolicy::Error),
        "overwrite" => Ok(ConflictPolicy::Overwrite),
        "skip" => Ok(ConflictPolicy::Skip),
        "rename" => Ok(ConflictPolicy::Rename),
        "overwrite-invalid" | "overwrite_invalid" => Ok(ConflictPolicy::OverwriteInvalid),
        other => Err(invalid_arg(format!("invalid on_conflict value {other:?}"))),
    }
}

fn cso_format(value: &str) -> Result<CsoFormat> {
    match value {
        "cso" | "CSO" => Ok(CsoFormat::Cso),
        "zso" | "ZSO" => Ok(CsoFormat::Zso),
        "dax" | "DAX" => Err(invalid_arg(
            "DAX is decode-only and cannot be a compression target",
        )),
        other => Err(invalid_arg(format!("invalid CSO format {other:?}"))),
    }
}

fn disc_mode(value: Option<&str>) -> Result<Option<DiscMode>> {
    match value {
        None | Some("auto") => Ok(None),
        Some("cd") => Ok(Some(DiscMode::Cd)),
        Some("dvd") => Ok(Some(DiscMode::Dvd)),
        Some(other) => Err(invalid_arg(format!("invalid CHD mode {other:?}"))),
    }
}

fn nx_mode(value: &str, block_size_exp: Option<u32>) -> Result<crate::nintendo::nx::NczMode> {
    match value {
        "solid" => Ok(crate::nintendo::nx::NczMode::Solid),
        "block" => Ok(crate::nintendo::nx::NczMode::Block {
            size_exp: block_size_exp
                .map(u8::try_from)
                .transpose()
                .map_err(|_| invalid_arg("options.block_size_exp must fit in u8"))?
                .unwrap_or(20),
        }),
        other => Err(invalid_arg(format!("invalid NX mode {other:?}"))),
    }
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn has_ext(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case(ext))
        .unwrap_or(false)
}

fn comparison_data(
    input: &Path,
    output: &Path,
    input_bytes: u64,
    output_bytes: u64,
) -> ComparisonData {
    let ratio_pct = (input_bytes > 0).then(|| {
        let saved = (1.0 - output_bytes as f64 / input_bytes as f64) * 100.0;
        (saved * 10.0).round() / 10.0
    });
    ComparisonData {
        input_bytes,
        output_bytes,
        ratio_pct,
        input_format: path_ext(input).to_ascii_uppercase(),
        output_format: path_ext(output).to_ascii_uppercase(),
    }
}

fn path_ext(path: &Path) -> &str {
    path.extension().and_then(|s| s.to_str()).unwrap_or("")
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

fn error_chain(err: &anyhow::Error) -> Option<String> {
    let details = err
        .chain()
        .skip(1)
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    (!details.is_empty()).then(|| details.join(": "))
}

pub fn is_cancelled_error(err: &anyhow::Error) -> bool {
    use crate::chd::error::ChdError;
    use crate::cso::CsoError;
    use crate::dat::DatError;
    use crate::nintendo::ctr::error::NintendoCTRError;
    use crate::nintendo::ctr::z3ds::error::Z3dsError;
    use crate::nintendo::nx::NxError;
    use crate::nintendo::rvz::RvzError;
    use crate::nintendo::wup::WupError;

    err.chain().any(|cause| {
        matches!(cause.downcast_ref::<ChdError>(), Some(ChdError::Cancelled))
            || matches!(cause.downcast_ref::<CsoError>(), Some(CsoError::Cancelled))
            || matches!(cause.downcast_ref::<RvzError>(), Some(RvzError::Cancelled))
            || matches!(cause.downcast_ref::<NxError>(), Some(NxError::Cancelled))
            || matches!(
                cause.downcast_ref::<Z3dsError>(),
                Some(Z3dsError::Cancelled)
            )
            || matches!(
                cause.downcast_ref::<NintendoCTRError>(),
                Some(NintendoCTRError::Cancelled)
            )
            || matches!(cause.downcast_ref::<WupError>(), Some(WupError::Cancelled))
            || matches!(cause.downcast_ref::<DatError>(), Some(DatError::Cancelled))
            || matches!(
                cause.downcast_ref::<crate::cue::merge::MergeError>(),
                Some(crate::cue::merge::MergeError::Cancelled)
            )
            || cause.to_string().eq_ignore_ascii_case("cancelled")
    })
}

pub async fn run_json_no_progress(request_json: &str) -> RunResponse {
    run_json_with_progress(request_json, &NoProgress, CancelToken::new()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn status_codes_are_stable() {
        assert_eq!(RunStatus::Ok.as_i32(), 0);
        assert_eq!(RunStatus::Failed.as_i32(), 1);
        assert_eq!(RunStatus::InvalidArgument.as_i32(), 2);
        assert_eq!(RunStatus::PartialFailure.as_i32(), 3);
        assert_eq!(RunStatus::Cancelled.as_i32(), 130);
        assert_eq!(RunStatus::InternalError.as_i32(), 255);
    }

    #[tokio::test]
    async fn invalid_json_returns_user_safe_error() {
        let res = run_json("{", CancelToken::new()).await;
        assert!(!res.ok);
        assert_eq!(res.status, 2);
        assert_eq!(res.code, "invalid_argument");
        assert_eq!(res.message, "Request JSON is invalid.");
    }

    #[tokio::test]
    async fn dry_run_cso_compress_returns_plan() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.iso");
        std::fs::write(&input, b"x").unwrap();
        let req = json!({
            "schema": RUN_SCHEMA,
            "operation": "cso.compress",
            "input": input,
            "dry_run": true
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        let data = serde_json::to_value(res.data.unwrap()).unwrap();
        assert_eq!(data["operation"], "cso.compress");
        assert_eq!(data["decision"], "New");
    }

    #[tokio::test]
    async fn recursive_dry_run_returns_plans_and_writes_report() {
        let dir = tempfile::tempdir().unwrap();
        let input_dir = dir.path().join("roms");
        let output_dir = dir.path().join("out");
        let report = dir.path().join("report.json");
        std::fs::create_dir(&input_dir).unwrap();
        std::fs::write(input_dir.join("a.iso"), b"a").unwrap();
        std::fs::write(input_dir.join("b.iso"), b"b").unwrap();

        let req = json!({
            "schema": RUN_SCHEMA,
            "operation": "cso.compress",
            "input": input_dir,
            "dry_run": true,
            "options": {
                "recursive": true,
                "output_dir": output_dir,
                "report": report
            }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        assert_eq!(res.totals.as_ref().unwrap().total_files, 2);
        let data = serde_json::to_value(res.data.as_ref().unwrap()).unwrap();
        assert_eq!(data["plans"].as_array().unwrap().len(), 2);
        assert!(report.exists());
    }

    #[tokio::test]
    async fn playlist_dry_run_returns_playlists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Game (Disc 1).cue"), b"").unwrap();
        std::fs::write(dir.path().join("Game (Disc 2).cue"), b"").unwrap();
        let req = json!({
            "schema": RUN_SCHEMA,
            "operation": "playlist.write",
            "input": dir.path(),
            "dry_run": true
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        let data = serde_json::to_value(res.data.as_ref().unwrap()).unwrap();
        assert_eq!(data["playlists"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn schema_manifest_lists_dat_operations() {
        let schema = schema_json();
        let ops = schema["operations"].as_array().unwrap();
        assert!(ops.iter().any(|op| op == "dat.verify"));
        assert!(ops.iter().any(|op| op == "dat.fixdat"));
        for alias in [
            "cso.to-chd",
            "chd.to-cso",
            "ctr.cdn-to-cia",
            "ctr.generate-cdn-ticket",
            "playlist",
            "info.read",
        ] {
            assert!(ops.iter().any(|op| op == alias), "missing {alias}");
        }
        assert!(
            schema["response"]["fields"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field == "schema")
        );
    }

    #[tokio::test]
    async fn disc_aliases_preserve_requested_operation_in_plans() {
        let dir = tempfile::tempdir().unwrap();
        for operation in [
            "dol.compress",
            "rvl.compress",
            "rvz.compress",
            "dol.decompress",
            "rvl.decompress",
            "rvz.decompress",
        ] {
            let input = dir.path().join(if operation.ends_with(".compress") {
                "game.iso"
            } else {
                "game.rvz"
            });
            std::fs::write(&input, b"x").unwrap();
            let req = json!({
                "operation": operation,
                "input": input,
                "output": dir.path().join(format!("{operation}.out")),
                "dry_run": true
            });
            let res = run_json(&req.to_string(), CancelToken::new()).await;
            assert!(res.ok, "{operation}: {res:?}");
            let data = serde_json::to_value(res.data.unwrap()).unwrap();
            assert_eq!(data["operation"], operation);
        }
    }

    #[tokio::test]
    async fn cdn_output_dir_alias_falls_back_and_canonical_wins() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("cdn");
        std::fs::create_dir(&input).unwrap();
        let legacy = dir.path().join("legacy");
        let canonical = dir.path().join("canonical");

        let legacy_req = json!({
            "operation": "ctr.cdn_to_cia",
            "input": input,
            "dry_run": true,
            "options": { "output_dir_cia": legacy }
        });
        let legacy_res = run_json(&legacy_req.to_string(), CancelToken::new()).await;
        let legacy_data = serde_json::to_value(legacy_res.data.unwrap()).unwrap();
        assert_eq!(
            legacy_data["output"].as_str(),
            legacy.join("cdn.cia").to_str()
        );

        let canonical_req = json!({
            "operation": "ctr.cdn_to_cia",
            "input": input,
            "dry_run": true,
            "options": { "output_dir": canonical, "output_dir_cia": legacy }
        });
        let canonical_res = run_json(&canonical_req.to_string(), CancelToken::new()).await;
        let canonical_data = serde_json::to_value(canonical_res.data.unwrap()).unwrap();
        assert_eq!(
            canonical_data["output"].as_str(),
            canonical.join("cdn.cia").to_str()
        );
    }

    #[tokio::test]
    async fn output_conflict_skip_avoids_fixdat_and_ticket_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("existing.xml");
        std::fs::write(&output, b"existing").unwrap();
        for operation in ["dat.fixdat", "ctr.generate_cdn_ticket"] {
            let req = json!({
                "operation": operation,
                "input": dir.path(),
                "output": output,
                "options": { "on_conflict": "skip" }
            });
            let res = run_json(&req.to_string(), CancelToken::new()).await;
            assert!(res.ok, "{operation}: {res:?}");
            assert_eq!(res.records[0].status, FileStatus::Skipped);
            assert_eq!(std::fs::read(&output).unwrap(), b"existing");
        }
    }

    #[tokio::test]
    async fn unknown_operation_is_invalid_argument_response() {
        let req: Value = json!({ "operation": "wat" });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(!res.ok);
        assert_eq!(res.status, 2);
        assert!(res.message.contains("unknown operation"));
    }

    #[tokio::test]
    async fn bad_schema_is_invalid_argument_response() {
        let req: Value = json!({ "schema": "wrong", "operation": "hash" });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(!res.ok);
        assert_eq!(res.status, 2);
        assert!(res.message.contains("unsupported schema"));
    }

    #[tokio::test]
    async fn recursive_file_input_is_invalid_argument_response() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.iso");
        std::fs::write(&input, b"x").unwrap();
        let req: Value = json!({
            "operation": "cso.compress",
            "input": input,
            "options": { "recursive": true }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(!res.ok);
        assert_eq!(res.status, 2);
        assert!(res.message.contains("recursive input"));
    }

    #[tokio::test]
    async fn missing_required_output_is_invalid_argument_response() {
        let req: Value = json!({
            "operation": "wup.decrypt",
            "input": "title"
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(!res.ok);
        assert_eq!(res.status, 2);
        assert!(res.message.contains("output path is required"));
    }

    #[tokio::test]
    async fn playlist_file_input_is_invalid_argument_response() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.cue");
        std::fs::write(&input, b"").unwrap();
        let req: Value = json!({
            "operation": "playlist.write",
            "input": input
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(!res.ok);
        assert_eq!(res.status, 2);
        assert!(res.message.contains("playlist input"));
    }

    #[tokio::test]
    async fn dat_scan_file_input_is_invalid_argument_response() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.iso");
        std::fs::write(&input, b"").unwrap();
        let req: Value = json!({
            "operation": "dat.scan",
            "input": input
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(!res.ok);
        assert_eq!(res.status, 2);
        assert!(res.message.contains("dat.scan input"));
    }

    #[test]
    fn config_defaults_fill_missing_options() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("rom-converto.toml");
        std::fs::write(
            &config,
            "[cso]\nblock_size = 32768\n[presets.fast.cso]\nblock_size = 65536\n",
        )
        .unwrap();
        let req = RunRequest {
            schema: Some(RUN_SCHEMA.to_string()),
            operation: "cso.compress".to_string(),
            input: None,
            output: None,
            config: Some(config),
            preset: Some("fast".to_string()),
            options: RunOptions::default(),
            dry_run: false,
        };
        let req = apply_config_defaults(req).unwrap();
        assert_eq!(req.options.block_size, Some(65536));
    }

    #[test]
    fn dat_checksum_bounds_use_defaults_and_validate_range() {
        let mut req = RunRequest {
            schema: None,
            operation: "dat.verify".to_string(),
            input: None,
            output: None,
            config: None,
            preset: None,
            options: RunOptions::default(),
            dry_run: false,
        };
        let algos = [HashAlgo::Crc32, HashAlgo::Sha1];
        assert_eq!(
            dat_checksum_bounds(&req, &algos).unwrap(),
            ChecksumBounds::new(HashAlgo::Crc32, HashAlgo::Sha256).unwrap()
        );

        req.options.input_checksum_min = Some("sha256".to_string());
        req.options.input_checksum_max = Some("crc32".to_string());
        assert!(dat_checksum_bounds(&req, &algos).is_err());
    }

    #[test]
    fn rename_transaction_handles_cycles_cross_platform() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();

        rename_transaction(
            &[(a.clone(), b.clone()), (b.clone(), a.clone())],
            true,
            &CancelToken::new(),
        )
        .unwrap();

        assert_eq!(std::fs::read(a).unwrap(), b"b");
        assert_eq!(std::fs::read(b).unwrap(), b"a");
    }

    #[test]
    fn rename_transaction_rolls_back_partial_publish() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        let x = dir.path().join("x.bin");
        let occupied = dir.path().join("occupied.bin");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        std::fs::write(&occupied, b"old").unwrap();

        assert!(
            rename_transaction(
                &[(a.clone(), x.clone()), (b.clone(), occupied.clone())],
                false,
                &CancelToken::new(),
            )
            .is_err()
        );

        assert_eq!(std::fs::read(a).unwrap(), b"a");
        assert_eq!(std::fs::read(b).unwrap(), b"b");
        assert_eq!(std::fs::read(occupied).unwrap(), b"old");
        assert!(!x.exists());
    }

    #[test]
    fn cancelled_rename_transaction_keeps_sources() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from.bin");
        let to = dir.path().join("to.bin");
        std::fs::write(&from, b"source").unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();

        assert!(rename_transaction(&[(from.clone(), to.clone())], true, &cancel).is_err());
        assert_eq!(std::fs::read(from).unwrap(), b"source");
        assert!(!to.exists());
    }
}
