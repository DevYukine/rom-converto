//! Operation table and the per-format handlers behind it: one [`OpSpec`]
//! per dispatchable operation name, plus the shared output-resolution and
//! file-op bookkeeping every convert handler runs through.

use super::dat::{dat_fixdat, dat_identify, dat_rename, dat_scan, dat_verify};
use super::models::{
    BasicPlanData, ComparisonData, PlaylistPlanData, PlaylistsData, RunComparisonData, RunData,
    RunOptions, RunPlansData, RunRequest, RunResponse, RunStatus, WupTitleInputOption,
};
use super::{RUN_SCHEMA, invalid_arg, is_cancelled_error};
use crate::chd::{ChdCodec, ChdOptions, DiscMode};
use crate::cso::{CsoCompressOptions, CsoFormat};
use crate::nintendo::legacy_input::{ALL_MIGRATE_FORMATS, DOL_MIGRATE_FORMATS, MigrateOptions};
use crate::nintendo::rvz::RvzCompressOptions;
use crate::util::fs::{file_len, has_ext};
use crate::util::{
    CancelToken, Cancelled, ConflictPolicy, ConflictResolution, FileStatus, HashAlgo, OutputVerify,
    PlanLine, ProgressReporter, ReportRecord, ReportRecordInput, ReportTotals, VerifyOutcome,
    hash_file, parse_algos, resolve_conflict, verify_existing_output,
};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub(crate) type OpFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<RunResponse>> + 'a>>;

/// One dispatchable operation: its canonical name, the aliases accepted for
/// it, the extensions a recursive run scans (`None` when the operation has
/// no batch mode), and its handler.
pub(crate) struct OpSpec {
    name: &'static str,
    aliases: &'static [&'static str],
    batch_exts: Option<&'static [&'static str]>,
    run: for<'a> fn(RunRequest, &'a dyn ProgressReporter, CancelToken) -> OpFuture<'a>,
}

