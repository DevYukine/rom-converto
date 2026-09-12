//! The `organize` runner op: scan a library directory, classify every unit
//! into its best archival format, and place it under
//! `<output_dir>/<console>/<name>.<ext>`, optionally renaming via the
//! Playmatch DAT database (`dat`), deleting placed sources (`move_source`)
//! and writing multi-disc playlists (`playlists`).
//!
//! With `dat`, a dry run still hashes every unit and queries the Playmatch
//! API; playlists are only written on real runs.

use super::defaults::apply_config_defaults;
use super::models::{
    OrganizeData, OrganizeRow, PlaylistPlanData, RunData, RunOptions, RunRequest, RunResponse,
    RunRow, RunStatus,
};
use super::ops::{
    batch_message, conflict_policy, elapsed_ms, preflight_space, prepare_output, required_input,
    run_single_request, totals_for_records,
};
use super::{RUN_SCHEMA, invalid_arg, is_cancelled_error, planned_verb};
use crate::dat::PlaymatchClient;
use crate::dat::rename::{RenameAction, plan_renames};
use crate::dat::run::{MatchPolicy, match_unit, rename_candidate};
use crate::dat::units::DatUnit;
use crate::info::{DetectedConsole, InfoOptions, InfoResult, SUPPORTED_INFO_EXTENSIONS};
use crate::microsoft::xdvdfs::PartitionKind;
use crate::playlist::{PlaylistMode, PlaylistOptions, plan_playlists};
use crate::util::template::retro_label_for_ext;
use crate::util::{
    CancelToken, Cancelled, ConflictPolicy, ConflictResolution, FileStatus, HashAlgo, OutputExists,
    OutputVerify, PlanDecision, ProgressReporter, ReportRecord, ReportRecordInput, ResolvedInput,
    TemplateTokens, apply_template, atomic_write, resolve_conflict, write_zip,
};
use anyhow::{Context, Result};
use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Layout applied when the request carries no `output_template`.
const DEFAULT_OUTPUT_TEMPLATE: &str = "{console}/{basename}.{ext}";

/// Output extensions that count as disc images for playlist grouping.
const PLAYLIST_DISC_EXTS: &[&str] = &["chd", "cue", "iso", "cso", "zso", "rvz"];

/// What organize plans to do with one unit.
#[derive(Debug, PartialEq)]
enum Action {
    /// Hand the unit to a child conversion op writing `ext`. `ext` is owned
    /// because the 3DS target extension comes from the z3ds map at runtime.
    Convert {
        op: &'static str,
        ext: String,
        format: Option<&'static str>,
    },
    /// Zip the source as a single member (cartridge ROMs, DS).
    Zip,
    /// The file is already in its best shape; place a copy.
    Copy,
    /// Leave the file alone, with the reason.
    Skip(&'static str),
}

/// Organizes a library directory: one [`OrganizeRow`] per unit, one
/// [`ReportRecord`] per row so reports and CLI totals keep one spelling.
pub(crate) async fn organize(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let root = required_input(&req)?;
    if !root.is_dir() {
        return Err(invalid_arg(format!(
            "organize input must be a directory: {}",
            root.display()
        )));
    }
    let output_dir =
        req.options.output_dir.clone().ok_or_else(|| {
            invalid_arg("output_dir is required for organize (--output-dir <DIR>)")
        })?;
    let template = req
        .options
        .output_template
        .clone()
        .unwrap_or_else(|| DEFAULT_OUTPUT_TEMPLATE.to_string());
    let units = crate::dat::units::collect_units(&root, req.options.max_depth, &cancel)
        .await
        .with_context(|| format!("scanning {}", root.display()))?;
    if units.is_empty() {
        return Err(invalid_arg(format!("no files found in {}", root.display())));
    }
    let dat_enabled = req.options.dat == Some(true);
    let move_source = req.options.move_source == Some(true);
    let dry_run = req.dry_run;

    let started = Instant::now();
    progress.batch_start(
        units.len() as u64,
        units.iter().map(DatUnit::size_bytes).sum(),
    );
    // The outer bar counts units; per-file byte progress goes to a child
    // channel so it never resets the outer counter (same split as dat_scan).
    progress.start(units.len() as u64, "Organizing");
    let file_progress = progress.child("file");

    // DAT naming is planned for the whole library at once, so rename
    // collisions and disc-set guards see every candidate.
    let naming = if dat_enabled {
        match_dat_units(&req, &units, file_progress.as_ref(), &cancel).await?
    } else {
        vec![DatNaming::default(); units.len()]
    };

    let mut rows = Vec::with_capacity(units.len());
    let mut disc_dirs = BTreeSet::new();
    for (index, unit) in units.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let row = organize_unit(
            &req,
            unit,
            &output_dir,
            &template,
            &naming[index],
            move_source,
            progress,
            file_progress.as_ref(),
            &cancel,
        )
        .await;
        let row = match row {
            Ok(row) => row,
            Err(err) if is_cancelled_error(&err) || cancel.is_cancelled() => return Err(err),
            Err(err) => {
                progress.warn(&err.to_string());
                OrganizeRow {
                    input: unit.display_path().to_path_buf(),
                    output: None,
                    console: None,
                    action: "organize".to_string(),
                    status: FileStatus::Failed,
                    planned: dry_run,
                    detail: Some(error_detail(&err)),
                    input_bytes: unit.size_bytes(),
                    output_bytes: 0,
                    elapsed_ms: 0,
                }
            }
        };
        if row.status == FileStatus::Ok
            && let Some(output) = &row.output
            && let Some(dir) = output.parent()
            && PLAYLIST_DISC_EXTS.contains(&ext_of(output))
        {
            disc_dirs.insert(dir.to_path_buf());
        }
        progress.row(&RunRow::Organize(row.clone()));
        progress.batch_advance(unit.size_bytes());
        progress.inc(1);
        rows.push(row);
    }
    progress.finish();

    let records: Vec<ReportRecord> = rows.iter().map(|row| row_record(row, dry_run)).collect();
    let playlists = if req.options.playlists == Some(true) && !dry_run {
        write_playlists(&req, &disc_dirs, progress, &cancel).await?
    } else {
        Vec::new()
    };

