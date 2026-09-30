//! The organize planning phase: one [`UnitPlan`] per scanned unit, built by
//! staging, detecting, classifying, and templating before any global pass
//! (DAT matching, selection, layout) or execution runs.

use super::dat_match::StagedDigests;
use super::ext_of;
use super::headers;
use super::place::LinkMode;
use super::trim;
use crate::dat::tags::GameTags;
use crate::dat::units::DatUnit;
use crate::info::{DetectedConsole, InfoOptions, InfoResult, SUPPORTED_INFO_EXTENSIONS};
use crate::microsoft::xdvdfs::PartitionKind;
use crate::runner::is_cancelled_error;
use crate::runner::models::RunRequest;
use crate::util::template::retro_label_for_ext;
use crate::util::{
    CancelToken, Cancelled, ChecksumBounds, ProgressReporter, ResolvedInput, TemplateTokens,
    apply_template,
};
use anyhow::{Context, Result};
use globset::GlobSet;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

/// What organize plans to do with one unit.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Action {
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
    /// Place a filesystem link to the source instead of a copy
    /// (`link_mode` turned a Copy into a Link at plan time).
    Link,
}

/// The row-facing verb for an action: the child op name or
/// "zip" | "copy" | "link".
pub(super) fn action_label(action: &Action) -> String {
    match action {
        Action::Convert { op, .. } => (*op).to_string(),
        Action::Zip => "zip".to_string(),
        Action::Copy => "copy".to_string(),
        Action::Link => "link".to_string(),
    }
}

/// The hash-verified Playmatch match behind a unit, when one exists.
#[derive(Debug)]
pub(super) struct GameRef {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub dat_name: Option<String>,
    pub platform: Option<String>,
    pub tags: GameTags,
}

/// Whether a planned unit stays in the run or is dropped with a reason.
/// Skipped plans still carry their tokens so their row can name a console.
#[derive(Debug, PartialEq)]
pub(super) enum Decision {
    Keep,
    Skip(String),
}

/// Member facts computed from an archive extraction while planning.
pub(super) struct MemberFacts {
    pub basis: PathBuf,
    pub digests: Option<StagedDigests>,
    pub crc: Option<u32>,
}

/// One unit's plan: its action, naming tokens, header/trim facts, and
/// (after the global passes) its desired output path.
pub(super) struct UnitPlan {
    /// Index of the unit this plan belongs to in the scanned `units` slice.
    pub index: usize,
    /// The unit's display path: the archive or plain file it came from.
    pub source: PathBuf,
    /// Lowercase extension of the detection source: the staged member for
    /// archives, else the file; empty for Skip plans.
    pub source_ext: String,
    /// Meaningless for Skip plans; [`Decision::Skip`] gates every dispatch.
    pub action: Action,
    /// Naming tokens; basename starts as the source stem and is replaced by
    /// the DAT stem via `apply_dat_naming`.
    pub tokens: TemplateTokens,
    /// The unit's directory relative to the scan root (`{input_dir}`).
    pub input_subdir: PathBuf,
    pub game: Option<GameRef>,
    pub header: Option<headers::RomHeader>,
    /// Bytes to skip when writing the source (console header strip).
    pub strip: u64,
    pub trim: Option<trim::TrimInfo>,
    /// Bytes to append when writing (trim padding); 0 = none.
    pub pad: u64,
    /// Fill byte for `pad` padding: 0xFF, or the fill a DAT match verified
    /// the padded form against.
    pub pad_fill: u8,
    /// This plan is a patched variant of `units[index]`, built from this
    /// patch file.
    pub patch: Option<PathBuf>,
    /// Why staging failed, on the Skip plans a non-recognition staging
    /// failure produces; the execute loop turns these into Failed rows.
    pub staging_error: Option<String>,
    /// Why the output path could not be resolved (a template failure); the
    /// execute loop turns this into a Failed row.
    pub output_error: Option<String>,
    /// Why DAT matching degraded the unit to keep-name; clean suspends for
    /// the run so the unit's previous DAT-named output survives.
    pub match_error: Option<String>,
    /// Archive member facts. Present only for Keep plans of staged archives.
    pub member: Option<MemberFacts>,
    /// The desired output path; `None` until `resolve_paths` runs.
    pub desired: Option<PathBuf>,
    pub decision: Decision,
}

impl UnitPlan {
    /// The extension the written payload carries: the headerless extension
    /// when a console header is stripped, else the source's own.
    pub(super) fn payload_ext(&self) -> &str {
        if self.strip > 0 {
            self.header
                .as_ref()
                .and_then(|header| header.headerless_ext)
                .unwrap_or(&self.source_ext)
        } else {
            &self.source_ext
        }
    }
}