pub(crate) static OPS: &[OpSpec] = &[
    OpSpec {
        name: "cso.compress",
        aliases: &[],
        batch_exts: Some(&["iso"]),
        run: |req, progress, cancel| Box::pin(cso_compress(req, progress, cancel)),
    },
    OpSpec {
        name: "cso.decompress",
        aliases: &[],
        batch_exts: Some(&["cso", "zso", "dax"]),
        run: |req, progress, cancel| Box::pin(cso_decompress(req, progress, cancel)),
    },
    OpSpec {
        name: "cso.verify",
        aliases: &[],
        batch_exts: Some(&["cso", "zso", "dax"]),
        run: |req, progress, cancel| Box::pin(cso_verify(req, progress, cancel)),
    },
    OpSpec {
        name: "cso.to_chd",
        aliases: &["cso.to-chd"],
        batch_exts: Some(&["cso", "zso", "dax"]),
        run: |req, progress, cancel| Box::pin(cso_to_chd(req, progress, cancel)),
    },
    OpSpec {
        name: "chd.compress",
        aliases: &[],
        batch_exts: Some(&["iso", "cue"]),
        run: |req, progress, cancel| Box::pin(chd_compress(req, progress, cancel)),
    },
    OpSpec {
        name: "chd.migrate",
        aliases: &[],
        batch_exts: Some(&["chd"]),
        run: |req, progress, cancel| Box::pin(chd_migrate(req, progress, cancel)),
    },
    OpSpec {
        name: "chd.extract",
        aliases: &[],
        batch_exts: Some(&["chd"]),
        run: |req, progress, cancel| Box::pin(chd_extract(req, progress, cancel)),
    },
    OpSpec {
        name: "chd.verify",
        aliases: &[],
        batch_exts: Some(&["chd"]),
        run: |req, progress, cancel| Box::pin(chd_verify(req, progress, cancel)),
    },
    OpSpec {
        name: "chd.to_cso",
        aliases: &["chd.to-cso"],
        batch_exts: Some(&["chd"]),
        run: |req, progress, cancel| Box::pin(chd_to_cso(req, progress, cancel)),
    },
    OpSpec {
        name: "dol.compress",
        aliases: &[],
        batch_exts: Some(&["iso", "gcm"]),
        run: |req, progress, cancel| Box::pin(rvz_compress(req, progress, cancel)),
    },
    OpSpec {
        name: "dol.decompress",
        aliases: &[],
        batch_exts: Some(&["rvz"]),
        run: |req, progress, cancel| Box::pin(rvz_decompress(req, progress, cancel)),
    },
    OpSpec {
        name: "dol.migrate",
        aliases: &[],
        batch_exts: Some(&["gcz", "iso"]),
        run: |req, progress, cancel| {
            Box::pin(migrate_disc(req, progress, cancel, DOL_MIGRATE_FORMATS))
        },
    },
    OpSpec {
        name: "dol.verify",
        aliases: &[],
        batch_exts: Some(&["iso", "gcm", "rvz", "gcz"]),
        run: |req, progress, cancel| Box::pin(dol_verify(req, progress, cancel)),
    },
    OpSpec {
        name: "rvl.compress",
        aliases: &[],
        batch_exts: Some(&["iso", "wbfs"]),
        run: |req, progress, cancel| Box::pin(rvz_compress(req, progress, cancel)),
    },
    OpSpec {
        name: "rvl.decompress",
        aliases: &[],
        batch_exts: Some(&["rvz"]),
        run: |req, progress, cancel| Box::pin(rvz_decompress(req, progress, cancel)),
    },
    OpSpec {
        name: "rvl.migrate",
        aliases: &[],
        batch_exts: Some(&["wia", "gcz", "iso"]),
        run: |req, progress, cancel| {
            Box::pin(migrate_disc(req, progress, cancel, ALL_MIGRATE_FORMATS))
        },
    },
    OpSpec {
        name: "rvl.verify",
        aliases: &[],
        batch_exts: Some(&["iso", "wbfs", "rvz", "wia", "gcz"]),
        run: |req, progress, cancel| Box::pin(rvl_verify(req, progress, cancel)),
    },
    OpSpec {
        name: "rvz.compress",
        aliases: &[],
        batch_exts: Some(&["iso", "gcm", "wbfs"]),
        run: |req, progress, cancel| Box::pin(rvz_compress(req, progress, cancel)),
    },
    OpSpec {
        name: "rvz.decompress",
        aliases: &[],
        batch_exts: Some(&["rvz"]),
        run: |req, progress, cancel| Box::pin(rvz_decompress(req, progress, cancel)),
    },
    OpSpec {
        name: "rvz.migrate",
        aliases: &[],
        batch_exts: Some(&["wia", "gcz", "iso"]),
        run: |req, progress, cancel| {
            Box::pin(migrate_disc(req, progress, cancel, ALL_MIGRATE_FORMATS))
        },
    },
    OpSpec {
        name: "ctr.cdn_to_cia",
        aliases: &["ctr.cdn-to-cia"],
        batch_exts: None,
        run: |req, progress, cancel| Box::pin(ctr_cdn_to_cia(req, progress, cancel)),
    },
    OpSpec {
        name: "ctr.generate_cdn_ticket",
        aliases: &["ctr.generate-cdn-ticket"],
        batch_exts: None,
        run: |req, _progress, cancel| Box::pin(ctr_generate_cdn_ticket(req, cancel)),
    },
    OpSpec {
        name: "ctr.decrypt",
        aliases: &[],
        batch_exts: Some(&["cia", "3ds", "cci", "cxi"]),
        run: |req, progress, cancel| Box::pin(ctr_decrypt(req, progress, cancel)),
    },
    OpSpec {
        name: "ctr.encrypt",
        aliases: &[],
        batch_exts: Some(&["cia", "3ds", "cci", "cxi"]),
        run: |req, progress, cancel| Box::pin(ctr_encrypt(req, progress, cancel)),
    },
    OpSpec {
        name: "ctr.compress",
        aliases: &[],
        batch_exts: Some(&["cia", "cci", "3ds", "cxi", "3dsx"]),
        run: |req, progress, cancel| Box::pin(ctr_compress(req, progress, cancel)),
    },
    OpSpec {
        name: "ctr.decompress",
        aliases: &[],
        batch_exts: Some(&["zcia", "zcci", "zcxi", "z3dsx"]),
        run: |req, progress, cancel| Box::pin(ctr_decompress(req, progress, cancel)),
    },
    OpSpec {
        name: "ctr.convert",
        aliases: &[],
        batch_exts: Some(&["cia", "3ds", "cci"]),
        run: |req, progress, cancel| Box::pin(ctr_convert(req, progress, cancel)),
    },
    OpSpec {
        name: "ctr.verify",
        aliases: &[],
        batch_exts: Some(&["cia", "3ds", "cci", "cxi", "zcia", "zcci", "zcxi"]),
        run: |req, progress, cancel| Box::pin(ctr_verify(req, progress, cancel)),
    },
    OpSpec {
        name: "nx.compress",
        aliases: &[],
        batch_exts: Some(&["nsp", "xci", "nca"]),
        run: |req, progress, cancel| Box::pin(nx_compress(req, progress, cancel)),
    },
    OpSpec {
        name: "nx.decompress",
        aliases: &[],
        batch_exts: Some(&["nsz", "xcz", "ncz"]),
        run: |req, progress, cancel| Box::pin(nx_decompress(req, progress, cancel)),
    },
    OpSpec {
        name: "nx.verify",
        aliases: &[],
        batch_exts: Some(&["nsp", "xci", "nca", "nsz", "xcz", "ncz"]),
        run: |req, progress, cancel| Box::pin(nx_verify(req, progress, cancel)),
    },
    OpSpec {
        name: "wup.compress",
        aliases: &[],
        batch_exts: Some(&["wud", "wux"]),
        run: |req, progress, cancel| Box::pin(wup_compress(req, progress, cancel)),
    },
    OpSpec {
        name: "wup.decrypt",
        aliases: &[],
        batch_exts: None,
        run: |req, progress, cancel| Box::pin(wup_decrypt(req, progress, cancel)),
    },
    OpSpec {
        name: "wup.verify",
        aliases: &[],
        batch_exts: Some(&["wud", "wux", "wua"]),
        run: |req, progress, cancel| Box::pin(wup_verify(req, progress, cancel)),
    },
    OpSpec {
        name: "cue.merge",
        aliases: &[],
        batch_exts: Some(&["cue"]),
        run: |req, progress, cancel| Box::pin(cue_merge(req, progress, cancel)),
    },
    OpSpec {
        name: "playlist.write",
        aliases: &["playlist"],
        batch_exts: None,
        run: |req, _progress, cancel| Box::pin(playlist_write(req, cancel)),
    },
    OpSpec {
        name: "dat.verify",
        aliases: &[],
        batch_exts: None,
        run: |req, progress, cancel| Box::pin(dat_verify(req, progress, cancel)),
    },
    OpSpec {
        name: "dat.scan",
        aliases: &[],
        batch_exts: None,
        run: |req, progress, cancel| Box::pin(dat_scan(req, progress, cancel)),
    },
    OpSpec {
        name: "dat.rename",
        aliases: &[],
        batch_exts: None,
        run: |req, progress, cancel| Box::pin(dat_rename(req, progress, cancel)),
    },
    OpSpec {
        name: "dat.identify",
        aliases: &[],
        batch_exts: None,
        run: |req, progress, cancel| Box::pin(dat_identify(req, progress, cancel)),
    },
    OpSpec {
        name: "dat.fixdat",
        aliases: &[],
        batch_exts: None,
        run: |req, progress, cancel| Box::pin(dat_fixdat(req, progress, cancel)),
    },
    OpSpec {
        name: "hash",
        aliases: &[],
        batch_exts: Some(&[
            "iso", "gcm", "wbfs", "rvz", "gcz", "wia", "nkit", "chd", "cso", "zso", "dax", "cue",
            "cia", "3ds", "cci", "cxi", "3dsx", "zcia", "zcci", "zcxi", "z3dsx", "nsp", "xci",
            "nca", "nsz", "xcz", "ncz", "wud", "wux",
        ]),
        run: |req, progress, cancel| Box::pin(hash(req, progress, cancel)),
    },
    OpSpec {
        name: "info",
        aliases: &["info.read"],
        batch_exts: None,
        run: |req, _progress, _cancel| Box::pin(std::future::ready(info(req))),
    },
];