    let ok = rows.iter().filter(|r| r.status == FileStatus::Ok).count();
    let skipped = rows
        .iter()
        .filter(|r| r.status == FileStatus::Skipped)
        .count();
    let failed = rows
        .iter()
        .filter(|r| r.status == FileStatus::Failed)
        .count();
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
        data: Some(RunData::Organize(OrganizeData {
            rows,
            dry_run,
            ok,
            skipped,
            failed,
            playlists,
        })),
    })
}

/// Plans and executes one unit, producing its row. Only cancellation aborts
/// the run: a broken output template yields one failed row per unit, and
/// every other per-unit failure becomes a `failed` row.
#[allow(clippy::too_many_arguments)]
async fn organize_unit(
    req: &RunRequest,
    unit: &DatUnit,
    output_dir: &Path,
    template: &str,
    naming: &DatNaming,
    move_source: bool,
    progress: &dyn ProgressReporter,
    file_progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<OrganizeRow> {
    let unit_started = Instant::now();
    let dry_run = req.dry_run;
    let primary = unit.display_path().to_path_buf();
    let input_bytes = unit.size_bytes();

    // Archive inputs are staged: the member is the detection/conversion
    // source while the archive stays the row's input. An archive without an
    // image member is unrecognized; any other staging failure (temp space,
    // corrupt archive) is still a skip, but says why.
    let resolved = match stage_unit(&primary, cancel).await {
        Ok(resolved) => resolved,
        Err(err) if is_cancelled_error(&err) => return Err(err),
        Err(err) => {
            let detail = if err.chain().any(|e| e.is::<crate::util::NoMatchingMember>()) {
                "unrecognized".to_string()
            } else {
                progress.warn(&err.to_string());
                error_detail(&err)
            };
            return Ok(skip_row(
                &primary,
                None,
                &detail,
                input_bytes,
                unit_started,
                dry_run,
            ));
        }
    };
    let source: &Path = resolved
        .as_ref()
        .map_or(primary.as_path(), ResolvedInput::path);

    let kind = crate::info::detect_console(source).ok();
    let info = crate::info::read_info(
        source,
        &InfoOptions {
            keys_path: req.options.keys.clone(),
            parent_path: None,
        },
    )
    .ok();
    let Some(kind) = kind else {
        return Ok(skip_row(
            &primary,
            None,
            "unrecognized",
            input_bytes,
            unit_started,
            dry_run,
        ));
    };
    let action = classify(kind, info.as_ref(), source);
    let action_str = match &action {
        Action::Convert { op, .. } => (*op).to_string(),
        Action::Zip => "zip".to_string(),
        Action::Copy => "copy".to_string(),
        Action::Skip(reason) => {
            return Ok(skip_row(
                &primary,
                None,
                reason,
                input_bytes,
                unit_started,
                dry_run,
            ));
        }
    };
    let target_ext = match &action {
        Action::Convert { ext, .. } => ext.clone(),
        Action::Zip => "zip".to_string(),
        Action::Copy => ext_of(source).to_string(),
        Action::Skip(_) => String::new(),
    };

    // Console label fallbacks: the shared table first, then a cartridge
    // extension guess for Retro files whose header did not parse, then the
    // DAT platform name for CHD/CSO/unknown labels.
    let mut tokens = TemplateTokens::new(info.as_ref(), source, &target_ext);
    if tokens.console.is_none() {
        tokens.console = crate::info::console_label(kind).map(str::to_string);
    }
    if tokens.console.is_none() && kind == DetectedConsole::Retro {
        tokens.console = retro_label_for_ext(ext_of(source)).map(str::to_string);
    }
    if tokens
        .console
        .as_deref()
        .is_none_or(|label| label == "CHD" || label == "CSO")
        && let Some(platform) = naming.platform.as_deref()
    {
        tokens.console = Some(platform.to_string());
    }
    if tokens.console.is_none() {
        progress.warn(&format!(
            "{}: console not identified, filing at the output root",
            primary.display()
        ));
    }
    if let Some(stem) = naming.stem.as_deref() {
        tokens.basename = stem.to_string();
    }

    let desired = output_dir.join(apply_template(template, &tokens)?);

    // Same-path guard: a unit whose desired output is one of its own source
    // files is already in place; nothing runs and nothing is deleted. The
    // comparison is by identity: a differently spelled or symlinked path to
    // the same file still counts.
    if unit_source_files(unit)
        .iter()
        .any(|path| same_file(path, &desired))
    {
        return Ok(skip_row(
            &primary,
            tokens.console,
            "already in place",
            input_bytes,
            unit_started,
            dry_run,
        ));
    }

    match &action {
        Action::Skip(_) => unreachable!("skip actions returned above"),
        Action::Convert { op, format, .. } => {
            run_conversion(
                req,
                unit,
                op,
                *format,
                &desired,
                source,
                tokens.console,
                input_bytes,
                move_source,
                unit_started,
                progress,
                file_progress,
                cancel,
            )
            .await
        }
        Action::Zip | Action::Copy => {
            run_place(
                req,
                &action,
                &action_str,
                &primary,
                source,
                &desired,
                unit,
                tokens.console,
                input_bytes,
                move_source,
                unit_started,
                progress,
                file_progress,
                cancel,
            )
            .await
        }
    }
}

/// Stages an archive input into a temp extraction, or `None` for a plain
/// file. Cancellation propagates; any other staging failure is returned, and
/// the caller skips the unit as unrecognized.
async fn stage_unit(primary: &Path, cancel: &CancelToken) -> Result<Option<ResolvedInput>> {
    if !crate::util::is_archive_path(primary) {
        return Ok(None);
    }
    let path = primary.to_path_buf();
    match tokio::task::spawn_blocking(move || {
        crate::util::resolve_input(&path, SUPPORTED_INFO_EXTENSIONS)
    })
    .await
    .context("staging archive input")?
    {
        Ok(resolved) => Ok(Some(resolved)),
        Err(err) if Cancelled::in_chain(&err) || cancel.is_cancelled() => {
            Err(anyhow::Error::from(Cancelled))
        }
        Err(err) => Err(err),
    }
}

/// The action table: maps a detected console plus parsed info to the best
/// archival action for the unit's source extension.
fn classify(kind: DetectedConsole, info: Option<&InfoResult>, source: &Path) -> Action {
    let ext = ext_of(source).to_ascii_lowercase();
    match kind {
        DetectedConsole::Dol => match ext.as_str() {
            "iso" | "gcm" => dol_rvl_convert("dol.compress"),
            "gcz" => Action::Convert {
                op: "dol.migrate",
                ext: "rvz".to_string(),
                format: None,
            },
            "rvz" => Action::Copy,
            _ => Action::Skip("unrecognized"),
        },
        DetectedConsole::Rvl => match ext.as_str() {
            "iso" | "wbfs" => dol_rvl_convert("rvl.compress"),
            "gcz" | "wia" => Action::Convert {
                op: "rvl.migrate",
                ext: "rvz".to_string(),
                format: None,
            },
            "rvz" => Action::Copy,
            _ => Action::Skip("unrecognized"),
        },
        DetectedConsole::Ctr => match ext.as_str() {
            "cia" | "cci" | "3ds" | "cxi" | "3dsx" => {
                // AM6: the compressed extension comes from the z3ds map
                // (cia -> zcia, cci/3ds -> zcci, cxi -> zcxi, 3dsx -> z3dsx).
                let compressed = crate::nintendo::ctr::z3ds::derive_compressed_path(source);
                let ext = compressed
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("z3ds")
                    .to_string();
                Action::Convert {
                    op: "ctr.compress",
                    ext,
                    format: None,
                }
            }
            "zcia" | "zcci" | "zcxi" | "z3dsx" | "ncch" => Action::Copy,
            _ => Action::Skip("unrecognized"),
        },
        DetectedConsole::Nx => match ext.as_str() {
            "nsp" => Action::Convert {
                op: "nx.compress",
                ext: "nsz".to_string(),
                format: None,
            },
            "xci" => Action::Convert {
                op: "nx.compress",
                ext: "xcz".to_string(),
                format: None,
            },
            "nsz" | "xcz" | "nca" => Action::Copy,
            _ => Action::Skip("unrecognized"),
        },
        DetectedConsole::Wup => match ext.as_str() {
            "wud" | "wux" => Action::Convert {
                op: "wup.compress",
                ext: "wua".to_string(),
                format: None,
            },
            "wua" => Action::Copy,
            _ => Action::Skip("unrecognized"),
        },
        DetectedConsole::Chd => match info {
            Some(InfoResult::Chd(chd)) if chd.version < 5 => Action::Convert {
                op: "chd.migrate",
                ext: "chd".to_string(),
                format: None,
            },
            _ => Action::Copy,
        },
        DetectedConsole::Cso => Action::Copy,
        DetectedConsole::Psx => match ext.as_str() {
            "cue" | "iso" => Action::Convert {
                op: "chd.compress",
                ext: "chd".to_string(),
                format: None,
            },
            _ => Action::Skip("unrecognized"),
        },
        DetectedConsole::Psp => match ext.as_str() {
            "iso" => Action::Convert {
                op: "cso.compress",
                ext: "cso".to_string(),
                format: Some("cso"),
            },
            _ => Action::Skip("unrecognized"),
        },
        DetectedConsole::Xbox => match ext.as_str() {
            "xiso" => Action::Copy,
            _ => match info {
                Some(InfoResult::Xbox(xiso)) if matches!(xiso.kind, PartitionKind::Trimmed) => {
                    Action::Copy
                }
                _ => Action::Convert {
                    op: "xbox.convert",
                    ext: "xiso".to_string(),
                    format: None,
                },
            },
        },
        DetectedConsole::Xenon => match ext.as_str() {
            "zar" => Action::Copy,
            _ => Action::Convert {
                op: "xenon.compress",
                ext: "zar".to_string(),
                format: None,
            },
        },
        DetectedConsole::Ps3 => match info {
            Some(InfoResult::Ps3(ps3)) if ps3.encrypted == Some(true) => Action::Convert {
                op: "ps3.decrypt",
                ext: "iso".to_string(),
                format: None,
            },
            _ => Action::Copy,
        },
        DetectedConsole::LaserDisc => match ext.as_str() {
            "avi" => Action::Convert {
                op: "chd.compress",
                ext: "chd".to_string(),
                format: None,
            },
            _ => Action::Skip("unrecognized"),
        },
        DetectedConsole::Ntr => match ext.as_str() {
            "nds" | "dsi" => Action::Zip,
            _ => Action::Skip("unrecognized"),
        },
        DetectedConsole::Retro => match ext.as_str() {
            "gdi" => Action::Skip("GDI disc sets are not supported"),
            ext if crate::info::retro::RETRO_EXTENSIONS.contains(&ext) => Action::Zip,
            // Sega disc images (Saturn, Sega CD, Dreamcast) compress to CHD.
            _ => Action::Convert {
                op: "chd.compress",
                ext: "chd".to_string(),
                format: None,
            },
        },
        DetectedConsole::Pbp
        | DetectedConsole::Vpk
        | DetectedConsole::Pkg
        | DetectedConsole::Ps4Pkg
        | DetectedConsole::Ps5Pkg => Action::Copy,
    }
}

/// A dol/rvl RVZ compression target.
fn dol_rvl_convert(op: &'static str) -> Action {
    Action::Convert {
        op,
        ext: "rvz".to_string(),
        format: None,
    }
}

/// The DAT naming inputs for one unit: the canonical stem to rename to and
/// the match's platform label.
#[derive(Clone, Default)]
struct DatNaming {
    stem: Option<String>,
    platform: Option<String>,
}

/// Matches every unit against the Playmatch database in one pre-pass so
/// [`plan_renames`] sees all candidates at once, and returns one
/// [`DatNaming`] per unit, aligned with `units`. Network and digest failures
/// degrade to keep-name with a warning; only cancellation propagates.
async fn match_dat_units(
    req: &RunRequest,
    units: &[DatUnit],
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<Vec<DatNaming>> {
    let algos = [HashAlgo::Crc32, HashAlgo::Sha1];
    let bounds = super::dat::dat_checksum_bounds(req, &algos)?;
    let policy = MatchPolicy {
        algos: &algos,
        bounds: &bounds,
        quick: false,
        cache: req.ctx.hash_cache.as_deref(),
    };
    let client = PlaymatchClient::new(req.options.api_base.as_deref());
    let mut candidates = Vec::with_capacity(units.len());
    let mut platforms = Vec::with_capacity(units.len());
    for unit in units {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let data = match match_unit("organize", &client, unit, policy, progress, cancel).await {
            Ok(data) => data,
            Err(err) => {
                let err = anyhow::Error::from(err);
                if is_cancelled_error(&err) {
                    return Err(err);
                }
                progress.warn(&format!(
                    "DAT match failed for {}: {err}",
                    unit.display_path().display()
                ));
                candidates.push(rename_candidate(unit.display_path().to_path_buf(), None));
                platforms.push(None);
                continue;
            }
        };
        candidates.push(rename_candidate(
            unit.display_path().to_path_buf(),
            data.matched.as_ref(),
        ));
        platforms.push(data.platform.clone());
    }
    // One planning pass over every candidate, so collisions and disc-set
    // guards see the whole library; look each unit's plan up by its `from`
    // path.
    let stems: HashMap<PathBuf, Option<String>> = plan_renames(&candidates)
        .into_iter()
        .map(|plan| {
            let stem = (plan.action == RenameAction::Rename)
                .then_some(plan.to)
                .flatten()
                .and_then(|to| to.file_stem().map(|s| s.to_string_lossy().into_owned()));
            (plan.from, stem)
        })
        .collect();
    Ok(units
        .iter()
        .zip(platforms)
        .map(|(unit, platform)| DatNaming {
            stem: stems.get(unit.display_path()).cloned().flatten(),
            platform,
        })
        .collect())
}

/// Runs a child conversion op for one unit and folds its plan or records into
/// the row. The unit's primary path replaces the staged member's in every
/// folded record and plan line.
#[allow(clippy::too_many_arguments)]
async fn run_conversion(
    req: &RunRequest,
    unit: &DatUnit,
    operation: &str,
    format: Option<&str>,
    desired: &Path,
    source: &Path,
    console: Option<String>,
    input_bytes: u64,
    move_source: bool,
    unit_started: Instant,
    progress: &dyn ProgressReporter,
    file_progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<OrganizeRow> {
    let primary = unit.display_path().to_path_buf();
    let dry_run = req.dry_run;
    let child = child_request(req, operation, source, desired, format);
    // The child takes the same config-default path as a direct run, then
    // loses the run-level knobs organize already owns; its per-file progress
    // goes to the child channel.
    let dispatched = match apply_config_defaults(child) {
        Ok(mut child) => {
            child.options.report = None;
            child.options.recursive = None;
            run_single_request(child, file_progress, cancel.clone()).await
        }
        Err(err) => Err(err),
    };
    match dispatched {
        Ok(mut response) => {
            if let Some(RunData::Plan(mut line)) = response.data.take() {
                line.input = primary.clone();
                let (status, detail) = plan_outcome(&line.decision);
                return Ok(OrganizeRow {
                    input: primary,
                    output: Some(line.output),
                    console,
                    action: operation.to_string(),
                    status,
                    planned: dry_run,
                    detail,
                    input_bytes,
                    output_bytes: 0,
                    elapsed_ms: elapsed_ms(unit_started),
                });
            }
            let mut status = FileStatus::Ok;
            let mut detail = None;
            let mut output = desired.to_path_buf();
            let mut output_bytes = 0u64;
            for mut record in response.records {
                record.input_path = primary.display().to_string();
                if !record.output_path.is_empty() {
                    output = PathBuf::from(&record.output_path);
                }
                output_bytes += record.output_bytes;
                match record.status {
                    FileStatus::Failed => {
                        status = FileStatus::Failed;
                        detail = record.error.clone();
                    }
                    FileStatus::Skipped if status == FileStatus::Ok => {
                        status = FileStatus::Skipped;
                        detail = record.error.clone();
                    }
                    _ => {}
                }
            }
            if status == FileStatus::Ok
                && move_source
                && let Err(err) = remove_sources(unit, &output).await
            {
                progress.warn(&format!(
                    "could not move source {}: {err}",
                    unit.display_path().display()
                ));
            }
            Ok(OrganizeRow {
                input: primary,
                output: (status == FileStatus::Ok).then_some(output),
                console,
                action: operation.to_string(),
                status,
                planned: dry_run,
                detail,
                input_bytes,
                output_bytes,
                elapsed_ms: elapsed_ms(unit_started),
            })
        }
        Err(err) if is_cancelled_error(&err) => Err(err),
        Err(err) if OutputExists::in_chain(&err) => {
            progress.warn(&err.to_string());
            Ok(skip_row(
                &primary,
                console,
                "output exists",
                input_bytes,
                unit_started,
                dry_run,
            ))
        }
        Err(err) => Ok(OrganizeRow {
            input: primary,
            output: None,
            console,
            action: operation.to_string(),
            status: FileStatus::Failed,
            planned: dry_run,
            detail: Some(error_detail(&err)),
            input_bytes,
            output_bytes: 0,
            elapsed_ms: elapsed_ms(unit_started),
        }),
    }
}

/// Places a unit by zipping or copying it, with the same conflict and dry-run
/// semantics as `prepare_output`, then optionally deletes the source files.
#[allow(clippy::too_many_arguments)]
async fn run_place(
    req: &RunRequest,
    action: &Action,
    action_str: &str,
    primary: &Path,
    source: &Path,
    desired: &Path,
    unit: &DatUnit,
    console: Option<String>,
    input_bytes: u64,
    move_source: bool,
    unit_started: Instant,
    progress: &dyn ProgressReporter,
    file_progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<OrganizeRow> {
    let dry_run = req.dry_run;
    let prepared = match prepare_output(
        progress,
        req,
        primary,
        desired,
        action_str,
        OutputVerify::None,
        cancel,
    )
    .await
    {
        Ok(prepared) => prepared,
        Err(err) if is_cancelled_error(&err) => return Err(err),
        Err(err) if OutputExists::in_chain(&err) => {
            progress.warn(&err.to_string());
            return Ok(skip_row(
                primary,
                console,
                "output exists",
                input_bytes,
                unit_started,
                dry_run,
            ));
        }
        Err(err) => {
            return Ok(OrganizeRow {
                input: primary.to_path_buf(),
                output: None,
                console,
                action: action_str.to_string(),
                status: FileStatus::Failed,
                planned: dry_run,
                detail: Some(error_detail(&err)),
                input_bytes,
                output_bytes: 0,
                elapsed_ms: elapsed_ms(unit_started),
            });
        }
    };
    if let Some(mut line) = prepared.line {
        line.input = primary.to_path_buf();
        let (status, detail) = plan_outcome(&line.decision);
        return Ok(OrganizeRow {
            input: primary.to_path_buf(),
            output: Some(line.output),
            console,
            action: action_str.to_string(),
            status,
            planned: dry_run,
            detail,
            input_bytes,
            output_bytes: 0,
            elapsed_ms: elapsed_ms(unit_started),
        });
    }
    let Some(output) = prepared.output else {
        return Ok(skip_row(
            primary,
            console,
            "output exists",
            input_bytes,
            unit_started,
            dry_run,
        ));
    };
    // Conversions can inflate a file well past its input size; refuse the
    // write when the output volume lacks room, per unit, unless the run
    // skipped the check.
    if req.options.skip_space_check != Some(true)
        && let Err(err) = preflight_space(output.parent().unwrap_or(&output), input_bytes)
    {
        return Ok(OrganizeRow {
            input: primary.to_path_buf(),
            output: None,
            console,
            action: action_str.to_string(),
            status: FileStatus::Failed,
            planned: dry_run,
            detail: Some(error_detail(&err)),
            input_bytes,
            output_bytes: 0,
            elapsed_ms: elapsed_ms(unit_started),
        });
    }
    let written = match action {
        Action::Zip => write_zip_output(source, desired, &output, file_progress, cancel).await,
        Action::Copy => copy_output(source, &output, cancel).await,
        Action::Skip(_) | Action::Convert { .. } => unreachable!("only zip/copy reach here"),
    };
    if let Err(err) = written {
        if is_cancelled_error(&err) {
            return Err(err);
        }
        return Ok(OrganizeRow {
            input: primary.to_path_buf(),
            output: None,
            console,
            action: action_str.to_string(),
            status: FileStatus::Failed,
            planned: dry_run,
            detail: Some(error_detail(&err)),
            input_bytes,
            output_bytes: 0,
            elapsed_ms: elapsed_ms(unit_started),
        });
    }
    // move_source renames a copy action: the file moved out of the library.
    // The rename happens only once the sources are really gone; a failed
    // delete keeps the row a copy and warns.
    let mut action = action_str.to_string();
    if move_source {
        match remove_sources(unit, &output).await {
            Ok(()) if action_str == "copy" => action = "move".to_string(),
            Ok(()) => {}
            Err(err) => progress.warn(&format!(
                "could not move source {}: {err}",
                unit.display_path().display()
            )),
        }
    }
    Ok(OrganizeRow {
        input: primary.to_path_buf(),
        output: Some(output.clone()),
        console,
        action,
        status: FileStatus::Ok,
        planned: dry_run,
        detail: None,
        input_bytes,
        output_bytes: crate::util::fs::file_len(&output),
        elapsed_ms: elapsed_ms(unit_started),
    })
}

/// Writes `source` as the single member of a new zip at `output`. The member
/// keeps the planned stem, even when a conflict renamed the output file.
async fn write_zip_output(
    source: &Path,
    desired: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<()> {
    let member = format!(
        "{}.{}",
        desired
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output"),
        ext_of(source)
    );
    let src = source.to_path_buf();
    let dst = output.to_path_buf();
    let cancel = cancel.clone();
    crate::util::spawn_blocking_with_progress(progress, move |progress| {
        write_zip(&src, &member, &dst, progress, &cancel)
    })
    .await
}

/// Copies `source` to `output` through a scratch sibling and rename, in
/// chunks so cancellation lands between them; the scratch file is removed on
/// any failure or cancellation.
async fn copy_output(source: &Path, output: &Path, cancel: &CancelToken) -> Result<()> {
    let src = source.to_path_buf();
    let dst = output.to_path_buf();
    let cancel = cancel.clone();
    tokio::task::spawn_blocking(move || {
        atomic_write(&dst, true, |file| {
            let mut input = std::fs::File::open(&src)?;
            let mut chunk = vec![0u8; 4 * 1024 * 1024];
            loop {
                if cancel.is_cancelled() {
                    return Err(anyhow::Error::from(Cancelled));
                }
                let read = input.read(&mut chunk)?;
                if read == 0 {
                    return Ok(());
                }
                file.write_all(&chunk[..read])?;
            }
        })
    })
    .await
    .context("copying output")??;
    Ok(())
}

/// True when both paths exist and resolve to the same file; any resolution
/// failure (including a missing path) is `false`. Organize compares by
/// identity, not lexically, so a differently spelled or symlinked path to
/// the same file never splits into "two files".
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Deletes a placed unit's source files (the cue plus every bin of a set),
/// never touching a file that is also the output.
async fn remove_sources(unit: &DatUnit, output: &Path) -> std::io::Result<()> {
    for path in unit_source_files(unit) {
        if same_file(&path, output) {
            continue;
        }
        tokio::fs::remove_file(&path).await?;
    }
    Ok(())
}

/// Every file a unit occupies: the file itself, or the cue and all bins of a
/// set.
fn unit_source_files(unit: &DatUnit) -> Vec<PathBuf> {
    match unit {
        DatUnit::File(path) => vec![path.clone()],
        DatUnit::CueSet { cue, bins } => {
            let mut files = Vec::with_capacity(bins.len() + 1);
            files.push(cue.clone());
            files.extend(bins.iter().cloned());
            files
        }
    }
}

/// Builds the child [`RunRequest`] for a conversion: only the shared run
/// knobs carry over, so the child picks up its own family's config defaults.
fn child_request(
    req: &RunRequest,
    operation: &str,
    input: &Path,
    output: &Path,
    format: Option<&str>,
) -> RunRequest {
    RunRequest {
        schema: None,
        operation: operation.to_string(),
        input: Some(input.to_path_buf()),
        output: Some(output.to_path_buf()),
        config: req.config.clone(),
        preset: req.preset.clone(),
        options: RunOptions {
            config: req.options.config.clone(),
            preset: req.options.preset.clone(),
            on_conflict: req.options.on_conflict.clone(),
            keys: req.options.keys.clone(),
            allow_encrypted: req.options.allow_encrypted,
            skip_space_check: req.options.skip_space_check,
            verify_after: req.options.verify_after,
            format: format.map(str::to_string),
            ..RunOptions::default()
        },
        dry_run: req.dry_run,
        ctx: req.ctx.clone(),
    }
}

/// Status and detail text for a dry-run plan decision.
fn plan_outcome(decision: &PlanDecision) -> (FileStatus, Option<String>) {
    match decision {
        PlanDecision::Skip => (FileStatus::Skipped, Some("output exists".to_string())),
        PlanDecision::KeepValid => (
            FileStatus::Skipped,
            Some("existing output verified valid".to_string()),
        ),
        PlanDecision::New => (FileStatus::Ok, Some("new".to_string())),
        PlanDecision::Overwrite => (FileStatus::Ok, Some("overwrite".to_string())),
        PlanDecision::Rename(path) => (
            FileStatus::Ok,
            Some(format!("rename to {}", path.display())),
        ),
        PlanDecision::RewriteInvalid => (FileStatus::Ok, Some("rewrite (invalid)".to_string())),
    }
}

/// A skipped row with `action = "skip"` and the reason as detail.
fn skip_row(
    input: &Path,
    console: Option<String>,
    reason: &str,
    input_bytes: u64,
    started: Instant,
    dry_run: bool,
) -> OrganizeRow {
    OrganizeRow {
        input: input.to_path_buf(),
        output: None,
        console,
        action: "skip".to_string(),
        status: FileStatus::Skipped,
        planned: dry_run,
        detail: Some(reason.to_string()),
        input_bytes,
        output_bytes: 0,
        elapsed_ms: elapsed_ms(started),
    }
}

/// The one report record an organize row produces, so `--report` and CLI
/// totals spell every action the same way.
fn row_record(row: &OrganizeRow, dry_run: bool) -> ReportRecord {
    ReportRecord::new(ReportRecordInput {
        input_path: row.input.display().to_string(),
        output_path: row
            .output
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        operation: planned_verb(&row.action, dry_run),
        status: row.status,
        input_bytes: row.input_bytes,
        output_bytes: row.output_bytes,
        elapsed_ms: row.elapsed_ms,
        error: (row.status != FileStatus::Ok)
            .then(|| row.detail.clone())
            .flatten(),
    })
}

/// Plans and writes the `.m3u` playlists for every output directory that
/// received a disc image. Existing playlists skip under the `error` policy.
/// A playlist that cannot be planned or written warns and stays out of the
/// result instead of aborting the run; only cancellation propagates.
async fn write_playlists(
    req: &RunRequest,
    disc_dirs: &BTreeSet<PathBuf>,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<Vec<PlaylistPlanData>> {
    let policy = match conflict_policy(req) {
        Ok(ConflictPolicy::Error) => ConflictPolicy::Skip,
        Ok(policy) => policy,
        Err(err) => {
            progress.warn(&format!("could not plan playlists: {err}"));
            return Ok(Vec::new());
        }
    };
    let mut playlists = Vec::new();
    for dir in disc_dirs {
        let plans = match plan_playlists(
            &PlaylistOptions {
                scan_dir: dir,
                output_dir: None,
                extensions: PLAYLIST_DISC_EXTS,
                mode: PlaylistMode::Multiple,
                max_depth: Some(1),
            },
            cancel,
        ) {
            Ok(plans) => plans,
            Err(err) => {
                let err = anyhow::Error::from(err);
                if is_cancelled_error(&err) {
                    return Err(err);
                }
                progress.warn(&format!(
                    "could not plan playlists in {}: {err}",
                    dir.display()
                ));
                return Ok(playlists);
            }
        };
        for plan in plans {
            if plan.has_duplicate_numbers {
                progress.warn(&format!(
                    "Duplicate disc numbers in set {}, including all entries",
                    plan.base_title
                ));
            }
            let entry_exts = plan
                .contents
                .lines()
                .filter_map(|line| Path::new(line).extension())
                .filter_map(|ext| ext.to_str());
            if let Some(mixed) = crate::util::mixed_playlist_extensions(entry_exts) {
                progress.warn(&format!(
                    "Mixed track formats ({mixed}) in set {}; emulators expect every disc \
                     in a playlist to use the same format",
                    plan.base_title
                ));
            }
            let resolution = match resolve_conflict(&plan.m3u_path, policy) {
                Ok(resolution) => resolution,
                Err(err) => {
                    progress.warn(&format!(
                        "could not resolve playlist {}: {err}",
                        plan.m3u_path.display()
                    ));
                    return Ok(playlists);
                }
            };
            // A playlist is only reported once it is on disk; a failed write
            // warns instead of counting.
            let written = match &resolution {
                ConflictResolution::Write(path) => {
                    match tokio::fs::write(path, &plan.contents).await {
                        Ok(()) => Some(path.clone()),
                        Err(err) => {
                            progress.warn(&format!(
                                "could not write playlist {}: {err}",
                                path.display()
                            ));
                            None
                        }
                    }
                }
                ConflictResolution::Skip => Some(plan.m3u_path.clone()),
            };
            if let Some(output) = written {
                playlists.push(PlaylistPlanData {
                    base_title: plan.base_title.clone(),
                    output,
                    contents: plan.contents.clone(),
                    disc_count: plan.disc_count,
                    has_duplicate_numbers: plan.has_duplicate_numbers,
                });
            }
        }
    }
    Ok(playlists)
}

/// Extension of `path`, without the dot; empty when absent.
fn ext_of(path: &Path) -> &str {
    path.extension().and_then(|ext| ext.to_str()).unwrap_or("")
}

/// Flattens an error's chain into one detail string.
fn error_detail(err: &anyhow::Error) -> String {
    let mut detail = err.to_string();
    for cause in err.chain().skip(1) {
        detail.push_str(": ");
        detail.push_str(&cause.to_string());
    }
    detail
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::RecordingProgress;
    use tempfile::TempDir;

    /// A 0xC0-byte GBA cartridge image: fixed 0x96 byte at 0xB2 and a title
    /// at 0xA0, enough for `nintendo::agb::parse` to succeed.
    fn gba_bytes() -> Vec<u8> {
        let mut data = vec![0u8; 0xC0];
        data[0xB2] = 0x96;
        data[0xA0..0xA8].copy_from_slice(b"TESTGAME");
        data
    }

    fn organize_request(input: &Path, output_dir: Option<&Path>, dry_run: bool) -> RunRequest {
        RunRequest {
            schema: None,
            operation: "organize".to_string(),
            input: Some(input.to_path_buf()),
            output: None,
            config: None,
            preset: None,
            options: RunOptions {
                output_dir: output_dir.map(Path::to_path_buf),
                ..RunOptions::default()
            },
            dry_run,
            ctx: crate::runner::models::RunContext::default(),
        }
    }

    fn row_for<'a>(data: &'a OrganizeData, name: &str) -> &'a OrganizeRow {
        data.rows
            .iter()
            .find(|row| {
                row.input
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy() == name)
            })
            .unwrap_or_else(|| panic!("no row for {name}"))
    }

    #[test]
    fn classify_maps_every_console_to_its_best_format() {
        let convert = |action: &Action| -> (&'static str, String, Option<&'static str>) {
            match action {
                Action::Convert { op, ext, format } => (*op, ext.clone(), *format),
                other => panic!("expected convert, got {other:?}"),
            }
        };
        let path = |ext: &str| PathBuf::from(format!("game.{ext}"));
        assert_eq!(
            convert(&classify(DetectedConsole::Dol, None, &path("iso"))),
            ("dol.compress", "rvz".to_string(), None)
        );
        assert_eq!(
            convert(&classify(DetectedConsole::Dol, None, &path("gcm"))),
            ("dol.compress", "rvz".to_string(), None)
        );
        assert_eq!(
            convert(&classify(DetectedConsole::Dol, None, &path("gcz"))),
            ("dol.migrate", "rvz".to_string(), None)
        );
        assert!(matches!(
            classify(DetectedConsole::Dol, None, &path("rvz")),
            Action::Copy
        ));
        assert_eq!(
            convert(&classify(DetectedConsole::Rvl, None, &path("wia"))),
            ("rvl.migrate", "rvz".to_string(), None)
        );
        assert_eq!(
            convert(&classify(DetectedConsole::Ctr, None, &path("3ds"))),
            ("ctr.compress", "zcci".to_string(), None)
        );
        assert_eq!(
            convert(&classify(DetectedConsole::Ctr, None, &path("cia"))),
            ("ctr.compress", "zcia".to_string(), None)
        );
        assert!(matches!(
            classify(DetectedConsole::Ctr, None, &path("zcci")),
            Action::Copy
        ));
        assert_eq!(
            convert(&classify(DetectedConsole::Nx, None, &path("xci"))),
            ("nx.compress", "xcz".to_string(), None)
        );
        assert_eq!(
            convert(&classify(DetectedConsole::Nx, None, &path("nsp"))),
            ("nx.compress", "nsz".to_string(), None)
        );
        assert!(matches!(
            classify(DetectedConsole::Nx, None, &path("nsz")),
            Action::Copy
        ));
        assert!(matches!(
            classify(DetectedConsole::Ntr, None, &path("nds")),
            Action::Zip
        ));
        assert!(matches!(
            classify(DetectedConsole::Retro, None, &path("gba")),
            Action::Zip
        ));
        assert_eq!(
            classify(DetectedConsole::Retro, None, &path("gdi")),
            Action::Skip("GDI disc sets are not supported")
        );
        assert!(matches!(
            classify(
                DetectedConsole::Chd,
                Some(&InfoResult::Chd(crate::disc::chd::info::ChdInfo {
                    version: 5,
                    ..Default::default()
                })),
                &path("chd")
            ),
            Action::Copy
        ));
        assert_eq!(
            convert(&classify(
                DetectedConsole::Chd,
                Some(&InfoResult::Chd(crate::disc::chd::info::ChdInfo {
                    version: 4,
                    ..Default::default()
                })),
                &path("chd")
            )),
            ("chd.migrate", "chd".to_string(), None)
        );
        assert_eq!(
            convert(&classify(DetectedConsole::Psx, None, &path("cue"))),
            ("chd.compress", "chd".to_string(), None)
        );
        assert_eq!(
            convert(&classify(DetectedConsole::Psp, None, &path("iso"))),
            ("cso.compress", "cso".to_string(), Some("cso"))
        );
        assert!(matches!(
            classify(DetectedConsole::Xbox, None, &path("xiso")),
            Action::Copy
        ));
        assert_eq!(
            convert(&classify(DetectedConsole::Xenon, None, &path("iso"))),
            ("xenon.compress", "zar".to_string(), None)
        );
        assert!(matches!(
            classify(DetectedConsole::Ps3, None, &path("iso")),
            Action::Copy
        ));
        assert_eq!(
            convert(&classify(
                DetectedConsole::Ps3,
                Some(&InfoResult::Ps3(crate::sony::ps3::Ps3Info {
                    encrypted: Some(true),
                    ..Default::default()
                })),
                &path("iso")
            )),
            ("ps3.decrypt", "iso".to_string(), None)
        );
        assert_eq!(
            convert(&classify(DetectedConsole::LaserDisc, None, &path("avi"))),
            ("chd.compress", "chd".to_string(), None)
        );
        assert!(matches!(
            classify(DetectedConsole::Pbp, None, &path("pbp")),
            Action::Copy
        ));
        // A standalone .bin never gets a console kind, so the detect-failure
        // path decides its skip before classify is reached.
        assert!(classify(DetectedConsole::Psx, None, &path("bin")).is_skip());
    }

    impl Action {
        fn is_skip(&self) -> bool {
            matches!(self, Action::Skip(_))
        }
    }

    #[tokio::test]
    async fn dry_run_plans_zip_and_skip_without_writing() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Test Game.gba"), gba_bytes()).unwrap();
        std::fs::write(lib.path().join("notes.txt"), b"not a rom").unwrap();

        let response = organize(
            organize_request(lib.path(), Some(out.path()), true),
            &RecordingProgress::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert!(data.dry_run);
        assert_eq!(data.rows.len(), 2);
        assert_eq!(data.ok, 1);
        assert_eq!(data.skipped, 1);

        let zip_row = row_for(&data, "Test Game.gba");
        assert_eq!(zip_row.action, "zip");
        assert_eq!(zip_row.status, FileStatus::Ok);
        assert!(zip_row.planned);
        assert_eq!(zip_row.console.as_deref(), Some("Game Boy Advance"));
        assert_eq!(
            zip_row.output,
            Some(out.path().join("Game Boy Advance").join("Test Game.zip"))
        );

        let skip_row = row_for(&data, "notes.txt");
        assert_eq!(skip_row.action, "skip");
        assert_eq!(skip_row.status, FileStatus::Skipped);
        assert_eq!(skip_row.detail.as_deref(), Some("unrecognized"));

        assert!(!out.path().join("Game Boy Advance").exists());
        assert_eq!(response.records.len(), 2);
    }

    #[tokio::test]
    async fn real_run_zips_the_retro_rom() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let bytes = gba_bytes();
        std::fs::write(lib.path().join("Test Game.gba"), &bytes).unwrap();

        let response = organize(
            organize_request(lib.path(), Some(out.path()), false),
            &RecordingProgress::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let zip_row = row_for(&data, "Test Game.gba");
        assert_eq!(zip_row.status, FileStatus::Ok);
        assert!(!zip_row.planned);
        let zip_path = zip_row.output.clone().unwrap();
        assert!(lib.path().join("Test Game.gba").exists());

        let file = std::fs::File::open(&zip_path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut member = archive.by_name("Test Game.gba").unwrap();
        let mut got = Vec::new();
        std::io::Read::read_to_end(&mut member, &mut got).unwrap();
        assert_eq!(got, bytes);
    }

    #[tokio::test]
    async fn move_source_deletes_the_gba_after_zipping() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Test Game.gba"), gba_bytes()).unwrap();
        std::fs::write(lib.path().join("notes.txt"), b"not a rom").unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.move_source = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let zip_row = row_for(&data, "Test Game.gba");
        assert_eq!(zip_row.status, FileStatus::Ok);
        assert_eq!(zip_row.action, "zip");
        assert!(zip_row.output.clone().unwrap().exists());
        assert!(!lib.path().join("Test Game.gba").exists());
        // Skipped rows never delete their source.
        assert!(lib.path().join("notes.txt").exists());
    }

    /// A minimal GameCube disc image: the 0xC2339F3D magic at 0x1C is what
    /// `detect_console` sniffs, and the RVZ compressor accepts the rest as
    /// opaque data.
    fn gcm_bytes() -> Vec<u8> {
        let mut data = vec![0u8; 0x2440 + 64];
        data[0x1C..0x20].copy_from_slice(&0xC2339F3Du32.to_be_bytes());
        data[..6].copy_from_slice(b"GTSTE0");
        data
    }

    #[tokio::test]
    async fn move_source_deletes_the_source_after_conversion() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Cube Game.iso"), gcm_bytes()).unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.move_source = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Cube Game.iso");
        assert_eq!(row.status, FileStatus::Ok, "{:?}", row.detail);
        assert_eq!(row.action, "dol.compress");
        assert_eq!(
            row.output.as_deref(),
            Some(out.path().join("GameCube").join("Cube Game.rvz").as_path())
        );
        assert!(!lib.path().join("Cube Game.iso").exists());
    }

    #[tokio::test]
    async fn existing_zip_output_is_skipped_under_the_error_policy() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Test Game.gba"), gba_bytes()).unwrap();
        let target_dir = out.path().join("Game Boy Advance");
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(target_dir.join("Test Game.zip"), b"stale").unwrap();

        let response = organize(
            organize_request(lib.path(), Some(out.path()), false),
            &RecordingProgress::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Test Game.gba");
        assert_eq!(row.status, FileStatus::Skipped);
        assert_eq!(row.action, "skip");
        assert_eq!(row.detail.as_deref(), Some("output exists"));
        assert_eq!(row.console.as_deref(), Some("Game Boy Advance"));
        assert_eq!(data.failed, 0);
        assert_eq!(
            std::fs::read(target_dir.join("Test Game.zip")).unwrap(),
            b"stale"
        );
    }

    #[tokio::test]
    async fn missing_output_dir_is_an_invalid_argument() {
        let lib = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Test Game.gba"), gba_bytes()).unwrap();
        let err = organize(
            organize_request(lib.path(), None, false),
            &RecordingProgress::default(),
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("output_dir"), "{err}");
    }

    #[tokio::test]
    async fn same_path_guard_skips_a_file_already_in_place() {
        let lib = TempDir::new().unwrap();
        std::fs::write(lib.path().join("game.xiso"), b"not a real xiso").unwrap();

        let mut req = organize_request(lib.path(), Some(lib.path()), false);
        req.options.output_template = Some("{basename}.{ext}".to_string());
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "game.xiso");
        assert_eq!(row.action, "skip");
        assert_eq!(row.status, FileStatus::Skipped);
        assert_eq!(row.detail.as_deref(), Some("already in place"));
        assert!(lib.path().join("game.xiso").exists());
    }

    /// The same-path guard compares by identity: an output dir spelled
    /// through a symlink to the library root still skips "already in place"
    /// instead of copying the file onto itself, even under `move_source`.
    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_output_dir_still_skips_in_place_files() {
        let lib = TempDir::new().unwrap();
        std::fs::write(lib.path().join("game.xiso"), b"not a real xiso").unwrap();
        let spelled = lib.path().join("spelled");
        std::os::unix::fs::symlink(lib.path(), &spelled).unwrap();

        let mut req = organize_request(lib.path(), Some(&spelled), false);
        req.options.move_source = Some(true);
        req.options.output_template = Some("{basename}.{ext}".to_string());
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "game.xiso");
        assert_eq!(row.action, "skip");
        assert_eq!(row.status, FileStatus::Skipped);
        assert_eq!(row.detail.as_deref(), Some("already in place"));
        assert_eq!(data.failed, 0);
        assert!(lib.path().join("game.xiso").exists());
    }

    /// A zip that carries no image member (a manuals archive, say) is a
    /// skipped row, never a failed one.
    #[tokio::test]
    async fn archive_without_a_rom_member_is_skipped() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let zip_path = lib.path().join("manuals.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("readme.txt", opts).unwrap();
        zip.finish().unwrap();

        let response = organize(
            organize_request(lib.path(), Some(out.path()), false),
            &RecordingProgress::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "manuals.zip");
        assert_eq!(row.action, "skip");
        assert_eq!(row.status, FileStatus::Skipped);
        assert_eq!(row.detail.as_deref(), Some("unrecognized"));
        assert_eq!(data.failed, 0);
    }

    #[tokio::test]
    async fn unknown_input_directory_is_rejected() {
        let err = organize(
            organize_request(
                Path::new("/nonexistent-library"),
                Some(Path::new("/tmp")),
                false,
            ),
            &RecordingProgress::default(),
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("must be a directory"), "{err}");
    }
}