/// Plans every unit and retains staged archive members for execution within
/// the [`RETAIN_CAP`] budget. DAT digests and patch CRCs are computed before
/// retention.
#[allow(clippy::too_many_arguments)]
pub(super) async fn plan_units(
    req: &RunRequest,
    units: &[DatUnit],
    root: &Path,
    link_mode: Option<LinkMode>,
    bounds: Option<&ChecksumBounds>,
    patch_index: &super::patch::PatchIndex,
    progress: &dyn ProgressReporter,
    file_progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<(Vec<UnitPlan>, Vec<Option<ResolvedInput>>)> {
    let budget = crate::util::available_space(&std::env::temp_dir())
        .unwrap_or(0)
        .saturating_div(4)
        .min(RETAIN_CAP);
    let mut retention = Retention {
        budget,
        used: 0,
        stopped: false,
    };
    let mut plans = Vec::with_capacity(units.len());
    let overwrite_invalid =
        super::conflict_policy(req)? == crate::util::ConflictPolicy::OverwriteInvalid;
    let mut stagings: Vec<Option<ResolvedInput>> =
        std::iter::repeat_with(|| None).take(units.len()).collect();
    let continuations = super::split_continuations(units);
    for (index, unit) in units.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let primary = unit.display_path();
        let staged = if continuations.contains(&super::entry_location(primary)) {
            Ok(None)
        } else {
            stage_for_plan(primary, cancel, &mut retention, &mut stagings).await
        };
        let (plan, resolved) = plan_unit(
            req,
            index,
            unit,
            root,
            link_mode,
            &continuations,
            staged,
            bounds,
            patch_index,
            progress,
            file_progress,
            cancel,
        )
        .await?;
        if (!req.dry_run || dry_run_retains(&plan, overwrite_invalid, patch_index))
            && matches!(plan.decision, Decision::Keep)
            && let Some(resolved) = resolved
        {
            // Any staging that still fits is kept: one oversized archive
            // early in the run must not cost every later unit its staging.
            let staged_bytes = resolved.staged_bytes();
            if !retention.stopped
                && retention
                    .used
                    .checked_add(staged_bytes)
                    .is_some_and(|used| used <= retention.budget)
            {
                retention.used += staged_bytes;
                stagings[index] = Some(resolved);
            }
        }
        plans.push(plan);
    }
    Ok((plans, stagings))
}

/// Whether a dry run keeps this plan's staging for execution: only the
/// paths that read member bytes in a dry run need it (a migrate's legacy
/// format probe, the source digest overwrite-invalid compares an existing
/// output against, a patch application), mirroring `dry_run_needs_member`.
/// Output paths are not resolved yet, so every place action under
/// overwrite-invalid qualifies.
fn dry_run_retains(
    plan: &UnitPlan,
    overwrite_invalid: bool,
    patch_index: &super::patch::PatchIndex,
) -> bool {
    let action_reads = match &plan.action {
        Action::Convert { op, .. } => matches!(*op, "dol.migrate" | "rvl.migrate"),
        Action::Zip | Action::Copy | Action::Link => overwrite_invalid,
    };
    action_reads
        || plan
            .member
            .as_ref()
            .and_then(|member| member.crc)
            .is_some_and(|crc| patch_index.contains(crc))
}

/// Upper bound on extracted archive members kept from planning until their
/// unit executes, so a real run extracts each archive once. The budget is
/// this cap or a quarter of the temp volume's free space, whichever is
/// smaller: a RAM-backed /tmp and outputs written to the same volume both
/// keep their headroom. Units past the budget extract again at execution
/// (a plain copy then moves that extraction into place instead of copying).
const RETAIN_CAP: u64 = 16 * 1024 * 1024 * 1024;

/// Retention state for one run: every staging that fits the budget is
/// kept, in unit order. `stopped` is set once a temp-space shortfall forced
/// an eviction, and nothing is retained after it.
struct Retention {
    budget: u64,
    used: u64,
    stopped: bool,
}

/// Stages one unit for planning. Running out of temp space while earlier
/// stagings are retained evicts them all, stops retention, and retries
/// once, so retention never causes a staging error of its own. Any other
/// staging error (a corrupt archive) leaves retention alone.
async fn stage_for_plan(
    primary: &Path,
    cancel: &CancelToken,
    retention: &mut Retention,
    stagings: &mut [Option<ResolvedInput>],
) -> Result<Option<ResolvedInput>> {
    match stage_unit(primary, cancel).await {
        Ok(resolved) => Ok(resolved),
        Err(err)
            if err
                .chain()
                .any(|cause| cause.is::<crate::util::TempSpaceShortfall>())
                && stagings.iter().any(Option::is_some) =>
        {
            stagings.iter_mut().for_each(|staging| *staging = None);
            retention.used = 0;
            retention.stopped = true;
            stage_unit(primary, cancel).await
        }
        Err(err) => Err(err),
    }
}

/// Sets the console label and its frontend variant together, so frontend
/// folder tokens never trail the console fallback.
pub(super) fn set_console(tokens: &mut TemplateTokens, label: Option<&str>, ext: &str) {
    tokens.console = label.map(str::to_string);
    tokens.frontend_console =
        crate::util::template::frontend_console_label(tokens.console.as_deref(), ext);
}

/// Validates `remove_headers` up front: every entry must name a detected
/// header extension, or be `all` (strip any detected header) or `none`
/// (strip nothing). A typo would otherwise silently strip nothing.
pub(super) fn validate_remove_headers(list: &[String]) -> Result<()> {
    for entry in list {
        let entry = entry.to_ascii_lowercase();
        if entry == "all" || entry == "none" {
            continue;
        }
        if !headers::KNOWN_EXTS.contains(&entry.as_str()) {
            return Err(crate::runner::invalid_arg(format!(
                "invalid remove_headers entry {entry:?}; expected one of {:?}, \"all\" or \"none\"",
                headers::KNOWN_EXTS
            )));
        }
    }
    Ok(())
}