pub(crate) fn find_op(operation: &str) -> Option<&'static OpSpec> {
    OPS.iter()
        .find(|op| op.name == operation || op.aliases.contains(&operation))
}

/// Every dispatchable operation name, canonical names and aliases alike.
pub(crate) fn operation_names() -> &'static [&'static str] {
    static NAMES: std::sync::LazyLock<Vec<&'static str>> = std::sync::LazyLock::new(|| {
        OPS.iter()
            .flat_map(|op| std::iter::once(op.name).chain(op.aliases.iter().copied()))
            .collect()
    });
    NAMES.as_slice()
}

pub(crate) async fn run_single_request(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let Some(op) = find_op(&req.operation) else {
        let other = &req.operation;
        return Err(invalid_arg(format!("unknown operation {other:?}")));
    };
    (op.run)(req, progress, cancel).await
}

pub(crate) async fn run_batch_request(
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
    let files =
        crate::util::fs::collect_files_with_exts(&root, exts, req.options.max_depth, &cancel)
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
            return Err(Cancelled.into());
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
                    records.push(ReportRecord::new(ReportRecordInput {
                        input_path: input.display().to_string(),
                        output_path: String::new(),
                        operation: (&req.operation).into(),
                        status: FileStatus::Ok,
                        input_bytes: file_len(&input),
                        output_bytes: 0,
                        elapsed_ms: 0,
                        error: None,
                    }));
                } else {
                    records.append(&mut response.records);
                }
            }
            Err(err) if cancel.is_cancelled() || is_cancelled_error(&err) => return Err(err),
            Err(err) => records.push(ReportRecord::new(ReportRecordInput {
                input_path: input.display().to_string(),
                output_path: String::new(),
                operation: (&req.operation).into(),
                status: FileStatus::Failed,
                input_bytes: file_len(&input),
                output_bytes: 0,
                elapsed_ms: 0,
                error: Some(err.to_string()),
            })),
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

pub(crate) async fn cso_compress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let format = cso_format(req.options.format.as_deref().unwrap_or("cso"))?;
    let input = required_input(&req)?;
    let desired = output_or(&req, || input.with_extension(format.extension()))?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            desired: &desired,
            operation: "cso.compress",
            verify: OutputVerify::Cso,
        },
        cancel,
        |input, output, cancel| async move {
            let opts = CsoCompressOptions {
                format,
                block_size: req.options.block_size,
                force: true,
            };
            crate::cso::compress_to_cso(progress, input, output, opts, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn cso_decompress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = output_or(&req, || input.with_extension("iso"))?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            desired: &desired,
            operation: "cso.decompress",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::cso::decompress_from_cso(progress, input, output, true, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn cso_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    crate::cso::verify_cso(
        progress,
        input.clone(),
        req.options.full.unwrap_or(true),
        cancel,
    )
    .await?;
    Ok(RunResponse::ok("CSO verification passed.", None))
}

pub(crate) async fn chd_compress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let req = &req;
    let input = required_input(req)?;
    let desired = output_or(req, || input.with_extension("chd"))?;
    convert_op(
        progress,
        req,
        ConvertTarget {
            input: &input,
            desired: &desired,
            operation: "chd.compress",
            verify: OutputVerify::Chd,
        },
        cancel,
        |input, output, cancel| async move {
            let opts = chd_options(req)?;
            let mode = disc_mode(req.options.mode.as_deref())?;
            crate::chd::convert_disc_to_chd(progress, input, output, mode, opts, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn chd_migrate(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let req = &req;
    let input = required_input(req)?;
    // A migrated CHD keeps the .chd extension, so the derived name carries a
    // v5 infix to stay off its own source.
    let desired = output_or(req, || crate::chd::migrated_chd_path(&input))?;
    convert_op(
        progress,
        req,
        ConvertTarget {
            input: &input,
            desired: &desired,
            operation: "chd.migrate",
            verify: OutputVerify::Chd,
        },
        cancel,
        |input, output, cancel| async move {
            crate::chd::migrate_chd_to_v5(progress, input, output, chd_options(req)?, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn chd_extract(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let req = &req;
    let input = required_input(req)?;
    let desired = output_or(req, || input.with_extension("iso"))?;
    convert_op(
        progress,
        req,
        ConvertTarget {
            input: &input,
            desired: &desired,
            operation: "chd.extract",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::chd::extract_from_chd(
                progress,
                input,
                output,
                req.options.parent.clone(),
                cancel,
            )
            .await
            .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn chd_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    crate::chd::verify_chd(
        progress,
        input,
        req.options.parent.clone(),
        req.options.fix.unwrap_or(false),
        cancel,
    )
    .await?;
    Ok(RunResponse::ok("CHD verification passed.", None))
}

pub(crate) async fn cso_to_chd(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let req = &req;
    let input = required_input(req)?;
    let desired = output_or(req, || input.with_extension("chd"))?;
    convert_op(
        progress,
        req,
        ConvertTarget {
            input: &input,
            desired: &desired,
            operation: "cso.to_chd",
            verify: OutputVerify::Chd,
        },
        cancel,
        |input, output, cancel| async move {
            let opts = chd_options(req)?;
            crate::pipeline::cso_to_chd(
                progress,
                input,
                output,
                disc_mode(req.options.mode.as_deref())?,
                opts,
                cancel,
            )
            .await
        },
    )
    .await
}

pub(crate) async fn chd_to_cso(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let format = cso_format(req.options.format.as_deref().unwrap_or("cso"))?;
    let input = required_input(&req)?;
    let desired = output_or(&req, || input.with_extension(format.extension()))?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            desired: &desired,
            operation: "chd.to_cso",
            verify: OutputVerify::Cso,
        },
        cancel,
        |input, output, cancel| async move {
            let opts = CsoCompressOptions {
                format,
                block_size: req.options.block_size,
                force: true,
            };
            crate::pipeline::chd_to_cso(progress, input, output, opts, cancel).await
        },
    )
    .await
}

pub(crate) async fn rvz_compress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let req = &req;
    let input = required_input(req)?;
    let desired = output_or(req, || crate::nintendo::rvz::derive_rvz_path(&input))?;
    convert_op(
        progress,
        req,
        ConvertTarget {
            input: &input,
            desired: &desired,
            operation: &req.operation,
            verify: OutputVerify::Rvz,
        },
        cancel,
        |input, output, cancel| async move {
            crate::nintendo::rvz::compress_disc(&input, &output, rvz_options(req), progress, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn rvz_decompress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = output_or(&req, || crate::nintendo::rvz::derive_disc_path(&input))?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            desired: &desired,
            operation: &req.operation,
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            if has_ext(&output, "wbfs") {
                crate::nintendo::rvz::decompress_disc_to_wbfs(&input, &output, progress, cancel)
                    .await
                    .map_err(anyhow::Error::from)
            } else {
                crate::nintendo::rvz::decompress_disc(&input, &output, progress, cancel)
                    .await
                    .map_err(anyhow::Error::from)
            }
        },
    )
    .await
}

pub(crate) async fn migrate_disc(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
    allowed: &'static [crate::nintendo::legacy_input::LegacyFormat],
) -> Result<RunResponse> {
    let req = &req;
    let input = required_input(req)?;
    let desired = output_or(req, || crate::nintendo::rvz::derive_rvz_path(&input))?;
    convert_op(
        progress,
        req,
        ConvertTarget {
            input: &input,
            desired: &desired,
            operation: &req.operation,
            verify: OutputVerify::Rvz,
        },
        cancel,
        |input, output, cancel| async move {
            let migrate = MigrateOptions {
                skip_verify: req.options.skip_verify.unwrap_or(false),
                deep_verify: req.options.deep.unwrap_or(false)
                    || req.options.deep_verify.unwrap_or(false),
            };
            crate::nintendo::legacy_input::migrate_disc(
                &input,
                &output,
                rvz_options(req),
                migrate,
                allowed,
                progress,
                cancel,
            )
            .await
            .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn hash(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let algos = req
        .options
        .algo
        .as_deref()
        .map(|s| parse_algos(s).map_err(invalid_arg))
        .transpose()?
        .unwrap_or_else(|| vec![HashAlgo::Crc32, HashAlgo::Sha1]);
    let digest = hash_file(&input, &algos, progress, &cancel)?;
    Ok(RunResponse::ok(
        "Hash complete.",
        Some(RunData::Hash(digest)),
    ))
}

pub(crate) async fn ctr_decrypt(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = output_or(&req, || crate::nintendo::ctr::derive_decrypted_path(&input))?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            desired: &desired,
            operation: "ctr.decrypt",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::nintendo::ctr::decrypt_rom(&input, &output, progress, cancel).await
        },
    )
    .await
}

pub(crate) async fn ctr_encrypt(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = output_or(&req, || crate::nintendo::ctr::derive_encrypted_path(&input))?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            desired: &desired,
            operation: "ctr.encrypt",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::nintendo::ctr::encrypt_rom(&input, &output, progress, cancel).await
        },
    )
    .await
}

pub(crate) async fn ctr_compress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = output_or(&req, || {
        crate::nintendo::ctr::z3ds::derive_compressed_path(&input)
    })?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            desired: &desired,
            operation: "ctr.compress",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::nintendo::ctr::z3ds::compress_rom(
                &input,
                &output,
                req.options.level,
                req.options.allow_encrypted.unwrap_or(false),
                progress,
                cancel,
            )
            .await
            .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn ctr_decompress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = output_or(&req, || {
        crate::nintendo::ctr::z3ds::derive_decompressed_path(&input)
    })?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            desired: &desired,
            operation: "ctr.decompress",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::nintendo::ctr::z3ds::decompress_rom(&input, &output, progress, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn ctr_convert(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = output_or(&req, || {
        crate::nintendo::ctr::convert::derive_converted_path(&input)
    })?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            desired: &desired,
            operation: "ctr.convert",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::nintendo::ctr::convert::convert_rom(&input, &output, progress, cancel).await
        },
    )
    .await
}

pub(crate) async fn ctr_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let result = crate::nintendo::ctr::verify::verify_ctr(
        &input,
        &crate::nintendo::ctr::verify::CtrVerifyOptions {
            verify_content_hashes: req.options.content_hashes.unwrap_or(false),
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

pub(crate) async fn ctr_cdn_to_cia(
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
    let compress = req.options.compress.unwrap_or(false);
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
        cleanup: req.options.cleanup.unwrap_or(false),
        recursive: false,
        ensure_ticket_exists: req.options.ensure_ticket_exists.unwrap_or(false),
        decrypt: req.options.decrypt.unwrap_or(false),
        compress,
        output_dir: req.options.output_dir.clone(),
        on_conflict: conflict_policy(&req)?,
    };
    run_file_op(&input, &output, "ctr.cdn_to_cia", || async {
        crate::nintendo::ctr::convert_cdn_to_cia(opts, progress, progress, cancel).await
    })
    .await
}

pub(crate) async fn ctr_generate_cdn_ticket(
    req: RunRequest,
    cancel: CancelToken,
) -> Result<RunResponse> {
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
            ReportRecordInput {
                input_path: input.display().to_string(),
                output_path: output.display().to_string(),
                operation: ("ctr.generate_cdn_ticket").into(),
                status: FileStatus::Ok,
                input_bytes: file_len(&input),
                output_bytes: file_len(&output),
                elapsed_ms: 0,
                error: None,
            },
        )),
    )
}

pub(crate) async fn dol_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let result = crate::nintendo::dol::verify::verify_dol(
        &input,
        &crate::nintendo::dol::verify::DolVerifyOptions {
            full: req.options.full.unwrap_or(false),
        },
        progress,
        &cancel,
    )?;
    Ok(RunResponse::ok(
        "DOL verification complete.",
        Some(RunData::DolVerify(result)),
    ))
}

pub(crate) async fn rvl_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let result = crate::nintendo::rvl::verify::verify_rvl(
        &input,
        &crate::nintendo::rvl::verify::RvlVerifyOptions {
            full: req.options.full.unwrap_or(false),
        },
        progress,
        &cancel,
    )?;
    Ok(RunResponse::ok(
        "RVL verification complete.",
        Some(RunData::RvlVerify(result)),
    ))
}

pub(crate) async fn nx_compress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let req = &req;
    let input = required_input(req)?;
    let keys = crate::nintendo::nx::load_keyset(req.options.keys.as_deref())?;
    let desired = output_or(req, || crate::nintendo::nx::derive_compressed_path(&input))?;
    convert_op(
        progress,
        req,
        ConvertTarget {
            input: &input,
            desired: &desired,
            operation: "nx.compress",
            verify: OutputVerify::Nx(Box::new(keys.clone())),
        },
        cancel,
        |input, output, cancel| async move {
            let kind = crate::nintendo::nx::detect_container(&input)?;
            let mut opts = crate::nintendo::nx::NxCompressOptions::for_kind(kind);
            if let Some(level) = req.options.level {
                opts.level = level;
            }
            if let Some(mode) = req.options.mode.as_deref() {
                opts.mode = nx_mode(mode, req.options.block_size_exp)?;
            }
            crate::nintendo::nx::compress_container_async(
                input, output, opts, keys, progress, cancel,
            )
            .await
            .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn nx_decompress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let keys = crate::nintendo::nx::load_keyset(req.options.keys.as_deref())?;
    let desired = output_or(&req, || {
        crate::nintendo::nx::derive_decompressed_path(&input)
    })?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            desired: &desired,
            operation: "nx.decompress",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::nintendo::nx::decompress_container_async(input, output, keys, progress, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn nx_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let keys = crate::nintendo::nx::load_keyset(req.options.keys.as_deref())?;
    let result = crate::nintendo::nx::verify_container_async(input, keys, progress, cancel).await?;
    Ok(RunResponse::ok(
        "NX verification complete.",
        Some(RunData::NxVerify(result)),
    ))
}

pub(crate) async fn wup_compress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let req = &req;
    let input = req
        .input
        .clone()
        .or_else(|| first_wup_input(req))
        .ok_or_else(|| invalid_arg("input path is required"))?;
    let desired = output_or(req, || crate::nintendo::wup::derive_wua_path(&input))?;
    convert_op(
        progress,
        req,
        ConvertTarget {
            input: &input,
            desired: &desired,
            operation: "wup.compress",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            let mut opts = crate::nintendo::wup::WupCompressOptions::default();
            if let Some(level) = req.options.level {
                opts.zstd_level = level;
            }
            let titles = wup_titles(req, &input)?;
            crate::nintendo::wup::compress_titles_async(titles, output, opts, progress, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn wup_decrypt(
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
    crate::nintendo::wup::decrypt_nus_title_async(input.clone(), output.clone(), progress, cancel)
        .await?;
    Ok(
        RunResponse::ok("WUP decrypt complete.", None).with_record(ReportRecord::new(
            ReportRecordInput {
                input_path: input.display().to_string(),
                output_path: output.display().to_string(),
                operation: ("wup.decrypt").into(),
                status: FileStatus::Ok,
                input_bytes: file_len(&input),
                output_bytes: 0,
                elapsed_ms: 0,
                error: None,
            },
        )),
    )
}

pub(crate) async fn wup_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let result =
        crate::nintendo::wup::verify_wup_async(input, req.options.key.clone(), progress, cancel)
            .await?;
    Ok(RunResponse::ok(
        "WUP verification complete.",
        Some(RunData::WupVerify(result)),
    ))
}

pub(crate) async fn cue_merge(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = req
        .output
        .clone()
        .ok_or_else(|| invalid_arg("output path is required"))?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            desired: &desired,
            operation: "cue.merge",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            if cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            crate::cue::merge::merge_bin(progress, input, output, true, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn playlist_write(req: RunRequest, cancel: CancelToken) -> Result<RunResponse> {
    let input = required_input(&req)?;
    if !input.is_dir() {
        return Err(invalid_arg(format!(
            "playlist input must be a directory: {}",
            input.display()
        )));
    }
    let extensions = req
        .options
        .extensions
        .as_deref()
        .unwrap_or("cue,chd,iso,cso,zso")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let extension_refs = extensions.iter().map(String::as_str).collect::<Vec<_>>();
    let mode = match req.options.playlist_mode.as_deref().unwrap_or("multiple") {
        "multiple" => crate::playlist::PlaylistMode::Multiple,
        "always" => crate::playlist::PlaylistMode::Always,
        other => return Err(invalid_arg(format!("invalid playlist_mode {other:?}"))),
    };
    let plans = crate::playlist::plan_playlists(
        &crate::playlist::PlaylistOptions {
            scan_dir: &input,
            output_dir: req.options.output_dir.as_deref(),
            extensions: &extension_refs,
            mode,
            max_depth: req.options.max_depth,
        },
        &cancel,
    )?;
    let started = Instant::now();
    let policy = conflict_policy(&req)?;
    let mut records = Vec::new();
    let mut playlists = Vec::new();
    for plan in plans {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
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
            records.push(ReportRecord::new(ReportRecordInput {
                input_path: input.display().to_string(),
                output_path: output.display().to_string(),
                operation: ("playlist.write").into(),
                status: if matches!(resolution, ConflictResolution::Skip) {
                    FileStatus::Skipped
                } else {
                    FileStatus::Ok
                },
                input_bytes: 0,
                output_bytes: 0,
                elapsed_ms: 0,
                error: None,
            }));
            continue;
        }
        match resolution {
            ConflictResolution::Write(path) => {
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&path, plan.contents).await?;
                records.push(ReportRecord::new(ReportRecordInput {
                    input_path: input.display().to_string(),
                    output_path: path.display().to_string(),
                    operation: ("playlist.write").into(),
                    status: FileStatus::Ok,
                    input_bytes: 0,
                    output_bytes: file_len(&path),
                    elapsed_ms: 0,
                    error: None,
                }));
            }
            ConflictResolution::Skip => records.push(ReportRecord::new(ReportRecordInput {
                input_path: input.display().to_string(),
                output_path: plan.m3u_path.display().to_string(),
                operation: ("playlist.write").into(),
                status: FileStatus::Skipped,
                input_bytes: 0,
                output_bytes: 0,
                elapsed_ms: 0,
                error: None,
            })),
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

pub(crate) fn info(req: RunRequest) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let keys_path = req.options.keys.clone();
    let info = crate::info::read_info(
        &input,
        &crate::info::InfoOptions {
            keys_path,
            parent_path: None,
        },
    )?;
    Ok(RunResponse::ok("Info read.", Some(RunData::Info(info))))
}

/// Shared convert prelude: resolve the output path, short-circuit a skip
/// or a dry run, then run `run` under the file-op bookkeeping.
pub(crate) struct ConvertTarget<'a> {
    pub input: &'a Path,
    pub desired: &'a Path,
    pub operation: &'a str,
    pub verify: OutputVerify,
}

pub(crate) async fn convert_op<F, Fut>(
    progress: &dyn ProgressReporter,
    req: &RunRequest,
    target: ConvertTarget<'_>,
    cancel: CancelToken,
    run: F,
) -> Result<RunResponse>
where
    F: FnOnce(PathBuf, PathBuf, CancelToken) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let ConvertTarget {
        input,
        desired,
        operation,
        verify,
    } = target;
    let plan = prepare_output(progress, req, input, desired, operation, verify, &cancel).await?;
    let Some(output) = plan.output else {
        return Ok(skipped(input, desired, operation));
    };
    if let Some(line) = plan.line {
        return Ok(RunResponse::ok(
            "Dry run planned.",
            Some(RunData::Plan(line)),
        ));
    }
    let source = input.to_path_buf();
    let target = output.clone();
    run_file_op(input, &output, operation, move || {
        run(source, target, cancel)
    })
    .await
}

pub(crate) fn chd_options(req: &RunRequest) -> Result<ChdOptions> {
    Ok(ChdOptions {
        hunk_size: req.options.hunk_size,
        codecs: opt_chd_codecs(req)?,
        level: req.options.level,
        force: true,
    })
}

pub(crate) fn rvz_options(req: &RunRequest) -> RvzCompressOptions {
    let mut opts = RvzCompressOptions::default();
    if let Some(level) = req.options.level {
        opts.compression_level = level;
    }
    if let Some(chunk_size) = req.options.chunk_size {
        opts.chunk_size = chunk_size;
    }
    opts
}

pub(crate) struct PreparedOutput {
    output: Option<PathBuf>,
    line: Option<PlanLine>,
}

pub(crate) async fn prepare_output(
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
            match verify_existing_output(progress, desired, verify, cancel.clone()).await? {
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
            match verify_existing_output(progress, desired, verify, cancel.clone()).await? {
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

pub(crate) async fn run_file_op<F, Fut>(
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
    let record = ReportRecord::new(ReportRecordInput {
        input_path: input.display().to_string(),
        output_path: output.display().to_string(),
        operation: operation.into(),
        status: FileStatus::Ok,
        input_bytes,
        output_bytes,
        elapsed_ms: elapsed_ms(started),
        error: None,
    });
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

pub(crate) fn skipped(input: &Path, desired: &Path, operation: &str) -> RunResponse {
    RunResponse::ok("Skipped existing output.", None).with_record(ReportRecord::new(
        ReportRecordInput {
            input_path: input.display().to_string(),
            output_path: desired.display().to_string(),
            operation: operation.into(),
            status: FileStatus::Skipped,
            input_bytes: 0,
            output_bytes: 0,
            elapsed_ms: 0,
            error: None,
        },
    ))
}

pub(crate) fn totals_for(record: &ReportRecord) -> ReportTotals {
    totals_for_records(std::slice::from_ref(record), record.elapsed_ms)
}

pub(crate) fn totals_for_records(records: &[ReportRecord], elapsed_ms: u64) -> ReportTotals {
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

pub(crate) fn batch_message(totals: &ReportTotals) -> String {
    if totals.failed == 0 {
        format!(
            "{} files completed ({} ok, {} skipped).",
            totals.total_files, totals.ok, totals.skipped
        )
    } else {
        format!("{} of {} files failed.", totals.failed, totals.total_files)
    }
}

pub(crate) fn child_options(options: &RunOptions) -> RunOptions {
    let mut options = options.clone();
    options.recursive = None;
    options.report = None;
    options
}

pub(crate) fn batch_exts(operation: &str) -> Result<&'static [&'static str]> {
    find_op(operation)
        .and_then(|op| op.batch_exts)
        .ok_or_else(|| {
            invalid_arg(format!(
                "operation {operation:?} does not support recursive runs"
            ))
        })
}

pub(crate) fn wup_titles(
    req: &RunRequest,
    fallback: &Path,
) -> Result<Vec<crate::nintendo::wup::TitleInput>> {
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

pub(crate) fn first_wup_input(req: &RunRequest) -> Option<PathBuf> {
    let first = req.options.inputs.as_ref()?.first()?;
    match first {
        WupTitleInputOption::Path(path) => Some(path.clone()),
        WupTitleInputOption::Object { path, .. } => Some(path.clone()),
    }
}

pub(crate) fn wup_format(value: &str) -> Result<crate::nintendo::wup::TitleInputFormat> {
    match value {
        "loadiine" => Ok(crate::nintendo::wup::TitleInputFormat::Loadiine),
        "nus" => Ok(crate::nintendo::wup::TitleInputFormat::Nus),
        "disc" => Ok(crate::nintendo::wup::TitleInputFormat::Disc),
        other => Err(invalid_arg(format!("invalid WUP input format {other:?}"))),
    }
}

pub(crate) fn required_input(req: &RunRequest) -> Result<PathBuf> {
    req.input
        .clone()
        .ok_or_else(|| invalid_arg("input path is required"))
}

pub(crate) fn output_or(req: &RunRequest, default: impl FnOnce() -> PathBuf) -> Result<PathBuf> {
    if req.output.is_some() && req.options.output_template.as_deref().is_some() {
        return Err(invalid_arg(
            "output_template conflicts with an explicit output path",
        ));
    }
    if let Some(output) = req.output.clone() {
        return Ok(output);
    }
    let derived = default();
    if let (Some(template), Some(input)) =
        (req.options.output_template.as_deref(), req.input.as_deref())
    {
        let ext = derived.extension().and_then(|s| s.to_str()).unwrap_or("");
        let keys_path = req.options.keys.clone();
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
        let base = req
            .options
            .output_dir
            .clone()
            .or_else(|| input.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        return Ok(base.join(rel));
    }
    Ok(crate::util::place_in_dir(
        &derived,
        req.options.output_dir.as_deref(),
    ))
}

pub(crate) fn opt_chd_codecs(req: &RunRequest) -> Result<Option<Vec<ChdCodec>>> {
    req.options
        .codecs
        .as_ref()
        .map(|names| {
            names
                .iter()
                .map(|name| name.parse::<ChdCodec>())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|err| invalid_arg(err.to_string()))
        })
        .transpose()
}

pub(crate) fn conflict_policy(req: &RunRequest) -> Result<ConflictPolicy> {
    match req.options.on_conflict.as_deref().unwrap_or("error") {
        "error" => Ok(ConflictPolicy::Error),
        "overwrite" => Ok(ConflictPolicy::Overwrite),
        "skip" => Ok(ConflictPolicy::Skip),
        "rename" => Ok(ConflictPolicy::Rename),
        "overwrite-invalid" | "overwrite_invalid" => Ok(ConflictPolicy::OverwriteInvalid),
        other => Err(invalid_arg(format!("invalid on_conflict value {other:?}"))),
    }
}

pub(crate) fn cso_format(value: &str) -> Result<CsoFormat> {
    match value {
        "cso" | "CSO" => Ok(CsoFormat::Cso),
        "zso" | "ZSO" => Ok(CsoFormat::Zso),
        "dax" | "DAX" => Err(invalid_arg(
            "DAX is decode-only and cannot be a compression target",
        )),
        other => Err(invalid_arg(format!("invalid CSO format {other:?}"))),
    }
}

pub(crate) fn disc_mode(value: Option<&str>) -> Result<Option<DiscMode>> {
    match value {
        None | Some("auto") => Ok(None),
        Some("cd") => Ok(Some(DiscMode::Cd)),
        Some("dvd") => Ok(Some(DiscMode::Dvd)),
        Some("ld") => Ok(Some(DiscMode::Ld)),
        Some(other) => Err(invalid_arg(format!("invalid CHD mode {other:?}"))),
    }
}

pub(crate) fn nx_mode(
    value: &str,
    block_size_exp: Option<u32>,
) -> Result<crate::nintendo::nx::NczMode> {
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

pub(crate) fn comparison_data(
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

pub(crate) fn path_ext(path: &Path) -> &str {
    path.extension().and_then(|s| s.to_str()).unwrap_or("")
}

pub(crate) fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}