/// Plans one unit: stage → detect → info → classify → tokens. Archive
/// member facts are computed before its staging is retained or dropped.
#[allow(clippy::too_many_arguments)]
async fn plan_unit(
    req: &RunRequest,
    index: usize,
    unit: &DatUnit,
    root: &Path,
    link_mode: Option<LinkMode>,
    continuations: &HashSet<PathBuf>,
    staged: Result<Option<ResolvedInput>>,
    bounds: Option<&ChecksumBounds>,
    patch_index: &super::patch::PatchIndex,
    progress: &dyn ProgressReporter,
    file_progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<(UnitPlan, Option<ResolvedInput>)> {
    let primary = unit.display_path().to_path_buf();

    // A split WUD continuation a scanned game_part1.wud owns converts
    // together with it and never dispatches on its own: its bytes are a
    // slice of the part1 disc, and converting it would fail as a truncated
    // image. A numbered file no scanned set reads (a stray case twin, or a
    // part past a gap) is not skipped, so it plans and reports on its own.
    if continuations.contains(&super::entry_location(&primary)) {
        return Ok((
            skip_plan(
                index,
                unit,
                root,
                &primary,
                "converted together with game_part1.wud".to_string(),
            ),
            None,
        ));
    }

    // Archive inputs are staged before detection; any staging failure keeps
    // today's skip/failed-row and clean-suspension behavior.
    let resolved = match staged {
        Ok(resolved) => resolved,
        Err(err) if is_cancelled_error(&err) => return Err(err),
        Err(err) => {
            let staging_error = if err.chain().any(|e| e.is::<crate::util::NoMatchingMember>()) {
                None
            } else {
                progress.warn(&err.to_string());
                Some(error_detail(&err))
            };
            let detail = staging_error
                .clone()
                .unwrap_or_else(|| "unrecognized".to_string());
            let mut plan = skip_plan(index, unit, root, &primary, detail);
            plan.staging_error = staging_error;
            return Ok((plan, None));
        }
    };
    let source: &Path = resolved
        .as_ref()
        .map_or(primary.as_path(), ResolvedInput::path);
    let source_ext = ext_of(source).to_ascii_lowercase();

    let kind = crate::info::detect_console(source).ok();
    // Info parsing (banner decode, PNG render for Wii discs) is
    // disk-heavy: it belongs on the blocking pool like every other read.
    let info_source = source.to_path_buf();
    let info_keys = req.options.keys.clone();
    let info = tokio::task::spawn_blocking(move || {
        crate::info::read_info(
            &info_source,
            &InfoOptions {
                keys_path: info_keys,
                parent_path: None,
            },
        )
    })
    .await
    .ok()
    .and_then(|info| info.ok());
    let Some(kind) = kind else {
        return Ok((
            skip_plan(index, unit, root, &primary, "unrecognized".to_string()),
            None,
        ));
    };
    let action = match classify(kind, info.as_ref(), source) {
        Ok(action) => action,
        Err(reason) => {
            // The classify-skip row still carries the console column.
            let plan = skip_plan_with_tokens(index, unit, root, source, info.as_ref(), reason);
            return Ok((plan, None));
        }
    };

    // `link_mode` turns a Copy into a Link at plan time.
    let action = if link_mode.is_some() && action == Action::Copy {
        Action::Link
    } else {
        action
    };
    // The naming tokens' extension: the convert target's, "zip" for a zip,
    // else the source's own.
    let tokens_ext = match &action {
        Action::Convert { ext, .. } => ext.clone(),
        Action::Zip => "zip".to_string(),
        Action::Copy | Action::Link => ext_of(source).to_string(),
    };

    // Console label fallbacks: the shared table first, then a cartridge
    // extension guess for Retro files whose header did not parse. The DAT
    // platform fallback (and the rename stem) is applied later by
    // `apply_dat_naming`, once matching has run.
    let mut tokens = TemplateTokens::new(info.as_ref(), source, &tokens_ext);
    if tokens.console.is_none() {
        set_console(&mut tokens, crate::info::console_label(kind), &source_ext);
    }
    if tokens.console.is_none() && kind == DetectedConsole::Retro {
        set_console(&mut tokens, retro_label_for_ext(&source_ext), &source_ext);
    }

    // Header stripping and trim padding are detected for cartridge-class
    // plain files; both are inert until the matching option is set.
    let mut header = None;
    let mut trim_info = None;
    let mut strip = 0u64;
    let mut pad = 0u64;
    if matches!(kind, DetectedConsole::Retro | DetectedConsole::Ntr)
        && matches!(unit, DatUnit::File(_))
    {
        let size = std::fs::metadata(source).map(|m| m.len()).unwrap_or(0);
        let head = read_head(source);
        header = headers::detect_header(&source_ext, size, &head);
        trim_info = trim::detect_trim(&source_ext, size, &head);
        if let Some(detected) = &header
            && let Some(list) = req.options.remove_headers.as_ref()
            // `all` strips any detected header; `none` names no header
            // extension, so it strips nothing. An actually empty list
            // strips every detected header.
            && (list.is_empty()
                || list
                    .iter()
                    .any(|e| e.eq_ignore_ascii_case("all") || e.eq_ignore_ascii_case(&source_ext)))
        {
            strip = detected.len;
        }
        if req.options.trim_add_padding == Some(true)
            && let Some(detected) = &trim_info
        {
            pad = detected.padded_size.saturating_sub(size);
        }
    }

    // A split dump's fixed `game_part1` stem would send every set to the
    // same output name: name the unit after the folder it sits in.
    if matches!(kind, DetectedConsole::Wup)
        && crate::nintendo::wup::disc::sector_stream::split_part_number(&primary).is_some()
        && let Some(dir_name) = primary
            .parent()
            .and_then(|dir| dir.file_name())
            .and_then(|name| name.to_str())
    {
        tokens.basename = dir_name.to_string();
    }

    let member = if let Some(resolved) = resolved.as_ref() {
        let basis = resolved.output_basis().to_path_buf();
        let digests = if let Some(bounds) = bounds {
            Some(
                super::dat_match::digest_staged(
                    &primary,
                    resolved.path(),
                    &basis,
                    bounds,
                    header.as_ref().map(|header| header.len),
                    trim_info.as_ref().map(|trim| trim.padded_size),
                    req.ctx
                        .hash_cache
                        .as_deref()
                        .filter(|_| resolved.sole_member()),
                    file_progress,
                    cancel,
                )
                .await?,
            )
        } else {
            None
        };
        let crc = if patch_index.is_empty() {
            None
        } else {
            let path = resolved.path().to_path_buf();
            let worker_cancel = cancel.clone();
            match tokio::task::spawn_blocking(move || {
                crate::util::hash::crc32_of_file(&path, &worker_cancel)
            })
            .await
            {
                Ok(Ok(crc)) => Some(crc),
                Ok(Err(err)) => {
                    let err = anyhow::Error::from(err);
                    if is_cancelled_error(&err) {
                        return Err(err);
                    }
                    progress.warn(&err.to_string());
                    None
                }
                Err(_err) if cancel.is_cancelled() => return Err(Cancelled.into()),
                Err(err) => {
                    progress.warn(&err.to_string());
                    None
                }
            }
        };
        Some(MemberFacts {
            basis,
            digests,
            crc,
        })
    } else {
        None
    };
    let plan = UnitPlan {
        index,
        source: primary,
        source_ext,
        action,
        tokens,
        input_subdir: input_subdir(unit, root),
        game: None,
        header,
        strip,
        trim: trim_info,
        pad,
        // Retail GBA/NDS carts leave unused space at 0xFF; a DAT-verified
        // padded match overrides this with the fill it verified.
        pad_fill: 0xFF,
        patch: None,
        staging_error: None,
        output_error: None,
        match_error: None,
        member,
        desired: None,
        decision: Decision::Keep,
    };
    Ok((plan, resolved))
}

/// A Skip plan for a unit that never became actionable: unrecognized
/// content, a staging failure (with `staging_error` set), or a split WUD
/// continuation. Tokens stay minimal (console `None`), matching the row
/// these plans produce.
fn skip_plan(
    index: usize,
    unit: &DatUnit,
    root: &Path,
    primary: &Path,
    reason: String,
) -> UnitPlan {
    let tokens = TemplateTokens::new(None, primary, "");
    UnitPlan {
        index,
        source: unit.display_path().to_path_buf(),
        source_ext: String::new(),
        action: Action::Copy,
        tokens,
        input_subdir: input_subdir(unit, root),
        game: None,
        header: None,
        strip: 0,
        trim: None,
        pad: 0,
        pad_fill: 0x00,
        patch: None,
        staging_error: None,
        output_error: None,
        match_error: None,
        member: None,
        desired: None,
        decision: Decision::Skip(reason),
    }
}

/// A Skip plan for a unit classify declined; its tokens keep the console
/// fallbacks so the row's console column still names a system.
fn skip_plan_with_tokens(
    index: usize,
    unit: &DatUnit,
    root: &Path,
    source: &Path,
    info: Option<&InfoResult>,
    reason: &'static str,
) -> UnitPlan {
    let mut tokens = TemplateTokens::new(info, source, "");
    if tokens.console.is_none() {
        let ext_lower = ext_of(source).to_ascii_lowercase();
        set_console(&mut tokens, retro_label_for_ext(ext_of(source)), &ext_lower);
    }
    UnitPlan {
        index,
        source: unit.display_path().to_path_buf(),
        source_ext: String::new(),
        action: Action::Copy,
        tokens,
        input_subdir: input_subdir(unit, root),
        game: None,
        header: None,
        strip: 0,
        trim: None,
        pad: 0,
        pad_fill: 0x00,
        patch: None,
        staging_error: None,
        output_error: None,
        match_error: None,
        member: None,
        desired: None,
        decision: Decision::Skip(reason.to_string()),
    }
}

/// The unit's directory relative to the scan root.
fn input_subdir(unit: &DatUnit, root: &Path) -> PathBuf {
    unit.display_path()
        .parent()
        .and_then(|dir| dir.strip_prefix(root).ok())
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

/// The first 512 bytes of a file, for header and trim detection; an empty
/// vec when the file cannot be read.
fn read_head(path: &Path) -> Vec<u8> {
    let mut buf = vec![0u8; 512];
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(_) => break,
        }
    }
    buf.truncate(filled);
    buf
}

/// Stages an archive input into a temp extraction, or `None` for a plain
/// file. Cancellation propagates; any other staging failure is returned
/// for the caller to record: an archive without a matching member skips
/// as unrecognized, anything else becomes a Failed row and suspends
/// clean.
pub(super) async fn stage_unit(
    primary: &Path,
    cancel: &CancelToken,
) -> Result<Option<ResolvedInput>> {
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
/// archival action for the unit's source extension. `Err` carries the skip
/// reason.
pub(super) fn classify(
    kind: DetectedConsole,
    info: Option<&InfoResult>,
    source: &Path,
) -> Result<Action, &'static str> {
    let ext = ext_of(source).to_ascii_lowercase();
    match kind {
        DetectedConsole::Dol => match ext.as_str() {
            "iso" | "gcm" => Ok(dol_rvl_convert("dol.compress")),
            "gcz" => Ok(Action::Convert {
                op: "dol.migrate",
                ext: "rvz".to_string(),
                format: None,
            }),
            "rvz" => Ok(Action::Copy),
            _ => Err("unrecognized"),
        },
        DetectedConsole::Rvl => match ext.as_str() {
            "iso" | "wbfs" => Ok(dol_rvl_convert("rvl.compress")),
            "gcz" | "wia" => Ok(Action::Convert {
                op: "rvl.migrate",
                ext: "rvz".to_string(),
                format: None,
            }),
            "rvz" => Ok(Action::Copy),
            _ => Err("unrecognized"),
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
                Ok(Action::Convert {
                    op: "ctr.compress",
                    ext,
                    format: None,
                })
            }
            "zcia" | "zcci" | "zcxi" | "z3dsx" | "ncch" => Ok(Action::Copy),
            _ => Err("unrecognized"),
        },
        DetectedConsole::Nx => match ext.as_str() {
            "nsp" => Ok(Action::Convert {
                op: "nx.compress",
                ext: "nsz".to_string(),
                format: None,
            }),
            "xci" => Ok(Action::Convert {
                op: "nx.compress",
                ext: "xcz".to_string(),
                format: None,
            }),
            "nsz" | "xcz" | "nca" => Ok(Action::Copy),
            _ => Err("unrecognized"),
        },
        DetectedConsole::Wup => match ext.as_str() {
            "wud" | "wux" => Ok(Action::Convert {
                op: "wup.compress",
                ext: "wua".to_string(),
                format: None,
            }),
            "wua" => Ok(Action::Copy),
            _ => Err("unrecognized"),
        },
        DetectedConsole::Chd => match info {
            Some(InfoResult::Chd(chd)) if chd.version < 5 => Ok(Action::Convert {
                op: "chd.migrate",
                ext: "chd".to_string(),
                format: None,
            }),
            _ => Ok(Action::Copy),
        },
        DetectedConsole::Cso => Ok(Action::Copy),
        DetectedConsole::Psx => match ext.as_str() {
            "cue" | "iso" => Ok(Action::Convert {
                op: "chd.compress",
                ext: "chd".to_string(),
                format: None,
            }),
            _ => Err("unrecognized"),
        },
        DetectedConsole::Psp => match ext.as_str() {
            "iso" => Ok(Action::Convert {
                op: "cso.compress",
                ext: "cso".to_string(),
                format: Some("cso"),
            }),
            _ => Err("unrecognized"),
        },
        DetectedConsole::Xbox => match ext.as_str() {
            "xiso" => Ok(Action::Copy),
            _ => match info {
                Some(InfoResult::Xbox(xiso)) if matches!(xiso.kind, PartitionKind::Trimmed) => {
                    Ok(Action::Copy)
                }
                _ => Ok(Action::Convert {
                    op: "xbox.convert",
                    ext: "xiso".to_string(),
                    format: None,
                }),
            },
        },
        DetectedConsole::Xenon => match ext.as_str() {
            "zar" => Ok(Action::Copy),
            _ => Ok(Action::Convert {
                op: "xenon.compress",
                ext: "zar".to_string(),
                format: None,
            }),
        },
        DetectedConsole::Ps3 => match info {
            Some(InfoResult::Ps3(ps3)) if ps3.encrypted == Some(true) => Ok(Action::Convert {
                op: "ps3.decrypt",
                ext: "iso".to_string(),
                format: None,
            }),
            _ => Ok(Action::Copy),
        },
        DetectedConsole::LaserDisc => match ext.as_str() {
            "avi" => Ok(Action::Convert {
                op: "chd.compress",
                ext: "chd".to_string(),
                format: None,
            }),
            _ => Err("unrecognized"),
        },
        DetectedConsole::Ntr => match ext.as_str() {
            "nds" | "dsi" => Ok(Action::Zip),
            _ => Err("unrecognized"),
        },
        DetectedConsole::Retro => match ext.as_str() {
            "gdi" => Err("GDI disc sets are not supported"),
            ext if crate::info::retro::RETRO_EXTENSIONS.contains(&ext) => Ok(Action::Zip),
            // Sega disc images (Saturn, Sega CD, Dreamcast) compress to CHD.
            _ => Ok(Action::Convert {
                op: "chd.compress",
                ext: "chd".to_string(),
                format: None,
            }),
        },
        DetectedConsole::Pbp
        | DetectedConsole::Vpk
        | DetectedConsole::Pkg
        | DetectedConsole::Ps4Pkg
        | DetectedConsole::Ps5Pkg => Ok(Action::Copy),
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

/// Resolves every Keep plan's desired output path from its tokens; Skip
/// plans keep `desired: None`. A Zip plan whose output matches `zip_exclude`
/// demotes to a link (when `link_mode` is
/// configured) or copy of the raw payload under the source's extension. A
/// plan whose template resolves to an invalid path (for example a hostile
/// DAT token containing `..`) records the failure in `output_error` and
/// keeps no desired path: the execute loop turns it into a Failed row.
pub(super) fn resolve_paths(
    plans: &mut [UnitPlan],
    output_dir: &Path,
    template: &str,
    zip_exclude: Option<&GlobSet>,
    link_mode: Option<LinkMode>,
) {
    for plan in plans.iter_mut() {
        if plan.decision != Decision::Keep {
            continue;
        }
        plan.tokens.input_dir = (!plan.input_subdir.as_os_str().is_empty())
            .then(|| plan.input_subdir.display().to_string());
        // A template naming {dat} without a DAT match would silently drop
        // the segment and place the unit under a path the user did not
        // ask for: that is an unresolved output, recorded as a failure.
        if template.contains("{dat}") && plan.tokens.dat.is_none() {
            plan.output_error =
                Some("output template uses {dat} but the unit has no DAT match".to_string());
            continue;
        }
        let mut desired = match apply_template(template, &plan.tokens) {
            Ok(desired) => output_dir.join(desired),
            Err(err) => {
                plan.output_error = Some(err.to_string());
                continue;
            }
        };
        if let Some(globs) = zip_exclude
            && matches!(plan.action, Action::Zip)
            && super::clean::glob_matches(globs, output_dir, &desired)
        {
            // The raw payload is copied instead of re-zipped; a configured
            // link mode still applies, so the demotion lands as a link
            // there. A stripped source keeps the headerless extension: the
            // placement still strips the header on the way out.
            plan.action = if link_mode.is_some() {
                Action::Link
            } else {
                Action::Copy
            };
            plan.tokens.ext = plan.payload_ext().to_string();
            match apply_template(template, &plan.tokens) {
                Ok(redirected) => desired = output_dir.join(redirected),
                Err(err) => {
                    plan.output_error = Some(err.to_string());
                    continue;
                }
            }
        }
        plan.desired = Some(desired);
    }
}

/// Flattens an error's chain into one detail string.
pub(super) fn error_detail(err: &anyhow::Error) -> String {
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

    #[test]
    fn classify_maps_every_console_to_its_best_format() {
        let convert =
            |action: Result<Action, &'static str>| -> (&'static str, String, Option<&'static str>) {
                match action.expect("expected convert") {
                    Action::Convert { op, ext, format } => (op, ext.clone(), format),
                    other => panic!("expected convert, got {other:?}"),
                }
            };
        let copy = |action: Result<Action, &'static str>| matches!(action, Ok(Action::Copy));
        let path = |ext: &str| PathBuf::from(format!("game.{ext}"));
        assert_eq!(
            convert(classify(DetectedConsole::Dol, None, &path("iso"))),
            ("dol.compress", "rvz".to_string(), None)
        );
        assert_eq!(
            convert(classify(DetectedConsole::Dol, None, &path("gcm"))),
            ("dol.compress", "rvz".to_string(), None)
        );
        assert_eq!(
            convert(classify(DetectedConsole::Dol, None, &path("gcz"))),
            ("dol.migrate", "rvz".to_string(), None)
        );
        assert!(copy(classify(DetectedConsole::Dol, None, &path("rvz"))));
        assert_eq!(
            convert(classify(DetectedConsole::Rvl, None, &path("wia"))),
            ("rvl.migrate", "rvz".to_string(), None)
        );
        assert_eq!(
            convert(classify(DetectedConsole::Ctr, None, &path("3ds"))),
            ("ctr.compress", "zcci".to_string(), None)
        );
        assert_eq!(
            convert(classify(DetectedConsole::Ctr, None, &path("cia"))),
            ("ctr.compress", "zcia".to_string(), None)
        );
        assert!(copy(classify(DetectedConsole::Ctr, None, &path("zcci"))));
        assert_eq!(
            convert(classify(DetectedConsole::Nx, None, &path("xci"))),
            ("nx.compress", "xcz".to_string(), None)
        );
        assert_eq!(
            convert(classify(DetectedConsole::Nx, None, &path("nsp"))),
            ("nx.compress", "nsz".to_string(), None)
        );
        assert!(copy(classify(DetectedConsole::Nx, None, &path("nsz"))));
        assert_eq!(
            classify(DetectedConsole::Ntr, None, &path("nds")),
            Ok(Action::Zip)
        );
        assert_eq!(
            classify(DetectedConsole::Retro, None, &path("gba")),
            Ok(Action::Zip)
        );
        assert_eq!(
            classify(DetectedConsole::Retro, None, &path("gdi")),
            Err("GDI disc sets are not supported")
        );
        assert!(copy(classify(
            DetectedConsole::Chd,
            Some(&InfoResult::Chd(crate::disc::chd::info::ChdInfo {
                version: 5,
                ..Default::default()
            })),
            &path("chd")
        )));
        assert_eq!(
            convert(classify(
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
            convert(classify(DetectedConsole::Psx, None, &path("cue"))),
            ("chd.compress", "chd".to_string(), None)
        );
        assert_eq!(
            convert(classify(DetectedConsole::Psp, None, &path("iso"))),
            ("cso.compress", "cso".to_string(), Some("cso"))
        );
        assert!(copy(classify(DetectedConsole::Xbox, None, &path("xiso"))));
        assert_eq!(
            convert(classify(DetectedConsole::Xenon, None, &path("iso"))),
            ("xenon.compress", "zar".to_string(), None)
        );
        assert!(copy(classify(DetectedConsole::Ps3, None, &path("iso"))));
        assert_eq!(
            convert(classify(
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
            convert(classify(DetectedConsole::LaserDisc, None, &path("avi"))),
            ("chd.compress", "chd".to_string(), None)
        );
        assert!(copy(classify(DetectedConsole::Pbp, None, &path("pbp"))));
        // A standalone .bin never gets a console kind, so the detect-failure
        // path decides its skip before classify is reached.
        assert!(classify(DetectedConsole::Psx, None, &path("bin")).is_err());
    }

    /// `zip_exclude` demotes a stripped zip to a copy under the headerless
    /// extension: the copy still strips the header, so naming the output
    /// `.smc` would keep the header in the name.
    #[test]
    fn zip_exclude_demotion_keeps_the_headerless_extension() {
        let mut tokens = TemplateTokens::new(None, Path::new("Game.smc"), "zip");
        tokens.console = Some("Super Nintendo".to_string());
        let mut plan = UnitPlan {
            index: 0,
            source: PathBuf::from("Game.smc"),
            source_ext: "smc".to_string(),
            action: Action::Zip,
            tokens,
            input_subdir: PathBuf::new(),
            game: None,
            header: Some(headers::RomHeader {
                kind: "SMC",
                len: 512,
                headerless_ext: Some("sfc"),
            }),
            strip: 512,
            trim: None,
            pad: 0,
            pad_fill: 0x00,
            patch: None,
            staging_error: None,
            output_error: None,
            match_error: None,
            member: None,
            desired: None,
            decision: Decision::Keep,
        };
        let out = tempfile::tempdir().unwrap();
        let globs =
            super::super::clean::compile_globs(&["**/*.zip".to_string()], "zip_exclude").unwrap();

        resolve_paths(
            std::slice::from_mut(&mut plan),
            out.path(),
            "{console}/{basename}.{ext}",
            Some(&globs),
            None,
        );

        let desired = plan.desired.as_ref().unwrap();
        assert_eq!(
            desired.file_name().and_then(|name| name.to_str()),
            Some("Game.sfc")
        );
        assert_eq!(plan.tokens.ext, "sfc");
        assert_eq!(plan.action, Action::Copy);

        // A configured link mode survives the demotion: the raw payload is
        // placed as a link instead of a copy, still under the headerless
        // extension.
        plan.action = Action::Zip;
        plan.tokens.ext = "zip".to_string();
        resolve_paths(
            std::slice::from_mut(&mut plan),
            out.path(),
            "{console}/{basename}.{ext}",
            Some(&globs),
            Some(LinkMode::Hard),
        );
        assert_eq!(plan.action, Action::Link);
        assert_eq!(
            plan.desired
                .unwrap()
                .file_name()
                .and_then(|name| name.to_str()),
            Some("Game.sfc")
        );
    }

    /// A plan whose template resolves to an invalid path (a token value
    /// containing `..`) records the failure in `output_error` (the execute
    /// loop turns it into a Failed row) and resolves no output path; its
    /// neighbours resolve normally.
    #[test]
    fn invalid_template_output_fails_only_its_plan() {
        let mut good = plan_with_stem(0);
        good.tokens.dat = Some("No-Intro Gameboy".to_string());
        let mut hostile = plan_with_stem(1);
        hostile.tokens.dat = Some("..".to_string());
        let mut plans = vec![good, hostile];
        let out = tempfile::tempdir().unwrap();

        resolve_paths(&mut plans, out.path(), "{dat}/{basename}.{ext}", None, None);

        assert_eq!(
            plans[0].decision,
            Decision::Keep,
            "the healthy plan keeps its resolved path"
        );
        assert!(plans[0].desired.is_some());
        assert_eq!(plans[1].decision, Decision::Keep);
        assert!(plans[1].desired.is_none());
        let error = plans[1].output_error.as_deref().expect("recorded error");
        assert!(error.contains("output template"), "{error}");
    }

    /// The written payload's extension: the headerless extension under a
    /// strip, else the source's own.
    #[test]
    fn payload_ext_follows_the_header() {
        let mut plan = plan_with_stem(0);
        assert_eq!(plan.payload_ext(), "gb");

        plan.strip = 512;
        assert_eq!(plan.payload_ext(), "gb", "no header: the source ext");
        plan.header = Some(headers::RomHeader {
            kind: "SMC",
            len: 512,
            headerless_ext: Some("sfc"),
        });
        assert_eq!(plan.payload_ext(), "sfc");
    }

    /// An unparseable cartridge still names its console and frontend
    /// folder from the extension fallback, so frontend tokens resolve
    /// instead of coming out empty.
    #[tokio::test]
    async fn unparseable_cartridge_still_resolves_the_frontend_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Game.3ds"), b"garbage").unwrap();
        std::fs::write(dir.path().join("Past Game.gbc"), b"garbage").unwrap();
        let req = crate::runner::models::RunRequest {
            schema: None,
            operation: "organize".to_string(),
            input: Some(dir.path().to_path_buf()),
            output: None,
            config: None,
            preset: None,
            options: crate::runner::models::RunOptions::default(),
            dry_run: false,
            ctx: Default::default(),
        };

        let ctr = DatUnit::File(dir.path().join("Game.3ds"));
        let (plan, _) = plan_unit(
            &req,
            0,
            &ctr,
            dir.path(),
            None,
            &HashSet::new(),
            Ok(None),
            None,
            &super::super::patch::PatchIndex::default(),
            &crate::util::NoProgress,
            &crate::util::NoProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(plan.tokens.console.as_deref(), Some("3DS"));
        assert_eq!(plan.tokens.frontend_console.as_deref(), Some("3DS"));

        let gbc = DatUnit::File(dir.path().join("Past Game.gbc"));
        let (plan, _) = plan_unit(
            &req,
            1,
            &gbc,
            dir.path(),
            None,
            &HashSet::new(),
            Ok(None),
            None,
            &super::super::patch::PatchIndex::default(),
            &crate::util::NoProgress,
            &crate::util::NoProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(plan.tokens.console.as_deref(), Some("Game Boy"));
        assert_eq!(
            plan.tokens.frontend_console.as_deref(),
            Some("Game Boy Color")
        );
    }

    /// A split WUD set plans as one unit: a `game_part2.wud` next to its
    /// `game_part1.wud` is a Skip ("converted together with
    /// game_part1.wud") and never dispatches, while part1 lists the
    /// discovered continuation parts among its sources.
    #[tokio::test]
    async fn split_wud_continuations_fold_into_part1() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["game_part1.wud", "game_part2.wud", "game_part3.wud"] {
            std::fs::write(dir.path().join(name), vec![0u8; 64]).unwrap();
        }
        let req = crate::runner::models::RunRequest {
            schema: None,
            operation: "organize".to_string(),
            input: Some(dir.path().to_path_buf()),
            output: None,
            config: None,
            preset: None,
            options: crate::runner::models::RunOptions::default(),
            dry_run: false,
            ctx: Default::default(),
        };

        let part1 = DatUnit::File(dir.path().join("game_part1.wud"));
        let part2 = DatUnit::File(dir.path().join("game_part2.wud"));
        let continuations = super::super::split_continuations(&[part1.clone(), part2.clone()]);
        let (plan, _) = plan_unit(
            &req,
            1,
            &part2,
            dir.path(),
            None,
            &continuations,
            Ok(None),
            None,
            &super::super::patch::PatchIndex::default(),
            &crate::util::NoProgress,
            &crate::util::NoProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            plan.decision,
            Decision::Skip("converted together with game_part1.wud".to_string())
        );

        let (plan, _) = plan_unit(
            &req,
            0,
            &part1,
            dir.path(),
            None,
            &continuations,
            Ok(None),
            None,
            &super::super::patch::PatchIndex::default(),
            &crate::util::NoProgress,
            &crate::util::NoProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(plan.decision, Decision::Keep, "{:?}", plan.decision);
        // The fixed game_part1 stem would collide across dumps: the unit
        // names itself after its folder instead.
        assert_eq!(
            plan.tokens.basename,
            dir.path().file_name().unwrap().to_string_lossy()
        );
    }

    /// `remove_headers` entries are validated up front: known extensions
    /// plus `all` and `none`; a typo is an invalid argument instead of a
    /// silent no-op.
    #[test]
    fn remove_headers_entries_are_validated() {
        assert!(
            validate_remove_headers(&[
                "all".to_string(),
                "none".to_string(),
                "NES".to_string(),
                "sfc".to_string()
            ])
            .is_ok()
        );
        let err = validate_remove_headers(&["nse".to_string()]).unwrap_err();
        assert!(err.to_string().contains("remove_headers"), "{err}");
    }

    /// `all` strips a detected header like the bare flag does; `none`
    /// strips nothing.
    #[tokio::test]
    async fn remove_headers_all_strips_and_none_keeps() {
        let dir = tempfile::tempdir().unwrap();
        let mut rom = vec![0u8; 16];
        rom[..4].copy_from_slice(b"NES\x1a");
        rom.extend_from_slice(b"nes-body");
        std::fs::write(dir.path().join("Game.nes"), &rom).unwrap();
        let unit = DatUnit::File(dir.path().join("Game.nes"));

        for (entries, expected_strip) in
            [(vec!["all".to_string()], 16), (vec!["none".to_string()], 0)]
        {
            let req = crate::runner::models::RunRequest {
                schema: None,
                operation: "organize".to_string(),
                input: Some(dir.path().to_path_buf()),
                output: None,
                config: None,
                preset: None,
                options: crate::runner::models::RunOptions {
                    remove_headers: Some(entries),
                    ..crate::runner::models::RunOptions::default()
                },
                dry_run: false,
                ctx: Default::default(),
            };
            let (plan, _) = plan_unit(
                &req,
                0,
                &unit,
                dir.path(),
                None,
                &HashSet::new(),
                Ok(None),
                None,
                &super::super::patch::PatchIndex::default(),
                &crate::util::NoProgress,
                &crate::util::NoProgress,
                &CancelToken::new(),
            )
            .await
            .unwrap();
            assert_eq!(plan.strip, expected_strip);
        }
    }

    fn plan_with_stem(index: usize) -> UnitPlan {
        let mut tokens = TemplateTokens::new(None, Path::new("Game.gb"), "zip");
        tokens.console = Some("Game Boy".to_string());
        UnitPlan {
            index,
            source: PathBuf::from("Game.gb"),
            source_ext: "gb".to_string(),
            action: Action::Zip,
            tokens,
            input_subdir: PathBuf::new(),
            game: None,
            header: None,
            strip: 0,
            trim: None,
            pad: 0,
            pad_fill: 0x00,
            patch: None,
            staging_error: None,
            output_error: None,
            match_error: None,
            member: None,
            desired: None,
            decision: Decision::Keep,
        }
    }
}
