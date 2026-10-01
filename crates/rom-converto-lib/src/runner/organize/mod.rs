//! The `organize` runner op: scan a library directory, classify every unit
//! into its best archival format, and place it under
//! `<output_dir>/<console>/<name>.<ext>`, optionally renaming via the
//! Playmatch DAT database (`dat`), deleting placed sources (`move_source`)
//! and writing multi-disc playlists (`playlists`).
//!
//! The op runs in phases: plan every unit ([`plan::plan_units`]), match DAT
//! names and apply the global selection/layout passes, then execute the
//! surviving plans. Playlists are derived data: their paths are planned
//! before clean so they join the keep set, then re-planned from the cleaned
//! disc dirs and written: an `.m3u` that is already current is skipped, not
//! rewritten, a symlinked `.m3u` is never written through, a differing
//! `.m3u` under the input root is kept unless it is the tool's own
//! derivation for the discs it lists, a read-only `.m3u` is refused, and a
//! planned `.m3u` whose discs clean removed is deleted. With `dat`, a dry
//! run still hashes every unit and queries the Playmatch API; playlists are
//! only written on real runs.

mod clean;
mod dat_match;
mod headers;
mod input;
mod layout;
mod patch;
mod place;
mod plan;
mod select;
mod trim;

use super::defaults::apply_config_defaults;
use super::models::{
    COULD_NOT_VERIFY, OrganizeData, OrganizeRow, PlaylistPlanData, RunData, RunOptions, RunRequest,
    RunResponse, RunRow, RunStatus, VERIFIED_VALID, VerifyVerdict,
};
use super::ops::{
    batch_message, conflict_policy, elapsed_ms, required_input, run_single_request,
    totals_for_records,
};
use super::{RUN_SCHEMA, invalid_arg, is_cancelled_error, planned_verb};
use crate::dat::units::DatUnit;
use crate::playlist::{PlaylistMode, PlaylistOptions, PlaylistPlan, plan_playlists};
use crate::util::{
    CancelToken, Cancelled, ConflictPolicy, FileStatus, OutputExists, PlanDecision,
    ProgressReporter, ReportRecord, ReportRecordInput, ResolvedInput, ZipFormat,
};
use anyhow::{Context, Result};
use dat_match::DatNaming;
use place::LinkMode;
use plan::{Action, Decision, UnitPlan, action_label};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::io::AsyncReadExt;

/// Layout applied when the request carries no `output_template`.
const DEFAULT_OUTPUT_TEMPLATE: &str = "{console}/{basename}.{ext}";

/// Output extensions that count as disc images for playlist grouping.
const PLAYLIST_DISC_EXTS: &[&str] = &["chd", "cue", "iso", "cso", "zso", "rvz"];

/// Longest existing `.m3u` held for the byte-identical comparison, matching
/// the bounded-line cap used for sheets; anything larger warns and counts as
/// differing instead of loading whole.
const MAX_PLAYLIST_COMPARE_BYTES: u64 = 16 * 1024 * 1024;

/// Organizes a library directory: one [`OrganizeRow`] per unit, one
/// [`ReportRecord`] per row so reports and CLI totals keep one spelling.
pub(crate) async fn organize(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    // organize runs under its own conflict policy: an unset `on_conflict`
    // is an error policy and the runner-level default never applies, so the
    // request is normalized once and every child dispatch sees the same
    // policy.
    let mut req = req;
    conflict_policy(&req)?;
    if req.options.on_conflict.is_none() {
        req.options.on_conflict = Some("error".to_string());
    }
    req.ctx.default_conflict = None;
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

    // Option values are validated up front, before any scanning or staging:
    // a bad glob, regex, link mode, or format is an invalid argument. The
    // parsed values flow into planning and placement, which never re-parse.
    patch::PatchIndex::validate_paths(req.options.patch.as_deref().unwrap_or(&[]))?;
    let zip_format = place::zip_format(&req.options)?;
    let link_mode = place::link_mode(&req.options)?;
    let delete_dirs = clean::move_delete_dirs(&req.options)?;
    let clean_options = clean::CleanOptions::from_options(&req.options)?;
    // Exclude globs compile before any scanning: a bad glob is an invalid
    // argument, never a mid-run failure after outputs were placed.
    let zip_exclude = req
        .options
        .zip_exclude
        .as_deref()
        .map(|pattern| clean::compile_globs(&[pattern.to_string()], "zip_exclude"))
        .transpose()?;
    let input_exclude = req
        .options
        .input_exclude
        .as_deref()
        .filter(|globs| !globs.is_empty())
        .map(|globs| clean::compile_globs(globs, "input_exclude"))
        .transpose()?;
    plan::validate_remove_headers(req.options.remove_headers.as_deref().unwrap_or(&[]))?;
    let filters = select::Filters::from_options(&req.options)?;
    let preferences = select::Preferences::from_options(&req.options)?;
    let letter_layout = layout::LetterLayout::from_options(&req.options)?;
    // `single` and the prefer_* orderings rank DAT matches; without `dat`
    // there are no matches to rank and the options would silently do
    // nothing. `prefer_filename_regex` is exempt: it also tie-breaks
    // dedupe_by_path, which is DAT-independent.
    let dat_enabled = req.options.dat == Some(true);
    if (preferences.single
        || preferences.prefer_verified
        || preferences.prefer_good
        || preferences.prefer_retail
        || preferences.prefer_parent
        || preferences.revision.is_some()
        || !preferences.game_regex.is_empty()
        || !preferences.languages.is_empty()
        || !preferences.regions.is_empty())
        && !dat_enabled
    {
        return Err(invalid_arg("single/prefer options need dat"));
    }

    let scanned = crate::dat::units::collect_units(&root, req.options.max_depth, &cancel)
        .await
        .with_context(|| format!("scanning {}", root.display()))?;
    // Capture every scanned unit's files before exclusions drop them: clean
    // must never eat a file the scan saw just because this run excluded it.
    let scanned_sources: Vec<PathBuf> = scanned.iter().flat_map(unit_source_files).collect();
    // How many scanned units claim each source file, by identity: a bin two
    // cue sheets share, or a split part two first parts read, is never
    // removed.
    let mut source_claims = source_claim_counts(&scanned);
    if scanned.is_empty() {
        return Err(invalid_arg(format!("no files found in {}", root.display())));
    }
    let units = input::retain_not_excluded(scanned, &root, input_exclude.as_ref());
    if units.is_empty() {
        // Every scanned file matches an exclude glob: like a filter that
        // removes everything, the run finishes with nothing to do.
        progress.warn(&format!(
            "every file in {} matches input_exclude; nothing to organize",
            root.display()
        ));
    }
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

    // Patch indexing and DAT checksum bounds are ready before planning so
    // staged members can compute every later hash while the extraction lives.
    let patch_index = patch::PatchIndex::build(
        req.options.patch.as_deref().unwrap_or(&[]),
        progress,
        &cancel,
    )
    .await?;
    let dat_bounds = if dat_enabled {
        Some(super::dat::dat_checksum_bounds(
            &req,
            &[crate::util::HashAlgo::Crc32, crate::util::HashAlgo::Sha1],
        )?)
    } else {
        None
    };

    // Phase 1: plan every unit, compute archive member facts, then retain a
    // bounded prefix of real-run stagings for execution.
    let (mut plans, mut stagings) = plan::plan_units(
        &req,
        &units,
        &root,
        link_mode,
        dat_bounds.as_ref(),
        &patch_index,
        progress,
        file_progress.as_ref(),
        &cancel,
    )
    .await?;

    // Phase 2: the global passes, in fixed order. Matching runs before patch
    // expansion, so `plans` holds only base plans here.
    let naming = if let Some(bounds) = dat_bounds.as_ref() {
        dat_match::match_plans(
            &req,
            &units,
            &mut plans,
            bounds,
            file_progress.as_ref(),
            &cancel,
        )
        .await?
    } else {
        vec![DatNaming::default(); units.len()]
    };
    select::apply_filters(&mut plans, &filters);
    select::apply_single(&mut plans, &preferences);
    select::dedupe_by_game(&mut plans, &preferences);
    // Patch matching uses plan-time CRCs for staged archive members and
    // streams plain-file CRCs only for the surviving plans.
    let crcs = if patch_index.is_empty() {
        Vec::new()
    } else {
        unit_crcs(&plans, &units, progress, &cancel).await?
    };
    patch::expand_patched(
        &mut plans,
        &patch_index,
        &|index: usize| crcs.get(index).copied().flatten(),
        req.options.patch_only == Some(true),
    );
    dat_match::apply_dat_naming(&mut plans, &naming);
    plan::resolve_paths(
        &mut plans,
        &output_dir,
        &template,
        zip_exclude.as_ref(),
        link_mode,
    );
    // Clean manages the template-level directory of each output; letter
    // subdirectories below it belong to this run's layout, so the dirs are
    // captured before the letter pass rewrites `desired`. Every plan still
    // carries its pre-letter dir here; the Keep filter is applied at clean
    // time, after dedupe has marked the losers.
    let mut managed_dirs: Vec<Vec<PathBuf>> = plans
        .iter()
        .map(|plan| match plan.desired.as_ref() {
            Some(desired) => desired
                .parent()
                .map(Path::to_path_buf)
                .into_iter()
                .collect(),
            None => Vec::new(),
        })
        .collect();
    // Dedupe the flat output paths before the letter pass: a same-path
    // duplicate must not inflate the letter bucket that computes chunk dirs
    // (two `Ga` plans and one `Gb` plan would chunk as G1=[Ga, Ga],
    // G2=[Gb] instead of one G bucket over the two survivors). Without a
    // letter layout this one pass stands; with one, the losers split out
    // here and rejoin their group's lettered path below, so the post-letter
    // pass can re-decide every group.
    let fold_case = select::probe_case_insensitive(&output_dir);
    select::dedupe_by_path(&mut plans, &preferences, fold_case);
    if let Some(layout) = letter_layout.as_ref() {
        // Only a dedupe loser is Skip with a resolved path: every other skip
        // decision predates `resolve_paths`. The mask indexes the original
        // vector, which keeps its order; execution and rows follow input
        // order under letter layouts too.
        let losers: Vec<bool> = plans
            .iter()
            .map(|plan| plan.desired.is_some() && matches!(plan.decision, Decision::Skip(_)))
            .collect();
        // Each survivor's flat path maps to its lettered path, so a
        // duplicate can follow its group. Keyed like `dedupe_by_path`
        // groups, so a duplicate whose spelling only folds to its group's
        // finds the lettered path too. The keys are snapshotted BEFORE the
        // letter pass; a survivor without a desired path (a skip decided
        // before `resolve_paths`) maps to `None` and never shifts the
        // pairing.
        let flat: Vec<Option<String>> = plans
            .iter()
            .map(|plan| {
                plan.desired
                    .as_ref()
                    .map(|d| select::path_key(d, fold_case))
            })
            .collect();
        // `apply_letter_dirs` only rewrites Keep plans, so the losers need
        // not split out of the vector: the pass letters the winners in
        // place.
        layout::apply_letter_dirs(&mut plans, layout);
        let mut lettered: HashMap<String, PathBuf> = HashMap::new();
        for (key, (plan, &loser)) in flat.iter().zip(plans.iter().zip(&losers)) {
            if let (Some(key), Some(desired)) = (key, plan.desired.as_ref())
                && !loser
            {
                lettered.insert(key.clone(), desired.clone());
            }
        }
        // The duplicates rejoin their group at the group's (lettered) path,
        // and the post-letter pass re-decides every group: `in_place` only
        // recognizes a unit that already sits at its output once the letter
        // dir is part of that path, and such a unit must win its group. A
        // loser's group winner is a Keep plan dedupe kept: it was lettered,
        // so the lookup cannot miss.
        for (plan, &loser) in plans.iter_mut().zip(&losers) {
            if !loser {
                continue;
            }
            let group = plan
                .desired
                .as_ref()
                .and_then(|d| lettered.get(&select::path_key(d, fold_case)))
                .expect("group winner survived the split")
                .clone();
            plan.desired = Some(group);
            plan.decision = Decision::Keep;
        }
        // A flat template resolves straight into the output dir, which clean
        // walks non-recursively: the post-letter dirs written below it (e.g.
        // out/A) become managed too, so stale files inside written letter
        // dirs are still cleaned. Runs after the rejoin, over every plan, so
        // a rejoined winner's letter dir is managed as well.
        for (plan, dirs) in plans.iter().zip(&mut managed_dirs) {
            if dirs.first().is_some_and(|dir| dir == &output_dir)
                && let Some(parent) = plan.desired.as_ref().and_then(|desired| desired.parent())
            {
                dirs.push(parent.to_path_buf());
            }
        }
        select::dedupe_by_path(&mut plans, &preferences, fold_case);
    }

    // Phase 3: execute the surviving plans.
    let mut rows = Vec::with_capacity(plans.len());
    let mut disc_dirs = BTreeSet::new();
    // Source files this run actually removed (`move_source`): the
    // input-side directories `delete_empty_dirs` may prune.
    let mut moved_sources: Vec<PathBuf> = Vec::new();
    // Every unit's own source files, computed once for the run: the file
    // itself (with its split-set parts) or a cue set's cue and bins.
    let unit_files: Vec<Vec<PathBuf>> = units.iter().map(unit_source_files).collect();
    // Sources by identity, with their owning unit: writing an output onto
    // another unit's unreleased source would destroy that unit before it
    // runs. Excluded sources are not units, so they get no owner here; the
    // under-input refusal covers them instead.
    let mut source_identities: HashMap<FileIdentity, Vec<(usize, PathBuf)>> = HashMap::new();
    for (index, files) in unit_files.iter().enumerate() {
        for path in files {
            if let Some(identity) = FileIdentity::of(path) {
                source_identities
                    .entry(identity)
                    .or_default()
                    .push((index, path.clone()));
            }
        }
    }
    let mut guards = WriteGuards {
        sources: &source_identities,
        realized: HashSet::new(),
        released: HashSet::new(),
        policy: conflict_policy(&req)?,
        root: std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone()),
    };
    let overlap = paths_overlap(&root, &output_dir);
    // A unit's batch bytes advance once, and its sources move out, only
    // when its last plan finishes.
    let patch_only = req.options.patch_only == Some(true);
    let mut unit_plans = vec![UnitBook::default(); units.len()];
    for plan in &plans {
        let book = &mut unit_plans[plan.index];
        book.plans += 1;
        if !matches!(plan.decision, Decision::Skip(_)) {
            book.keeps += 1;
        }
        // A base plan that lost dedupe_by_path still carries its desired
        // path: the unit's archival output went to a sibling, so the source
        // must never move; its patched variants are derived artifacts.
        if plan.patch.is_none()
            && plan.desired.is_some()
            && matches!(plan.decision, Decision::Skip(_))
        {
            book.base_lost = true;
        }
        // A base plan that never produced an output path (a patch-only run,
        // a staging failure, an invalid output path, or a failed DAT match)
        // leaves the unit's original unplaced: the source stays. The base
        // plan's DAT failure is remembered separately: patched variants
        // carry none, but the degrade still keeps the source.
        if plan.patch.is_none()
            && (patch_only
                || plan.staging_error.is_some()
                || plan.output_error.is_some()
                || plan.match_error.is_some())
        {
            book.base_lost = true;
        }
        if plan.patch.is_none() && plan.match_error.is_some() {
            book.base_match_error = true;
        }
    }
    // Release sources that selection or planning removed before execution.
    for (index, book) in unit_plans.iter().enumerate() {
        if book.keeps == 0 {
            stagings[index] = None;
        }
    }
    // Row indices per unit, with whether the plan was a Keep plan: a source
    // removal failure flips only the rows that claimed the move.
    type SettledRows = (usize, usize, Vec<(usize, bool)>);
    let mut unit_rows: Vec<SettledRows> = vec![(0, 0, Vec::new()); units.len()];
    for plan in &plans {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let unit = &units[plan.index];
        let unit_started = Instant::now();
        // The write guards: an existing output that is another unit's
        // unreleased source, whose own location is under the input root, or
        // one this run already realized, is refused instead of overwritten.
        // Under overwrite the refusal runs up front. Under
        // overwrite-invalid a placement waits until the existing output's
        // verify (a valid output is kept, not overwritten). A conversion
        // whose desired path the guard would refuse never rewrites it: a
        // realized path returns the keep row without dispatching, and the
        // other causes dispatch the child with a skip policy, so the
        // foreign or input-tree file is kept and the row reports a keep
        // rather than a failure.
        let own_sources = &unit_files[plan.index];
        let convert_keep_reason = (guards.policy == ConflictPolicy::OverwriteInvalid
            && matches!(plan.action, Action::Convert { .. }))
        .then(|| guards.would_refuse_cause(plan, own_sources))
        .flatten();
        let refused = if guards.policy == ConflictPolicy::Overwrite {
            guards.refuse(plan, own_sources, unit.size_bytes(), unit_started, dry_run)
        } else {
            None
        };
        let row = match refused {
            Some(row) => row,
            None => {
                // A fallback extraction can run short of temp space while
                // other units' stagings are retained: free them all and run
                // the unit once more.
                let row = loop {
                    let row = execute_unit(
                        &req,
                        plan,
                        stagings[plan.index].as_ref(),
                        unit,
                        own_sources,
                        zip_format,
                        link_mode,
                        &guards,
                        convert_keep_reason.clone(),
                        progress,
                        file_progress.as_ref(),
                        &cancel,
                    )
                    .await;
                    match &row {
                        Err(err)
                            if err
                                .chain()
                                .any(|cause| cause.is::<crate::util::TempSpaceShortfall>())
                                && stagings.iter().any(Option::is_some) =>
                        {
                            stagings.iter_mut().for_each(|staging| *staging = None);
                        }
                        _ => break row,
                    }
                };
                match row {
                    Ok(row) => row,
                    Err(err) if is_cancelled_error(&err) || cancel.is_cancelled() => {
                        return Err(err);
                    }
                    Err(err) => {
                        progress.warn(&err.to_string());
                        failed_row(
                            unit.display_path(),
                            None,
                            "organize".to_string(),
                            plan::error_detail(&err),
                            unit.size_bytes(),
                            unit_started,
                            dry_run,
                        )
                    }
                }
            }
        };
        // Every non-failed row with a disc-image output feeds playlist
        // planning: skipped-but-valid outputs and dry-run plans count too.
        // The same rows join the realized set: a later unit desiring the
        // same file by identity is refused above.
        if row.status != FileStatus::Failed
            && let Some(output) = &row.output
        {
            guards.realized(output);
            if let Some(dir) = output.parent()
                && PLAYLIST_DISC_EXTS.contains(&ext_of(output))
            {
                disc_dirs.insert(dir.to_path_buf());
            }
        }
        let (done, ok_rows) = {
            let state = &mut unit_rows[plan.index];
            state.0 += 1;
            state.1 += usize::from(keep_satisfied(&row, &plan.action));
            state
                .2
                .push((rows.len(), !matches!(plan.decision, Decision::Skip(_))));
            (state.0, state.1)
        };
        rows.push(row);
        let UnitBook {
            plans: plan_count,
            keeps: keep_count,
            base_lost,
            base_match_error,
        } = unit_plans[plan.index];
        if done == plan_count {
            stagings[plan.index] = None;
            // The unit's last plan: the batch advances once per unit.
            progress.batch_advance(unit.size_bytes());
            progress.inc(1);
            // Sources move out only after every Keep plan of the unit
            // (base plus patched variants) is satisfied, and never when the
            // base plan lost dedupe_by_path or was dropped by patch_only:
            // in both cases the unit's own original must stay.
            if move_source && keep_count > 0 && ok_rows == keep_count && !base_lost {
                let state = &unit_rows[plan.index];
                let outputs: Vec<PathBuf> = state
                    .2
                    .iter()
                    .filter_map(|&(index, _)| rows[index].output.clone())
                    .collect();
                // Child records can claim success without the output landing
                // on disk; never delete sources for an output that is not
                // there (a link is checked by its own path).
                let missing: Vec<&PathBuf> = if dry_run {
                    // A dry run wrote nothing: it applies the same release
                    // gate below, on paths that only exist as plans.
                    Vec::new()
                } else {
                    outputs
                        .iter()
                        .filter(|out| std::fs::symlink_metadata(out).is_err())
                        .collect()
                };
                let (removed, failures, kept_notes) = if missing.is_empty() {
                    // An archive holding several entries is never removed:
                    // every entry this run did not place would be destroyed
                    // with it.
                    match archive_member_note(unit).await {
                        Some(note) => {
                            if !dry_run {
                                progress.warn(&note);
                            }
                            (Vec::new(), Vec::new(), vec![note])
                        }
                        None => {
                            remove_sources(
                                unit,
                                own_sources,
                                &outputs,
                                &guards.root,
                                &mut source_claims,
                                dry_run,
                            )
                            .await
                        }
                    }
                } else {
                    (
                        Vec::new(),
                        missing
                            .iter()
                            .map(|out| {
                                (
                                    (*out).clone(),
                                    std::io::Error::new(
                                        std::io::ErrorKind::NotFound,
                                        format!("output {} is missing", out.display()),
                                    ),
                                )
                            })
                            .collect(),
                        Vec::new(),
                    )
                };
                if failures.is_empty() {
                    // A dry run reports the same relabel and notes as the
                    // real run, and tracks what the unit would release so
                    // later units see the sources a real run frees.
                    // Conversions and zips keep their action names; plain
                    // copies rename to "move" once their source is actually
                    // gone (a kept archive or bin is not a move).
                    if kept_notes.is_empty() && !removed.is_empty() {
                        for &(index, _) in state.2.iter() {
                            if rows[index].action == "copy" && rows[index].status == FileStatus::Ok
                            {
                                rows[index].action = "move".to_string();
                            }
                        }
                    }
                    for note in &kept_notes {
                        for &(index, _) in state.2.iter() {
                            rows[index].detail = Some(match rows[index].detail.take() {
                                Some(existing) => format!("{existing}; {note}"),
                                None => note.clone(),
                            });
                        }
                    }
                    if !dry_run {
                        moved_sources.extend(removed.iter().cloned());
                    }
                    guards.release(removed);
                } else {
                    // The rows claim a move that did not happen: mark them
                    // failed so the exit code and the report show it. Only
                    // the rows whose plan was Keep claimed a move.
                    let detail = failures
                        .iter()
                        .map(|(_, err)| format!("source not removed: {err}"))
                        .collect::<Vec<_>>()
                        .join("; ");
                    for &(index, keep_plan) in state.2.iter() {
                        if keep_plan {
                            rows[index].status = FileStatus::Failed;
                            rows[index].detail = Some(detail.clone());
                        }
                    }
                    for (path, err) in failures {
                        progress.warn(&format!("could not move source {}: {err}", path.display()));
                    }
                }
            } else if move_source
                && !dry_run
                && keep_count > 0
                && ok_rows == keep_count
                && base_lost
                && base_match_error
            {
                // A DAT-degraded unit keeps its source silently otherwise:
                // the row stays Ok, so it says why the source stayed.
                for &(index, _) in unit_rows[plan.index].2.iter() {
                    rows[index].detail = Some(match rows[index].detail.take() {
                        Some(existing) => {
                            format!("{existing}; source kept: DAT match failed")
                        }
                        None => "source kept: DAT match failed".to_string(),
                    });
                }
            }
            // The unit has settled: stream its rows once, with their final
            // actions and statuses. Buffering them until here (after the
            // last plan and the source removal) keeps rows from being
            // streamed twice or under an action that later changes.
            for &(index, _) in unit_rows[plan.index].2.iter() {
                progress.row(&RunRow::Organize(rows[index].clone()));
            }
        }
    }
    progress.finish();

    let mut records: Vec<ReportRecord> = rows.iter().map(|row| row_record(row, dry_run)).collect();

    // Clean the output directory of stale files once every output is placed,
    // BEFORE the playlists are re-planned and written: a playlist is built
    // from the cleaned disc directories, so a stale disc never lands in a
    // fresh .m3u. The planned `.m3u` paths join the keep set first, so clean
    // can never eat a playlist this run is about to re-derive. A directory
    // counts as managed when a plan resolved into it and its row did not
    // fail: written, planned (dry run), or kept because a valid output
    // already existed, so re-runs keep cleaning.
    let mut planned_playlists: Vec<PlaylistPlan> = Vec::new();
    let mut warned_playlist_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    // The keep set every deletion pass obeys: placed rows' paths and the
    // scan's sources. The playlist additions below join `clean`'s copy only:
    // a planned playlist is keep-eligible just until it stops being derived,
    // so a stale one must stay deletable.
    let mut unit_keep = clean_keep_set(&plans, &rows);
    unit_keep.extend(scanned_sources.iter().cloned());
    // Files a dry-run clean only planned to remove: the post-clean playlist
    // scan must ignore them, so staleness previews what a real run sees.
    let mut clean_planned_deletes: BTreeSet<PathBuf> = BTreeSet::new();
    // Disc dirs whose playlist plan failed: their playlists are unplan-able,
    // never stale.
    let mut failed_playlist_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    // Clean runs only when every plan produced (or kept) a well-defined
    // output: a unit that failed before its output path existed has a
    // previous output that must not be swept.
    let plan_errored = plans.iter().any(|plan| {
        plan.staging_error.is_some() || plan.output_error.is_some() || plan.match_error.is_some()
    });
    let clean_active = clean_options.is_some() && !plan_errored;
    if clean_options.is_some() {
        if overlap {
            progress.warn(
                "input and output directories overlap: clean never deletes files under the \
                 input directory",
            );
        }
        if plan_errored {
            progress.warn(
                "clean skipped: a unit failed to stage, resolve its output path, or match the \
                 DAT, so previous outputs must not be swept",
            );
            let row = OrganizeRow {
                input: output_dir.clone(),
                output: None,
                console: None,
                action: "clean".to_string(),
                status: FileStatus::Failed,
                planned: dry_run,
                verify: None,
                detail: Some(
                    "clean skipped: a unit failed to stage, resolve its output path, or match \
                     the DAT"
                        .to_string(),
                ),
                input_bytes: 0,
                output_bytes: 0,
                elapsed_ms: 0,
            };
            progress.row(&RunRow::Organize(row.clone()));
            records.push(row_record(&row, dry_run));
            rows.push(row);
        }
    }
    if clean_active {
        let clean_options = clean_options
            .as_ref()
            .expect("clean_active implies options");
        // Playlists are derived data. Plan their paths BEFORE clean, from
        // the disc dirs this run produced, so every planned `.m3u` joins the
        // keep set: clean can never eat a playlist that is about to be
        // re-derived. The post-clean write pass re-plans below, sharing this
        // warned-dirs set so a dir whose plan fails warns only once.
        let mut keep_playlists: Vec<PathBuf> = Vec::new();
        if req.options.playlists == Some(true) {
            let (playlist_plans, failed) = plan_disc_playlists(
                &disc_dirs,
                dry_run,
                &mut warned_playlist_dirs,
                &BTreeSet::new(),
                progress,
                &cancel,
            )?;
            planned_playlists = playlist_plans;
            failed_playlist_dirs = failed;
            keep_playlists.extend(planned_playlists.iter().map(|plan| plan.m3u_path.clone()));
        }
        // The Keep filter runs here, after dedupe: a dedupe_by_path loser's
        // dirs must not mark anything managed, and a failed row's dirs are
        // not written either.
        let written_dirs: BTreeSet<PathBuf> = managed_dirs
            .iter()
            .zip(plans.iter())
            .zip(&rows)
            .filter(|((_, plan), row)| {
                matches!(plan.decision, Decision::Keep) && row.status != FileStatus::Failed
            })
            .flat_map(|((dirs, _), _)| dirs.iter().cloned())
            .collect();
        // The keep set stays raw: `clean_output`'s `KeepSet` does the
        // canonicalizing and records link identity itself, so a placed
        // symlink is kept as its own link instead of collapsing into the
        // file it points at.
        let mut keep = unit_keep.clone();
        keep.extend(keep_playlists);
        // The input root is passed whatever the trees do: a written dir
        // reached through a symlink can hold entries whose own location
        // lies under the input root, and clean never deletes those.
        let clean_rows = clean::clean_output(
            &output_dir,
            &written_dirs,
            keep,
            &guards.root,
            clean_options,
            conflict_policy(&req)?,
            dry_run,
            progress,
            &cancel,
        )
        .await?;
        // Only rows whose file actually went away preview staleness: a
        // failed row's file is still on disk and still derives playlists.
        clean_planned_deletes = clean_rows
            .iter()
            .filter(|row| row.status == FileStatus::Ok)
            .map(|row| row.input.clone())
            .collect();
        for row in clean_rows {
            progress.row(&RunRow::Organize(row.clone()));
            records.push(row_record(&row, dry_run));
            rows.push(row);
        }
    }

    // Playlists are planned in dry runs too (for the returned data only, a
    // dry run never writes). Every planned or written playlist streams and
    // records one "playlist" row so the CLI prints it and totals count it.
    let (playlists, playlist_rows, post_clean_planned, post_clean_failed) =
        if req.options.playlists == Some(true) {
            write_playlists(
                &req,
                &disc_dirs,
                &mut warned_playlist_dirs,
                &clean_planned_deletes,
                &guards.root,
                progress,
                &cancel,
            )
            .await?
        } else {
            (Vec::new(), Vec::new(), BTreeSet::new(), BTreeSet::new())
        };
    for row in playlist_rows {
        progress.row(&RunRow::Organize(row.clone()));
        records.push(row_record(&row, dry_run));
        rows.push(row);
    }
    // A pre-clean planned `.m3u` the post-clean PLAN no longer contains was
    // built from discs clean just removed: the derived file is stale too.
    // Staleness is planned-minus-planned, never derived from rows: a
    // playlist that failed to read or write, or was refused as a symlink,
    // was still planned, so the file on disk survives. A dir whose plan
    // failed is skipped: nothing could be planned there, so its playlists
    // are unverifiable, not stale. Deleted backup-aware like the clean rows,
    // and only under `clean`: without it nothing was removed, so every
    // planned playlist still derives.
    if clean_active {
        let clean_options = clean_options
            .as_ref()
            .expect("clean_active implies options");
        failed_playlist_dirs.extend(post_clean_failed);
        let stale = stale_playlist_paths(
            &planned_playlists,
            &post_clean_planned,
            &failed_playlist_dirs,
        );
        let stale_rows = clean::delete_stale_playlists(
            &output_dir,
            &stale,
            unit_keep,
            &guards.root,
            clean_options,
            dry_run,
            progress,
            &cancel,
        )
        .await?;
        for row in stale_rows {
            progress.row(&RunRow::Organize(row.clone()));
            records.push(row_record(&row, dry_run));
            rows.push(row);
        }
    }
    if move_source && !dry_run && delete_dirs != clean::MoveDeleteDirs::Never {
        // Only directories that actually lost files to this run's moves are
        // candidates for pruning.
        let touched: BTreeSet<PathBuf> = moved_sources
            .iter()
            .filter_map(|path| path.parent().map(Path::to_path_buf))
            .collect();
        let removed = clean::delete_empty_dirs(&root, &touched, delete_dirs);
        if !removed.is_empty() {
            progress.set_phase(&format!("removed {} empty directories", removed.len()));
        }
    }

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

fn dry_run_needs_member(
    plan: &UnitPlan,
    policy: ConflictPolicy,
    desired: &Path,
    guards: &WriteGuards<'_>,
) -> bool {
    let action_needs_member = match &plan.action {
        Action::Convert { op, .. } => matches!(*op, "dol.migrate" | "rvl.migrate"),
        Action::Zip | Action::Copy | Action::Link => {
            policy == ConflictPolicy::OverwriteInvalid
                && desired.exists()
                && !guards.is_released(desired)
        }
    };
    action_needs_member || plan.patch.is_some()
}

/// Executes one planned unit, producing its row. Only cancellation aborts
/// the run: every per-unit failure becomes a `failed` row.
#[allow(clippy::too_many_arguments)]
async fn execute_unit(
    req: &RunRequest,
    plan: &UnitPlan,
    staged_member: Option<&ResolvedInput>,
    unit: &DatUnit,
    own_sources: &[PathBuf],
    zip_format: ZipFormat,
    link_mode: Option<LinkMode>,
    guards: &WriteGuards<'_>,
    convert_keep_reason: Option<RefusalCause>,
    progress: &dyn ProgressReporter,
    file_progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<OrganizeRow> {
    let unit_started = Instant::now();
    let dry_run = req.dry_run;
    let primary = unit.display_path().to_path_buf();
    let input_bytes = unit.size_bytes();
    let console = plan.tokens.console.clone();

    // A plan-time staging or output-path failure never produced an output
    // path: the row fails, and clean is skipped for the run so the unit's
    // previous output keeps its protection.
    if let Some(error) = plan.staging_error.as_ref().or(plan.output_error.as_ref()) {
        return Ok(failed_row(
            &primary,
            console,
            "organize".to_string(),
            error.clone(),
            input_bytes,
            unit_started,
            dry_run,
        ));
    }
    if let Decision::Skip(reason) = &plan.decision {
        // A plan-phase skip kept no output: it never ran, so the row names
        // no file for playlists or clean to protect.
        return Ok(skip_row(
            &primary,
            console,
            reason,
            None,
            input_bytes,
            unit_started,
            dry_run,
        ));
    }
    if console.is_none() {
        progress.warn(&format!(
            "{}: console not identified, filing at the output root",
            primary.display()
        ));
    }
    let desired = plan
        .desired
        .as_ref()
        .expect("Keep plan without a resolved output path");

    // Same-path guard: a unit whose desired output is one of its own source
    // files is already in place; nothing runs and nothing is deleted. The
    // comparison is the same directory entry with parents resolved: a final
    // symlink or a hardlink twin at the output path is not in place.
    if own_sources.iter().any(|path| same_location(path, desired)) {
        return Ok(skip_row(
            &primary,
            console,
            "already in place",
            Some(desired.clone()),
            input_bytes,
            unit_started,
            dry_run,
        ));
    }

    let archive = crate::util::is_archive_path(&primary);
    let needs_member =
        !dry_run || !archive || dry_run_needs_member(plan, guards.policy, desired, guards);
    let fallback = if archive && staged_member.is_none() && needs_member {
        match plan::stage_unit(&primary, cancel).await {
            Ok(resolved) => resolved,
            Err(err) if is_cancelled_error(&err) => return Err(err),
            // The execute loop frees retained stagings and retries.
            Err(err)
                if err
                    .chain()
                    .any(|cause| cause.is::<crate::util::TempSpaceShortfall>()) =>
            {
                return Err(err);
            }
            Err(err) => {
                progress.warn(&err.to_string());
                return Ok(failed_row(
                    &primary,
                    console,
                    "organize".to_string(),
                    plan::error_detail(&err),
                    input_bytes,
                    unit_started,
                    dry_run,
                ));
            }
        }
    } else {
        None
    };
    let mut source: &Path = if archive && staged_member.is_none() && fallback.is_none() {
        plan.member
            .as_ref()
            .expect("staged archive plan without member facts")
            .basis
            .as_path()
    } else {
        staged_member
            .or(fallback.as_ref())
            .map(ResolvedInput::path)
            .unwrap_or(primary.as_path())
    };
    let actual_member = !archive || staged_member.is_some() || fallback.is_some();

    // A patched plan writes the patched ROM into a temp dir and uses it as
    // the source for the rest of the pipeline. Patch parsing and application
    // are blocking file work; they run on the blocking pool so a large patch
    // never stalls the async runtime. A failure is a failed row, like every
    // other placement error.
    let patched = match (&plan.patch, actual_member) {
        (Some(patch_path), true) => {
            let temp = match tempfile::TempDir::with_prefix("rom-converto-patch") {
                Ok(temp) => temp,
                Err(err) => {
                    return Ok(failed_row(
                        &primary,
                        console,
                        action_label(&plan.action),
                        err.to_string(),
                        input_bytes,
                        unit_started,
                        dry_run,
                    ));
                }
            };
            let target = temp.path().join(
                source
                    .file_name()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("patched")),
            );
            let patch_path = patch_path.clone();
            let src = source.to_path_buf();
            let dst = target.clone();
            let cancel = cancel.clone();
            let applied = tokio::task::spawn_blocking(move || -> Result<()> {
                let patch = crate::patch::Patch::open(&patch_path, &cancel)?;
                patch.apply(&src, &dst, &cancel)
            })
            .await;
            match applied {
                Ok(Ok(())) => Some((temp, target)),
                Ok(Err(err)) if is_cancelled_error(&err) => return Err(err),
                Ok(Err(err)) => {
                    return Ok(failed_row(
                        &primary,
                        console,
                        action_label(&plan.action),
                        plan::error_detail(&err),
                        input_bytes,
                        unit_started,
                        dry_run,
                    ));
                }
                Err(err) => {
                    return Ok(failed_row(
                        &primary,
                        console,
                        action_label(&plan.action),
                        plan::error_detail(&err.into()),
                        input_bytes,
                        unit_started,
                        dry_run,
                    ));
                }
            }
        }
        (_, false) | (None, true) => None,
    };
    if let Some((_, target)) = &patched {
        source = target.as_path();
    }

    // Linked sources must be plain files: retained or fallback archive
    // members and patched ROMs live in temp directories. A link also cannot
    // carry a rewritten payload, so a header strip or trim pad places as a
    // copy too.
    let demote_reason = if matches!(plan.action, Action::Link) {
        if plan.strip > 0 || plan.pad > 0 {
            Some("copied: link cannot carry a stripped/padded payload")
        } else if plan.member.is_some() || plan.patch.is_some() {
            Some("copied: linked sources must be plain files")
        } else {
            None
        }
    } else {
        None
    };

    match &plan.action {
        Action::Convert { op, format, .. } => {
            // A dry run mirrors a source this run frees: the desired path
            // still exists, but a real run finds it gone, so the plan is
            // a new output rather than what the child would see on disk.
            if dry_run && convert_keep_reason.is_none() && guards.is_released(desired) {
                let (status, detail) = plan_outcome(&PlanDecision::New);
                return Ok(OrganizeRow {
                    input: primary,
                    output: Some(desired.clone()),
                    console,
                    action: (*op).to_string(),
                    status,
                    planned: true,
                    verify: None,
                    detail,
                    input_bytes,
                    output_bytes: 0,
                    elapsed_ms: elapsed_ms(unit_started),
                });
            }
            run_conversion(
                req,
                plan,
                unit,
                op,
                *format,
                source,
                convert_keep_reason,
                unit_started,
                progress,
                file_progress,
                cancel,
            )
            .await
        }
        Action::Zip | Action::Copy | Action::Link => {
            // A fallback extraction lives only for this plan: placement may
            // move it into place instead of copying it. Only unix zip/7z/tar
            // extractions qualify: they are created with the default file
            // mode, while unrar applies archive attributes and a Windows
            // temp file keeps the temp dir's ACL.
            let consume_source = cfg!(unix)
                && fallback.is_some()
                && patched.is_none()
                && !ext_of(&primary).eq_ignore_ascii_case("rar");
            place::run_place(
                req,
                plan,
                unit,
                own_sources,
                source,
                consume_source,
                demote_reason,
                zip_format,
                link_mode,
                guards,
                progress,
                file_progress,
                cancel,
            )
            .await
        }
    }
}

/// Runs a child conversion op for one unit and folds its plan or records into
/// the row. The unit's primary path replaces the staged member's in the
/// folded plan line.
#[allow(clippy::too_many_arguments)]
async fn run_conversion(
    req: &RunRequest,
    plan: &UnitPlan,
    unit: &DatUnit,
    operation: &str,
    format: Option<&str>,
    source: &Path,
    convert_keep_reason: Option<RefusalCause>,
    unit_started: Instant,
    progress: &dyn ProgressReporter,
    file_progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<OrganizeRow> {
    let primary = unit.display_path().to_path_buf();
    let dry_run = req.dry_run;
    let console = plan.tokens.console.clone();
    let desired = plan
        .desired
        .as_ref()
        .expect("Keep plan without a resolved output path");
    let input_bytes = unit.size_bytes();
    // A desired path this run already realized is refused before the child
    // could see it: nothing was written there on this run's behalf, so the
    // row reports the keep directly, identically in dry and real runs.
    if let Some(RefusalCause::Realized) = &convert_keep_reason {
        return Ok(OrganizeRow {
            input: primary,
            output: Some(desired.clone()),
            console,
            action: operation.to_string(),
            status: FileStatus::Skipped,
            planned: dry_run,
            verify: None,
            detail: Some("kept: this run already wrote the output".to_string()),
            input_bytes,
            output_bytes: 0,
            elapsed_ms: elapsed_ms(unit_started),
        });
    }
    // The other causes keep the existing output through the child's skip
    // policy; the row's detail names the cause either way.
    let convert_keep_detail = convert_keep_reason.as_ref().map(|cause| match cause {
        RefusalCause::UnderInput => "kept: existing output is in the input tree, not rewritten",
        RefusalCause::Source(_) => "kept: it is another unit's source file",
        RefusalCause::Realized => unreachable!("realized keeps return above"),
    });
    let child = child_request(req, operation, source, desired, format);
    let dispatched = match apply_config_defaults(child) {
        Ok(mut child) => {
            child.options.report = None;
            child.options.recursive = None;
            // A guarded conversion keeps the existing output instead of
            // failing: the child skips, and the row says why.
            if convert_keep_reason.is_some() {
                child.options.on_conflict = Some("skip".to_string());
            }
            run_single_request(child, file_progress, cancel.clone()).await
        }
        Err(err) => Err(err),
    };
    match dispatched {
        Ok(mut response) => {
            let data = response.data.take();
            if let Some(RunData::Plan(mut line)) = data {
                line.input = primary.clone();
                let (status, detail) = plan_outcome(&line.decision);
                let detail = if convert_keep_reason.is_some() && status == FileStatus::Skipped {
                    convert_keep_detail.map(str::to_string)
                } else {
                    detail
                };
                return Ok(OrganizeRow {
                    input: primary,
                    output: Some(line.output),
                    console,
                    action: operation.to_string(),
                    status,
                    planned: dry_run,
                    verify: None,
                    detail,
                    input_bytes,
                    output_bytes: 0,
                    elapsed_ms: elapsed_ms(unit_started),
                });
            }
            let (status, detail, output, output_bytes, verify) = fold_conversion_outcome(
                data,
                std::mem::take(&mut response.records),
                req.options.verify_after == Some(true),
            );
            let detail = if convert_keep_reason.is_some() && status == FileStatus::Skipped {
                convert_keep_detail.map(str::to_string)
            } else {
                detail
            };
            Ok(OrganizeRow {
                input: primary,
                // A skipped row names an output only when a record carried
                // one: an "already done" skip has no output to name.
                output: match status {
                    // A verify-failed row names the output it checked.
                    FileStatus::Failed if verify.is_none() => None,
                    FileStatus::Failed | FileStatus::Skipped => output,
                    _ => Some(output.unwrap_or_else(|| desired.to_path_buf())),
                },
                console,
                action: operation.to_string(),
                status,
                planned: dry_run,
                verify,
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
                Some(desired.clone()),
                input_bytes,
                unit_started,
                dry_run,
            ))
        }
        Err(err) => Ok(failed_row(
            &primary,
            console,
            operation.to_string(),
            plan::error_detail(&err),
            input_bytes,
            unit_started,
            dry_run,
        )),
    }
}

/// Folds a dispatched child conversion response into the row's outcome
/// fields: the records' worst status wins, the last named output path and
/// summed bytes carry. Under `verify_after`, a passing comparison marks the
/// row verified; a comparison that failed fails the row, and one that never
/// produced a verdict fails it as unverified, so `keep_satisfied` is false
/// and the source is never released.
fn fold_conversion_outcome(
    data: Option<RunData>,
    records: Vec<ReportRecord>,
    verify_after: bool,
) -> (
    FileStatus,
    Option<String>,
    Option<PathBuf>,
    u64,
    Option<VerifyVerdict>,
) {
    let mut status = FileStatus::Ok;
    let mut detail = None;
    let mut output: Option<PathBuf> = None;
    let mut output_bytes = 0u64;
    let mut verify = None;
    for record in records {
        if !record.output_path.is_empty() {
            output = Some(PathBuf::from(&record.output_path));
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
    if let Some(RunData::Comparison(comparison)) = &data
        && let Some(report) = &comparison.comparison.verify
    {
        if report.ok && verify_after {
            // The check ran and passed: the row names its verdict.
            verify = Some(VerifyVerdict::Verified);
        } else if !report.ok && verify_after {
            status = FileStatus::Failed;
            // A failed verdict means the output is invalid and stays swept as
            // stale; an unverified verdict means the check never produced one:
            // a transient error whose detail keeps the output's clean
            // protection.
            verify = Some(match report.verdict {
                VerifyVerdict::Unverified => VerifyVerdict::Unverified,
                _ => VerifyVerdict::Failed,
            });
            detail = match report.verdict {
                VerifyVerdict::Unverified => {
                    let cause = report
                        .message
                        .strip_prefix(COULD_NOT_VERIFY)
                        .unwrap_or(&report.message);
                    Some(format!("verify error: {cause}"))
                }
                _ => Some(format!("{VERIFY_FAILED_PREFIX}: {}", report.message)),
            };
        }
    }
    // Under `verify_after` a conversion with no comparison verdict at all
    // (the op checks nothing about its output) cannot release the source:
    // the row fails as unverified, like a check that could not run.
    if verify_after && verify.is_none() && status == FileStatus::Ok {
        status = FileStatus::Failed;
        verify = Some(VerifyVerdict::Unverified);
        detail = Some("verify error: the operation produced no comparison verdict".to_string());
    }
    // A skipped conversion kept an existing output: the row names it, but
    // nothing was written, so it counts no bytes. A verify-failed row is
    // the exception: the output was written (invalid) and the child's
    // record already carries its length.
    if verify.is_none() && status != FileStatus::Ok {
        output_bytes = 0;
    }
    (status, detail, output, output_bytes, verify)
}

/// A row satisfies its unit's Keep plan for `move_source`: it landed Ok, or
/// (under `overwrite-invalid`) it skipped because the existing output
/// verified valid (that is the output this source would produce again). A
/// plain skip-policy "output exists" keeps the input. The verified-valid
/// branch releases a source only when the output path holds a regular file:
/// a symlink points at a file elsewhere, and releasing the source would
/// make the organized output depend on it. It also only lets Zip/Copy/Link
/// rows release a source, because a conversion's check is container
/// self-consistency, not proof that this source produced the output.
fn keep_satisfied(row: &OrganizeRow, action: &Action) -> bool {
    if row.status == FileStatus::Ok {
        return true;
    }
    row.status == FileStatus::Skipped
        && row.detail.as_deref() == Some(VERIFIED_VALID)
        && !matches!(action, Action::Convert { .. })
        && row.output.as_ref().is_some_and(|output| {
            std::fs::symlink_metadata(output).is_ok_and(|meta| meta.is_file())
        })
}

/// The clean keep-set over placed rows: every Keep plan's desired path and
/// every realized output survive; a failed row's too, because an untouched
/// previous output survives a transient failure and a written output whose
/// source removal failed survives. The exception is a verify-failed row:
/// its output is invalid, so only the desired path is kept when it differs
/// from the written output (an `on_conflict rename` slot from an earlier
/// run), and the invalid write itself stays swept as stale.
fn clean_keep_set(plans: &[UnitPlan], rows: &[OrganizeRow]) -> BTreeSet<PathBuf> {
    let mut keep = BTreeSet::new();
    for (plan, row) in plans.iter().zip(rows) {
        let verify_failed = row.verify == Some(VerifyVerdict::Failed);
        if matches!(plan.decision, Decision::Keep)
            && let Some(desired) = &plan.desired
            && !(verify_failed && row.output.as_ref() == Some(desired))
        {
            keep.insert(desired.clone());
        }
        if !verify_failed && let Some(output) = &row.output {
            keep.insert(output.clone());
        }
    }
    keep
}

/// Patch CRCs for staged archive members were computed during planning.
/// Plain files are streamed here only for surviving plans; cue sets have no
/// single-file CRC and yield `None`. Read errors warn and yield `None`.
async fn unit_crcs(
    plans: &[UnitPlan],
    units: &[DatUnit],
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<Vec<Option<u32>>> {
    let mut executes = vec![false; units.len()];
    let mut base_plans = vec![None; units.len()];
    for plan in plans {
        base_plans[plan.index] = Some(plan);
        if !matches!(plan.decision, Decision::Skip(_)) {
            executes[plan.index] = true;
        }
    }
    let mut crcs: Vec<Option<u32>> = vec![None; units.len()];
    for (index, unit) in units.iter().enumerate() {
        if !executes[index] || cancel.is_cancelled() {
            continue;
        }
        let DatUnit::File(path) = unit else {
            continue;
        };
        if crate::util::is_archive_path(path) {
            crcs[index] = base_plans[index]
                .and_then(|plan| plan.member.as_ref())
                .and_then(|member| member.crc);
            continue;
        }
        let hash_path = path.clone();
        let hash_cancel = cancel.clone();
        let hashed = tokio::task::spawn_blocking(move || {
            crate::util::hash::crc32_of_file(&hash_path, &hash_cancel)
                .with_context(|| format!("reading {}", hash_path.display()))
        })
        .await;
        match hashed {
            Ok(Ok(crc)) => crcs[index] = Some(crc),
            Ok(Err(err)) if is_cancelled_error(&err) => return Err(err),
            Ok(Err(err)) => progress.warn(&err.to_string()),
            Err(err) => return Err(err.into()),
        }
    }
    Ok(crcs)
}

/// Why the write guards refuse a desired output path. The cause carries
/// what the detail names: this run's earlier output, another scanned
/// unit's source file (with the owner's path), or an existing file whose
/// own location is under the input root.
#[derive(Clone, Debug)]
enum RefusalCause {
    Realized,
    Source(PathBuf),
    UnderInput,
}

/// Identity of an existing file or directory: (device, inode) where the
/// platform exposes it, else the canonical path. Two paths to the same
/// file share an identity whatever their spelling, including case variants
/// on case-insensitive volumes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum FileIdentity {
    #[cfg(unix)]
    Inode(u64, u64),
    Path(PathBuf),
}

impl FileIdentity {
    /// Identity of `path`'s target (symlinks followed); `None` when it does
    /// not exist.
    fn of(path: &Path) -> Option<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(path)
                .ok()
                .map(|meta| Self::Inode(meta.dev(), meta.ino()))
        }
        #[cfg(not(unix))]
        {
            std::fs::canonicalize(path).ok().map(Self::Path)
        }
    }

    /// Identity for a path that may not exist yet: the real identity when
    /// it does, else its canonical parent joined with its file name, so two
    /// spellings of one planned output share a key.
    fn keyed(path: &Path) -> Self {
        Self::of(path).unwrap_or_else(|| {
            let parent = path
                .parent()
                .map(|parent| {
                    std::fs::canonicalize(parent).unwrap_or_else(|_| {
                        std::path::absolute(parent).unwrap_or_else(|_| parent.to_path_buf())
                    })
                })
                .unwrap_or_else(|| PathBuf::from(""));
            Self::Path(parent.join(path.file_name().unwrap_or_default()))
        })
    }
}

/// True when both paths name the same directory entry: the same location,
/// however spelled, not merely the same content (a hardlinked copy in
/// another directory is a different location and survives its twin's
/// removal).
fn same_location(a: &Path, b: &Path) -> bool {
    if entry_location(a) == entry_location(b) {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirEntryExt, MetadataExt};
        if let (Ok(meta_a), Ok(meta_b)) =
            (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b))
            && meta_a.is_file()
            && meta_b.is_file()
            && (meta_a.dev(), meta_a.ino()) == (meta_b.dev(), meta_b.ino())
        {
            if meta_a.nlink() == 1 && meta_b.nlink() == 1 {
                return true;
            }
            if let (Some(parent_a), Some(parent_b)) = (a.parent(), b.parent())
                && FileIdentity::of(parent_a)
                    .zip(FileIdentity::of(parent_b))
                    .is_some_and(|(a, b)| a == b)
            {
                let (name_a, name_b) = (a.file_name(), b.file_name());
                if name_a == name_b {
                    return true;
                }
                if let Ok(entries) = std::fs::read_dir(parent_a) {
                    let mut links = 0;
                    for entry in entries {
                        let Ok(entry) = entry else {
                            return false;
                        };
                        if entry.ino() == meta_a.ino() {
                            links += 1;
                            if links > 1 {
                                return name_a == name_b;
                            }
                        }
                    }
                    return links == 1;
                }
            }
        }
    }
    #[cfg(not(unix))]
    if cfg!(windows)
        && let (Some(meta_a), Some(meta_b)) = (
            std::fs::symlink_metadata(a).ok(),
            std::fs::symlink_metadata(b).ok(),
        )
        && !meta_a.is_symlink()
        && !meta_b.is_symlink()
        && meta_a.is_file()
        && meta_b.is_file()
        && FileIdentity::of(a) == FileIdentity::of(b)
    {
        return entry_location(a).to_string_lossy().to_lowercase()
            == entry_location(b).to_string_lossy().to_lowercase();
    }
    false
}

#[cfg(unix)]
fn single_link_identity(path: &Path) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    (meta.nlink() == 1).then_some(FileIdentity::Inode(meta.dev(), meta.ino()))
}

/// Syncs outputs before sources are removed. Unsupported sync operations
/// keep the previous behavior; file writeback errors must keep the sources.
fn sync_outputs(outputs: &[PathBuf]) -> std::io::Result<()> {
    for output in outputs {
        if !std::fs::metadata(output).is_ok_and(|meta| meta.is_file()) {
            continue;
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(output);
        #[cfg(unix)]
        let file = file.or_else(|err| {
            if err.kind() == std::io::ErrorKind::PermissionDenied {
                std::fs::File::open(output)
            } else {
                Err(err)
            }
        });
        if let Ok(file) = file
            && let Err(err) = sync_file_data(&file)
            && sync_error_keeps_sources(&err)
        {
            return Err(std::io::Error::new(
                err.kind(),
                format!("could not sync output {}: {err}", output.display()),
            ));
        }
    }
    #[cfg(unix)]
    {
        let mut synced = HashSet::new();
        for parent in outputs.iter().filter_map(|output| output.parent()) {
            if synced.insert(parent)
                && let Ok(directory) = std::fs::File::open(parent)
            {
                #[cfg(target_vendor = "apple")]
                let _ = sync_file_data(&directory);
                #[cfg(not(target_vendor = "apple"))]
                let _ = directory.sync_all();
            }
        }
    }
    Ok(())
}

fn sync_error_keeps_sources(err: &std::io::Error) -> bool {
    #[cfg(unix)]
    if err.raw_os_error() == Some(5) {
        // EIO has the same value on supported Unix targets.
        return true;
    }
    // An aborted journal turns the filesystem read-only and a soft network
    // mount times out: both mean the output may not be on disk.
    matches!(
        err.kind(),
        std::io::ErrorKind::StorageFull
            | std::io::ErrorKind::QuotaExceeded
            | std::io::ErrorKind::StaleNetworkFileHandle
            | std::io::ErrorKind::ReadOnlyFilesystem
            | std::io::ErrorKind::TimedOut
    )
}

fn sync_file_data(file: &std::fs::File) -> std::io::Result<()> {
    #[cfg(target_vendor = "apple")]
    {
        use std::os::fd::AsRawFd;
        // Plain fsync avoids a drive-wide cache flush for every moved unit.
        loop {
            if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
                return Ok(());
            }
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }
    #[cfg(not(target_vendor = "apple"))]
    file.sync_data()
}

/// The entry's own location: its parent canonicalized (resolving spelling
/// and symlinked directories) with the file name re-joined. The entry's own
/// final symlink is never followed, so a link is judged by where it sits.
fn entry_location(path: &Path) -> PathBuf {
    match path.parent().map(std::fs::canonicalize) {
        Some(Ok(parent)) => parent.join(path.file_name().unwrap_or_default()),
        _ => path.to_path_buf(),
    }
}
#[cfg(unix)]
fn directory_identity_in_ancestors(path: &Path, identity: &FileIdentity) -> bool {
    path.ancestors()
        .any(|ancestor| FileIdentity::of(ancestor).as_ref() == Some(identity))
}

pub(super) fn entry_location_is_under_root(
    location: &Path,
    root: &Path,
    identity: Option<&FileIdentity>,
    parents: &mut HashMap<PathBuf, bool>,
) -> bool {
    if location.starts_with(root) {
        return true;
    }
    #[cfg(unix)]
    if let (Some(identity), Some(parent)) = (identity, location.parent()) {
        return *parents
            .entry(parent.to_path_buf())
            .or_insert_with(|| directory_identity_in_ancestors(parent, identity));
    }
    #[cfg(not(unix))]
    let _ = (identity, parents);
    false
}

/// True when either path contains the other: overlapping input and output
/// trees make clean dangerous, because excluded inputs live inside the
/// cleaned tree. The kernel-resolved paths are compared as a fast path,
/// then directory identities cover distinct spellings of the same tree.
fn paths_overlap(a: &Path, b: &Path) -> bool {
    let (a, b) = (real_layout(a), real_layout(b));
    if a == b || a.starts_with(&b) || b.starts_with(&a) {
        return true;
    }
    // On Windows both sides are canonical paths, so the prefix test decides.
    #[cfg(unix)]
    let aliased = FileIdentity::of(&a)
        .is_some_and(|identity| directory_identity_in_ancestors(&b, &identity))
        || FileIdentity::of(&b)
            .is_some_and(|identity| directory_identity_in_ancestors(&a, &identity));
    #[cfg(not(unix))]
    let aliased = false;
    aliased
}

/// Absolutizes `path`, then resolves it the way the kernel would: the
/// deepest existing ancestor of the spelling is canonicalized (a symlinked
/// prefix is followed), and only the not-yet-existing tail is folded
/// lexically onto that resolved prefix, its `..` climbing from where the
/// prefix really lives. A folded tail can land on an existing symlink
/// (`missing/../link/out`), so the step repeats on its own result until
/// the path stops changing (bounded, so a link cycle cannot spin).
fn real_layout(path: &Path) -> PathBuf {
    let mut current = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    for _ in 0..40 {
        let next = resolve_deepest_prefix(&current);
        if next == current {
            break;
        }
        current = next;
    }
    current
}

/// One resolution step of [`real_layout`]: canonicalize the deepest
/// existing prefix of the absolute `path` and fold the missing tail onto it.
fn resolve_deepest_prefix(path: &Path) -> PathBuf {
    let components: Vec<std::path::Component<'_>> = path.components().collect();
    // The deepest existing raw prefix decides the resolved side: the
    // kernel resolves a symlink before applying '..', so a tail `..` must
    // climb from where the prefix points, not from where it is spelled.
    let mut resolved = None;
    let mut split = components.len();
    while split > 0 {
        let prefix: PathBuf = components[..split].iter().collect();
        if let Ok(canonical) = std::fs::canonicalize(&prefix) {
            resolved = Some(canonical);
            break;
        }
        split -= 1;
    }
    let Some(mut resolved) = resolved else {
        return path.to_path_buf();
    };
    for component in &components[split..] {
        match component {
            std::path::Component::ParentDir => {
                resolved.pop();
            }
            std::path::Component::RootDir | std::path::Component::CurDir => {}
            other => resolved.push(other.as_os_str()),
        }
    }
    resolved
}

/// True when `path`'s own location sits inside the input root.
fn location_under_root(path: &Path, root: &Path) -> bool {
    #[cfg(unix)]
    let identity = FileIdentity::of(root);
    #[cfg(not(unix))]
    let identity = None;
    entry_location_is_under_root(
        &entry_location(path),
        root,
        identity.as_ref(),
        &mut HashMap::new(),
    )
}

fn restore_source(source: &Path, temporary: &Path) -> std::io::Result<()> {
    let outcome = match std::fs::symlink_metadata(source) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            std::fs::rename(temporary, source)
        }
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "the source name is occupied",
        )),
        Err(err) => Err(err),
    };
    outcome.map_err(|err| {
        std::io::Error::new(
            err.kind(),
            format!(
                "could not restore {} from {}: {err}",
                source.display(),
                temporary.display()
            ),
        )
    })
}

/// Renaming first reveals aliases even on filesystems with unreliable identities.
/// A failed probe or unlink restores the entry unless its old name is occupied.
async fn remove_source(
    source: &Path,
    outputs: &[PathBuf],
    synced: &mut bool,
) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(source) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(err) => return Err(err),
        Ok(_) => {}
    }
    let output_identity = |output: &Path| -> std::io::Result<FileIdentity> {
        let meta = std::fs::metadata(output)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(FileIdentity::Inode(meta.dev(), meta.ino()))
        }
        #[cfg(not(unix))]
        {
            let _ = meta;
            // Some virtual and RAM-disk drives cannot report a final path; the
            // metadata call above still catches an output that vanished.
            match std::fs::canonicalize(output) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                    std::path::absolute(output).map(FileIdentity::Path)
                }
                resolved => resolved.map(FileIdentity::Path),
            }
        }
    };
    let identities = outputs
        .iter()
        .map(|output| output_identity(output))
        .collect::<std::io::Result<Vec<_>>>()?;
    if !*synced {
        let outputs = outputs.to_vec();
        tokio::task::spawn_blocking(move || sync_outputs(&outputs))
            .await
            .map_err(std::io::Error::other)??;
        *synced = true;
    }
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let parent = source.parent().unwrap_or_else(|| Path::new("."));
    let temporary = loop {
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = parent.join(format!(".rom-converto-move-{}-{id}", std::process::id()));
        match std::fs::symlink_metadata(&path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => break path,
            Err(err) => return Err(err),
            Ok(_) => {}
        }
    };
    match tokio::fs::rename(source, &temporary).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(err) => return Err(err),
    }
    let outcome = if outputs
        .iter()
        .zip(&identities)
        .any(|(output, identity)| output_identity(output).as_ref().ok() != Some(identity))
    {
        Ok(false)
    } else {
        match tokio::fs::remove_file(&temporary).await {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(true),
            removed => removed.map(|()| true),
        }
    };
    if !matches!(outcome, Ok(true)) {
        restore_source(source, &temporary)?;
    }
    outcome
}

/// Deletes a placed unit's source files (the cue plus every bin of a set;
/// a dry run applies the same gates and only reports what it would delete),
/// never touching a file that is also one of the unit's outputs. A bin
/// outside the input root (an absolute or `..` FILE reference) and a bin
/// another scanned unit claims are never removed: the returned note lines
/// say why each kept source stayed, and a split WUD set is released
/// all-or-nothing (one kept part keeps the whole set, so no orphaned
/// prefix survives). Returns the removed paths, one entry per file it
/// could not remove (a partial removal fails the unit's rows and skips
/// the emptied-dir pruning), and the notes. An already-missing file counts
/// as removed. Bins go before the cue; the first failed bin stops the
/// remaining removals and keeps the cue for recovery.
async fn remove_sources(
    unit: &DatUnit,
    own_sources: &[PathBuf],
    outputs: &[PathBuf],
    root: &Path,
    claims: &mut HashMap<FileIdentity, usize>,
    dry_run: bool,
) -> (Vec<PathBuf>, Vec<(PathBuf, std::io::Error)>, Vec<String>) {
    let (cue, bins): (Option<PathBuf>, Vec<PathBuf>) = match unit {
        DatUnit::File(_) => {
            // A split WUD set releases with its first part, the last part
            // first so an interrupted run still resumes from part1.
            let mut parts = own_sources.to_vec();
            parts.reverse();
            (None, parts)
        }
        DatUnit::CueSet { cue, bins } => (Some(cue.clone()), bins.clone()),
    };
    // The caller hands over the run's canonical input root.
    let mut removed = Vec::new();
    let mut failures = Vec::new();
    let mut kept = Vec::new();
    let mut synced = false;
    #[cfg(unix)]
    let output_identities: HashSet<FileIdentity> = if dry_run {
        outputs
            .iter()
            .filter_map(|output| FileIdentity::of(output))
            .collect()
    } else {
        HashSet::new()
    };
    let same_output_location =
        |source: &Path| outputs.iter().any(|output| same_location(source, output));
    let shares_output_identity = |_source: &Path| {
        #[cfg(unix)]
        {
            dry_run
                && single_link_identity(_source)
                    .is_some_and(|identity| output_identities.contains(&identity))
        }
        #[cfg(not(unix))]
        false
    };
    // A split set is released all-or-nothing: when any part would stay
    // (it is an output, another unit's source, or lies outside the
    // input), no part is removed, so the set never splits into an
    // orphaned prefix that later runs fail on as truncated.
    if cue.is_none() {
        let mut any_kept = false;
        let mut notes: Vec<String> = Vec::new();
        for part in &bins {
            if same_output_location(part) {
                any_kept = true;
            } else if shares_output_identity(part) {
                any_kept = true;
                notes.push(format!("kept {}: it is also an output", part.display()));
            } else {
                let claimed = FileIdentity::of(part)
                    .and_then(|identity| claims.get(&identity).copied())
                    .unwrap_or(1);
                if claimed > 1 {
                    any_kept = true;
                    notes.push(format!(
                        "kept {}: another scanned unit references it",
                        part.display()
                    ));
                } else if !entry_location(part).starts_with(root) {
                    any_kept = true;
                    notes.push(format!(
                        "kept {}: it lies outside the input",
                        part.display()
                    ));
                }
            }
        }
        if any_kept {
            kept.extend(notes);
            return (removed, failures, kept);
        }
    }
    let mut removal_stopped = false;
    for (position, bin) in bins.iter().enumerate() {
        if same_output_location(bin) {
            continue;
        }
        if shares_output_identity(bin) {
            kept.push(format!("kept {}: it is also an output", bin.display()));
            continue;
        }
        let claimed = FileIdentity::of(bin)
            .and_then(|identity| claims.get(&identity).copied())
            .unwrap_or(1);
        if claimed > 1 {
            kept.push(format!(
                "kept {}: another scanned unit references it",
                bin.display()
            ));
            continue;
        }
        if !entry_location(bin).starts_with(root) {
            kept.push(format!("kept {}: it lies outside the input", bin.display()));
            continue;
        }
        // A dry run applies every gate above and reports what it would
        // remove, touching nothing.
        let outcome = if dry_run {
            Ok(true)
        } else {
            remove_source(bin, outputs, &mut synced).await
        };
        removal_stopped = match outcome {
            Ok(true) => {
                removed.push(bin.clone());
                false
            }
            Ok(false) => {
                kept.push(format!("kept {}: it is also an output", bin.display()));
                true
            }
            Err(err) => {
                failures.push((bin.clone(), err));
                true
            }
        };
        if removal_stopped {
            if cue.is_none() {
                for part in &bins[position + 1..] {
                    failures.push((
                        part.clone(),
                        std::io::Error::other(
                            "not attempted: an earlier part of the split set failed to be removed",
                        ),
                    ));
                }
            }
            break;
        }
    }
    if let Some(cue) = cue.as_ref().filter(|_| removal_stopped) {
        let detail = if removed.is_empty() {
            "not removed: a bin could not be removed".to_string()
        } else {
            format!(
                "not removed: a bin could not be removed (already removed: {})",
                removed
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        failures.push((cue.clone(), std::io::Error::other(detail)));
        return (removed, failures, kept);
    }
    if let Some(cue) = &cue {
        if same_output_location(cue) {
            return (removed, failures, kept);
        }
        if shares_output_identity(cue) {
            kept.push(format!("kept {}: it is also an output", cue.display()));
            return (removed, failures, kept);
        }
        let outcome = if dry_run {
            Ok(true)
        } else {
            remove_source(cue, outputs, &mut synced).await
        };
        let released = match outcome {
            Ok(true) => true,
            Ok(false) => {
                kept.push(format!("kept {}: it is also an output", cue.display()));
                false
            }
            Err(err) => {
                failures.push((cue.clone(), err));
                false
            }
        };
        if released {
            removed.push(cue.clone());
            // The cue is gone: its claim on every bin went with it, so the
            // last claimant's release removes a shared bin instead of
            // orphaning it.
            for bin in &bins {
                if let Some(identity) = FileIdentity::of(bin) {
                    claims
                        .entry(identity)
                        .and_modify(|count| *count = count.saturating_sub(1));
                }
            }
        }
    }
    (removed, failures, kept)
}

/// How many scanned units claim each source file, keyed by the file's
/// identity: a bin two cue sheets share, however spelled, is never removed,
/// and neither is a split part two first parts (case twins on a
/// case-sensitive volume) both read.
fn source_claim_counts(units: &[DatUnit]) -> HashMap<FileIdentity, usize> {
    let continuations = split_continuations(units);
    let mut claims: HashMap<FileIdentity, usize> = HashMap::new();
    for unit in units {
        // A continuation a scanned first part owns is claimed through that
        // set, once per set that reads it, not as a unit of its own. A
        // stray continuation no set reads claims itself.
        if let DatUnit::File(path) = unit
            && continuations.contains(&entry_location(path))
        {
            continue;
        }
        for path in unit_source_files(unit) {
            if let Some(identity) = FileIdentity::of(&path) {
                *claims.entry(identity).or_default() += 1;
            }
        }
    }
    claims
}

/// Why an archive unit's source must stay when its outputs landed: it holds
/// several entries, so removing it would destroy every entry this run did
/// not place, or it could not be inspected. `None` when the source holds
/// exactly one entry and is safe to remove. The listing runs on the
/// blocking pool: a 7z or rar header parse never stalls the runtime.
async fn archive_member_note(unit: &DatUnit) -> Option<String> {
    let DatUnit::File(path) = unit else {
        return None;
    };
    if !crate::util::is_archive_path(path) {
        return None;
    }
    let display = path.display().to_string();
    let path = path.clone();
    let listed =
        tokio::task::spawn_blocking(move || crate::util::archive::entry_count(&path)).await;
    match listed {
        Ok(Ok(count)) if count > 1 => Some(format!("source kept: {display} holds {count} entries")),
        Ok(Ok(_)) => None,
        Ok(Err(err)) => Some(format!("source kept: could not inspect {display}: {err}")),
        Err(err) => Some(format!("source kept: could not inspect {display}: {err}")),
    }
}

/// The part number of a `.wud` split-set file (`game_part<N>.wud`, any
/// case), or `None` for any other file.
fn wud_part_number(path: &Path) -> Option<u32> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .filter(|ext| ext.eq_ignore_ascii_case("wud"))?;
    crate::nintendo::wup::disc::sector_stream::split_part_number(path)
}

/// The directory entry a converter opening `game_part<N>.wud` in `parent`
/// reads, in its real on-disk spelling, or `None` when that name is not a
/// file. `names` is the directory listing. On a case-sensitive volume the
/// match is the exact lowercase name (a case twin is a different file the
/// converter never reads); on a case-insensitive volume the exact name
/// reaches one entry, whatever its spelling, and that entry is returned.
fn split_part_entry(parent: &Path, number: u32, names: &[std::ffi::OsString]) -> Option<PathBuf> {
    let want = format!("game_part{number}.wud");
    if !parent.join(&want).is_file() {
        return None;
    }
    let exact = names
        .iter()
        .find(|name| name.to_str() == Some(want.as_str()));
    let folded = names.iter().find(|name| {
        name.to_str()
            .is_some_and(|text| text.eq_ignore_ascii_case(&want))
    });
    let name = exact
        .or(folded)
        .cloned()
        .unwrap_or_else(|| std::ffi::OsString::from(&want));
    Some(parent.join(name))
}

/// The split WUD set `path` belongs to, exactly as the converter reads it:
/// the first part (the unit's own path, or `game_part1.wud` when `path` is
/// a continuation) followed by `game_part<N>.wud` for consecutive N, each
/// in its real spelling (see [`split_part_entry`]), stopping at the first
/// missing N. The directory is listed once. A path that is not a numbered
/// `.wud`, or a set of one, is just itself.
fn split_set_files(path: &Path) -> Vec<PathBuf> {
    let Some(number) = wud_part_number(path) else {
        return vec![path.to_path_buf()];
    };
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let dir = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent.as_path()
    };
    let names: Vec<std::ffi::OsString> = std::fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|entry| entry.file_name()).collect())
        .unwrap_or_default();
    let first = if number == 1 {
        Some(path.to_path_buf())
    } else {
        split_part_entry(&parent, 1, &names)
    };
    let Some(first) = first else {
        return vec![path.to_path_buf()];
    };
    let mut parts = vec![first];
    for next in 2u32..=12 {
        match split_part_entry(&parent, next, &names) {
            Some(part) => parts.push(part),
            None => break,
        }
    }
    if parts.len() > 1 {
        parts
    } else {
        vec![path.to_path_buf()]
    }
}

/// The own locations of every continuation part a scanned split set's
/// first part owns (the parts the converter reads together with its
/// `game_part1.wud`). A numbered file no scanned set reads (a stray case
/// twin, or a part past a gap) is not in it.
fn split_continuations(units: &[DatUnit]) -> HashSet<PathBuf> {
    units
        .iter()
        .filter(|unit| matches!(unit, DatUnit::File(path) if wud_part_number(path) == Some(1)))
        .flat_map(|unit| {
            unit_source_files(unit)
                .into_iter()
                .skip(1)
                .map(|part| entry_location(&part))
        })
        .collect()
}

/// Every file a unit occupies: the file itself (a split WUD's first part
/// owns every continuation part of its set, in their on-disk spelling), or
/// the cue and all bins of a set.
fn unit_source_files(unit: &DatUnit) -> Vec<PathBuf> {
    match unit {
        DatUnit::File(path) => {
            // Only a `.wud` first part owns the continuations, for release
            // and claims; continuations are skipped at plan time.
            if wud_part_number(path) == Some(1) {
                return split_set_files(path);
            }
            vec![path.clone()]
        }
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

/// Detail prefix of a row whose written output failed its verification: the
/// output is invalid, so anything sitting at the path stays stale. A verify
/// that could not run at all uses `verify error:` instead: a transient I/O
/// error keeps the output protected.
pub(crate) const VERIFY_FAILED_PREFIX: &str = "verify failed";

/// Status and detail text for a dry-run plan decision.
fn plan_outcome(decision: &PlanDecision) -> (FileStatus, Option<String>) {
    match decision {
        PlanDecision::Skip => (FileStatus::Skipped, Some("output exists".to_string())),
        PlanDecision::KeepValid => (FileStatus::Skipped, Some(VERIFIED_VALID.to_string())),
        PlanDecision::New => (FileStatus::Ok, Some("new".to_string())),
        PlanDecision::Overwrite => (FileStatus::Ok, Some("overwrite".to_string())),
        PlanDecision::Rename(path) => (
            FileStatus::Ok,
            Some(format!("rename to {}", path.display())),
        ),
        PlanDecision::RewriteInvalid => (FileStatus::Ok, Some("rewrite (invalid)".to_string())),
    }
}

/// A skipped row with `action = "skip"` and the reason as detail. A skip
/// that kept an existing output (`output exists`, verified valid, already in
/// place) carries it, so playlist planning and the clean keep-set see the
/// file on re-runs.
fn skip_row(
    input: &Path,
    console: Option<String>,
    reason: &str,
    output: Option<PathBuf>,
    input_bytes: u64,
    started: Instant,
    dry_run: bool,
) -> OrganizeRow {
    OrganizeRow {
        input: input.to_path_buf(),
        output,
        console,
        action: "skip".to_string(),
        status: FileStatus::Skipped,
        planned: dry_run,
        verify: None,
        detail: Some(reason.to_string()),
        input_bytes,
        output_bytes: 0,
        elapsed_ms: elapsed_ms(started),
    }
}

/// A failed placement row: the unit could not produce its output.
fn failed_row(
    input: &Path,
    console: Option<String>,
    action: String,
    detail: String,
    input_bytes: u64,
    started: Instant,
    dry_run: bool,
) -> OrganizeRow {
    OrganizeRow {
        input: input.to_path_buf(),
        output: None,
        console,
        action,
        status: FileStatus::Failed,
        planned: dry_run,
        verify: None,
        detail: Some(detail),
        input_bytes,
        output_bytes: 0,
        elapsed_ms: elapsed_ms(started),
    }
}

/// Per-unit bookkeeping of the execute loop: the plans a unit has (base
/// plus patched variants), how many are Keep plans, whether the base plan
/// must never release the source, and whether the base plan's DAT match
/// failed.
#[derive(Clone, Copy, Default)]
struct UnitBook {
    plans: usize,
    keeps: usize,
    base_lost: bool,
    base_match_error: bool,
}

/// The pre-write guards for one run: the identity of every scanned source
/// with its owning unit, the own locations of the source paths this run has
/// released (removed, or in a dry run, would have removed), the run's
/// conflict policy, and the canonical input root. Refusals apply only where
/// a write could actually destroy content, which is the overwrite and
/// overwrite-invalid policies.
pub(super) struct WriteGuards<'a> {
    sources: &'a HashMap<FileIdentity, Vec<(usize, PathBuf)>>,
    realized: HashSet<FileIdentity>,
    released: HashSet<PathBuf>,
    policy: ConflictPolicy,
    root: PathBuf,
}

impl WriteGuards<'_> {
    /// A guard set that never refuses, for tests placing into separate
    /// trees.
    #[cfg(test)]
    pub(super) fn never(root: PathBuf) -> WriteGuards<'static> {
        static EMPTY: std::sync::OnceLock<HashMap<FileIdentity, Vec<(usize, PathBuf)>>> =
            std::sync::OnceLock::new();
        WriteGuards {
            sources: EMPTY.get_or_init(HashMap::new),
            realized: HashSet::new(),
            released: HashSet::new(),
            policy: ConflictPolicy::Error,
            root,
        }
    }

    /// Records source paths this run released (by their own location), so
    /// a later unit desiring one of them is not refused: in a real run the
    /// file is gone, and a dry run mirrors that.
    fn release(&mut self, paths: Vec<PathBuf>) {
        self.released
            .extend(paths.iter().map(|path| entry_location(path)));
    }

    /// True when `path`'s own location is a source this run released: a
    /// real run finds nothing there, so a dry run treats it as absent.
    fn is_released(&self, path: &Path) -> bool {
        self.released.contains(&entry_location(path))
            || FileIdentity::of(path)
                .and_then(|identity| self.sources.get(&identity))
                .is_some_and(|owners| {
                    owners.iter().any(|(_, source)| {
                        self.released.contains(&entry_location(source))
                            && same_location(source, path)
                    })
                })
    }

    /// Why the guard would refuse a write to the plan's desired output:
    /// this run's earlier output, another scanned unit's source file, or
    /// an existing file whose own location is under the input root.
    fn would_refuse_cause(&self, plan: &UnitPlan, own_sources: &[PathBuf]) -> Option<RefusalCause> {
        if !matches!(plan.decision, Decision::Keep) {
            return None;
        }
        let desired = plan.desired.as_ref()?;
        if own_sources.iter().any(|path| same_location(path, desired)) {
            return None;
        }
        let identity = FileIdentity::keyed(desired);
        // This run's earlier output is refused even when it does not exist
        // on disk yet (a dry run previews the refusal).
        if self.realized.contains(&identity) {
            return Some(RefusalCause::Realized);
        }
        // Only an existing file can be overwritten; one metadata lookup per
        // plan gates everything below.
        std::fs::symlink_metadata(desired).ok()?;
        // A source this run already released is gone in a real run; a dry
        // run only planned its removal, so the file still exists but must
        // not be refused. Only the desired path's own location counts: a
        // hard link to a released source is a different entry that stays.
        if self.is_released(desired) {
            return None;
        }
        let owner = self.sources.get(&identity).and_then(|owners| {
            owners
                .iter()
                .find(|(owner, path)| *owner != plan.index && !self.is_released(path))
                .map(|(_, path)| path.clone())
        });
        if let Some(owner) = owner {
            return Some(RefusalCause::Source(owner));
        }
        // An input file the scan dropped (past --max-depth, a sidecar) is
        // still never overwritten when it sits under the input root.
        if location_under_root(desired, &self.root) {
            return Some(RefusalCause::UnderInput);
        }
        None
    }

    /// The refusal detail for a cause: names the desired path and says why.
    fn refusal_detail(cause: &RefusalCause, desired: &Path) -> String {
        match cause {
            RefusalCause::Realized => format!(
                "refusing to overwrite {}: this run already wrote it",
                desired.display()
            ),
            RefusalCause::Source(owner) => format!(
                "refusing to overwrite {}: it is another unit's source file ({})",
                desired.display(),
                owner.display()
            ),
            RefusalCause::UnderInput => format!(
                "refusing to overwrite {}: it is under the input directory",
                desired.display()
            ),
        }
    }

    /// The failed row a refusal produces.
    fn refusal(
        &self,
        plan: &UnitPlan,
        detail: String,
        input_bytes: u64,
        started: Instant,
        dry_run: bool,
    ) -> OrganizeRow {
        failed_row(
            &plan.source,
            plan.tokens.console.clone(),
            action_label(&plan.action),
            detail,
            input_bytes,
            started,
            dry_run,
        )
    }

    /// Records the identity of an output this run realized, so a later unit
    /// desiring the same file is refused.
    fn realized(&mut self, output: &Path) {
        self.realized.insert(FileIdentity::keyed(output));
    }

    /// `Some` failed row when the plan's desired output is (by identity)
    /// another unit's source that this run has not removed, a file this
    /// run already realized, or an existing file whose own location is
    /// under the input root other than the unit's own source. Overwriting
    /// any of these destroys content `move_source` has already committed
    /// to, so the plan is refused instead of written.
    fn refuse(
        &self,
        plan: &UnitPlan,
        own_sources: &[PathBuf],
        input_bytes: u64,
        started: Instant,
        dry_run: bool,
    ) -> Option<OrganizeRow> {
        if !matches!(
            self.policy,
            ConflictPolicy::Overwrite | ConflictPolicy::OverwriteInvalid
        ) {
            return None;
        }
        let cause = self.would_refuse_cause(plan, own_sources)?;
        let desired = plan.desired.as_ref()?;
        Some(self.refusal(
            plan,
            Self::refusal_detail(&cause, desired),
            input_bytes,
            started,
            dry_run,
        ))
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

/// What the planner would write for exactly the discs an existing `.m3u`
/// lists, judged by the shape of its lines alone: every line must be a
/// bare file name (a single path component) with a disc extension, the
/// names must form one disc group whose base title equals the `.m3u`'s
/// stem, and the lines must already sit in planner order. File existence
/// plays no part: a listed disc that is gone still refreshes (the new
/// plan writes what exists now). `None` for comments, absolute or
/// subdirectory spellings, other titles, or a reordered listing: a user
/// file, never rewritten.
fn own_playlist_derivation(m3u_stem: &str, existing: Option<&[u8]>) -> Option<String> {
    let bytes = existing?;
    let mut listed: Vec<(u32, String)> = Vec::new();
    for line in String::from_utf8_lossy(bytes).lines() {
        if line.trim_start().starts_with('#') {
            return None;
        }
        // The planner writes bare file names only: an absolute path, a
        // subdirectory spelling, or a `..` climb is a user file.
        let path = Path::new(line);
        let mut components = path.components();
        if !matches!(components.next(), Some(std::path::Component::Normal(_)))
            || components.next().is_some()
        {
            return None;
        }
        let ext = path.extension().and_then(|ext| ext.to_str())?;
        if !PLAYLIST_DISC_EXTS.contains(&ext.to_ascii_lowercase().as_str()) {
            return None;
        }
        let stem = path.file_stem().and_then(|stem| stem.to_str())?;
        let number = crate::playlist::parse_disc_token(stem).map_or(0, |(_, number)| number);
        listed.push((number, line.to_string()));
    }
    if listed.is_empty() {
        return None;
    }
    // Planner order is the sorted order: a hand-reordered listing is a
    // user edit even when it names the same discs.
    let mut ordered = listed.clone();
    ordered.sort();
    if listed != ordered {
        return None;
    }
    // The lines must form exactly one group titled like the .m3u.
    let paths: Vec<PathBuf> = listed.iter().map(|(_, line)| PathBuf::from(line)).collect();
    let groups = crate::playlist::group_disc_files(&paths);
    if groups.len() != 1 || groups[0].base_title != m3u_stem {
        return None;
    }
    Some(format!(
        "{}\n",
        ordered
            .into_iter()
            .map(|(_, line)| line)
            .collect::<Vec<_>>()
            .join("\n")
    ))
}

/// Plans (never writes) the `.m3u` playlists for every output directory that
/// received a disc image. A planned (dry-run) disc dir may not exist on disk
/// yet (nothing was written), so it scans to nothing, silently. A dir whose
/// plan fails warns and is skipped instead of aborting the run; only
/// cancellation propagates. Returns the plans plus every dir whose plan
/// failed: nothing could be planned there, so its playlists must never be
/// treated as stale. Lines deriving a file in `exclude` (a dry-run clean's
/// planned deletes) drop out of each plan's contents, so the plan matches
/// what a real run's scan would see; a plan left with too few entries to
/// derive a playlist is dropped.
fn plan_disc_playlists(
    disc_dirs: &BTreeSet<PathBuf>,
    dry_run: bool,
    warned: &mut BTreeSet<PathBuf>,
    exclude: &BTreeSet<PathBuf>,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<(Vec<PlaylistPlan>, BTreeSet<PathBuf>)> {
    let mut plans = Vec::new();
    let mut failed_dirs = BTreeSet::new();
    for dir in disc_dirs {
        // A planned (dry-run) disc dir may not exist on disk yet (nothing
        // was written), so it scans to nothing, silently.
        if dry_run && !dir.exists() {
            continue;
        }
        let dir_plans = match plan_playlists(
            &PlaylistOptions {
                scan_dir: dir,
                output_dir: None,
                extensions: PLAYLIST_DISC_EXTS,
                mode: PlaylistMode::Multiple,
                max_depth: Some(1),
            },
            cancel,
        ) {
            Ok(dir_plans) => dir_plans,
            Err(err) => {
                let err = anyhow::Error::from(err);
                if is_cancelled_error(&err) {
                    return Err(err);
                }
                failed_dirs.insert(dir.clone());
                // The pre-clean keep-set pass and the post-clean write pass
                // plan the same dirs: a dir whose plan fails warns once.
                if warned.insert(dir.clone()) {
                    progress.warn(&format!(
                        "could not plan playlists in {}: {err}",
                        dir.display()
                    ));
                }
                continue;
            }
        };
        plans.extend(
            dir_plans
                .into_iter()
                .filter_map(|plan| filter_playlist_exclusions(plan, dir, exclude)),
        );
    }
    Ok((plans, failed_dirs))
}

/// Folds the dry-run clean's planned deletes into one playlist plan: excluded
/// lines drop out of the contents, and the duplicate-number flag is recomputed
/// over what remains by the same grouping the playlist planner uses. The plan
/// survives only when enough entries remain for its mode (two in Multiple,
/// which is what this planner scans), because a shrunken set still derives a
/// playlist, while an emptied one derives nothing.
fn filter_playlist_exclusions(
    plan: PlaylistPlan,
    scan_dir: &Path,
    exclude: &BTreeSet<PathBuf>,
) -> Option<PlaylistPlan> {
    let plan_dir = plan.m3u_path.parent().unwrap_or(scan_dir);
    let kept: Vec<&str> = plan
        .contents
        .lines()
        .filter(|line| !exclude.contains(&plan_dir.join(line)))
        .collect();
    if kept.len() < 2 {
        return None;
    }
    let kept_paths: Vec<PathBuf> = kept.iter().map(|line| plan_dir.join(line)).collect();
    let has_duplicate_numbers = crate::playlist::group_disc_files(&kept_paths)
        .into_iter()
        .any(|group| group.has_duplicate_numbers);
    Some(PlaylistPlan {
        base_title: plan.base_title,
        m3u_path: plan.m3u_path,
        disc_count: kept.len(),
        has_duplicate_numbers,
        contents: format!("{}\n", kept.join("\n")),
    })
}

/// The pre-clean planned `.m3u` paths the post-clean plan no longer derives:
/// their discs went to clean, so the derived file is stale too. A dir whose
/// playlist plan failed is skipped: its playlists are unplan-able, not
/// stale, and never deleted for it.
fn stale_playlist_paths(
    planned: &[PlaylistPlan],
    post_clean_planned: &BTreeSet<PathBuf>,
    failed_dirs: &BTreeSet<PathBuf>,
) -> Vec<PathBuf> {
    planned
        .iter()
        .map(|plan| plan.m3u_path.clone())
        .filter(|path| !post_clean_planned.contains(path))
        .filter(|path| !failed_dirs.iter().any(|dir| path.starts_with(dir)))
        .collect()
}

/// Plans the playlists from the cleaned disc dirs and writes them as
/// derived data: an `.m3u` that already exists byte-identical is skipped as
/// `already current` and nothing is written; an `.m3u` that differs is
/// rewritten with a row saying so, unless it is a symlink, which is never
/// written through, or it sits under the input root and is not the tool's
/// own derivation for the discs it lists (a user file, which is kept), or
/// it is read-only, which is refused with a warning in dry runs too. A dry
/// run only plans, reporting each planned path as a planned row. Every
/// planned, written, or already-current playlist also yields one "playlist"
/// row (input = the disc dir, output = the .m3u path).
/// A playlist that cannot be planned or written warns and stays out of the
/// result instead of aborting the run; only cancellation propagates.
/// Returns the rows' data, the rows, every post-clean planned `.m3u`
/// path (planned, never derived from rows, so a playlist that failed to
/// read or write, or was refused as a symlink, still counts as current as
/// far as staleness goes) and every dir whose post-clean plan failed.
/// Plans deriving a file in `exclude` (a dry-run clean's planned deletes)
/// are dropped.
async fn write_playlists(
    req: &RunRequest,
    disc_dirs: &BTreeSet<PathBuf>,
    warned: &mut BTreeSet<PathBuf>,
    exclude: &BTreeSet<PathBuf>,
    input_root: &Path,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<(
    Vec<PlaylistPlanData>,
    Vec<OrganizeRow>,
    BTreeSet<PathBuf>,
    BTreeSet<PathBuf>,
)> {
    let dry_run = req.dry_run;
    let mut playlists = Vec::new();
    let mut rows = Vec::new();
    let mut planned = BTreeSet::new();
    let (disc_plans, failed_dirs) =
        plan_disc_playlists(disc_dirs, dry_run, warned, exclude, progress, cancel)?;
    for plan in disc_plans {
        planned.insert(plan.m3u_path.clone());
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
        let dir = plan
            .m3u_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        // Derived data: a byte-identical .m3u is already current. The read is
        // side-effect free, so a dry run does it too: an unchanged playlist
        // plans a skip instead of a rewrite. Reading it failing with anything
        // but "not there" warns like a failed write. The read is bounded: a
        // playlist past the 16 MiB comparison cap warns and counts as
        // differing (an empty read never equals a written plan), so it is
        // rewritten instead of held whole in memory.
        let existing = match tokio::fs::File::open(&plan.m3u_path).await {
            Ok(file) => {
                let mut file = file.take(MAX_PLAYLIST_COMPARE_BYTES + 1);
                let mut bytes = Vec::new();
                match file.read_to_end(&mut bytes).await {
                    Ok(_) if bytes.len() as u64 > MAX_PLAYLIST_COMPARE_BYTES => {
                        progress.warn(&format!(
                            "existing playlist {} exceeds the 16 MiB comparison cap; \
                             treating it as differing",
                            plan.m3u_path.display()
                        ));
                        Some(Vec::new())
                    }
                    Ok(_) => Some(bytes),
                    Err(err) => {
                        progress.warn(&format!(
                            "could not read existing playlist {}: {err}",
                            plan.m3u_path.display()
                        ));
                        continue;
                    }
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                progress.warn(&format!(
                    "could not read existing playlist {}: {err}",
                    plan.m3u_path.display()
                ));
                continue;
            }
        };
        if existing.as_deref() == Some(plan.contents.as_bytes()) {
            playlists.push(PlaylistPlanData {
                base_title: plan.base_title.clone(),
                output: plan.m3u_path.clone(),
                contents: plan.contents.clone(),
                disc_count: plan.disc_count,
                has_duplicate_numbers: plan.has_duplicate_numbers,
            });
            rows.push(OrganizeRow {
                input: dir,
                output: Some(plan.m3u_path.clone()),
                console: None,
                action: "playlist".to_string(),
                status: FileStatus::Skipped,
                planned: dry_run,
                verify: None,
                detail: Some("already current".to_string()),
                input_bytes: 0,
                output_bytes: 0,
                elapsed_ms: 0,
            });
            continue;
        }
        // The existing playlist differs: a .m3u whose own location is under
        // the input root is rewritten only when it is the tool's own
        // derivation for the discs it lists; anything else (comments,
        // reordering, foreign lines, subdirectory spellings) is a user
        // file and is kept, whatever the conflict policy says.
        if std::fs::symlink_metadata(&plan.m3u_path).is_ok()
            && location_under_root(&plan.m3u_path, input_root)
            && existing
                .as_deref()
                .zip(own_playlist_derivation(
                    plan.m3u_path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or_default(),
                    existing.as_deref(),
                ))
                .is_none_or(|(bytes, derived)| bytes != derived.as_bytes())
        {
            rows.push(OrganizeRow {
                input: dir,
                output: Some(plan.m3u_path.clone()),
                console: None,
                action: "playlist".to_string(),
                status: FileStatus::Skipped,
                planned: dry_run,
                verify: None,
                detail: Some("playlist is under the input directory".to_string()),
                input_bytes: 0,
                output_bytes: 0,
                elapsed_ms: 0,
            });
            continue;
        }
        // The existing playlist differs: refuse to write through a symlink;
        // the link belongs to whatever it points at, not to this tree. A dry
        // run refuses too: its plan must not claim a rewrite it could never
        // perform.
        if std::fs::symlink_metadata(&plan.m3u_path).is_ok_and(|meta| meta.is_symlink()) {
            progress.warn(&format!(
                "playlist is a symlink: {}",
                plan.m3u_path.display()
            ));
            rows.push(OrganizeRow {
                input: dir,
                output: Some(plan.m3u_path.clone()),
                console: None,
                action: "playlist".to_string(),
                status: FileStatus::Skipped,
                planned: dry_run,
                verify: None,
                detail: Some("playlist is a symlink".to_string()),
                input_bytes: 0,
                output_bytes: 0,
                elapsed_ms: 0,
            });
            continue;
        }
        // A playlist is only reported once it is on disk (or, in a dry run,
        // once its planned path is known); a failed write warns instead of
        // counting. An atomic replace would silently drop the read-only
        // flag, so the refusal is made here; a dry run refuses too, so its
        // plan never claims a rewrite the real run would refuse.
        let written = if std::fs::symlink_metadata(&plan.m3u_path)
            .is_ok_and(|meta| meta.permissions().readonly())
        {
            let message = format!(
                "could not write playlist {}: it is read-only",
                plan.m3u_path.display()
            );
            progress.warn(&message);
            None
        } else if dry_run {
            Some(plan.m3u_path.clone())
        } else {
            let path = plan.m3u_path.clone();
            let contents = plan.contents.clone();
            match tokio::task::spawn_blocking(move || {
                let mode = std::fs::metadata(&path).ok().map(|meta| meta.permissions());
                crate::util::atomic_write(&path, true, |file| {
                    use std::io::Write;
                    file.write_all(contents.as_bytes())?;
                    // The permissions must land on the scratch file: the
                    // rename swaps the whole inode, so a fixup on the
                    // final path would be lost. A rewrite keeps the
                    // existing mode; a new file keeps the scratch's
                    // creation mode, the process default (0666 & !umask
                    // on unix), like any File::create.
                    if let Some(mode) = &mode {
                        file.set_permissions(mode.clone())?;
                    }
                    Ok::<(), anyhow::Error>(())
                })
            })
            .await
            {
                Ok(Ok(())) => Some(plan.m3u_path.clone()),
                Ok(Err(err)) => {
                    progress.warn(&format!(
                        "could not write playlist {}: {err:#}",
                        plan.m3u_path.display()
                    ));
                    None
                }
                Err(err) => {
                    progress.warn(&format!(
                        "could not write playlist {}: {err}",
                        plan.m3u_path.display()
                    ));
                    None
                }
            }
        };
        if let Some(output) = written {
            playlists.push(PlaylistPlanData {
                base_title: plan.base_title.clone(),
                output,
                contents: plan.contents.clone(),
                disc_count: plan.disc_count,
                has_duplicate_numbers: plan.has_duplicate_numbers,
            });
            rows.push(OrganizeRow {
                input: dir,
                output: Some(plan.m3u_path.clone()),
                console: None,
                action: "playlist".to_string(),
                status: FileStatus::Ok,
                planned: dry_run,
                verify: None,
                // The playlist existed but differed: say the row rewrote it.
                detail: existing
                    .is_some()
                    .then(|| "rewrote playlist that differed".to_string()),
                input_bytes: 0,
                output_bytes: 0,
                elapsed_ms: 0,
            });
        }
    }
    Ok((playlists, rows, planned, failed_dirs))
}

/// Extension of `path`, without the dot; empty when absent.
fn ext_of(path: &Path) -> &str {
    path.extension().and_then(|ext| ext.to_str()).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::RecordingProgress;
    use crate::runner::models::{
        ComparisonData, ProgressEvent, RunComparisonData, VerifyReport, VerifyVerdict,
    };

    /// A [`ProgressReporter`] capturing streamed organize rows:
    /// [`RecordingProgress`] drops `row` events, so capture them here. A
    /// poisoned mutex only means a reporting caller panicked; the recorded
    /// rows stay well formed, so keep appending.
    #[derive(Default)]
    struct RowRecorder(std::sync::Mutex<Vec<OrganizeRow>>);

    impl RowRecorder {
        fn rows(&self) -> Vec<OrganizeRow> {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    impl ProgressReporter for RowRecorder {
        fn start(&self, _: u64, _: &str) {}
        fn inc(&self, _: u64) {}
        fn finish(&self) {}
        fn row(&self, row: &RunRow) {
            if let RunRow::Organize(row) = row {
                self.0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(row.clone());
            }
        }
    }
    use std::io::Write as _;
    use tempfile::TempDir;

    /// The run's warning messages, for assertions on what a failed write
    /// or skip reported.
    fn warnings(progress: &RecordingProgress) -> Vec<String> {
        progress
            .take_events()
            .into_iter()
            .filter_map(|event| match event {
                ProgressEvent::Warn { message } => Some(message),
                _ => None,
            })
            .collect()
    }

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

    /// Patches are matched after filtering and best-release selection: an
    /// eliminated original never spawns a patched output, while a kept one
    /// gains a patched sibling named after the patch.
    #[tokio::test]
    async fn patches_expand_only_surviving_plans() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let patches = TempDir::new().unwrap();
        let bytes = gba_bytes();
        std::fs::write(lib.path().join("Kept Game (USA).gba"), &bytes).unwrap();
        std::fs::write(lib.path().join("Dropped Game (USA) (Beta).gba"), &bytes).unwrap();
        // One IPS patch keyed on the shared CRC: a 4-byte record at offset 0.
        let crc = crate::util::hash::CRC32.checksum(&bytes);
        let mut ips = b"PATCH".to_vec();
        ips.extend_from_slice(&[0, 0, 0, 0, 4]);
        ips.extend_from_slice(b"ZZZZ");
        ips.extend_from_slice(b"EOF");
        std::fs::write(patches.path().join(format!("Hack [{crc:08X}].ips")), ips).unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.no_type = Some(vec!["beta".to_string()]);
        req.options.patch = Some(vec![patches.path().to_path_buf()]);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let outputs: Vec<String> = data
            .rows
            .iter()
            .filter(|row| row.status == FileStatus::Ok)
            .filter_map(|row| row.output.as_ref())
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            outputs,
            vec![
                "Kept Game (USA).zip".to_string(),
                format!("Hack [{crc:08X}].zip")
            ]
        );
        assert_eq!(data.skipped, 1);
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

    #[cfg(unix)]
    #[tokio::test]
    async fn case_variant_in_place_with_an_external_hardlink_survives_move() {
        let base = TempDir::new().unwrap();
        let lib = base.path().join("lib");
        std::fs::create_dir(&lib).unwrap();
        let source = lib.join("game.xiso");
        let desired = lib.join("Game.xiso");
        let twin = base.path().join("outside.xiso");
        std::fs::write(&source, b"source").unwrap();
        std::fs::hard_link(&source, &twin).unwrap();
        let folds_case = select::probe_case_insensitive(&lib);
        if !folds_case {
            std::fs::hard_link(&source, &desired).unwrap();
        }
        assert_eq!(same_location(&source, &desired), folds_case);

        let mut req = organize_request(&lib, Some(&lib), false);
        req.options.output_template = Some("Game.{ext}".to_string());
        req.options.move_source = Some(true);
        req.options.on_conflict = Some("overwrite-invalid".to_string());
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert_eq!(data.failed, 0, "{:?}", data.rows);
        assert_eq!(std::fs::read(&source).unwrap(), b"source");
        assert_eq!(std::fs::read(&desired).unwrap(), b"source");
        assert_eq!(std::fs::read(&twin).unwrap(), b"source");
        if folds_case {
            assert_eq!(
                row_for(&data, "game.xiso").detail.as_deref(),
                Some("already in place")
            );
        } else {
            assert!(!same_location(&source, &desired));
            assert_eq!(std::fs::read_dir(&lib).unwrap().count(), 2);
        }
    }

    #[cfg(unix)]
    #[test]
    fn same_location_with_hardlinks_and_a_symlinked_parent() {
        let base = TempDir::new().unwrap();
        let lib = base.path().join("lib");
        let alias = base.path().join("alias");
        std::fs::create_dir(&lib).unwrap();
        let source = lib.join("Game.gba");
        let twin = lib.join("Twin.gba");
        std::fs::write(&source, b"source").unwrap();
        std::fs::hard_link(&source, &twin).unwrap();
        std::os::unix::fs::symlink(&lib, &alias).unwrap();
        assert!(same_location(&source, &alias.join("Game.gba")));
        assert!(!same_location(&source, &alias.join("Twin.gba")));
        if select::probe_case_insensitive(&lib) {
            assert!(!same_location(&source, &alias.join("twin.gba")));
        }
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

    /// `link_mode = hardlink` places an `.xiso` copy action as a filesystem
    /// link: the output shares the source's inode and the row says "link".
    #[cfg(unix)]
    #[tokio::test]
    async fn link_mode_hardlink_places_a_hardlink() {
        use std::os::unix::fs::MetadataExt;

        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Game.xiso"), b"not a real xiso").unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.link_mode = Some("hardlink".to_string());
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Game.xiso");
        assert_eq!(row.status, FileStatus::Ok, "{:?}", row.detail);
        assert_eq!(row.action, "link");
        let output = row.output.clone().unwrap();
        assert_eq!(
            std::fs::metadata(lib.path().join("Game.xiso"))
                .unwrap()
                .ino(),
            std::fs::metadata(&output).unwrap().ino()
        );
        assert_eq!(
            std::fs::read(lib.path().join("Game.xiso")).unwrap(),
            std::fs::read(&output).unwrap()
        );
    }

    /// A symlink `link_mode` cannot also delete sources: the link would be
    /// the only thing left pointing at a vanished target.
    #[tokio::test]
    async fn symlink_link_mode_with_move_source_is_invalid_arg() {
        let lib = TempDir::new().unwrap();
        std::fs::write(lib.path().join("game.xiso"), b"not a real xiso").unwrap();

        let mut req = organize_request(lib.path(), Some(Path::new("/tmp")), false);
        req.options.link_mode = Some("symlink".to_string());
        req.options.move_source = Some(true);
        let err = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");
    }

    /// `zip_format = rvzstd` writes an RVZSTD archive, verifiable through the
    /// structured-zip validator.
    #[tokio::test]
    async fn rvzstd_zip_format_writes_rvzstd() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let bytes = gba_bytes();
        std::fs::write(lib.path().join("Test Game.gba"), &bytes).unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.zip_format = Some("rvzstd".to_string());
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let zip_row = row_for(&data, "Test Game.gba");
        assert_eq!(zip_row.status, FileStatus::Ok, "{:?}", zip_row.detail);
        let zip_path = zip_row.output.clone().unwrap();

        let (format, entries) = crate::util::validate_torrentzip(&zip_path)
            .unwrap()
            .expect("structured zip");
        assert_eq!(format, crate::util::ZipFormat::RvZstd);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Test Game.gba");
        assert_eq!(entries[0].size, bytes.len() as u64);
    }

    /// Playlists are planned even in dry runs, without writing, so their
    /// paths reach the clean keep-set and the returned plan data.
    #[tokio::test]
    async fn dry_run_plans_playlists_without_writing() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 1).rvz"), gcm_bytes()).unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 2).rvz"), gcm_bytes()).unwrap();

        // Seed the outputs with a real run, then plan over them: a dry run
        // under `overwrite-invalid` keeps the valid outputs and reports
        // them as skipped-with-output, which feeds playlist planning.
        organize(
            organize_request(lib.path(), Some(out.path()), false),
            &RecordingProgress::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        assert!(
            out.path()
                .join("GameCube")
                .join("Grouped Game (Disc 1).rvz")
                .exists()
        );

        let mut req = organize_request(lib.path(), Some(out.path()), true);
        req.options.playlists = Some(true);
        req.options.on_conflict = Some("overwrite-invalid".to_string());
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert!(data.dry_run);
        assert_eq!(data.playlists.len(), 1, "{:?}", data.rows);
        let playlist = &data.playlists[0];
        assert_eq!(playlist.base_title, "Grouped Game");
        assert_eq!(playlist.disc_count, 2);
        assert_eq!(
            playlist.output,
            out.path().join("GameCube").join("Grouped Game.m3u")
        );
        // Planned, never written.
        assert!(!playlist.output.exists());
        assert!(
            !out.path()
                .join("GameCube")
                .join("Grouped Game.m3u")
                .exists()
        );
    }

    /// A stripped console header is named on the successful row, and the
    /// zip member carries the headerless extension.
    #[tokio::test]
    async fn header_stripping_is_named_on_the_row() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        // The copier header is only detected when the total size is a
        // multiple of 1024 plus 512, so the body is a full 1024-byte block.
        let mut rom = vec![0u8; 512];
        rom.extend_from_slice(&[0xEEu8; 1024]);
        std::fs::write(lib.path().join("Copied Game.smc"), &rom).unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.remove_headers = Some(vec!["smc".to_string()]);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Copied Game.smc");
        assert_eq!(row.status, FileStatus::Ok, "{:?}", row.detail);
        assert_eq!(row.action, "zip");
        assert_eq!(row.detail.as_deref(), Some("header stripped: SMC"));

        let zip_path = row.output.clone().unwrap();
        let (_, entries) = crate::util::validate_torrentzip(&zip_path)
            .unwrap()
            .expect("structured zip");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Copied Game.sfc");
        assert_eq!(entries[0].size, rom.len() as u64 - 512);
    }

    /// An IPS patch keyed on the source CRC, matching the pattern of
    /// `patches_expand_only_surviving_plans`.
    fn ips_for(patches: &TempDir, name: &str, bytes: &[u8]) -> u32 {
        let crc = crate::util::hash::CRC32.checksum(bytes);
        let mut ips = b"PATCH".to_vec();
        ips.extend_from_slice(&[0, 0, 0, 0, 4]);
        ips.extend_from_slice(b"ZZZZ");
        ips.extend_from_slice(b"EOF");
        std::fs::write(patches.path().join(format!("{name} [{crc:08X}].ips")), ips).unwrap();
        crc
    }

    /// `move_source` deletes a unit's source only after ALL its plans
    /// (base plus patched variant) succeeded: both outputs exist, the
    /// source is gone, and zip rows keep their action name.
    #[tokio::test]
    async fn move_source_deletes_after_every_plan_of_the_unit() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let patches = TempDir::new().unwrap();
        let bytes = gba_bytes();
        std::fs::write(lib.path().join("Kept Game (USA).gba"), &bytes).unwrap();
        let crc = ips_for(&patches, "Hack", &bytes);

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.move_source = Some(true);
        req.options.patch = Some(vec![patches.path().to_path_buf()]);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let ok_rows: Vec<&OrganizeRow> = data
            .rows
            .iter()
            .filter(|row| row.status == FileStatus::Ok)
            .collect();
        assert_eq!(ok_rows.len(), 2, "{:?}", data.rows);
        assert!(ok_rows.iter().all(|row| row.action == "zip"));
        let base = out
            .path()
            .join("Game Boy Advance")
            .join("Kept Game (USA).zip");
        let patched = out
            .path()
            .join("Game Boy Advance")
            .join(format!("Hack [{crc:08X}].zip"));
        assert!(base.exists());
        assert!(patched.exists());
        assert!(!lib.path().join("Kept Game (USA).gba").exists());
    }

    /// The flip side: when one plan of the unit fails or skips, the source
    /// stays: here the patched output already exists under the error
    /// policy, so the patched row skips and the source survives.
    #[tokio::test]
    async fn move_source_keeps_the_source_when_a_plan_skips() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let patches = TempDir::new().unwrap();
        let bytes = gba_bytes();
        std::fs::write(lib.path().join("Kept Game (USA).gba"), &bytes).unwrap();
        let crc = ips_for(&patches, "Hack", &bytes);
        let target_dir = out.path().join("Game Boy Advance");
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(target_dir.join(format!("Hack [{crc:08X}].zip")), b"stale").unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.move_source = Some(true);
        req.options.patch = Some(vec![patches.path().to_path_buf()]);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert_eq!(data.ok, 1);
        assert_eq!(data.skipped, 1);
        assert!(lib.path().join("Kept Game (USA).gba").exists());
        assert!(target_dir.join("Kept Game (USA).zip").exists());
        assert_eq!(
            std::fs::read(target_dir.join(format!("Hack [{crc:08X}].zip"))).unwrap(),
            b"stale"
        );
    }

    /// Linked sources must be plain files: an archive member is staged into
    /// a temp extraction, so a `link_mode` plan for it places a copy and
    /// says so, never a link into the temp dir.
    #[cfg(unix)]
    #[tokio::test]
    async fn link_mode_copies_staged_and_patched_sources() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let zip_path = lib.path().join("Cube Game.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("Cube Game.rvz", opts).unwrap();
        zip.write_all(&gcm_bytes()).unwrap();
        zip.finish().unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.link_mode = Some("symlink".to_string());
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Cube Game.zip");
        assert_eq!(row.status, FileStatus::Ok, "{:?}", row.detail);
        assert_eq!(row.action, "copy");
        assert_eq!(
            row.detail.as_deref(),
            Some("copied: linked sources must be plain files")
        );
        let output = row.output.clone().unwrap();
        assert!(!std::fs::symlink_metadata(&output).unwrap().is_symlink());
        assert_eq!(std::fs::read(&output).unwrap(), gcm_bytes());
    }

    /// Clean with overlapping input/output trees protects even excluded
    /// inputs: the keep-set captures every file the scan saw, before
    /// exclusions drop them.
    #[tokio::test]
    async fn clean_with_overlapping_dirs_protects_excluded_inputs() {
        let lib = TempDir::new().unwrap();
        std::fs::create_dir_all(lib.path().join("Game Boy Advance")).unwrap();
        std::fs::write(
            lib.path()
                .join("Game Boy Advance")
                .join("Excluded Game.gba"),
            gba_bytes(),
        )
        .unwrap();
        std::fs::write(lib.path().join("Kept Game.gba"), gba_bytes()).unwrap();

        let mut req = organize_request(lib.path(), Some(lib.path()), false);
        req.options.clean = Some(true);
        req.options.input_exclude = Some(vec!["**/Excluded Game.gba".to_string()]);
        let progress = RecordingProgress::default();
        let response = organize(req, &progress, CancelToken::new()).await.unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert_eq!(data.failed, 0, "{:?}", data.rows);
        assert!(
            lib.path()
                .join("Game Boy Advance")
                .join("Excluded Game.gba")
                .exists(),
            "the excluded input must survive clean"
        );
        assert!(
            lib.path()
                .join("Game Boy Advance")
                .join("Kept Game.zip")
                .exists()
        );
        let warnings: Vec<String> = progress
            .take_events()
            .into_iter()
            .filter_map(|event| match event {
                ProgressEvent::Warn { message } => Some(message),
                _ => None,
            })
            .collect();
        assert!(
            warnings.iter().any(|message| message.contains("overlap")),
            "{warnings:?}"
        );
    }

    /// A minimal plan for CRC bookkeeping tests.
    fn crc_plan(index: usize) -> UnitPlan {
        UnitPlan {
            index,
            source: PathBuf::from("Game.gba"),
            source_ext: "gba".to_string(),
            action: Action::Copy,
            tokens: crate::util::TemplateTokens::new(None, Path::new("Game.gba"), "gba"),
            input_subdir: PathBuf::new(),
            game: None,
            header: None,
            strip: 0,
            trim: None,
            pad: 0,
            pad_fill: 0,
            patch: None,
            desired: None,
            decision: Decision::Keep,
            staging_error: None,
            output_error: None,
            match_error: None,
            member: None,
        }
    }

    /// Skip plans never hash, and a missing source warns into `None`
    /// instead of aborting the CRC pass.
    #[tokio::test]
    async fn unit_crcs_skips_skip_plans_and_warns_on_read_errors() {
        let dir = TempDir::new().unwrap();
        let game = dir.path().join("Game.gba");
        std::fs::write(&game, b"rom bytes").unwrap();
        let missing = dir.path().join("Missing.gba");
        let units = vec![
            DatUnit::File(dir.path().join("Skipped.gba")),
            DatUnit::File(missing.clone()),
            DatUnit::File(game.clone()),
        ];
        let mut skip = crc_plan(0);
        skip.decision = Decision::Skip("unrecognized".to_string());
        let plans = vec![skip, crc_plan(1), crc_plan(2)];

        let progress = RecordingProgress::default();
        let crcs = unit_crcs(&plans, &units, &progress, &CancelToken::new())
            .await
            .unwrap();

        assert_eq!(
            crcs,
            vec![
                None,
                None,
                Some(crate::util::hash::CRC32.checksum(b"rom bytes"))
            ]
        );
        let warnings: Vec<String> = progress
            .take_events()
            .into_iter()
            .filter_map(|event| match event {
                ProgressEvent::Warn { message } => Some(message),
                _ => None,
            })
            .collect();
        assert!(
            warnings
                .iter()
                .any(|message| message.contains("Missing.gba")),
            "{warnings:?}"
        );
    }

    /// Playlists are derived data. A re-run with `--clean --playlists` keeps
    /// the previous `.m3u` twice over: its planned path joins the keep set,
    /// so clean never deletes it (no clean row for it), and a byte-identical
    /// playlist is skipped as `already current` instead of being rewritten.
    /// A changed disc set is rewritten even under `on_conflict = error`:
    /// playlists never rename, error, or skip on a conflict.
    #[tokio::test]
    async fn rerun_under_overwrite_invalid_keeps_the_playlist_with_clean() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 1).rvz"), gcm_bytes()).unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 2).rvz"), gcm_bytes()).unwrap();

        let request = || {
            let mut req = organize_request(lib.path(), Some(out.path()), false);
            req.options.playlists = Some(true);
            req.options.clean = Some(true);
            req.options.on_conflict = Some("overwrite-invalid".to_string());
            req
        };
        organize(request(), &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let m3u = out.path().join("GameCube").join("Grouped Game.m3u");
        assert!(m3u.exists());
        let before = std::fs::read(&m3u).unwrap();

        let response = organize(request(), &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Grouped Game (Disc 1).rvz");
        assert_eq!(row.status, FileStatus::Skipped, "{:?}", row.detail);
        assert_eq!(row.detail.as_deref(), Some(VERIFIED_VALID));
        assert_eq!(
            row.output.as_deref(),
            Some(
                out.path()
                    .join("GameCube")
                    .join("Grouped Game (Disc 1).rvz")
                    .as_path()
            )
        );
        // Nothing was written: a skipped row counts no output bytes.
        assert_eq!(row.output_bytes, 0);
        // Clean kept the old .m3u: no clean row names it.
        assert!(
            !data
                .rows
                .iter()
                .any(|row| row.action == "clean" && row.input == m3u),
            "{:?}",
            data.rows
        );
        // The unchanged playlist is skipped, not rewritten.
        let playlist_row = data
            .rows
            .iter()
            .find(|row| row.action == "playlist")
            .expect("playlist row");
        assert_eq!(playlist_row.status, FileStatus::Skipped, "{playlist_row:?}");
        assert_eq!(playlist_row.detail.as_deref(), Some("already current"));
        assert_eq!(playlist_row.output.as_deref(), Some(m3u.as_path()));
        assert!(!playlist_row.planned);
        assert_eq!(data.playlists.len(), 1, "{:?}", data.rows);
        assert_eq!(std::fs::read(&m3u).unwrap(), before, "nothing rewritten");

        // A changed disc set is rewritten under `on_conflict = error`.
        std::fs::write(lib.path().join("Grouped Game (Disc 3).rvz"), gcm_bytes()).unwrap();
        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.playlists = Some(true);
        req.options.clean = Some(true);
        req.options.on_conflict = Some("error".to_string());
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let playlist_row = data
            .rows
            .iter()
            .find(|row| row.action == "playlist")
            .expect("playlist row");
        assert_eq!(playlist_row.status, FileStatus::Ok, "{playlist_row:?}");
        assert_eq!(
            playlist_row.detail.as_deref(),
            Some("rewrote playlist that differed")
        );
        assert_eq!(data.playlists[0].disc_count, 3);
        let contents = std::fs::read_to_string(&m3u).unwrap();
        assert!(contents.contains("Grouped Game (Disc 3).rvz"), "{contents}");
    }

    /// A dry-run clean's planned delete of one disc shrinks the playlist plan
    /// to the two remaining entries instead of dropping it: the playlist is
    /// planned as a rewrite of what the real run would derive, and clean
    /// never plans the `.m3u` itself as stale.
    #[tokio::test]
    async fn dry_run_excluding_one_disc_keeps_a_two_disc_playlist_plan() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        for disc in 1..=3 {
            std::fs::write(
                lib.path().join(format!("Grouped Game (Disc {disc}).rvz")),
                gcm_bytes(),
            )
            .unwrap();
        }
        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.playlists = Some(true);
        organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let m3u = out.path().join("GameCube").join("Grouped Game.m3u");
        assert!(m3u.exists());

        // The library loses disc 3, so the dry-run clean plans to delete it
        // from the output tree.
        std::fs::remove_file(lib.path().join("Grouped Game (Disc 3).rvz")).unwrap();
        let mut req = organize_request(lib.path(), Some(out.path()), true);
        req.options.playlists = Some(true);
        req.options.clean = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };

        // The playlist is still planned, now deriving the two survivors.
        let playlist_row = data
            .rows
            .iter()
            .find(|row| row.action == "playlist")
            .expect("playlist row");
        assert!(playlist_row.planned, "{playlist_row:?}");
        assert_eq!(playlist_row.output.as_deref(), Some(m3u.as_path()));
        assert_eq!(data.playlists.len(), 1, "{:?}", data.rows);
        assert_eq!(data.playlists[0].disc_count, 2);
        assert_eq!(data.playlists[0].contents.lines().count(), 2);
        assert!(!data.playlists[0].has_duplicate_numbers);
        // Clean never plans the still-derived `.m3u` as stale.
        assert!(
            !data
                .rows
                .iter()
                .any(|row| row.action == "clean" && row.input == m3u),
            "{:?}",
            data.rows
        );
    }

    /// A dry run reads the existing `.m3u` too (reads are side-effect free)
    /// and plans a skip when it is already current, instead of planning a
    /// rewrite of identical bytes.
    #[tokio::test]
    async fn dry_run_reports_an_already_current_playlist_as_skipped() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 1).rvz"), gcm_bytes()).unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 2).rvz"), gcm_bytes()).unwrap();
        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.playlists = Some(true);
        organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let m3u = out.path().join("GameCube").join("Grouped Game.m3u");
        let before = std::fs::read(&m3u).unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), true);
        req.options.playlists = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let playlist_row = data
            .rows
            .iter()
            .find(|row| row.action == "playlist")
            .expect("playlist row");
        assert_eq!(playlist_row.status, FileStatus::Skipped, "{playlist_row:?}");
        assert_eq!(playlist_row.detail.as_deref(), Some("already current"));
        assert!(playlist_row.planned, "{playlist_row:?}");
        assert_eq!(playlist_row.output.as_deref(), Some(m3u.as_path()));
        assert_eq!(std::fs::read(&m3u).unwrap(), before, "nothing written");
    }

    /// With `--dir-letter`, dedupe_by_path sees the final post-letter paths:
    /// a unit already sitting at its output (`G/Game.zip` under a flat
    /// template) wins its group over a sibling that would write the same
    /// path, instead of the sibling overwriting it.
    #[tokio::test]
    async fn dir_letter_dedupe_lets_the_in_place_unit_win() {
        let lib = TempDir::new().unwrap();
        let gba = gba_bytes();
        for dir in ["G", "A"] {
            let zip_path = lib.path().join(dir).join("Game.zip");
            std::fs::create_dir_all(zip_path.parent().unwrap()).unwrap();
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("Game.gba", opts).unwrap();
            zip.write_all(&gba).unwrap();
            zip.finish().unwrap();
        }

        let mut req = organize_request(lib.path(), Some(lib.path()), false);
        req.options.output_template = Some("{basename}.{ext}".to_string());
        req.options.dir_letter = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let in_place = data
            .rows
            .iter()
            .find(|row| row.input == lib.path().join("G").join("Game.zip"))
            .expect("in-place unit row");
        assert_eq!(in_place.status, FileStatus::Skipped, "{in_place:?}");
        assert_eq!(in_place.detail.as_deref(), Some("already in place"));
        assert_eq!(
            in_place.output.as_deref(),
            Some(lib.path().join("G").join("Game.zip").as_path())
        );
        let sibling = data
            .rows
            .iter()
            .find(|row| row.input == lib.path().join("A").join("Game.zip"))
            .expect("sibling row");
        assert_eq!(sibling.status, FileStatus::Skipped, "{sibling:?}");
        assert_eq!(
            sibling.detail.as_deref(),
            Some(
                format!(
                    "duplicate output {}",
                    lib.path().join("G").join("Game.zip").display()
                )
                .as_str()
            )
        );
        assert_eq!(sibling.output, None, "the loser never ran");
    }

    /// A skip decided before `resolve_paths` (no desired path) must not
    /// shift the flat→lettered snapshot: zipping the snapshot over the full
    /// plan list keeps the duplicate's group findable, so exactly one
    /// output lands, at the lettered path. A pathless skip plan ordered
    /// before the group keeps its position in the pairing, so the rejoined
    /// duplicate follows its group's lettered path instead of writing a
    /// second output at an un-lettered path.
    #[tokio::test]
    async fn dir_letter_rejoin_ignores_pathless_skip_plans() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        // Sorts before the duplicate group, and skips with no desired path.
        std::fs::write(lib.path().join("0notes.txt"), b"not a rom").unwrap();
        let make_zip = |dir: &str| {
            let zip_path = lib.path().join(dir).join("Game.zip");
            std::fs::create_dir_all(zip_path.parent().unwrap()).unwrap();
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("Game.gba", opts).unwrap();
            zip.write_all(&gba_bytes()).unwrap();
            zip.finish().unwrap();
        };
        make_zip("A");
        make_zip("B");

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.output_template = Some("{basename}.{ext}".to_string());
        req.options.dir_letter = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert_eq!(data.failed, 0, "{:?}", data.rows);
        let outputs: Vec<&Path> = data
            .rows
            .iter()
            .filter(|row| row.status == FileStatus::Ok)
            .filter_map(|row| row.output.as_deref())
            .collect();
        assert_eq!(
            outputs,
            vec![out.path().join("G").join("Game.zip").as_path()],
            "{:?}",
            data.rows
        );
        assert!(!out.path().join("Game.zip").exists());
    }

    /// A rejoined in-place winner's letter dir is managed too: the winner's
    /// managed dirs were captured pre-letter, so the letter dir is added
    /// only after the rejoin. With the input and output trees overlapping,
    /// every file under the input root is protected, so the stale `.m3u`
    /// the scan would have dropped as a playlist sidecar survives clean,
    /// and the zip already sitting at its output path is left untouched.
    #[tokio::test]
    async fn clean_reaches_a_rejoined_winner_s_letter_dir() {
        let lib = TempDir::new().unwrap();
        for dir in ["G", "A"] {
            let zip_path = lib.path().join(dir).join("Game.zip");
            std::fs::create_dir_all(zip_path.parent().unwrap()).unwrap();
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("Game.gba", opts).unwrap();
            zip.write_all(&gba_bytes()).unwrap();
            zip.finish().unwrap();
        }
        let letter_dir = lib.path().join("G");
        std::fs::write(letter_dir.join("Old Game.m3u"), b"stale").unwrap();

        let mut req = organize_request(lib.path(), Some(lib.path()), false);
        req.options.output_template = Some("{basename}.{ext}".to_string());
        req.options.dir_letter = Some(true);
        req.options.clean = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert_eq!(data.failed, 0, "{:?}", data.rows);
        let in_place = data
            .rows
            .iter()
            .find(|row| row.input == letter_dir.join("Game.zip"))
            .expect("in-place unit row");
        assert_eq!(in_place.status, FileStatus::Skipped, "{in_place:?}");
        assert_eq!(in_place.detail.as_deref(), Some("already in place"));
        let sibling = data
            .rows
            .iter()
            .find(|row| row.input == lib.path().join("A").join("Game.zip"))
            .expect("sibling row");
        assert_eq!(sibling.status, FileStatus::Skipped, "{sibling:?}");
        assert!(letter_dir.join("Game.zip").exists());
        // The trees overlap: nothing under the input root is deleted, not
        // even a playlist sidecar the scan dropped, and no clean row names
        // one.
        assert!(letter_dir.join("Old Game.m3u").exists());
        assert!(
            !data
                .rows
                .iter()
                .any(|row| row.action == "clean" && row.status == FileStatus::Ok),
            "{:?}",
            data.rows
        );
    }

    /// A duplicate rejoins its group by the same folded key
    /// `dedupe_by_path` groups by: a unit already sitting at the lowercase
    /// spelling of its lettered path wins a group whose winner spells the
    /// path capitalized, instead of staying skipped while the sibling
    /// rewrites the file under it. Needs a case-insensitive file system.
    #[cfg(any(windows, target_os = "macos"))]
    #[tokio::test]
    async fn dir_letter_rejoin_folds_the_duplicate_s_path_case() {
        let lib = TempDir::new().unwrap();
        if !case_insensitive_volume(lib.path()) {
            return;
        }
        let make_zip = |path: &Path, member: &str, bytes: &[u8]| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file(member, opts).unwrap();
            zip.write_all(bytes).unwrap();
            zip.finish().unwrap();
        };
        // The sibling sorts first and wins the first (flat) dedupe; its
        // payload differs, so a rewrite of the in-place file is detectable.
        let mut sibling_bytes = gba_bytes();
        sibling_bytes[0xA0..0xA8].copy_from_slice(b"OTHERGAM");
        make_zip(
            &lib.path().join("g").join("game.zip"),
            "game.gba",
            &gba_bytes(),
        );
        make_zip(
            &lib.path().join("a").join("Game.zip"),
            "Game.gba",
            &sibling_bytes,
        );

        let mut req = organize_request(lib.path(), Some(lib.path()), false);
        req.options.output_template = Some("{basename}.{ext}".to_string());
        req.options.dir_letter = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert_eq!(data.failed, 0, "{:?}", data.rows);
        let in_place = data
            .rows
            .iter()
            .find(|row| row.input == lib.path().join("g").join("game.zip"))
            .expect("in-place unit row");
        assert_eq!(in_place.status, FileStatus::Skipped, "{in_place:?}");
        assert_eq!(in_place.detail.as_deref(), Some("already in place"));
        assert_eq!(
            in_place.output.as_deref(),
            Some(lib.path().join("G").join("Game.zip").as_path())
        );
        let sibling = data
            .rows
            .iter()
            .find(|row| row.input == lib.path().join("a").join("Game.zip"))
            .expect("sibling row");
        assert_eq!(sibling.status, FileStatus::Skipped, "{sibling:?}");
        assert_eq!(
            sibling.detail.as_deref(),
            Some(
                format!(
                    "duplicate output {}",
                    lib.path().join("G").join("Game.zip").display()
                )
                .as_str()
            )
        );
        assert_eq!(data.ok, 0, "nothing ran: the file was already in place");
        let file = std::fs::File::open(lib.path().join("g").join("game.zip")).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut member = archive.by_name("game.gba").unwrap();
        let mut got = Vec::new();
        std::io::Read::read_to_end(&mut member, &mut got).unwrap();
        assert_eq!(got, gba_bytes(), "the sibling never rewrote the file");
    }

    /// A playlist that fails to write is not stale: the stale set is the
    /// pre-clean plan minus the post-clean PLAN, so a read-only `.m3u`
    /// whose write warns survives with its old contents instead of being
    /// deleted as no-longer-derived.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_playlist_that_fails_to_write_is_not_stale() {
        use std::os::unix::fs::PermissionsExt;

        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 1).rvz"), gcm_bytes()).unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 2).rvz"), gcm_bytes()).unwrap();

        let request = || {
            let mut req = organize_request(lib.path(), Some(out.path()), false);
            req.options.playlists = Some(true);
            req.options.clean = Some(true);
            req.options.on_conflict = Some("overwrite-invalid".to_string());
            req
        };
        organize(request(), &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let m3u = out.path().join("GameCube").join("Grouped Game.m3u");
        assert!(m3u.exists());
        let before = std::fs::read(&m3u).unwrap();

        // A changed disc set forces a rewrite; the read-only .m3u refuses it.
        std::fs::write(lib.path().join("Grouped Game (Disc 3).rvz"), gcm_bytes()).unwrap();
        let mut perms = std::fs::metadata(&m3u).unwrap().permissions();
        perms.set_mode(0o444);
        std::fs::set_permissions(&m3u, perms).unwrap();

        let progress = RecordingProgress::default();
        let response = organize(request(), &progress, CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert_eq!(data.failed, 0, "{:?}", data.rows);
        let warnings: Vec<String> = progress
            .take_events()
            .into_iter()
            .filter_map(|event| match event {
                ProgressEvent::Warn { message } => Some(message),
                _ => None,
            })
            .collect();
        assert!(
            warnings
                .iter()
                .any(|message| message.contains("could not write playlist")),
            "{warnings:?}"
        );
        // The refused file survives untouched: written no, stale no.
        assert_eq!(std::fs::read(&m3u).unwrap(), before);
        assert!(
            !data
                .rows
                .iter()
                .any(|row| row.action == "clean" && row.input == m3u),
            "{:?}",
            data.rows
        );
    }

    /// A row flipped Failed by a `remove_sources` failure keeps its written
    /// output: the zip exists, so clean must not eat it; only a
    /// verify-failed row's output stays unprotected.
    #[cfg(unix)]
    #[tokio::test]
    async fn clean_keeps_the_output_of_a_failed_move() {
        use std::os::unix::fs::PermissionsExt;

        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        // One unit cannot give up its source (read-only directory), one can.
        let stuck = lib.path().join("Stuck");
        std::fs::create_dir_all(&stuck).unwrap();
        std::fs::write(stuck.join("Stuck Game.gba"), gba_bytes()).unwrap();
        std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o555)).unwrap();
        std::fs::write(lib.path().join("Moved Game.gba"), gba_bytes()).unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.move_source = Some(true);
        req.options.clean = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o755)).unwrap();

        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let failed = row_for(&data, "Stuck Game.gba");
        assert_eq!(failed.status, FileStatus::Failed, "{:?}", failed.detail);
        assert!(
            failed
                .detail
                .as_deref()
                .is_some_and(|detail| detail.starts_with("source not removed: ")),
            "{:?}",
            failed.detail
        );
        // The written output survives its failed move: no clean row names it.
        let stuck_zip = out.path().join("Game Boy Advance").join("Stuck Game.zip");
        assert!(
            stuck_zip.exists(),
            "a written output survives a failed move"
        );
        assert!(
            !data
                .rows
                .iter()
                .any(|row| row.action == "clean" && row.input == stuck_zip),
            "{:?}",
            data.rows
        );
        // The succeeded unit's output is untouched.
        assert!(
            out.path()
                .join("Game Boy Advance")
                .join("Moved Game.zip")
                .exists()
        );
    }

    /// Only a verify-failed row's paths stay unprotected from clean: any
    /// other failed row keeps its desired path (an untouched previous output
    /// survives a transient failure) and its output (a written output whose
    /// source removal failed survives).
    #[test]
    fn clean_keep_set_spares_only_verify_failed_rows() {
        let plan = |desired: &str| UnitPlan {
            index: 0,
            source: PathBuf::from("src"),
            source_ext: String::new(),
            action: Action::Zip,
            tokens: crate::util::TemplateTokens::new(None, Path::new("src"), "zip"),
            input_subdir: PathBuf::new(),
            game: None,
            header: None,
            strip: 0,
            trim: None,
            pad: 0,
            pad_fill: 0,
            patch: None,
            desired: Some(PathBuf::from(desired)),
            decision: Decision::Keep,
            staging_error: None,
            output_error: None,
            match_error: None,
            member: None,
        };
        let row = |output: &str, verify: Option<VerifyVerdict>, detail: Option<&str>| OrganizeRow {
            input: PathBuf::from("src"),
            output: Some(PathBuf::from(output)),
            console: None,
            action: "zip".to_string(),
            status: FileStatus::Failed,
            planned: false,
            verify,
            detail: detail.map(str::to_string),
            input_bytes: 0,
            output_bytes: 0,
            elapsed_ms: 0,
        };
        // A failed row whose source removal failed keeps its output.
        let plans = vec![plan("/out/Game.zip")];
        let keep = clean_keep_set(&plans, &[row("/out/Game.zip", None, None)]);
        assert!(keep.contains(Path::new("/out/Game.zip")));
        // A verify-failed row whose output sits at the desired path sweeps
        // the path as stale.
        assert!(
            clean_keep_set(
                &plans,
                &[row("/out/Game.zip", Some(VerifyVerdict::Failed), None)]
            )
            .is_empty()
        );
        // Under `on_conflict rename` the invalid write went to a slot and
        // the untouched file at the desired path survives: only the output
        // drops from the keep set.
        let keep = clean_keep_set(
            &plans,
            &[row("/out/Game (1).zip", Some(VerifyVerdict::Failed), None)],
        );
        assert!(keep.contains(Path::new("/out/Game.zip")));
        assert!(!keep.contains(Path::new("/out/Game (1).zip")));
        // The verdict is structural, not textual: a failed row whose detail
        // reads like a verify failure but carries no failed verdict keeps
        // its protection...
        let keep = clean_keep_set(
            &plans,
            &[row(
                "/out/Game.zip",
                None,
                Some("verify failed: crc mismatch"),
            )],
        );
        assert!(keep.contains(Path::new("/out/Game.zip")));
        // ...and a verify that could not run is a transient error, not a
        // bad output: the path keeps its clean protection.
        let keep = clean_keep_set(
            &plans,
            &[row("/out/Game.zip", Some(VerifyVerdict::Unverified), None)],
        );
        assert!(keep.contains(Path::new("/out/Game.zip")));
    }

    /// With `--dir-letter --dir-letter-limit 2`, dedupe_by_path runs before
    /// the letter pass: a same-path duplicate (`Ga` twice) never inflates the
    /// letter bucket, so the chunk dirs are computed over the two survivors
    /// (one plain `G` bucket) instead of G1=[Ga, Ga], G2=[Gb].
    #[tokio::test]
    async fn dir_letter_chunks_over_the_dedupe_survivors() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Ga.gba"), gba_bytes()).unwrap();
        std::fs::write(lib.path().join("Gb.gba"), gba_bytes()).unwrap();
        std::fs::create_dir_all(lib.path().join("dup")).unwrap();
        std::fs::write(lib.path().join("dup").join("Ga.gba"), gba_bytes()).unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.dir_letter = Some(true);
        req.options.dir_letter_limit = Some(2);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let gba_dir = out.path().join("Game Boy Advance");
        // Exactly one Ga.zip, and both survivors chunk into plain `G`: the
        // duplicate never inflated the bucket.
        assert_eq!(
            data.rows
                .iter()
                .filter(|row| row.status == FileStatus::Ok
                    && row
                        .output
                        .as_ref()
                        .is_some_and(|out| out.file_name() == Some(std::ffi::OsStr::new("Ga.zip"))))
                .count(),
            1,
            "{:?}",
            data.rows
        );
        assert!(gba_dir.join("G").join("Ga.zip").exists());
        assert!(gba_dir.join("G").join("Gb.zip").exists());
        assert!(!gba_dir.join("G1").exists(), "{:?}", data.rows);
        assert!(!gba_dir.join("G2").exists());
        let loser = data
            .rows
            .iter()
            .find(|row| {
                row.detail
                    .as_deref()
                    .is_some_and(|d| d.starts_with("duplicate output"))
            })
            .expect("the duplicate is skipped");
        assert_eq!(loser.status, FileStatus::Skipped, "{loser:?}");
        assert_eq!(loser.output, None, "the loser never ran");
    }

    /// A stale disc set's old `.m3u` is deleted in the same run that cleans
    /// the stale discs: the pre-clean plan derived it, the post-clean plan no
    /// longer does, so the derived file goes too, as a clean row.
    #[tokio::test]
    async fn clean_deletes_a_stale_playlist_no_longer_derived() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 1).rvz"), gcm_bytes()).unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 2).rvz"), gcm_bytes()).unwrap();
        let disc_dir = out.path().join("GameCube");
        std::fs::create_dir_all(&disc_dir).unwrap();
        // A stale set from an earlier run, plus its old playlist.
        std::fs::write(disc_dir.join("Old Game (Disc 1).rvz"), gcm_bytes()).unwrap();
        std::fs::write(disc_dir.join("Old Game (Disc 2).rvz"), gcm_bytes()).unwrap();
        std::fs::write(disc_dir.join("Old Game.m3u"), b"Old Game (Disc 1).rvz\n").unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.playlists = Some(true);
        req.options.clean = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert_eq!(data.failed, 0, "{:?}", data.rows);
        // The stale discs went.
        assert!(!disc_dir.join("Old Game (Disc 1).rvz").exists());
        assert!(!disc_dir.join("Old Game (Disc 2).rvz").exists());
        // Their old .m3u went in the same run: a clean row names it deleted.
        let m3u_row = data
            .rows
            .iter()
            .find(|row| row.action == "clean" && row.input == disc_dir.join("Old Game.m3u"))
            .expect("the stale playlist is cleaned");
        assert_eq!(m3u_row.detail.as_deref(), Some("deleted"));
        assert!(!disc_dir.join("Old Game.m3u").exists());
        // The fresh set's playlist was still written from the cleaned dir.
        let contents = std::fs::read_to_string(disc_dir.join("Grouped Game.m3u")).unwrap();
        assert!(contents.contains("Grouped Game (Disc 1).rvz"), "{contents}");
        assert!(!contents.contains("Old Game"), "{contents}");
    }

    /// A dry run deletes nothing, yet must still preview the stale
    /// playlist: clean planned to remove the stale discs, so the post-clean
    /// plan ignores them and the derived `.m3u` shows up as a
    /// `would delete` clean row.
    #[tokio::test]
    async fn dry_run_previews_a_stale_playlist_as_would_delete() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 1).rvz"), gcm_bytes()).unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 2).rvz"), gcm_bytes()).unwrap();
        let disc_dir = out.path().join("GameCube");
        std::fs::create_dir_all(&disc_dir).unwrap();
        // A stale set from an earlier run, plus its old playlist.
        std::fs::write(disc_dir.join("Old Game (Disc 1).rvz"), gcm_bytes()).unwrap();
        std::fs::write(disc_dir.join("Old Game (Disc 2).rvz"), gcm_bytes()).unwrap();
        std::fs::write(disc_dir.join("Old Game.m3u"), b"Old Game (Disc 1).rvz\n").unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), true);
        req.options.playlists = Some(true);
        req.options.clean = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert_eq!(data.failed, 0, "{:?}", data.rows);
        // The dry run planned the stale playlist's deletion without
        // touching anything.
        let m3u_row = data
            .rows
            .iter()
            .find(|row| row.action == "clean" && row.input == disc_dir.join("Old Game.m3u"))
            .unwrap_or_else(|| {
                panic!(
                    "the stale playlist is planned for deletion: {:#?}",
                    data.rows
                )
            });
        assert_eq!(m3u_row.detail.as_deref(), Some("would delete"));
        assert!(m3u_row.planned);
        assert!(disc_dir.join("Old Game.m3u").exists());
        assert!(disc_dir.join("Old Game (Disc 1).rvz").exists());
    }

    /// A dir whose playlist plan failed is never stale: nothing could be
    /// planned there, so its playlists are unverifiable, not derived from
    /// removed discs, and never deleted for it.
    #[test]
    fn a_dir_with_a_failed_playlist_plan_is_never_stale() {
        let planned = |path: &str| PlaylistPlan {
            base_title: "Game".to_string(),
            m3u_path: PathBuf::from(path),
            contents: String::new(),
            disc_count: 2,
            has_duplicate_numbers: false,
        };
        let plans = vec![planned("/out/Sys/Game.m3u"), planned("/out/Sys/Other.m3u")];
        let post_clean = BTreeSet::from([PathBuf::from("/out/Sys/Other.m3u")]);
        assert_eq!(
            stale_playlist_paths(&plans, &post_clean, &BTreeSet::new()),
            vec![PathBuf::from("/out/Sys/Game.m3u")]
        );
        // The whole dir failed to plan post-clean: nothing under it goes.
        let failed = BTreeSet::from([PathBuf::from("/out/Sys")]);
        assert!(stale_playlist_paths(&plans, &post_clean, &failed).is_empty());
    }

    /// A dir whose scan fails reports itself as failed, so the stale pass
    /// can exempt it: no plans, and the dir in the failed set.
    #[test]
    fn plan_disc_playlists_reports_failed_dirs() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("Missing");
        let mut warned = BTreeSet::new();
        let (plans, failed) = plan_disc_playlists(
            &BTreeSet::from([missing.clone()]),
            false,
            &mut warned,
            &BTreeSet::new(),
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .unwrap();
        assert!(plans.is_empty());
        assert_eq!(failed, BTreeSet::from([missing]));
    }

    /// The comparison fold under `verify_after` splits every verdict: a
    /// passing report marks the row verified, one that ran and failed fails
    /// the row (the output is invalid, swept as stale), one that could not
    /// run fails it as unverified (the detail keeps the output's clean
    /// protection), and an operation that produced no comparison verdict at
    /// all fails as unverified, so the source is never released. Without
    /// `verify_after` the reports are not surfaced at all.
    #[test]
    fn fold_conversion_outcome_splits_verify_verdicts() {
        let comparison = |verdict: VerifyVerdict, ok: bool, message: &str| {
            Some(RunData::Comparison(RunComparisonData {
                comparison: ComparisonData {
                    input_bytes: 10,
                    output_bytes: 4,
                    ratio_pct: None,
                    input_format: "iso".to_string(),
                    output_format: "rvz".to_string(),
                    output_sha1: None,
                    verify: Some(VerifyReport {
                        ok,
                        round_trip: ok,
                        verdict,
                        message: message.to_string(),
                    }),
                },
            }))
        };

        // A verify-failed row reports the output file's length on disk:
        // the output was written, it just failed the check, like a place
        // row does.
        let dir = TempDir::new().unwrap();
        let out_path = dir.path().join("game.rvz");
        std::fs::write(&out_path, b"0123456789").unwrap();
        let failed = ReportRecord::new(ReportRecordInput {
            input_path: "game.iso".to_string(),
            output_path: out_path.display().to_string(),
            operation: "dol.compress".to_string(),
            status: FileStatus::Ok,
            input_bytes: 10,
            output_bytes: 10,
            elapsed_ms: 1,
            error: None,
        });
        let (status, detail, output, bytes, verify) = fold_conversion_outcome(
            comparison(VerifyVerdict::Failed, false, "sha1 mismatch"),
            vec![failed],
            true,
        );
        assert_eq!(status, FileStatus::Failed);
        assert_eq!(detail.as_deref(), Some("verify failed: sha1 mismatch"));
        assert_eq!(output.as_deref(), Some(out_path.as_path()));
        assert_eq!(bytes, 10);
        assert_eq!(verify, Some(VerifyVerdict::Failed));

        // A ran-and-passed report marks the row verified.
        let passing = ReportRecord::new(ReportRecordInput {
            input_path: "game.iso".to_string(),
            output_path: "out/GameCube/game.rvz".to_string(),
            operation: "dol.compress".to_string(),
            status: FileStatus::Ok,
            input_bytes: 10,
            output_bytes: 4,
            elapsed_ms: 1,
            error: None,
        });
        let (status, detail, _, _, verify) = fold_conversion_outcome(
            comparison(VerifyVerdict::Verified, true, "Verified"),
            vec![passing],
            true,
        );
        assert_eq!(status, FileStatus::Ok);
        assert_eq!(detail, None);
        assert_eq!(verify, Some(VerifyVerdict::Verified));

        // A check that could not run still fails the row, and the detail
        // keeps the output's clean protection.
        let (status, detail, _, _, verify) = fold_conversion_outcome(
            comparison(
                VerifyVerdict::Unverified,
                false,
                "Could not verify: permission denied",
            ),
            Vec::new(),
            true,
        );
        assert_eq!(status, FileStatus::Failed, "the row still fails");
        assert_eq!(detail.as_deref(), Some("verify error: permission denied"));
        assert_eq!(verify, Some(VerifyVerdict::Unverified));

        // An operation with no integrity check for its output (the
        // comparison carries no report) folds Ok with no verdict on its
        // own; under `verify_after` the row fails as unverified so the
        // source is kept.
        let no_report = Some(RunData::Comparison(RunComparisonData {
            comparison: ComparisonData {
                input_bytes: 10,
                output_bytes: 4,
                ratio_pct: None,
                input_format: "iso".to_string(),
                output_format: "zar".to_string(),
                output_sha1: None,
                verify: None,
            },
        }));
        let (status, detail, _, _, verify) = fold_conversion_outcome(no_report, Vec::new(), true);
        assert_eq!(status, FileStatus::Failed);
        assert_eq!(
            detail.as_deref(),
            Some("verify error: the operation produced no comparison verdict")
        );
        assert_eq!(verify, Some(VerifyVerdict::Unverified));
        // Data of some other shape fails the same way.
        let (status, _, _, _, verify) = fold_conversion_outcome(
            Some(RunData::Organize(
                crate::runner::models::OrganizeData::default(),
            )),
            Vec::new(),
            true,
        );
        assert_eq!(status, FileStatus::Failed);
        assert_eq!(verify, Some(VerifyVerdict::Unverified));

        // Without `verify_after` a failed report is not surfaced: the child
        // only produced one because it was asked to verify.
        let (status, detail, _, _, verify) = fold_conversion_outcome(
            comparison(VerifyVerdict::Failed, false, "sha1 mismatch"),
            Vec::new(),
            false,
        );
        assert_eq!(status, FileStatus::Ok);
        assert_eq!(detail, None);
        assert_eq!(verify, None);
    }

    /// Under a letter layout the rejoined dedupe loser keeps its input
    /// position: rows follow the scan order, instead of the loser jumping
    /// to the end of the run.
    #[tokio::test]
    async fn dir_letter_rows_follow_the_input_order() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let make = |dir: &str, name: &str| {
            let path = lib.path().join(dir).join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, gba_bytes()).unwrap();
        };
        // `B/Grouped Game.gba` resolves to the same output as `A`'s: a
        // dedupe loser that rejoins its group's lettered path.
        make("A", "Grouped Game.gba");
        make("A", "Other.gba");
        make("B", "Grouped Game.gba");
        make("C", "Zed.gba");

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.dir_letter = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert_eq!(data.failed, 0, "{:?}", data.rows);
        let order: Vec<&Path> = data.rows.iter().map(|row| row.input.as_path()).collect();
        assert_eq!(
            order,
            vec![
                lib.path().join("A").join("Grouped Game.gba").as_path(),
                lib.path().join("A").join("Other.gba").as_path(),
                lib.path().join("B").join("Grouped Game.gba").as_path(),
                lib.path().join("C").join("Zed.gba").as_path(),
            ],
            "{:?}",
            data.rows
        );
        let loser = data
            .rows
            .iter()
            .find(|row| row.input == lib.path().join("B").join("Grouped Game.gba"))
            .expect("loser row");
        assert_eq!(loser.status, FileStatus::Skipped, "{loser:?}");
        assert!(
            loser
                .detail
                .as_deref()
                .is_some_and(|d| d.starts_with("duplicate output"))
        );
        // The winner still landed at its lettered path.
        assert!(
            out.path()
                .join("Game Boy Advance")
                .join("G")
                .join("Grouped Game.zip")
                .exists(),
            "{:?}",
            data.rows
        );
    }

    /// An `.m3u` that differs and is a symlink is never written through: the
    /// row skips as `playlist is a symlink` and the link's target is
    /// untouched.
    #[cfg(unix)]
    #[tokio::test]
    async fn write_playlists_refuses_to_write_through_a_symlink() {
        use std::os::unix::fs::symlink;

        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 1).rvz"), gcm_bytes()).unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 2).rvz"), gcm_bytes()).unwrap();
        let disc_dir = out.path().join("GameCube");
        std::fs::create_dir_all(&disc_dir).unwrap();
        let target = disc_dir.join("user-data.m3u");
        std::fs::write(&target, b"user curated").unwrap();
        symlink(&target, disc_dir.join("Grouped Game.m3u")).unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.playlists = Some(true);
        let progress = RecordingProgress::default();
        let response = organize(req, &progress, CancelToken::new()).await.unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = data
            .rows
            .iter()
            .find(|row| row.action == "playlist")
            .expect("playlist row");
        assert_eq!(row.status, FileStatus::Skipped, "{row:?}");
        assert_eq!(row.detail.as_deref(), Some("playlist is a symlink"));
        assert_eq!(
            row.output.as_deref(),
            Some(disc_dir.join("Grouped Game.m3u").as_path())
        );
        // The link and its target are untouched.
        assert_eq!(std::fs::read(&target).unwrap(), b"user curated".as_slice());
        let warnings: Vec<String> = progress
            .take_events()
            .into_iter()
            .filter_map(|event| match event {
                ProgressEvent::Warn { message } => Some(message),
                _ => None,
            })
            .collect();
        assert!(
            warnings
                .iter()
                .any(|message| message.contains("playlist is a symlink")),
            "{warnings:?}"
        );
    }

    /// The symlink refusal also fires in a dry run: a plan must not report a
    /// rewrite it could never perform through a link, and the link's target
    /// stays untouched.
    #[cfg(unix)]
    #[tokio::test]
    async fn dry_run_refuses_a_symlinked_playlist() {
        use std::os::unix::fs::symlink;

        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 1).rvz"), gcm_bytes()).unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 2).rvz"), gcm_bytes()).unwrap();

        // A real run first: the dry run plans from the on-disk disc set.
        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.playlists = Some(true);
        organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let disc_dir = out.path().join("GameCube");
        let target = disc_dir.join("user-data.m3u");
        std::fs::write(&target, b"user curated").unwrap();
        std::fs::remove_file(disc_dir.join("Grouped Game.m3u")).unwrap();
        symlink(&target, disc_dir.join("Grouped Game.m3u")).unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), true);
        req.options.playlists = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = data
            .rows
            .iter()
            .find(|row| row.action == "playlist")
            .expect("playlist row");
        assert_eq!(row.status, FileStatus::Skipped, "{row:?}");
        assert_eq!(row.detail.as_deref(), Some("playlist is a symlink"));
        assert!(row.planned, "{row:?}");
        assert_eq!(
            row.output.as_deref(),
            Some(disc_dir.join("Grouped Game.m3u").as_path())
        );
        // The link's target is untouched: the dry run wrote nothing through it.
        assert_eq!(std::fs::read(&target).unwrap(), b"user curated".as_slice());
    }

    /// A read failure on the existing `.m3u` warns with the read verb, not
    /// the write verb, and stays out of the result without aborting.
    #[tokio::test]
    async fn unreadable_existing_playlist_warns_the_read_verb() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 1).rvz"), gcm_bytes()).unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 2).rvz"), gcm_bytes()).unwrap();
        let disc_dir = out.path().join("GameCube");
        // A directory at the `.m3u` path cannot be read as a playlist.
        std::fs::create_dir_all(disc_dir.join("Grouped Game.m3u")).unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.playlists = Some(true);
        let progress = RecordingProgress::default();
        let response = organize(req, &progress, CancelToken::new()).await.unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert!(
            data.rows.iter().all(|row| row.action != "playlist"),
            "{:?}",
            data.rows
        );
        let warnings: Vec<String> = progress
            .take_events()
            .into_iter()
            .filter_map(|event| match event {
                ProgressEvent::Warn { message } => Some(message),
                _ => None,
            })
            .collect();
        assert!(
            warnings
                .iter()
                .any(|message| message.contains("could not read existing playlist")),
            "{warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .all(|message| !message.contains("could not write playlist")),
            "{warnings:?}"
        );
        // The directory survives: clean only ever removes files.
        assert!(disc_dir.join("Grouped Game.m3u").is_dir());
    }

    /// A dry run plans playlists for dirs that may not exist yet (nothing
    /// was written): that is silent, not a warning.
    #[tokio::test]
    async fn dry_run_playlists_skip_missing_disc_dirs_silently() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 1).rvz"), gcm_bytes()).unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 2).rvz"), gcm_bytes()).unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), true);
        req.options.playlists = Some(true);
        let progress = RecordingProgress::default();
        let response = organize(req, &progress, CancelToken::new()).await.unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert!(data.playlists.is_empty());
        assert!(
            !out.path().join("GameCube").exists(),
            "a dry run creates nothing"
        );
        let warnings: Vec<String> = progress
            .take_events()
            .into_iter()
            .filter_map(|event| match event {
                ProgressEvent::Warn { message } => Some(message),
                _ => None,
            })
            .collect();
        assert!(
            warnings
                .iter()
                .all(|message| !message.contains("could not plan playlists")),
            "{warnings:?}"
        );
    }

    /// A link plan whose payload would change (a header strip or trim pad)
    /// places as a copy and says so, like staged and patched sources.
    #[cfg(unix)]
    #[tokio::test]
    async fn link_plan_with_header_strip_places_a_copy() {
        use std::os::unix::fs::MetadataExt;

        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let mut rom = vec![0u8; 512];
        rom.extend_from_slice(&gba_bytes());
        let source = lib.path().join("Copied Game.smc");
        std::fs::write(&source, &rom).unwrap();

        let plan = UnitPlan {
            index: 0,
            source: source.clone(),
            source_ext: "smc".to_string(),
            action: Action::Link,
            tokens: crate::util::TemplateTokens::new(None, &source, "sfc"),
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
            pad_fill: 0,
            patch: None,
            desired: Some(out.path().join("Copied Game.sfc")),
            decision: Decision::Keep,
            staging_error: None,
            output_error: None,
            match_error: None,
            member: None,
        };
        let unit = DatUnit::File(source.clone());
        let progress = RecordingProgress::default();
        let row = execute_unit(
            &organize_request(lib.path(), Some(out.path()), false),
            &plan,
            None,
            &unit,
            &unit_source_files(&unit),
            ZipFormat::TorrentZip,
            Some(LinkMode::Hard),
            &WriteGuards::never(out.path().to_path_buf()),
            None,
            &progress,
            &progress,
            &CancelToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(row.status, FileStatus::Ok, "{:?}", row.detail);
        assert_eq!(row.action, "copy");
        assert_eq!(
            row.detail.as_deref(),
            Some("copied: link cannot carry a stripped/padded payload; header stripped: SMC")
        );
        let output = row.output.unwrap();
        assert_ne!(
            std::fs::metadata(&source).unwrap().ino(),
            std::fs::metadata(&output).unwrap().ino(),
            "the demoted link must not share the source's inode"
        );
        assert_eq!(std::fs::read(&output).unwrap(), gba_bytes());
    }

    /// `move_source` under `overwrite-invalid` deletes the input when the
    /// existing output verified valid; a plain skip-policy "output exists"
    /// keeps it.
    #[tokio::test]
    async fn move_source_deletes_only_when_the_output_verified_valid() {
        let kept_lib = TempDir::new().unwrap();
        let kept_out = TempDir::new().unwrap();
        std::fs::write(kept_lib.path().join("Test Game.gba"), gba_bytes()).unwrap();
        let target_dir = kept_out.path().join("Game Boy Advance");
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(target_dir.join("Test Game.zip"), b"stale").unwrap();

        let mut req = organize_request(kept_lib.path(), Some(kept_out.path()), false);
        req.options.move_source = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Test Game.gba");
        assert_eq!(row.detail.as_deref(), Some("output exists"));
        assert!(
            kept_lib.path().join("Test Game.gba").exists(),
            "a plain skip-policy skip keeps the input"
        );

        // The valid-output case: a first run writes the zip, the re-run
        // verifies it and then deletes the input.
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Test Game.gba"), gba_bytes()).unwrap();
        organize(
            organize_request(lib.path(), Some(out.path()), false),
            &RecordingProgress::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.move_source = Some(true);
        req.options.on_conflict = Some("overwrite-invalid".to_string());
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Test Game.gba");
        assert_eq!(row.status, FileStatus::Skipped, "{:?}", row.detail);
        assert_eq!(row.detail.as_deref(), Some(VERIFIED_VALID));
        assert!(
            !lib.path().join("Test Game.gba").exists(),
            "the input is deleted once the output verified valid"
        );
        assert!(row.output.clone().unwrap().exists());
    }

    /// A conversion whose existing output verified valid under
    /// `overwrite-invalid` carries the marker on its row, but keeps its
    /// source under `move_source`: the conversion's check only proves the
    /// container is self-consistent, not that this source produced it.
    #[tokio::test]
    async fn conversion_kept_valid_row_carries_the_verified_valid_marker() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Cube Game.iso"), gcm_bytes()).unwrap();
        // The first run writes the .rvz; the re-run verifies it.
        organize(
            organize_request(lib.path(), Some(out.path()), false),
            &RecordingProgress::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.move_source = Some(true);
        req.options.on_conflict = Some("overwrite-invalid".to_string());
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Cube Game.iso");
        assert_eq!(row.status, FileStatus::Skipped, "{:?}", row.detail);
        assert_eq!(row.detail.as_deref(), Some(VERIFIED_VALID));
        assert_eq!(row.action, "dol.compress");
        assert!(row.output.is_some());
        assert!(
            lib.path().join("Cube Game.iso").exists(),
            "a kept-valid conversion never releases its source"
        );
    }

    /// A conversion whose op carries `OutputVerify::None` (no integrity
    /// check, e.g. `ctr.compress`) never claims an existing output verified
    /// valid under `overwrite-invalid`: it is kept because nobody could
    /// check, not because it passed, so the row never carries the
    /// `VERIFIED_VALID` marker and the stale output is left untouched.
    #[tokio::test]
    async fn overwrite_invalid_never_claims_an_unchecked_output_valid() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Game.cia"), b"not a real cia").unwrap();
        // ctr.compress carries `OutputVerify::None`: stage a stale output at
        // the exact path organize would target, without a first real run
        // (which would fail on this garbage content before writing
        // anything).
        let target_dir = out.path().join("3DS");
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(target_dir.join("Game.zcia"), b"stale").unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.on_conflict = Some("overwrite-invalid".to_string());
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Game.cia");
        assert_eq!(row.status, FileStatus::Skipped, "{:?}", row.detail);
        assert_ne!(row.detail.as_deref(), Some(VERIFIED_VALID));
        assert_eq!(
            std::fs::read(target_dir.join("Game.zcia")).unwrap(),
            b"stale",
            "an unchecked output is kept, not rewritten"
        );
    }

    /// A source that cannot be removed fails its unit's rows: the detail
    /// names the removal error, the copy keeps its action name, and the run
    /// reports a failure.
    #[cfg(unix)]
    #[tokio::test]
    async fn move_source_failure_fails_the_rows() {
        use std::os::unix::fs::PermissionsExt;

        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Test Game.gba"), gba_bytes()).unwrap();
        // A read-only library directory cannot give up its source file.
        std::fs::set_permissions(lib.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.move_source = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        std::fs::set_permissions(lib.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Test Game.gba");
        assert_eq!(row.status, FileStatus::Failed, "{:?}", row.detail);
        assert_eq!(row.action, "zip", "a failed move is never renamed");
        assert!(
            row.detail
                .as_deref()
                .is_some_and(|detail| detail.starts_with("source not removed: ")),
            "{:?}",
            row.detail
        );
        assert!(lib.path().join("Test Game.gba").exists());
        assert!(!response.ok, "a failed removal must fail the run");
    }

    /// A `zip_exclude` demotion places a copy, not a zip: `move_source`
    /// still renames a successful copy's action to "move" once its source
    /// is gone, and a removal failure leaves it "copy" and fails the row;
    /// the same rename/keep behavior a zip action gets.
    #[cfg(unix)]
    #[tokio::test]
    async fn zip_exclude_copy_renames_to_move_or_fails_the_row() {
        use std::os::unix::fs::PermissionsExt;

        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Test Game.gba"), gba_bytes()).unwrap();
        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.zip_exclude = Some("**/*.zip".to_string());
        req.options.move_source = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Test Game.gba");
        assert_eq!(row.status, FileStatus::Ok, "{:?}", row.detail);
        assert_eq!(row.action, "move");
        assert!(!lib.path().join("Test Game.gba").exists());

        // A source that cannot be removed keeps the "copy" name and fails.
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Test Game.gba"), gba_bytes()).unwrap();
        std::fs::set_permissions(lib.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.zip_exclude = Some("**/*.zip".to_string());
        req.options.move_source = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        std::fs::set_permissions(lib.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Test Game.gba");
        assert_eq!(row.status, FileStatus::Failed, "{:?}", row.detail);
        assert_eq!(row.action, "copy", "a failed move is never renamed");
        assert!(lib.path().join("Test Game.gba").exists());
    }

    /// A multi-plan unit's rows are streamed exactly once each, only after
    /// the unit settles, and always with the final action: `move_source`
    /// renames the buffered copy rows to "move" before they are emitted, so
    /// the stream never carries a stale action or a duplicate row.
    #[tokio::test]
    async fn move_source_restreams_the_renamed_rows_of_a_multi_plan_unit() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let patches = TempDir::new().unwrap();
        let bytes = gba_bytes();
        std::fs::write(lib.path().join("Kept Game (USA).gba"), &bytes).unwrap();
        let crc = ips_for(&patches, "Hack", &bytes);

        let recorder = RowRecorder::default();
        let mut req = organize_request(lib.path(), Some(out.path()), false);
        // `zip_exclude` demotes both the base and the patched plan to
        // copies, so the rename to "move" applies to every row of the unit.
        req.options.zip_exclude = Some("**/*.zip".to_string());
        req.options.move_source = Some(true);
        req.options.patch = Some(vec![patches.path().to_path_buf()]);
        let response = organize(req, &recorder, CancelToken::new()).await.unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };

        // Both rows carry the final "move" action and the source is gone.
        let final_actions: Vec<&str> = data.rows.iter().map(|r| r.action.as_str()).collect();
        assert_eq!(final_actions, ["move", "move"], "{:?}", data.rows);
        assert!(!lib.path().join("Kept Game (USA).gba").exists());

        // Each (input, output) is streamed exactly once, with the final
        // action: no copy-named previews, no re-emitted rows.
        let streamed = recorder.rows();
        assert_eq!(streamed.len(), data.rows.len(), "{streamed:?}");
        let mut streamed_actions: std::collections::HashMap<(PathBuf, Option<PathBuf>), Vec<&str>> =
            std::collections::HashMap::new();
        for row in &streamed {
            streamed_actions
                .entry((row.input.clone(), row.output.clone()))
                .or_default()
                .push(row.action.as_str());
        }
        assert_eq!(streamed_actions.len(), data.rows.len(), "{streamed:?}");
        for final_row in &data.rows {
            let actions = &streamed_actions[&(final_row.input.clone(), final_row.output.clone())];
            assert_eq!(
                actions.as_slice(),
                [final_row.action.as_str()],
                "{actions:?}"
            );
        }
        assert!(streamed.iter().all(|row| row.status == FileStatus::Ok));
        let patched = format!("Hack [{crc:08X}].gba");
        assert!(
            streamed.iter().any(|row| row
                .output
                .as_ref()
                .is_some_and(|output| output.file_name().and_then(|name| name.to_str())
                    == Some(patched.as_str()))),
            "{streamed:?}"
        );
    }

    /// Clean keeps a placed symlink: the keep set records the link's own
    /// identity instead of collapsing it into the file it points at.
    #[cfg(unix)]
    #[tokio::test]
    async fn clean_keeps_a_placed_symlink() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Game.xiso"), b"not a real xiso").unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.link_mode = Some("symlink".to_string());
        req.options.clean = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert_eq!(data.failed, 0, "{:?}", data.rows);
        let row = row_for(&data, "Game.xiso");
        assert_eq!(row.status, FileStatus::Ok, "{:?}", row.detail);
        let output = row.output.clone().unwrap();
        let meta = std::fs::symlink_metadata(&output).unwrap();
        assert!(meta.file_type().is_symlink(), "the placement is a link");
        assert!(output.exists(), "clean must not delete the placed symlink");
    }

    /// `remove_sources` deletes the cue and every bin, never a file that is
    /// also one of the unit's outputs, and reports what it removed.
    #[tokio::test]
    async fn remove_sources_removes_cue_and_bins_but_not_outputs() {
        let dir = TempDir::new().unwrap();
        let cue = dir.path().join("Game.cue");
        let bin = dir.path().join("Game.bin");
        let twin = dir.path().join("Twin.bin");
        std::fs::write(&cue, b"cue").unwrap();
        std::fs::write(&bin, b"bin").unwrap();
        std::fs::write(&twin, b"twin").unwrap();

        let unit = DatUnit::CueSet {
            cue: cue.clone(),
            bins: vec![bin.clone(), twin.clone()],
        };
        let mut claims = HashMap::new();
        let outputs = [twin.clone()];
        let (removed, failures, kept) = remove_sources(
            &unit,
            &unit_source_files(&unit),
            &outputs,
            &std::fs::canonicalize(dir.path()).unwrap(),
            &mut claims,
            false,
        )
        .await;

        assert!(kept.is_empty(), "{kept:?}");

        assert!(failures.is_empty(), "{failures:?}");
        // The bins go before the cue.
        assert_eq!(removed, vec![bin.clone(), cue.clone()]);
        assert!(!cue.exists());
        assert!(!bin.exists());
        assert!(twin.exists(), "an output is never removed");
    }

    /// A failed bin removal keeps all later bins and the cue for recovery.
    /// Bins successfully removed before the failure stay in `removed`.
    #[tokio::test]
    async fn remove_sources_reports_files_it_could_not_remove() {
        let dir = TempDir::new().unwrap();
        let cue = dir.path().join("Game.cue");
        let bin = dir.path().join("Game.bin");
        let stuck = dir.path().join("Stuck.bin");
        let remaining = dir.path().join("Remaining.bin");
        std::fs::write(&cue, b"cue").unwrap();
        std::fs::write(&bin, b"bin").unwrap();
        std::fs::create_dir(&stuck).unwrap();
        std::fs::write(&remaining, b"remaining").unwrap();

        let unit = DatUnit::CueSet {
            cue: cue.clone(),
            bins: vec![bin.clone(), stuck.clone(), remaining.clone()],
        };
        let mut claims = HashMap::new();
        let (removed, failures, kept) = remove_sources(
            &unit,
            &unit_source_files(&unit),
            &[],
            &std::fs::canonicalize(dir.path()).unwrap(),
            &mut claims,
            false,
        )
        .await;

        assert!(kept.is_empty(), "{kept:?}");
        // The failed bin stops the loop; later bins and the cue stay.
        assert_eq!(removed, vec![bin.clone()]);
        assert_eq!(std::fs::read(remaining).unwrap(), b"remaining");
        assert!(stuck.is_dir(), "the failed source is restored");
        assert!(
            cue.exists(),
            "the cue stays when any bin could not be removed"
        );
        assert_eq!(failures.len(), 2);
        assert_eq!(failures[0].0, stuck);
        assert_eq!(failures[1].0, cue);
        assert_eq!(
            failures[1].1.to_string(),
            format!(
                "not removed: a bin could not be removed (already removed: {})",
                bin.display()
            )
        );
    }
    /// A hardlinked twin is a different directory entry, so removing one
    /// source name leaves the twin intact.
    #[tokio::test]
    async fn remove_sources_removes_only_the_source_name_of_a_hardlink() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("input/Game.gba");
        let twin = dir.path().join("twin/Game.gba");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::create_dir_all(twin.parent().unwrap()).unwrap();
        std::fs::write(&source, b"source").unwrap();
        std::fs::hard_link(&source, &twin).unwrap();
        assert!(!same_location(&source, &twin));

        let unit = DatUnit::File(source.clone());
        let mut claims = HashMap::new();
        let (removed, failures, kept) = remove_sources(
            &unit,
            &unit_source_files(&unit),
            std::slice::from_ref(&twin),
            &std::fs::canonicalize(source.parent().unwrap()).unwrap(),
            &mut claims,
            false,
        )
        .await;

        assert_eq!(removed, vec![source.clone()]);
        assert!(failures.is_empty(), "{failures:?}");
        assert!(kept.is_empty(), "{kept:?}");
        assert!(!source.exists());
        assert_eq!(std::fs::read(twin).unwrap(), b"source");
    }

    /// The rename probe keeps a source even when another hardlink hides
    /// the output symlink's dependency on the original entry.
    #[cfg(unix)]
    #[tokio::test]
    async fn remove_sources_keeps_a_source_sharing_identity_with_an_output() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("input/Game.gba");
        let output = dir.path().join("output/Game.zip");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::create_dir_all(output.parent().unwrap()).unwrap();
        std::fs::write(&source, b"source").unwrap();
        let twin = dir.path().join("twin.gba");
        std::fs::hard_link(&source, &twin).unwrap();
        std::os::unix::fs::symlink(&source, &output).unwrap();
        assert!(!same_location(&source, &output));

        let unit = DatUnit::File(source.clone());
        let mut claims = HashMap::new();
        let (removed, failures, kept) = remove_sources(
            &unit,
            &unit_source_files(&unit),
            std::slice::from_ref(&output),
            &std::fs::canonicalize(source.parent().unwrap()).unwrap(),
            &mut claims,
            false,
        )
        .await;

        assert!(removed.is_empty(), "{removed:?}");
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(
            kept,
            vec![format!("kept {}: it is also an output", source.display())]
        );
        assert!(source.exists());
        assert!(output.exists());
        assert_eq!(std::fs::read(&twin).unwrap(), b"source");
    }

    #[tokio::test]
    async fn remove_sources_probe_removes_a_source_with_a_separate_output() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("Game.gba");
        let output = dir.path().join("Game.zip");
        std::fs::write(&source, b"source").unwrap();
        std::fs::write(&output, b"output").unwrap();
        let unit = DatUnit::File(source.clone());
        let (removed, failures, kept) = remove_sources(
            &unit,
            &unit_source_files(&unit),
            std::slice::from_ref(&output),
            &std::fs::canonicalize(dir.path()).unwrap(),
            &mut HashMap::new(),
            false,
        )
        .await;
        assert_eq!(removed, vec![source.clone()]);
        assert!(failures.is_empty(), "{failures:?}");
        assert!(kept.is_empty(), "{kept:?}");
        assert!(!source.exists());
        assert_eq!(std::fs::read(&output).unwrap(), b"output");
    }

    #[test]
    fn output_sync_errors_keep_sources_only_for_writeback_failures() {
        use std::io::{Error, ErrorKind};
        for kind in [
            ErrorKind::StorageFull,
            ErrorKind::QuotaExceeded,
            ErrorKind::StaleNetworkFileHandle,
        ] {
            assert!(sync_error_keeps_sources(&Error::from(kind)), "{kind:?}");
        }
        for kind in [
            ErrorKind::Other,
            ErrorKind::PermissionDenied,
            ErrorKind::Unsupported,
            ErrorKind::InvalidInput,
            ErrorKind::WriteZero,
            ErrorKind::Interrupted,
        ] {
            assert!(!sync_error_keeps_sources(&Error::from(kind)), "{kind:?}");
        }
        #[cfg(unix)]
        {
            assert!(sync_error_keeps_sources(&Error::from_raw_os_error(5)));
            assert!(!sync_error_keeps_sources(&Error::from_raw_os_error(9)));
        }
        #[cfg(windows)]
        for code in [1, 5, 50] {
            assert!(!sync_error_keeps_sources(&Error::from_raw_os_error(code)));
        }
    }

    #[test]
    fn restore_source_keeps_an_occupied_source_name_and_the_temporary_file() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("Game.gba");
        let temporary = dir.path().join(".rom-converto-move-test");
        std::fs::write(&temporary, b"original").unwrap();
        std::fs::write(&source, b"occupying").unwrap();
        let err = restore_source(&source, &temporary).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        let detail = err.to_string();
        assert!(detail.contains(&source.display().to_string()), "{detail}");
        assert!(
            detail.contains(&temporary.display().to_string()),
            "{detail}"
        );
        assert_eq!(std::fs::read(&source).unwrap(), b"occupying");
        assert_eq!(std::fs::read(&temporary).unwrap(), b"original");
    }

    #[tokio::test]
    async fn remove_sources_reports_a_missing_output_before_the_probe() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("Game.gba");
        let output = dir.path().join("missing.zip");
        std::fs::write(&source, b"source").unwrap();
        let unit = DatUnit::File(source.clone());
        let (removed, failures, kept) = remove_sources(
            &unit,
            &unit_source_files(&unit),
            &[output],
            &std::fs::canonicalize(dir.path()).unwrap(),
            &mut HashMap::new(),
            false,
        )
        .await;
        assert!(removed.is_empty(), "{removed:?}");
        assert!(kept.is_empty(), "{kept:?}");
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, source);
        assert_eq!(failures[0].1.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(std::fs::read(&source).unwrap(), b"source");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn remove_sources_stops_a_split_set_on_a_kept_part() {
        let dir = TempDir::new().unwrap();
        let parts: Vec<PathBuf> = ["game_part1.wud", "game_part2.wud", "game_part3.wud"]
            .iter()
            .map(|name| dir.path().join(name))
            .collect();
        for part in &parts {
            std::fs::write(part, b"part").unwrap();
        }
        let output = dir.path().join("output.wud");
        std::os::unix::fs::symlink(&parts[2], &output).unwrap();
        let unit = DatUnit::File(parts[0].clone());
        let (removed, failures, kept) = remove_sources(
            &unit,
            &unit_source_files(&unit),
            std::slice::from_ref(&output),
            &std::fs::canonicalize(dir.path()).unwrap(),
            &mut HashMap::new(),
            false,
        )
        .await;
        assert!(removed.is_empty(), "{removed:?}");
        assert_eq!(
            kept,
            vec![format!("kept {}: it is also an output", parts[2].display())]
        );
        assert_eq!(failures.len(), 2);
        assert_eq!(failures[0].0, parts[1]);
        assert_eq!(failures[1].0, parts[0]);
        assert!(
            failures
                .iter()
                .all(|(_, err)| err.to_string().contains("not attempted"))
        );
        for part in &parts {
            assert_eq!(std::fs::read(part).unwrap(), b"part");
        }
        assert_eq!(std::fs::read(output).unwrap(), b"part");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn remove_sources_stops_a_cue_set_on_a_kept_bin() {
        let dir = TempDir::new().unwrap();
        let cue = dir.path().join("Game.cue");
        let bin = dir.path().join("Game.bin");
        let remaining = dir.path().join("Remaining.bin");
        let output = dir.path().join("output.bin");
        std::fs::write(&cue, b"cue").unwrap();
        std::fs::write(&bin, b"bin").unwrap();
        std::fs::write(&remaining, b"remaining").unwrap();
        std::os::unix::fs::symlink(&bin, &output).unwrap();
        let unit = DatUnit::CueSet {
            cue: cue.clone(),
            bins: vec![bin.clone(), remaining.clone()],
        };
        let (removed, failures, kept) = remove_sources(
            &unit,
            &unit_source_files(&unit),
            &[output],
            &std::fs::canonicalize(dir.path()).unwrap(),
            &mut HashMap::new(),
            false,
        )
        .await;
        assert!(removed.is_empty(), "{removed:?}");
        assert_eq!(
            kept,
            vec![format!("kept {}: it is also an output", bin.display())]
        );
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, cue);
        assert_eq!(
            failures[0].1.to_string(),
            "not removed: a bin could not be removed"
        );
        assert_eq!(std::fs::read(cue).unwrap(), b"cue");
        assert_eq!(std::fs::read(bin).unwrap(), b"bin");
        assert_eq!(std::fs::read(remaining).unwrap(), b"remaining");
    }

    /// Playlists are built from the CLEANED disc dirs: a stale disc in a
    /// managed dir is removed by clean before the playlist is written, so
    /// the .m3u never lists it.
    #[tokio::test]
    async fn playlists_are_written_from_the_cleaned_disc_dirs() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 1).rvz"), gcm_bytes()).unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 2).rvz"), gcm_bytes()).unwrap();

        let request = |out: &TempDir| {
            let mut req = organize_request(lib.path(), Some(out.path()), false);
            req.options.playlists = Some(true);
            req.options.clean = Some(true);
            req
        };
        organize(
            request(&out),
            &RecordingProgress::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        let disc_dir = out.path().join("GameCube");
        let m3u = disc_dir.join("Grouped Game.m3u");
        assert!(m3u.exists());

        // A stale disc appears in the managed dir between the two runs.
        std::fs::write(disc_dir.join("Grouped Game (Disc 3).rvz"), b"stale").unwrap();
        organize(
            request(&out),
            &RecordingProgress::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();

        let contents = std::fs::read_to_string(&m3u).unwrap();
        assert!(contents.contains("Grouped Game (Disc 1).rvz"), "{contents}");
        assert!(contents.contains("Grouped Game (Disc 2).rvz"), "{contents}");
        assert!(
            !contents.contains("Disc 3"),
            "the stale disc must not reach the playlist: {contents}"
        );
        assert!(!disc_dir.join("Grouped Game (Disc 3).rvz").exists());
    }

    /// A dedupe_by_path loser's desired path stays out of clean's
    /// keep-set: its stale un-lettered file is cleaned, while the winner's
    /// lettered output survives.
    #[tokio::test]
    async fn clean_removes_a_dedupe_loser_s_stale_un_lettered_path() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::create_dir_all(lib.path().join("x")).unwrap();
        std::fs::create_dir_all(lib.path().join("y")).unwrap();
        std::fs::write(lib.path().join("x").join("Game.gba"), gba_bytes()).unwrap();
        std::fs::write(lib.path().join("y").join("Game.gba"), gba_bytes()).unwrap();
        // The loser's un-lettered desired path already exists, from an
        // earlier run without the letter layout.
        let gba_dir = out.path().join("Game Boy Advance");
        std::fs::create_dir_all(&gba_dir).unwrap();
        std::fs::write(gba_dir.join("Game.zip"), b"stale").unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.clean = Some(true);
        req.options.dir_letter = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert_eq!(data.failed, 0, "{:?}", data.rows);
        // x wins the shared output path and lands under its letter dir.
        assert!(gba_dir.join("G").join("Game.zip").exists());
        assert!(
            !gba_dir.join("Game.zip").exists(),
            "the loser's un-lettered path must be cleaned"
        );
        let loser = data
            .rows
            .iter()
            .find(|row| {
                row.input.file_name().is_some_and(|name| name == "Game.gba")
                    && row.status == FileStatus::Skipped
            })
            .expect("a dedupe loser row");
        assert!(
            loser
                .detail
                .as_deref()
                .is_some_and(|detail| detail.starts_with("duplicate output")),
            "{:?}",
            loser.detail
        );
    }

    /// With a flat template the pre-letter managed dir is the output dir
    /// itself, which clean walks non-recursively: the post-letter dir is
    /// added as managed too, so stale files inside written letter dirs are
    /// cleaned.
    #[tokio::test]
    async fn clean_reaches_stale_files_inside_letter_dirs() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Game.xiso"), b"not a real xiso").unwrap();
        let letter_dir = out.path().join("G");
        std::fs::create_dir_all(&letter_dir).unwrap();
        std::fs::write(letter_dir.join("stale.sav"), b"stale").unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.clean = Some(true);
        req.options.dir_letter = Some(true);
        req.options.output_template = Some("{basename}.{ext}".to_string());
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert_eq!(data.failed, 0, "{:?}", data.rows);
        assert!(letter_dir.join("Game.xiso").exists());
        assert!(
            !letter_dir.join("stale.sav").exists(),
            "stale files inside a written letter dir must be cleaned"
        );
    }

    /// An in-place unit wins dedupe_by_path over a sibling: with input and
    /// output trees overlapping, the zip whose desired output is its own
    /// source file stays "already in place" and byte-identical, while the
    /// plain sibling that loses the group is skipped.
    #[tokio::test]
    async fn dedupe_prefers_a_unit_already_in_place() {
        let lib = TempDir::new().unwrap();
        let gba_dir = lib.path().join("GBA");
        std::fs::create_dir_all(&gba_dir).unwrap();
        let zip_path = gba_dir.join("foo.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("foo.gba", opts).unwrap();
        zip.write_all(&gba_bytes()).unwrap();
        zip.finish().unwrap();
        std::fs::write(gba_dir.join("foo.gba"), gba_bytes()).unwrap();

        let mut req = organize_request(lib.path(), Some(lib.path()), false);
        req.options.output_template = Some("{input_dir}/{basename}.{ext}".to_string());
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert_eq!(data.failed, 0, "{:?}", data.rows);

        let zip_row = row_for(&data, "foo.zip");
        assert_eq!(zip_row.status, FileStatus::Skipped, "{:?}", zip_row.detail);
        assert_eq!(zip_row.detail.as_deref(), Some("already in place"));
        assert_eq!(zip_row.output.as_deref(), Some(zip_path.as_path()));

        let gba_row = row_for(&data, "foo.gba");
        assert_eq!(gba_row.status, FileStatus::Skipped);
        assert!(
            gba_row
                .detail
                .as_deref()
                .is_some_and(|detail| detail.starts_with("duplicate output")),
            "{:?}",
            gba_row.detail
        );
        // Neither file was touched.
        assert!(gba_dir.join("foo.gba").exists());
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&zip_path).unwrap()).unwrap();
        assert!(archive.by_name("foo.gba").is_ok());
    }

    /// A unit whose base plan lost dedupe_by_path is never move-satisfied:
    /// its patched variant's success must not delete the source, because
    /// the unit's archival output went to the sibling.
    #[tokio::test]
    async fn move_source_keeps_the_source_when_the_base_loses_dedupe_by_path() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let patches = TempDir::new().unwrap();
        let a = gba_bytes();
        let mut b = a.clone();
        b[0xA0..0xA8].copy_from_slice(b"OTHERGAM");
        std::fs::create_dir_all(lib.path().join("x")).unwrap();
        std::fs::create_dir_all(lib.path().join("y")).unwrap();
        std::fs::write(lib.path().join("x").join("Game.gba"), &a).unwrap();
        std::fs::write(lib.path().join("y").join("Game.gba"), &b).unwrap();
        let crc_a = ips_for(&patches, "HackA", &a);
        let crc_b = ips_for(&patches, "HackB", &b);

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.move_source = Some(true);
        req.options.patch = Some(vec![patches.path().to_path_buf()]);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        // x's base and patch land; y's base loses the shared Game.zip slot
        // while its patch still lands.
        assert_eq!(data.ok, 3, "{:?}", data.rows);
        assert_eq!(data.skipped, 1, "{:?}", data.rows);
        let gba_dir = out.path().join("Game Boy Advance");
        assert!(gba_dir.join("Game.zip").exists());
        assert!(gba_dir.join(format!("HackA [{crc_a:08X}].zip")).exists());
        assert!(gba_dir.join(format!("HackB [{crc_b:08X}].zip")).exists());
        // x's source moved out; y's source stayed despite its patched
        // variant succeeding.
        assert!(!lib.path().join("x").join("Game.gba").exists());
        assert!(lib.path().join("y").join("Game.gba").exists());
    }

    /// Clean rows and playlist rows stream through `progress.row` and land
    /// in the run's rows and records, so the CLI prints them and the totals
    /// count them.
    #[tokio::test]
    async fn clean_and_playlist_rows_stream_and_count() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 1).rvz"), gcm_bytes()).unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 2).rvz"), gcm_bytes()).unwrap();
        let disc_dir = out.path().join("GameCube");
        std::fs::create_dir_all(&disc_dir).unwrap();
        std::fs::write(disc_dir.join("stale.sav"), b"stale").unwrap();

        let recorder = RowRecorder::default();
        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.playlists = Some(true);
        req.options.clean = Some(true);
        let response = organize(req, &recorder, CancelToken::new()).await.unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };

        let streamed = recorder.rows();
        let clean = streamed
            .iter()
            .find(|row| row.action == "clean")
            .expect("clean row streamed");
        assert_eq!(clean.input, disc_dir.join("stale.sav"));
        assert_eq!(clean.output, None);
        assert_eq!(clean.detail.as_deref(), Some("deleted"));

        let playlist = streamed
            .iter()
            .find(|row| row.action == "playlist")
            .expect("playlist row streamed");
        assert_eq!(playlist.input, disc_dir);
        assert_eq!(
            playlist.output.as_deref(),
            Some(disc_dir.join("Grouped Game.m3u").as_path())
        );
        assert_eq!(playlist.status, FileStatus::Ok);
        assert!(!playlist.planned);

        // Both rows also landed in the returned rows and the records.
        assert_eq!(data.rows.len(), 4, "{:?}", data.rows);
        assert_eq!(data.ok, 4, "{:?}", data.rows);
        assert!(data.rows.iter().any(|row| row.action == "clean"));
        assert!(data.rows.iter().any(|row| row.action == "playlist"));
        assert_eq!(response.records.len(), 4);
        assert!(
            response
                .records
                .iter()
                .any(|record| record.operation == "clean")
        );
        assert!(
            response
                .records
                .iter()
                .any(|record| record.operation == "playlist")
        );
    }

    /// `single` and the prefer_* orderings rank DAT matches; without `dat`
    /// they are rejected up front instead of silently doing nothing.
    #[tokio::test]
    async fn single_and_prefer_options_need_dat() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.single = Some(true);
        let err = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("single/prefer options need dat"),
            "{err}"
        );

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.prefer_region = Some(vec!["USA".to_string()]);
        let err = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("single/prefer options need dat"),
            "{err}"
        );
    }

    /// `prefer_filename_regex` is a DAT-independent tie-break: it also ranks
    /// dedupe_by_path winners, so it is accepted without `dat`. Two inputs
    /// collide on one output path (a plain file and an archive whose member
    /// shares the stem), and the regex picks the winner by the input's own
    /// file name.
    #[tokio::test]
    async fn prefer_filename_regex_breaks_an_output_path_tie() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        // The plain file sorts first and would win the path tie on its own;
        // the archive holds the same-named member, so both resolve to
        // GameBoy Advance/Game.zip.
        std::fs::create_dir_all(lib.path().join("a")).unwrap();
        std::fs::write(lib.path().join("a").join("Game.gba"), gba_bytes()).unwrap();
        std::fs::create_dir_all(lib.path().join("b")).unwrap();
        let mut zip = zip::ZipWriter::new(
            std::fs::File::create(lib.path().join("b").join("Game.zip")).unwrap(),
        );
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("Game.gba", opts).unwrap();
        zip.write_all(&gba_bytes()).unwrap();
        zip.finish().unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.prefer_filename_regex = Some(vec!["\\.zip$".to_string()]);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let zip_row = row_for(&data, "Game.zip");
        assert_eq!(zip_row.status, FileStatus::Ok, "{:?}", zip_row.detail);
        assert_eq!(zip_row.action, "zip");
        let gba_row = row_for(&data, "Game.gba");
        assert_eq!(gba_row.status, FileStatus::Skipped, "{gba_row:?}");
        assert!(
            gba_row
                .detail
                .as_deref()
                .is_some_and(|detail| detail.starts_with("duplicate")),
            "{gba_row:?}"
        );
    }

    /// A not-yet-existing output dir under the root overlaps: both sides
    /// are absolutized and canonicalized on their deepest existing ancestor
    /// before the prefix check, so no current-directory games are needed.
    #[test]
    fn paths_overlap_sees_a_missing_output_dir_under_the_root() {
        let root = TempDir::new().unwrap();
        let overlap = paths_overlap(root.path(), &root.path().join("out/deeper"));
        assert!(overlap, "a missing output dir under the root overlaps");
    }

    /// The pre-write guard refuses a plan whose desired output is (by
    /// identity) another unit's source that this run has not removed, a
    /// file this run already realized, or (under an overwrite policy) an
    /// existing file whose own location is under the input root and that
    /// is not the unit's own source. Other policies never refuse.
    #[test]
    fn write_guard_refuses_foreign_sources_and_realized_outputs() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("Game.zip");
        std::fs::write(&source, b"archive").unwrap();
        let stray = dir.path().join("stray.txt");
        std::fs::write(&stray, b"user file").unwrap();
        let mut plan = crc_plan(7);
        plan.source = dir.path().join("plain.gba");
        plan.desired = Some(source.clone());
        let identity = FileIdentity::of(&source).unwrap();
        let identities: HashMap<FileIdentity, Vec<(usize, PathBuf)>> =
            [(identity.clone(), vec![(3usize, source.clone())])]
                .into_iter()
                .collect();
        let canonical_root = std::fs::canonicalize(dir.path()).unwrap();
        let guards = || WriteGuards {
            sources: &identities,
            realized: HashSet::new(),
            released: HashSet::new(),
            policy: ConflictPolicy::Overwrite,
            root: canonical_root.clone(),
        };

        // Another unit's source: refused.
        let refused = guards()
            .refuse(&plan, &[], 1, Instant::now(), false)
            .expect("a desired path owned by another unit is refused");
        assert_eq!(refused.status, FileStatus::Failed);
        assert!(
            refused
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("another unit's source")),
            "{refused:?}"
        );

        // An existing file under the input root that is nobody's source:
        // refused.
        let mut stray_plan = crc_plan(7);
        stray_plan.source = dir.path().join("plain.gba");
        stray_plan.desired = Some(stray.clone());
        let refused = guards()
            .refuse(&stray_plan, &[], 1, Instant::now(), false)
            .expect("an existing file under the input root is refused");
        assert!(
            refused
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("under the input directory")),
            "{refused:?}"
        );

        // The unit's own source (an in-place zip) is not refused here.
        let own = vec![source.clone()];
        let guards = WriteGuards {
            sources: &identities,
            realized: HashSet::new(),
            released: HashSet::new(),
            policy: ConflictPolicy::Overwrite,
            root: dir.path().to_path_buf(),
        };
        assert!(
            guards
                .refuse(&plan, &own, 1, Instant::now(), false)
                .is_none()
        );

        // Under an error policy nothing can be overwritten: no refusal.
        let guards = WriteGuards {
            sources: &identities,
            realized: HashSet::new(),
            released: HashSet::new(),
            policy: ConflictPolicy::Error,
            root: canonical_root.clone(),
        };
        assert!(
            guards
                .refuse(&plan, &[], 1, Instant::now(), false)
                .is_none()
        );

        // A realized output is refused.
        let mut guards = WriteGuards {
            sources: &HashMap::new(),
            realized: HashSet::new(),
            released: HashSet::new(),
            policy: ConflictPolicy::Overwrite,
            root: canonical_root.clone(),
        };
        guards.realized(&source);
        let refused = guards
            .refuse(&plan, &[], 1, Instant::now(), false)
            .expect("a realized output is refused");
        assert!(
            refused
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("already wrote")),
            "{refused:?}"
        );

        // Skip plans never write, so they are never refused.
        let guards = WriteGuards {
            sources: &identities,
            realized: HashSet::new(),
            released: HashSet::new(),
            policy: ConflictPolicy::Overwrite,
            root: canonical_root,
        };
        let mut skip = plan;
        skip.decision = Decision::Skip("duplicate output".to_string());
        assert!(
            guards
                .refuse(&skip, &[], 1, Instant::now(), false)
                .is_none()
        );
    }

    /// In an overlapping tree, one unit's output must never replace another
    /// unit's source: the plain file desires the archive's own path, the
    /// run refuses the write, and the archive keeps its member.
    #[tokio::test]
    async fn an_output_never_replaces_another_unit_s_source() {
        let lib = TempDir::new().unwrap();
        // The plain file's desired path is Game Boy Advance/Game.zip, so the
        // archive holding a differently named member sits exactly there.
        let gba_dir = lib.path().join("Game Boy Advance");
        std::fs::create_dir_all(&gba_dir).unwrap();
        std::fs::write(lib.path().join("Game.gba"), gba_bytes()).unwrap();
        let zip_path = gba_dir.join("Game.zip");
        let mut other = gba_bytes();
        other[0xA0..0xA8].copy_from_slice(b"OTHERGAM");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("Other.gba", opts).unwrap();
        zip.write_all(&other).unwrap();
        zip.finish().unwrap();

        let mut req = organize_request(lib.path(), Some(lib.path()), false);
        req.options.on_conflict = Some("overwrite".to_string());
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let plain = row_for(&data, "Game.gba");
        assert_eq!(plain.status, FileStatus::Failed, "{plain:?}");
        assert!(
            plain
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("another unit's source")),
            "{plain:?}"
        );
        // The archive kept its source and placed its member untouched.
        assert_eq!(row_for(&data, "Game.zip").status, FileStatus::Ok);
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&zip_path).unwrap()).unwrap();
        let mut got = Vec::new();
        std::io::Read::read_to_end(&mut archive.by_name("Other.gba").unwrap(), &mut got).unwrap();
        assert_eq!(got, other, "the archive source was never overwritten");
        let placed = gba_dir.join("Other.zip");
        let mut placed_archive =
            zip::ZipArchive::new(std::fs::File::open(&placed).unwrap()).unwrap();
        let mut member = placed_archive.by_name("Other.gba").unwrap();
        let mut placed_bytes = Vec::new();
        std::io::Read::read_to_end(&mut member, &mut placed_bytes).unwrap();
        assert_eq!(placed_bytes, other, "{:?}", data.rows);
    }

    /// An archive holding several entries is never removed by
    /// `move_source`: the placed member plus sidecars (a save, a patch,
    /// another ROM) would be destroyed with it, so the source stays, the
    /// rows say why, and nothing is renamed to a move.
    #[tokio::test]
    async fn move_source_keeps_a_multi_member_archive() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let zip_path = lib.path().join("Pack.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("Aaa.gba", opts).unwrap();
        zip.write_all(&gba_bytes()).unwrap();
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("Zzz.gba", opts).unwrap();
        zip.write_all(&gba_bytes()).unwrap();
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("readme.txt", opts).unwrap();
        zip.write_all(b"sidecar").unwrap();
        zip.finish().unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.move_source = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Pack.zip");
        assert_eq!(row.status, FileStatus::Ok, "{row:?}");
        assert_eq!(row.action, "zip", "the source did not move");
        assert!(
            row.detail
                .as_deref()
                .is_some_and(|detail| detail.contains("holds 3 entries")),
            "{row:?}"
        );
        assert!(zip_path.exists(), "the archive stays");
        assert_eq!(data.ok, 1, "{:?}", data.rows);
        assert_eq!(data.failed, 0, "{:?}", data.rows);
    }

    /// A patch-only run never releases the unpatched original: the base
    /// plan is dropped, the patched variant is the run's output, and the
    /// source stays.
    #[tokio::test]
    async fn patch_only_never_releases_the_unpatched_source() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let patches = TempDir::new().unwrap();
        let bytes = gba_bytes();
        std::fs::write(lib.path().join("Game.gba"), &bytes).unwrap();
        let crc = ips_for(&patches, "Hack", &bytes);

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.move_source = Some(true);
        req.options.patch = Some(vec![patches.path().to_path_buf()]);
        req.options.patch_only = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let base = row_for(&data, "Game.gba");
        assert_eq!(base.status, FileStatus::Skipped, "{base:?}");
        assert_eq!(base.detail.as_deref(), Some("patch-only"));
        let patched = data
            .rows
            .iter()
            .find(|row| row.action == "zip" && row.status == FileStatus::Ok)
            .expect("the patched variant lands");
        assert!(
            patched
                .output
                .as_deref()
                .is_some_and(|output| output.file_name().is_some_and(|name| name
                    .to_string_lossy()
                    .starts_with(&format!("Hack [{crc:08X}]"))))
        );
        assert!(
            lib.path().join("Game.gba").exists(),
            "the unpatched original stays: {:?}",
            data.rows
        );
    }

    /// A plan-time staging failure is a failed row, not a skip: the run
    /// does not exit ok while the unit produced nothing.
    #[tokio::test]
    async fn a_failed_staging_archive_fails_its_row() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        // A .gz that is not a tar cannot be opened as an archive; that is a
        // staging failure, not an unrecognized unit.
        std::fs::write(lib.path().join("Broken.gz"), b"not gzip").unwrap();

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
        let row = row_for(&data, "Broken.gz");
        assert_eq!(row.status, FileStatus::Failed, "{row:?}");
        assert!(data.failed == 1 && data.ok == 0, "{:?}", data.rows);
    }

    /// Under `verify_after` a passing zip placement names its verdict, so
    /// consumers can tell a verified output from an unchecked one. Links
    /// write nothing of their own and stay unverified.
    #[tokio::test]
    async fn verify_after_names_a_passing_verdict_on_placements() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Game.gba"), gba_bytes()).unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.verify_after = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Game.gba");
        assert_eq!(row.status, FileStatus::Ok, "{row:?}");
        assert_eq!(row.verify, Some(VerifyVerdict::Verified));
    }

    /// `keep_satisfied` releases a kept-valid output's source only when the
    /// output path holds a regular file: a symlink points at a file
    /// elsewhere, and the source stays.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_kept_valid_symlink_output_keeps_its_source() {
        use std::os::unix::fs::symlink;

        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Game.gba"), gba_bytes()).unwrap();
        // A valid TorrentZip lives outside the output tree; the output path
        // is a symlink to it from an earlier run.
        let elsewhere = TempDir::new().unwrap();
        let target = elsewhere.path().join("Real.zip");
        let raw = elsewhere.path().join("Game.gba");
        std::fs::write(&raw, gba_bytes()).unwrap();
        crate::util::write_torrentzip(
            &crate::util::ZipMember {
                name: "Game.gba".to_string(),
                path: raw,
                skip: 0,
                pad: 0,
                fill: 0,
            },
            &target,
            crate::util::ZipFormat::TorrentZip,
            &crate::util::NoProgress,
            &CancelToken::new(),
        )
        .unwrap();
        let output_dir = out.path().join("Game Boy Advance");
        std::fs::create_dir_all(&output_dir).unwrap();
        symlink(&target, output_dir.join("Game.zip")).unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.move_source = Some(true);
        req.options.on_conflict = Some("overwrite-invalid".to_string());
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Game.gba");
        assert_eq!(row.status, FileStatus::Skipped, "{row:?}");
        assert_eq!(row.detail.as_deref(), Some(VERIFIED_VALID));
        assert!(
            lib.path().join("Game.gba").exists(),
            "the output is a symlink to a file elsewhere: the source stays, {:?}",
            data.rows
        );
    }

    /// A source removal failure flips only the rows whose plan was Keep:
    /// one unit's base lands while its patched variant loses the shared
    /// output path, so the failed move flips the base row and the variant
    /// row keeps its skip.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_source_removal_flips_only_keep_rows() {
        use std::os::unix::fs::PermissionsExt;

        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let patches = TempDir::new().unwrap();
        let bytes = gba_bytes();
        // Both units hold the same ROM, so both match the one patch: each
        // unit gets a base plan and a patched variant, and the two variants
        // collide on the patch stem.
        std::fs::create_dir_all(lib.path().join("x")).unwrap();
        std::fs::create_dir_all(lib.path().join("y")).unwrap();
        std::fs::write(lib.path().join("x").join("Game.gba"), &bytes).unwrap();
        std::fs::write(lib.path().join("y").join("Other.gba"), &bytes).unwrap();
        ips_for(&patches, "Hack", &bytes);
        // The source directories are read-only, so `move_source` cannot
        // remove either file after the outputs land.
        let read_only = std::fs::Permissions::from_mode(0o555);
        let writable = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(lib.path(), read_only.clone()).unwrap();
        std::fs::set_permissions(lib.path().join("x"), read_only.clone()).unwrap();
        std::fs::set_permissions(lib.path().join("y"), read_only.clone()).unwrap();
        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.move_source = Some(true);
        req.options.patch = Some(vec![patches.path().to_path_buf()]);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        std::fs::set_permissions(lib.path().join("x"), writable.clone()).unwrap();
        std::fs::set_permissions(lib.path().join("y"), writable.clone()).unwrap();
        std::fs::set_permissions(lib.path(), writable).unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        // Three Keep rows claimed the move (both bases, the winning
        // variant) and failed; the losing variant's skip row keeps its
        // outcome.
        let failed: Vec<&OrganizeRow> = data
            .rows
            .iter()
            .filter(|row| row.status == FileStatus::Failed)
            .collect();
        assert_eq!(failed.len(), 3, "{:?}", data.rows);
        assert!(
            failed.iter().all(|row| row
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("source not removed"))),
            "{failed:?}"
        );
        let skipped: Vec<&OrganizeRow> = data
            .rows
            .iter()
            .filter(|row| row.status == FileStatus::Skipped)
            .collect();
        assert_eq!(skipped.len(), 1, "{:?}", data.rows);
        assert!(
            skipped[0]
                .detail
                .as_deref()
                .is_some_and(|detail| detail.starts_with("duplicate")),
            "{:?}",
            skipped[0]
        );
        assert_eq!(skipped[0].status, FileStatus::Skipped);
        // Every source survived the failed moves.
        assert!(lib.path().join("x").join("Game.gba").exists());
        assert!(lib.path().join("y").join("Other.gba").exists());
    }

    /// With overlapping input and output trees, clean protects every file
    /// under the input root: a stale file below `--max-depth` in a written
    /// output directory is never a unit, and is still never deleted.
    #[tokio::test]
    async fn clean_never_deletes_files_below_max_depth() {
        let lib = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Game.gba"), gba_bytes()).unwrap();
        let gba_dir = lib.path().join("Game Boy Advance");
        std::fs::create_dir_all(gba_dir.join("sub")).unwrap();
        std::fs::write(gba_dir.join("sub").join("old.nes"), b"stale").unwrap();

        let mut req = organize_request(lib.path(), Some(lib.path()), false);
        req.options.clean = Some(true);
        req.options.max_depth = Some(1);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert!(
            gba_dir.join("sub").join("old.nes").exists(),
            "a file under the input root survives clean: {:?}",
            data.rows
        );
        assert!(
            !data
                .rows
                .iter()
                .any(|row| row.action == "clean" && row.status == FileStatus::Ok),
            "{:?}",
            data.rows
        );
        assert_eq!(row_for(&data, "Game.gba").status, FileStatus::Ok);
    }

    /// A zip whose desired output is its own path is skipped as already in
    /// place and left byte-identical, whatever its members hold: nothing
    /// re-encodes the user's archive in place.
    #[tokio::test]
    async fn a_multi_member_zip_at_its_own_path_is_left_byte_identical() {
        let lib = TempDir::new().unwrap();
        let zip_path = lib.path().join("Pack.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("Pack.gba", opts).unwrap();
        zip.write_all(&gba_bytes()).unwrap();
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("save.sav", opts).unwrap();
        zip.write_all(b"sav bytes").unwrap();
        zip.finish().unwrap();
        let before = std::fs::read(&zip_path).unwrap();

        let mut req = organize_request(lib.path(), Some(lib.path()), false);
        req.options.output_template = Some("{basename}.{ext}".to_string());
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Pack.zip");
        assert_eq!(row.status, FileStatus::Skipped, "{row:?}");
        assert_eq!(row.detail.as_deref(), Some("already in place"));
        assert_eq!(
            std::fs::read(&zip_path).unwrap(),
            before,
            "the in-place archive is untouched"
        );
    }

    /// With overlapping trees, clean never deletes anything under the
    /// input root: a file symlink, a directory symlink, an OS junk file,
    /// and a file inside a junk directory all survive a real clean run.
    #[cfg(unix)]
    #[tokio::test]
    async fn clean_never_deletes_input_links_junk_or_junk_dirs() {
        use std::os::unix::fs::symlink;

        let lib = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Game.gba"), gba_bytes()).unwrap();
        let gba_dir = lib.path().join("Game Boy Advance");
        std::fs::create_dir_all(&gba_dir).unwrap();
        let target = gba_dir.join("real.dat");
        std::fs::write(&target, b"target").unwrap();
        let link_target = gba_dir.join("elsewhere");
        std::fs::create_dir_all(&link_target).unwrap();
        std::fs::write(link_target.join("nested.txt"), b"nested").unwrap();
        symlink(&target, gba_dir.join("hosts-link")).unwrap();
        symlink(&link_target, gba_dir.join("dir-link")).unwrap();
        std::fs::write(gba_dir.join(".DS_Store"), b"junk").unwrap();
        std::fs::create_dir_all(gba_dir.join("@eaDir")).unwrap();
        std::fs::write(gba_dir.join("@eaDir").join("thumb.jpg"), b"thumb").unwrap();

        let mut req = organize_request(lib.path(), Some(lib.path()), false);
        req.options.clean = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert_eq!(row_for(&data, "Game.gba").status, FileStatus::Ok);
        assert!(gba_dir.join("hosts-link").exists(), "{:?}", data.rows);
        assert!(gba_dir.join("dir-link").exists(), "{:?}", data.rows);
        assert!(gba_dir.join(".DS_Store").exists(), "{:?}", data.rows);
        assert!(
            gba_dir.join("@eaDir").join("thumb.jpg").exists(),
            "{:?}",
            data.rows
        );
        assert!(
            !data
                .rows
                .iter()
                .any(|row| row.action == "clean" && row.status == FileStatus::Ok),
            "{:?}",
            data.rows
        );
    }

    /// organize resolves an unset on_conflict to the error policy and never
    /// takes the runner-level default: an existing output is kept and its
    /// bytes are untouched.
    #[tokio::test]
    async fn organize_ignores_the_context_default_conflict() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Game.gba"), gba_bytes()).unwrap();
        // A first run writes the output; the second runs with the context
        // defaulting to overwrite.
        organize(
            organize_request(lib.path(), Some(out.path()), false),
            &RecordingProgress::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        let output = out.path().join("Game Boy Advance").join("Game.zip");
        let before = std::fs::read(&output).unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.ctx.default_conflict = Some(crate::util::ConflictPolicy::Overwrite);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Game.gba");
        assert_eq!(row.status, FileStatus::Skipped, "{row:?}");
        assert_eq!(row.detail.as_deref(), Some("output exists"));
        assert_eq!(std::fs::read(&output).unwrap(), before);
    }

    /// A conversion's family config cannot hand the child a different
    /// conflict policy than organize runs under: with `[dol] on_conflict`
    /// set to overwrite and an existing conversion output, the child skips
    /// like its organize parent would.
    #[tokio::test]
    async fn a_conversion_child_follows_organize_s_conflict_policy() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Game.gcm"), gcm_bytes()).unwrap();
        // A first run writes the conversion output.
        organize(
            organize_request(lib.path(), Some(out.path()), false),
            &RecordingProgress::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        let output = out.path().join("GameCube").join("Game.rvz");
        assert!(output.exists());
        let before = std::fs::read(&output).unwrap();

        let config = lib.path().join("converto.toml");
        std::fs::write(&config, "[dol]\non_conflict = \"overwrite\"\n").unwrap();
        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.config = Some(config);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Game.gcm");
        assert_eq!(row.status, FileStatus::Skipped, "{row:?}");
        assert_eq!(row.detail.as_deref(), Some("output exists"));
        assert_eq!(std::fs::read(&output).unwrap(), before);
    }

    /// A hard link is placed to the file a symlinked source points at, so
    /// the output survives the input link going away.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_hard_linked_output_survives_a_symlinked_source() {
        use std::os::unix::fs::symlink;

        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let real = lib.path().join("Real.gba");
        std::fs::write(&real, gba_bytes()).unwrap();
        let link = lib.path().join("Link.gba");
        symlink(&real, &link).unwrap();

        let plan = UnitPlan {
            index: 0,
            source: link.clone(),
            source_ext: "gba".to_string(),
            action: Action::Link,
            tokens: crate::util::TemplateTokens::new(None, &link, "gba"),
            input_subdir: PathBuf::new(),
            game: None,
            header: None,
            strip: 0,
            trim: None,
            pad: 0,
            pad_fill: 0,
            patch: None,
            desired: Some(out.path().join("Link.gba")),
            decision: Decision::Keep,
            staging_error: None,
            output_error: None,
            match_error: None,
            member: None,
        };
        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.link_mode = Some("hardlink".to_string());
        let progress = RecordingProgress::default();
        let row = execute_unit(
            &req,
            &plan,
            None,
            &DatUnit::File(link.clone()),
            &unit_source_files(&DatUnit::File(link.clone())),
            ZipFormat::TorrentZip,
            Some(place::LinkMode::Hard),
            &WriteGuards::never(out.path().to_path_buf()),
            None,
            &progress,
            &progress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(row.status, FileStatus::Ok, "{row:?}");
        let output = out.path().join("Link.gba");
        // The input link goes away; the hard-linked output keeps its bytes.
        std::fs::remove_file(&link).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), gba_bytes());
    }

    /// An output-path failure fails the unit's row instead of panicking on
    /// a missing path, and suspends clean for the run: another unit still
    /// resolves and writes the output root, so the stale file there would
    /// be swept if clean ran.
    #[tokio::test]
    async fn an_output_path_error_fails_the_row_and_suspends_clean() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Game.gba"), gba_bytes()).unwrap();
        // The control-character stem sanitizes away entirely, so this
        // unit's output path never resolves while Game.gba's does.
        std::fs::write(lib.path().join("\u{7}\u{7}.gba"), gba_bytes()).unwrap();
        std::fs::write(out.path().join("keep.zip"), b"kept").unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.output_template = Some("{basename}".to_string());
        req.options.clean = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "\u{7}\u{7}.gba");
        assert_eq!(row.status, FileStatus::Failed, "{row:?}");
        assert_eq!(row_for(&data, "Game.gba").status, FileStatus::Ok);
        // Clean was suspended: the stale file survives and a failed clean
        // row names the suspension.
        assert!(out.path().join("keep.zip").exists());
        let clean_failed: Vec<&OrganizeRow> = data
            .rows
            .iter()
            .filter(|row| row.action == "clean" && row.status == FileStatus::Failed)
            .collect();
        assert_eq!(clean_failed.len(), 1, "{:?}", data.rows);
    }

    /// A `..` output template fails the row at output-path resolution, and
    /// the failed resolution suspends clean for the run: a failed clean
    /// row names why.
    #[tokio::test]
    async fn a_dot_dot_output_stem_fails_the_row() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Game.gba"), gba_bytes()).unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.output_template = Some("../{basename}.{ext}".to_string());
        req.options.clean = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Game.gba");
        assert_eq!(row.status, FileStatus::Failed, "{row:?}");
        // The failed resolution suspends clean for the run: the failed
        // clean row is emitted whatever else was written.
        assert!(
            data.rows
                .iter()
                .any(|row| row.action == "clean" && row.status == FileStatus::Failed),
            "{:?}",
            data.rows
        );
    }

    /// Disjoint sibling trees do not overlap.
    #[test]
    fn disjoint_sibling_trees_do_not_overlap() {
        let base = TempDir::new().unwrap();
        std::fs::create_dir_all(base.path().join("in")).unwrap();
        std::fs::create_dir_all(base.path().join("out")).unwrap();
        assert!(!paths_overlap(
            &base.path().join("in"),
            &base.path().join("out")
        ));
        assert!(paths_overlap(
            &base.path().join("in"),
            &base.path().join("in").join("deeper")
        ));
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn a_dry_run_releases_firmlink_spellings_but_not_hardlink_twins() {
        let base = tempfile::tempdir_in("/private/var/tmp").unwrap();
        let root = base.path().join("lib");
        std::fs::create_dir(&root).unwrap();
        let source = root.join("Game.gba");
        let twin = root.join("Twin.gba");
        std::fs::write(&source, b"source").unwrap();
        std::fs::hard_link(&source, &twin).unwrap();
        let alias = Path::new("/System/Volumes/Data").join(source.strip_prefix("/").unwrap());
        let identity = FileIdentity::of(&source).unwrap();
        let sources = [(identity, vec![(1, source.clone())])]
            .into_iter()
            .collect();
        let mut guards = WriteGuards {
            sources: &sources,
            realized: HashSet::new(),
            released: HashSet::new(),
            policy: ConflictPolicy::OverwriteInvalid,
            root,
        };
        let mut plan = crc_plan(0);
        plan.desired = Some(alias);
        assert!(guards.refuse(&plan, &[], 1, Instant::now(), true).is_some());
        guards.release(vec![source]);
        assert!(guards.refuse(&plan, &[], 1, Instant::now(), true).is_none());
        plan.desired = Some(twin);
        assert!(guards.refuse(&plan, &[], 1, Instant::now(), true).is_some());
    }

    /// A firmlink spelling is the same tree even when canonical paths differ.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn firmlink_alias_keeps_in_place_sources_and_clean_candidates() {
        fn data_volume_alias(path: &Path) -> PathBuf {
            Path::new("/System/Volumes/Data").join(path.strip_prefix("/").unwrap())
        }

        let base = tempfile::tempdir_in("/private/var/tmp").unwrap();
        let move_lib = base.path().join("move/lib");
        let move_alias = data_volume_alias(&move_lib);
        let archive = move_lib.join("Game Boy Advance").join("Test Game.zip");
        std::fs::create_dir_all(archive.parent().unwrap()).unwrap();
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("Test Game.gba", options).unwrap();
        zip.write_all(&gba_bytes()).unwrap();
        zip.finish().unwrap();
        std::fs::hard_link(&archive, base.path().join("outside.zip")).unwrap();
        assert!(same_location(&archive, &data_volume_alias(&archive)));

        for policy in ["overwrite-invalid", "overwrite"] {
            let mut req = organize_request(&move_lib, Some(&move_alias), false);
            req.options.move_source = Some(true);
            req.options.on_conflict = Some(policy.to_string());
            let response = organize(req, &RecordingProgress::default(), CancelToken::new())
                .await
                .unwrap();
            let Some(RunData::Organize(data)) = response.data else {
                panic!("expected organize data");
            };
            let row = row_for(&data, "Test Game.zip");
            assert_eq!(row.status, FileStatus::Skipped, "{policy}: {row:?}");
            assert_eq!(row.detail.as_deref(), Some("already in place"));
            assert!(archive.exists(), "{policy}: the only copy survives");
        }

        let clean_lib = base.path().join("clean/lib");
        let clean_alias = data_volume_alias(&clean_lib);
        let console = clean_lib.join("Game Boy Advance");
        std::fs::create_dir_all(&console).unwrap();
        std::fs::write(clean_lib.join("Test Game.gba"), gba_bytes()).unwrap();
        let deep = console.join("Deep Game.gba");
        let playlist = console.join("My List.m3u");
        std::fs::write(&deep, b"deep game").unwrap();
        std::fs::write(&playlist, b"playlist").unwrap();

        let progress = RecordingProgress::default();
        let mut req = organize_request(&clean_lib, Some(&clean_alias), false);
        req.options.max_depth = Some(1);
        req.options.clean = Some(true);
        organize(req, &progress, CancelToken::new()).await.unwrap();

        assert!(deep.exists(), "clean never deletes a file under input");
        assert!(
            playlist.exists(),
            "clean never deletes a playlist under input"
        );
        assert!(
            warnings(&progress)
                .iter()
                .any(|warning| warning.contains("input and output directories overlap")),
            "the overlap warning is reported"
        );
    }

    /// With overlapping trees spelled differently, clean keeps every entry
    /// whose own location is under the input root, in a console folder the
    /// run wrote to: a link whose target lies outside the input root, and
    /// a dangling link, with the input root spelled through a symlinked
    /// alias.
    #[cfg(unix)]
    #[tokio::test]
    async fn clean_keeps_input_links_under_a_differently_spelled_root() {
        use std::os::unix::fs::symlink;

        let base = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let lib = base.path().join("lib");
        let console = lib.join("Game Boy Advance");
        std::fs::create_dir_all(&console).unwrap();
        std::fs::write(lib.join("Game.gba"), gba_bytes()).unwrap();
        std::fs::create_dir_all(outside.path().join("elsewhere")).unwrap();
        std::fs::write(
            outside.path().join("elsewhere").join("target.txt"),
            b"target",
        )
        .unwrap();
        symlink(outside.path().join("elsewhere"), console.join("out-link")).unwrap();
        symlink(console.join("missing.txt"), console.join("dangling-link")).unwrap();
        // The input root is reached through a symlinked alias, so every
        // decision below is made on the canonical root.
        let alias = base.path().join("alias");
        symlink(base.path(), &alias).unwrap();

        let mut req = organize_request(&alias, Some(lib.as_path()), false);
        req.options.clean = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert!(console.join("Game.zip").exists(), "{:?}", data.rows);
        for link in ["out-link", "dangling-link"] {
            assert!(
                std::fs::symlink_metadata(console.join(link)).is_ok(),
                "{link}: {:?}",
                data.rows
            );
        }
        assert!(
            !data
                .rows
                .iter()
                .any(|row| row.action == "clean" && row.status == FileStatus::Ok),
            "{:?}",
            data.rows
        );
    }

    /// An archive holding a ROM plus a nested archive is never released
    /// under --move: the nested archive would be destroyed with it.
    #[tokio::test]
    async fn move_source_keeps_an_archive_with_a_nested_archive() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let nested = lib.path().join("Saves.zip");
        let mut inner = zip::ZipWriter::new(std::fs::File::create(&nested).unwrap());
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        inner.start_file("save.sav", opts).unwrap();
        inner.write_all(b"save").unwrap();
        inner.finish().unwrap();

        let pack = lib.path().join("Game.zip");
        let mut outer = zip::ZipWriter::new(std::fs::File::create(&pack).unwrap());
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        outer.start_file("Game.gba", opts).unwrap();
        outer.write_all(&gba_bytes()).unwrap();
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        outer.start_file("Saves.zip", opts).unwrap();
        outer
            .write_all(std::fs::read(&nested).unwrap().as_slice())
            .unwrap();
        outer.finish().unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.move_source = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Game.zip");
        assert_eq!(row.status, FileStatus::Ok, "{row:?}");
        assert_eq!(row.action, "zip", "the source did not move");
        assert!(pack.exists(), "the archive with the nested zip stays");
    }

    /// A plan-time staging failure suspends clean: the previous output in
    /// the managed dir survives and a failed clean row names why. A good
    /// unit still writes the dir, so the stale file would be swept if
    /// clean ran.
    #[tokio::test]
    async fn a_failed_staging_unit_suspends_clean() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Broken.gz"), b"not gzip").unwrap();
        std::fs::write(lib.path().join("Game.gba"), gba_bytes()).unwrap();
        let gba_dir = out.path().join("Game Boy Advance");
        std::fs::create_dir_all(&gba_dir).unwrap();
        std::fs::write(gba_dir.join("keep.zip"), b"kept").unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.clean = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert_eq!(row_for(&data, "Broken.gz").status, FileStatus::Failed);
        assert_eq!(row_for(&data, "Game.gba").status, FileStatus::Ok);
        assert!(gba_dir.join("keep.zip").exists());
        assert!(
            data.rows
                .iter()
                .any(|row| row.action == "clean" && row.status == FileStatus::Failed),
            "{:?}",
            data.rows
        );
    }

    /// A disc whose clean delete failed keeps its playlist derived: the
    /// failed clean row is emitted, the playlist still derives from the
    /// surviving discs, and the planned rewrite fails on the read-only
    /// dir, so the bytes stay and the run warns instead of claiming a
    /// write.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_disc_delete_keeps_the_playlist_derived() {
        use std::os::unix::fs::PermissionsExt;

        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        for disc in 1..=2 {
            std::fs::write(
                lib.path().join(format!("Grouped Game (Disc {disc}).rvz")),
                gcm_bytes(),
            )
            .unwrap();
        }
        let mut first = organize_request(lib.path(), Some(out.path()), false);
        first.options.playlists = Some(true);
        organize(first, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let disc_dir = out.path().join("GameCube");
        let m3u = disc_dir.join("Grouped Game.m3u");
        let before = std::fs::read_to_string(&m3u).unwrap();
        // A stale disc appears and the disc dir is made read-only, so
        // clean's delete of it fails.
        let stale = disc_dir.join("Grouped Game (Disc 3).rvz");
        std::fs::write(&stale, b"stale").unwrap();
        let read_only = std::fs::Permissions::from_mode(0o555);
        std::fs::set_permissions(&disc_dir, read_only).unwrap();
        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.playlists = Some(true);
        req.options.clean = Some(true);
        let progress = RecordingProgress::default();
        let response = organize(req, &progress, CancelToken::new()).await;
        std::fs::set_permissions(&disc_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let response = response.unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        // The delete failed, so Disc 3 stays in the plan: the playlist is
        // still derived and a rewrite is attempted, which fails on the
        // read-only dir. The warning says so, no row claims the playlist
        // was already current, and the bytes are untouched.
        assert!(
            data.rows
                .iter()
                .any(|row| row.action == "clean" && row.status == FileStatus::Failed),
            "{:?}",
            data.rows
        );
        assert!(
            warnings(&progress)
                .iter()
                .any(|message| message.contains("could not write playlist")),
            "{:?}",
            warnings(&progress)
        );
        assert!(
            !data.rows.iter().any(|row| row.action == "playlist"
                && row.status == FileStatus::Skipped
                && row.detail.as_deref() == Some("already current")),
            "{:?}",
            data.rows
        );
        assert_eq!(std::fs::read_to_string(&m3u).unwrap(), before);
    }

    /// In overlapping trees the run's own `.m3u` refreshes in place when a
    /// disc is added, while a user-edited `.m3u` is kept byte-identical.
    #[tokio::test]
    async fn in_place_playlists_refresh_their_own_m3u_and_keep_a_user_edit() {
        let lib = TempDir::new().unwrap();
        for disc in 1..=2 {
            std::fs::write(
                lib.path().join(format!("Grouped Game (Disc {disc}).rvz")),
                gcm_bytes(),
            )
            .unwrap();
        }
        let request = || {
            let mut req = organize_request(lib.path(), Some(lib.path()), false);
            req.options.playlists = Some(true);
            req
        };
        organize(request(), &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let m3u = lib.path().join("GameCube").join("Grouped Game.m3u");
        std::fs::write(lib.path().join("Grouped Game (Disc 3).rvz"), gcm_bytes()).unwrap();
        organize(request(), &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let contents = std::fs::read_to_string(&m3u).unwrap();
        assert!(
            contents.contains("Disc 3"),
            "the tool's own m3u refreshes: {contents}"
        );

        // A user edit (comment, reversed order) is never overwritten.
        std::fs::write(
            &m3u,
            "# mine\nGrouped Game (Disc 3).rvz\nGrouped Game (Disc 1).rvz\n",
        )
        .unwrap();
        let response = organize(request(), &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = data
            .rows
            .iter()
            .find(|row| row.action == "playlist" && row.output.as_deref() == Some(m3u.as_path()))
            .expect("playlist row");
        assert_eq!(row.status, FileStatus::Skipped, "{row:?}");
        assert_eq!(
            row.detail.as_deref(),
            Some("playlist is under the input directory")
        );
        assert!(std::fs::read_to_string(&m3u).unwrap().starts_with("# mine"));
    }

    /// A copy whose source was kept (a demoted multi-entry archive) is
    /// never renamed to a move.
    #[tokio::test]
    async fn a_kept_copy_source_is_never_renamed_to_move() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let pack = lib.path().join("Pack.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&pack).unwrap());
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("Game.gba", opts).unwrap();
        zip.write_all(&gba_bytes()).unwrap();
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("notes.txt", opts).unwrap();
        zip.write_all(b"sidecar").unwrap();
        zip.finish().unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.move_source = Some(true);
        req.options.zip_exclude = Some("**/*.zip".to_string());
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Pack.zip");
        assert_eq!(row.status, FileStatus::Ok, "{row:?}");
        assert_eq!(row.action, "copy", "the source was kept, not moved");
        assert!(pack.exists());
    }

    /// Two cue sets referencing one bin produce a claim count of two
    /// through the same counting the run uses, so `remove_sources` keeps
    /// the bin until its last claimant releases it.
    #[cfg(unix)]
    #[tokio::test]
    async fn two_cue_sheets_claim_their_shared_bin_twice() {
        let lib = TempDir::new().unwrap();
        let bin = lib.path().join("shared.bin");
        std::fs::write(&bin, b"shared track").unwrap();
        let units: Vec<DatUnit> = ["A", "B"]
            .iter()
            .map(|name| {
                std::fs::write(
                    lib.path().join(format!("{name}.cue")),
                    "FILE \"shared.bin\" BINARY\n  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n",
                )
                .unwrap();
                DatUnit::CueSet {
                    cue: lib.path().join(format!("{name}.cue")),
                    bins: vec![bin.clone()],
                }
            })
            .collect();
        let mut claims = source_claim_counts(&units);
        assert_eq!(claims.get(&FileIdentity::of(&bin).unwrap()), Some(&2));

        // Either set's removal keeps the bin and says why.
        let first = &units[0];
        let (removed, failures, kept) = remove_sources(
            first,
            &unit_source_files(first),
            &[],
            &std::fs::canonicalize(lib.path()).unwrap(),
            &mut claims,
            false,
        )
        .await;
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(removed, vec![first.display_path()]);
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert!(kept[0].contains("another scanned unit"), "{kept:?}");
        assert!(bin.exists(), "the shared bin survives");

        // The last claimant's release removes the shared bin: the first
        // cue's removal decremented the claim.
        let second = &units[1];
        let (removed, failures, kept) = remove_sources(
            second,
            &unit_source_files(second),
            &[],
            &std::fs::canonicalize(lib.path()).unwrap(),
            &mut claims,
            false,
        )
        .await;
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(
            removed,
            vec![bin.clone(), second.display_path().to_path_buf()]
        );
        assert!(kept.is_empty(), "{kept:?}");
        assert!(!bin.exists(), "the last claimant releases the shared bin");
    }

    /// A split WUD set releases every part under --move, last part first;
    /// a failed removal keeps every part.
    #[cfg(unix)]
    #[tokio::test]
    async fn remove_sources_releases_every_split_part() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let parts: Vec<PathBuf> = ["part1", "part2", "part3"]
            .iter()
            .map(|n| dir.path().join(format!("game_{n}.wud")))
            .collect();
        for part in &parts {
            std::fs::write(part, b"part").unwrap();
        }
        // Every part is scanned as its own unit; part1's set claims each
        // part once and the continuations never claim for themselves.
        let scanned: Vec<DatUnit> = parts.iter().cloned().map(DatUnit::File).collect();
        let mut claims = source_claim_counts(&scanned);
        for part in &parts {
            assert_eq!(claims.get(&FileIdentity::of(part).unwrap()), Some(&1));
        }
        assert_eq!(claims.len(), 3);

        let unit = DatUnit::File(parts[0].clone());
        let (removed, failures, kept) = remove_sources(
            &unit,
            &unit_source_files(&unit),
            &[],
            &std::fs::canonicalize(dir.path()).unwrap(),
            &mut claims,
            false,
        )
        .await;
        assert!(failures.is_empty(), "{failures:?}");
        assert!(kept.is_empty(), "{kept:?}");
        // Last part first so an interrupted run still resumes from part1.
        assert_eq!(
            removed,
            vec![parts[2].clone(), parts[1].clone(), parts[0].clone()]
        );
        assert!(parts.iter().all(|part| !part.exists()));

        // A failed removal keeps every part on disk: the parent directory
        // is made read-only so no part can be removed.
        for part in &parts {
            std::fs::write(part, b"part").unwrap();
        }
        let read_only = std::fs::Permissions::from_mode(0o555);
        let writable = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(dir.path(), read_only).unwrap();
        let (removed, failures, kept) = remove_sources(
            &unit,
            &unit_source_files(&unit),
            &[],
            &std::fs::canonicalize(dir.path()).unwrap(),
            &mut claims,
            false,
        )
        .await;
        std::fs::set_permissions(dir.path(), writable).unwrap();
        assert_eq!(failures.len(), 3, "{failures:?}");
        // The release stops at the first failed part: the remaining parts
        // are reported as not attempted, not as their own failures.
        assert!(
            failures[1..]
                .iter()
                .all(|(_, err)| err.to_string().contains("not attempted")),
            "{failures:?}"
        );
        assert!(removed.is_empty(), "{removed:?}");
        assert_eq!(kept.len(), 0, "{kept:?}");
        assert!(parts.iter().all(|part| part.exists()));
    }

    /// In overlapping trees a user-authored `.m3u` under the input root is
    /// never rewritten: the run skips it with a row and the bytes survive.
    #[tokio::test]
    async fn a_user_playlist_under_input_is_never_rewritten() {
        let lib = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 1).rvz"), gcm_bytes()).unwrap();
        std::fs::write(lib.path().join("Grouped Game (Disc 2).rvz"), gcm_bytes()).unwrap();
        let disc_dir = lib.path().join("GameCube");
        std::fs::create_dir_all(&disc_dir).unwrap();
        std::fs::rename(
            lib.path().join("Grouped Game (Disc 1).rvz"),
            disc_dir.join("Grouped Game (Disc 1).rvz"),
        )
        .unwrap();
        std::fs::rename(
            lib.path().join("Grouped Game (Disc 2).rvz"),
            disc_dir.join("Grouped Game (Disc 2).rvz"),
        )
        .unwrap();
        let user_m3u = disc_dir.join("Grouped Game.m3u");
        std::fs::write(
            &user_m3u,
            b"# mine\nGrouped Game (Disc 2).rvz\nGrouped Game (Disc 1).rvz\n",
        )
        .unwrap();
        let before = std::fs::read(&user_m3u).unwrap();

        let mut req = organize_request(lib.path(), Some(lib.path()), false);
        req.options.playlists = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = data
            .rows
            .iter()
            .find(|row| {
                row.action == "playlist" && row.output.as_deref() == Some(user_m3u.as_path())
            })
            .expect("playlist row");
        assert_eq!(row.status, FileStatus::Skipped, "{row:?}");
        assert_eq!(
            row.detail.as_deref(),
            Some("playlist is under the input directory")
        );
        assert_eq!(std::fs::read(&user_m3u).unwrap(), before);
    }

    /// Under overwrite-invalid a conversion whose desired path holds a
    /// corrupt foreign file that is another unit's source keeps that file:
    /// the child is dispatched with a skip policy, the row reports the
    /// keep, and the bytes are unchanged.
    #[tokio::test]
    async fn a_convert_plan_keeps_a_foreign_source_under_overwrite_invalid() {
        let lib = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Game.gcm"), gcm_bytes()).unwrap();
        let gba_dir = lib.path().join("GameCube");
        std::fs::create_dir_all(&gba_dir).unwrap();
        std::fs::write(gba_dir.join("Game.rvz"), b"foreign corrupt bytes").unwrap();

        let mut req = organize_request(lib.path(), Some(lib.path()), false);
        req.options.on_conflict = Some("overwrite-invalid".to_string());
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Game.gcm");
        assert_eq!(row.status, FileStatus::Skipped, "{row:?}");
        assert!(
            row.detail
                .as_deref()
                .is_some_and(|detail| detail.starts_with("kept:")
                    && detail.contains("another unit's source")),
            "{row:?}"
        );
        assert_eq!(
            std::fs::read(gba_dir.join("Game.rvz")).unwrap(),
            b"foreign corrupt bytes",
            "the foreign source was never rewritten"
        );
    }

    /// An in-place re-run under overwrite-invalid keeps a valid earlier
    /// output under the input tree and exits ok instead of failing. With
    /// `--max-depth 1` the earlier `.rvz` is not a unit of its own, so the
    /// gcm's own conversion is the guarded keep.
    #[tokio::test]
    async fn an_in_place_convert_rerun_under_overwrite_invalid_exits_ok() {
        let lib = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Game.gcm"), gcm_bytes()).unwrap();
        // A first run writes the conversion output in place.
        organize(
            organize_request(lib.path(), Some(lib.path()), false),
            &RecordingProgress::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        let output = lib.path().join("GameCube").join("Game.rvz");
        assert!(output.exists());
        let before = std::fs::read(&output).unwrap();

        let mut req = organize_request(lib.path(), Some(lib.path()), false);
        req.options.on_conflict = Some("overwrite-invalid".to_string());
        req.options.max_depth = Some(1);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert_eq!(data.failed, 0, "{:?}", data.rows);
        // The conversion is kept, not rewritten: the existing output is a
        // valid result under the input tree, and the row says why.
        let row = row_for(&data, "Game.gcm");
        assert_eq!(row.status, FileStatus::Skipped, "{row:?}");
        assert_eq!(
            row.detail.as_deref(),
            Some("kept: existing output is in the input tree, not rewritten"),
            "{row:?}"
        );
        assert_eq!(std::fs::read(&output).unwrap(), before);
    }

    /// A placement under overwrite-invalid keeps a valid earlier output
    /// that sits past `--max-depth` in the overlapping tree: the row skips
    /// as verified-valid and the bytes are untouched.
    #[tokio::test]
    async fn a_placement_keeps_a_valid_output_past_max_depth() {
        let lib = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Game.gba"), gba_bytes()).unwrap();
        let out = lib.path().join("out");
        organize(
            organize_request(lib.path(), Some(out.as_path()), false),
            &RecordingProgress::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        let output = out.join("Game Boy Advance").join("Game.zip");
        assert!(output.exists());
        let before = std::fs::read(&output).unwrap();

        let mut req = organize_request(lib.path(), Some(out.as_path()), false);
        req.options.on_conflict = Some("overwrite-invalid".to_string());
        req.options.max_depth = Some(1);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Game.gba");
        assert_eq!(row.status, FileStatus::Skipped, "{row:?}");
        assert_eq!(row.detail.as_deref(), Some(VERIFIED_VALID), "{row:?}");
        assert_eq!(std::fs::read(&output).unwrap(), before);
    }

    /// A corrupt foreign file at a placement's desired path, past
    /// `--max-depth` so it is not a unit, is refused under
    /// overwrite-invalid instead of being rewritten: the row fails and the
    /// bytes are unchanged.
    #[tokio::test]
    async fn a_placement_refuses_a_corrupt_foreign_file_under_the_input_tree() {
        let lib = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Game.gba"), gba_bytes()).unwrap();
        let out = lib.path().join("out");
        organize(
            organize_request(lib.path(), Some(out.as_path()), false),
            &RecordingProgress::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        let output = out.join("Game Boy Advance").join("Game.zip");
        std::fs::write(&output, b"corrupt").unwrap();

        let mut req = organize_request(lib.path(), Some(out.as_path()), false);
        req.options.on_conflict = Some("overwrite-invalid".to_string());
        req.options.max_depth = Some(1);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Game.gba");
        assert_eq!(row.status, FileStatus::Failed, "{row:?}");
        assert!(
            row.detail
                .as_deref()
                .is_some_and(|detail| detail.contains("under the input directory")),
            "{row:?}"
        );
        assert_eq!(std::fs::read(&output).unwrap(), b"corrupt");
    }

    /// A symlink at the desired path pointing at the unit's own source is
    /// not "already in place": the comparison is the directory entry, not
    /// the link's target, so under overwrite the link is replaced by the
    /// real output and the source file survives.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_at_the_desired_path_is_not_in_place() {
        use std::os::unix::fs::symlink;

        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let source = lib.path().join("Game.gba");
        std::fs::write(&source, gba_bytes()).unwrap();
        let desired = out.path().join("Game.gba");
        symlink(&source, &desired).unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.on_conflict = Some("overwrite".to_string());

        let plan = UnitPlan {
            index: 0,
            source: source.clone(),
            source_ext: "gba".to_string(),
            action: Action::Copy,
            tokens: crate::util::TemplateTokens::new(None, &source, "gba"),
            input_subdir: PathBuf::new(),
            game: None,
            header: None,
            strip: 0,
            trim: None,
            pad: 0,
            pad_fill: 0,
            patch: None,
            desired: Some(desired.clone()),
            decision: Decision::Keep,
            staging_error: None,
            output_error: None,
            match_error: None,
            member: None,
        };
        let sources: HashMap<FileIdentity, Vec<(usize, PathBuf)>> = HashMap::new();
        let guards = WriteGuards {
            sources: &sources,
            realized: HashSet::new(),
            released: HashSet::new(),
            policy: ConflictPolicy::Overwrite,
            root: lib.path().to_path_buf(),
        };
        let progress = RecordingProgress::default();
        let row = execute_unit(
            &req,
            &plan,
            None,
            &DatUnit::File(source.clone()),
            &unit_source_files(&DatUnit::File(source.clone())),
            ZipFormat::TorrentZip,
            None,
            &guards,
            None,
            &progress,
            &progress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(row.status, FileStatus::Ok, "{row:?}");
        assert!(
            !std::fs::symlink_metadata(&desired).unwrap().is_symlink(),
            "the link was replaced by the real output"
        );
        assert_eq!(std::fs::read(&desired).unwrap(), gba_bytes());
        assert_eq!(std::fs::read(&source).unwrap(), gba_bytes());
    }

    /// A hardlink twin at the desired path is not "already in place": it
    /// is another directory entry, so under overwrite the run places its
    /// own output there and the source file keeps its bytes.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_hardlink_twin_at_the_desired_path_is_not_in_place() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let source = lib.path().join("Game.gba");
        std::fs::write(&source, gba_bytes()).unwrap();
        let desired = out.path().join("Game.gba");
        std::fs::hard_link(&source, &desired).unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.on_conflict = Some("overwrite".to_string());

        let plan = UnitPlan {
            index: 0,
            source: source.clone(),
            source_ext: "gba".to_string(),
            action: Action::Copy,
            tokens: crate::util::TemplateTokens::new(None, &source, "gba"),
            input_subdir: PathBuf::new(),
            game: None,
            header: None,
            strip: 0,
            trim: None,
            pad: 0,
            pad_fill: 0,
            patch: None,
            desired: Some(desired.clone()),
            decision: Decision::Keep,
            staging_error: None,
            output_error: None,
            match_error: None,
            member: None,
        };
        let sources: HashMap<FileIdentity, Vec<(usize, PathBuf)>> = HashMap::new();
        let guards = WriteGuards {
            sources: &sources,
            realized: HashSet::new(),
            released: HashSet::new(),
            policy: ConflictPolicy::Overwrite,
            root: lib.path().to_path_buf(),
        };
        let progress = RecordingProgress::default();
        let row = execute_unit(
            &req,
            &plan,
            None,
            &DatUnit::File(source.clone()),
            &unit_source_files(&DatUnit::File(source.clone())),
            ZipFormat::TorrentZip,
            None,
            &guards,
            None,
            &progress,
            &progress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(row.status, FileStatus::Ok, "{row:?}");
        assert_eq!(std::fs::read(&desired).unwrap(), gba_bytes());
        assert_eq!(
            std::fs::read(&source).unwrap(),
            gba_bytes(),
            "the source keeps its bytes"
        );
    }

    /// A dry run refuses a desired path this run already realized even
    /// when nothing is on disk yet: the realized check runs before the
    /// existence gate. The two desired paths spell one location through a
    /// symlinked output subdir.
    #[cfg(unix)]
    #[test]
    fn a_dry_run_refuses_a_realized_path_it_cannot_see_yet() {
        use std::os::unix::fs::symlink;

        let base = TempDir::new().unwrap();
        let out = base.path().join("out");
        std::fs::create_dir_all(out.join("sub")).unwrap();
        symlink(out.join("sub"), out.join("sub-link")).unwrap();

        let sources: HashMap<FileIdentity, Vec<(usize, PathBuf)>> = HashMap::new();
        let mut guards = WriteGuards {
            sources: &sources,
            realized: HashSet::new(),
            released: HashSet::new(),
            policy: ConflictPolicy::Overwrite,
            root: base.path().to_path_buf(),
        };
        guards.realized(&out.join("sub").join("Game.zip"));
        let mut plan = crc_plan(0);
        plan.desired = Some(out.join("sub-link").join("Game.zip"));

        let row = guards
            .refuse(&plan, &[], 0, Instant::now(), true)
            .expect("the realized refusal fires before the existence gate");
        assert!(
            row.detail
                .as_deref()
                .is_some_and(|detail| detail.contains("this run already wrote it")),
            "{row:?}"
        );
    }

    /// A degraded DAT lookup suspends clean for the run and keeps the
    /// source under --move: the E2E run through the mock API still places
    /// the unit at keep-name, the failed clean row is emitted, the stale
    /// file survives, and the source is not removed.
    #[tokio::test]
    async fn a_degraded_dat_match_suspends_clean_and_keeps_the_source() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Game.gba"), gba_bytes()).unwrap();
        let gba_dir = out.path().join("Game Boy Advance");
        std::fs::create_dir_all(&gba_dir).unwrap();
        std::fs::write(gba_dir.join("keep.zip"), b"kept").unwrap();

        // The bulk lookup finds nothing; the per-unit retry request gets a
        // body the client cannot parse, so the lookup degrades.
        let server = crate::dat::client::mock_api::spawn(move |_request_line, _request_body| {
            "not json".to_string()
        });
        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.dat = Some(true);
        req.options.api_base = Some(server.url().to_string());
        req.options.move_source = Some(true);
        req.options.clean = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        drop(server);
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };

        // The unit still places at keep-name; its source is kept because
        // the DAT match failed.
        let row = row_for(&data, "Game.gba");
        assert_eq!(row.status, FileStatus::Ok, "{row:?}");
        assert!(
            row.detail
                .as_deref()
                .is_some_and(|detail| detail.contains("source kept: DAT match failed")),
            "{row:?}"
        );
        assert!(lib.path().join("Game.gba").exists(), "the source is kept");
        // Clean was suspended: the stale file survives and a failed clean
        // row names the suspension.
        assert!(gba_dir.join("keep.zip").exists());
        assert!(
            data.rows
                .iter()
                .any(|row| row.action == "clean" && row.status == FileStatus::Failed),
            "{:?}",
            data.rows
        );
    }

    /// A newly written `.m3u` gets the process's default file mode (0666
    /// masked by the umask), exactly what `File::create` gives: the
    /// scratch file behind the atomic write is created with that default
    /// mode.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_new_playlist_gets_the_process_default_mode() {
        use std::os::unix::fs::PermissionsExt;

        let lib = TempDir::new().unwrap();
        for disc in 1..=2 {
            std::fs::write(
                lib.path().join(format!("Grouped Game (Disc {disc}).rvz")),
                gcm_bytes(),
            )
            .unwrap();
        }
        // The mode a plain File::create yields under this process's umask.
        let reference = lib.path().join("reference");
        std::fs::File::create(&reference).unwrap();
        let expected = std::fs::metadata(&reference).unwrap().permissions().mode() & 0o777;
        let mut req = organize_request(lib.path(), Some(lib.path()), false);
        req.options.playlists = Some(true);
        organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let m3u = lib.path().join("GameCube").join("Grouped Game.m3u");
        let mode = std::fs::metadata(&m3u).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, expected, "{mode:o} vs {expected:o}");
    }

    /// Only a `.wud` first part owns a split set: any other numbered first
    /// part (a `.gba`, a `.wux`, an archive) is just itself, so releasing
    /// it can never delete the sibling `.wud` continuations.
    #[test]
    fn only_a_wud_first_part_owns_the_split_set() {
        let dir = TempDir::new().unwrap();
        for name in [
            "game_part1.wud",
            "game_part2.wud",
            "game_part3.wud",
            "game_part1.gba",
            "game_part1.wux",
            "game_part1.zip",
        ] {
            std::fs::write(dir.path().join(name), b"part").unwrap();
        }
        for other in ["game_part1.gba", "game_part1.wux", "game_part1.zip"] {
            let path = dir.path().join(other);
            assert_eq!(
                unit_source_files(&DatUnit::File(path.clone())),
                vec![path],
                "{other} must not own the .wud parts"
            );
        }
        let part1 = dir.path().join("game_part1.wud");
        assert_eq!(
            unit_source_files(&DatUnit::File(part1.clone())).len(),
            3,
            "the .wud first part owns every part"
        );
    }

    /// A placeable `game_part1.gba` beside a `.wud` set, run under
    /// `--move`: the `.gba` is released, and every `.wud` part survives.
    /// The release of the `.gba` alone deletes exactly one file.
    #[tokio::test]
    async fn moving_a_part1_gba_never_releases_the_wud_parts() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        for name in ["game_part1.wud", "game_part2.wud", "game_part3.wud"] {
            std::fs::write(lib.path().join(name), b"part").unwrap();
        }
        std::fs::write(lib.path().join("game_part1.gba"), gba_bytes()).unwrap();

        // The release itself removes exactly the .gba.
        let unit = DatUnit::File(lib.path().join("game_part1.gba"));
        let mut claims = HashMap::new();
        let (removed, failures, kept) = remove_sources(
            &unit,
            &unit_source_files(&unit),
            &[],
            &std::fs::canonicalize(lib.path()).unwrap(),
            &mut claims,
            false,
        )
        .await;
        assert!(
            failures.is_empty() && kept.is_empty(),
            "{failures:?} {kept:?}"
        );
        assert_eq!(removed, vec![lib.path().join("game_part1.gba")]);
        for name in ["game_part1.wud", "game_part2.wud", "game_part3.wud"] {
            assert!(
                lib.path().join(name).exists(),
                "{name} survives the release"
            );
        }

        // And the whole run leaves every .wud part in place.
        std::fs::write(lib.path().join("game_part1.gba"), gba_bytes()).unwrap();
        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.move_source = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        for name in ["game_part1.wud", "game_part2.wud", "game_part3.wud"] {
            assert!(
                lib.path().join(name).exists(),
                "{name} must survive: {:?}",
                data.rows
            );
        }
    }

    /// `remove_sources` keeps a bin another cue sheet references (whatever
    /// spelling claims it), and a bin outside the input root, and says so
    /// on the returned notes.
    #[tokio::test]
    async fn remove_sources_keeps_shared_and_out_of_root_bins() {
        let dir = TempDir::new().unwrap();
        let elsewhere = TempDir::new().unwrap();
        let cue = dir.path().join("Game.cue");
        let shared = dir.path().join("Shared.bin");
        let orphan = elsewhere.path().join("Orphan.bin");
        std::fs::write(&cue, b"cue").unwrap();
        std::fs::write(&shared, b"shared").unwrap();
        std::fs::write(&orphan, b"orphan").unwrap();

        let unit = DatUnit::CueSet {
            cue: cue.clone(),
            bins: vec![shared.clone(), orphan.clone()],
        };
        let mut claims = HashMap::new();
        // The bin is referenced by this set and one other (whose spelling
        // differs), and the out-of-root bin only by this set.
        claims.insert(FileIdentity::of(&shared).unwrap(), 2usize);
        claims.insert(FileIdentity::of(&orphan).unwrap(), 1usize);
        claims.insert(FileIdentity::of(&cue).unwrap(), 1usize);

        let (removed, failures, kept) = remove_sources(
            &unit,
            &unit_source_files(&unit),
            &[],
            &std::fs::canonicalize(dir.path()).unwrap(),
            &mut claims,
            false,
        )
        .await;
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(removed, vec![cue.clone()]);
        assert_eq!(kept.len(), 2, "{kept:?}");
        assert!(kept[0].contains("another scanned unit"), "{kept:?}");
        assert!(kept[1].contains("outside the input"), "{kept:?}");
        assert!(shared.exists());
        assert!(orphan.exists());
        assert!(!cue.exists());
    }

    /// A bin referenced through a `..`-climbing spelling is outside the
    /// input root whatever its spelling reads like: it is kept.
    #[cfg(unix)]
    #[tokio::test]
    async fn remove_sources_keeps_a_parent_climbing_bin() {
        let base = TempDir::new().unwrap();
        let dir = base.path().join("lib");
        let cue = dir.join("sub").join("Game.cue");
        let orphan = base.path().join("outside").join("Orphan.bin");
        std::fs::create_dir_all(cue.parent().unwrap()).unwrap();
        std::fs::create_dir_all(orphan.parent().unwrap()).unwrap();
        std::fs::write(&cue, b"cue").unwrap();
        std::fs::write(&orphan, b"orphan").unwrap();

        // The cue's FILE line climbs out of the input root: sub/../../outside.
        let climbing = dir
            .join("sub")
            .join("..")
            .join("..")
            .join("outside")
            .join("Orphan.bin");
        let unit = DatUnit::CueSet {
            cue: cue.clone(),
            bins: vec![climbing],
        };
        let mut claims = HashMap::new();
        claims.insert(FileIdentity::of(&orphan).unwrap(), 1usize);
        claims.insert(FileIdentity::of(&cue).unwrap(), 1usize);

        let (removed, failures, kept) = remove_sources(
            &unit,
            &unit_source_files(&unit),
            &[],
            &std::fs::canonicalize(dir).unwrap(),
            &mut claims,
            false,
        )
        .await;
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(removed, vec![cue.clone()]);
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert!(kept[0].contains("outside the input"), "{kept:?}");
        assert!(orphan.exists(), "the out-of-root bin stays");
    }

    /// A released source is gone at its own location only: a hard link to
    /// it is another entry that stays. Desiring the released path itself is
    /// allowed even when a hard-linked bin of another unit shares its
    /// identity, and desiring a hard link of a released source still falls
    /// through to the under-input refusal.
    #[cfg(unix)]
    #[test]
    fn a_released_source_is_gone_at_its_own_location_only() {
        let dir = TempDir::new().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let a = root.join("A.gba");
        std::fs::write(&a, b"rom").unwrap();
        let b = root.join("B.gba");
        std::fs::hard_link(&a, &b).unwrap();
        let c = root.join("C.gba");
        std::fs::hard_link(&a, &c).unwrap();

        let identity = FileIdentity::of(&a).unwrap();
        // Two units own the one identity (hard-linked bins); one path of
        // it is released.
        let shared: HashMap<FileIdentity, Vec<(usize, PathBuf)>> = [(
            identity.clone(),
            vec![(1usize, a.clone()), (2usize, b.clone())],
        )]
        .into_iter()
        .collect();
        let mut guards = WriteGuards {
            sources: &shared,
            realized: HashSet::new(),
            released: HashSet::new(),
            policy: ConflictPolicy::Overwrite,
            root: root.clone(),
        };
        guards.release(vec![a.clone()]);
        let mut plan = crc_plan(0);
        plan.desired = Some(a.clone());
        assert!(
            guards
                .refuse(&plan, &[], 1, Instant::now(), false)
                .is_none(),
            "the released path itself is free even beside an unreleased hard link"
        );

        // A hard link of a released source is a different entry: not
        // released, an existing file under the input root, so refused.
        let only_a: HashMap<FileIdentity, Vec<(usize, PathBuf)>> =
            [(identity, vec![(1usize, a.clone())])]
                .into_iter()
                .collect();
        let mut guards = WriteGuards {
            sources: &only_a,
            realized: HashSet::new(),
            released: HashSet::new(),
            policy: ConflictPolicy::Overwrite,
            root,
        };
        guards.release(vec![a]);
        let mut plan = crc_plan(0);
        plan.desired = Some(c);
        let refused = guards
            .refuse(&plan, &[], 1, Instant::now(), false)
            .expect("a hard link of a released source still sits under the input");
        assert!(
            refused
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("under the input directory")),
            "{refused:?}"
        );
    }

    /// The kernel resolves a symlink before applying `..`: `link/../out`
    /// with `link -> in/deep` is `in/out`, overlapping `in`. A lexical
    /// fold first would put it at `base/out` and miss the overlap.
    #[cfg(unix)]
    #[test]
    fn a_dotdot_after_a_symlink_resolves_from_the_link_target() {
        let base = TempDir::new().unwrap();
        let input = base.path().join("in");
        std::fs::create_dir_all(input.join("deep")).unwrap();
        std::os::unix::fs::symlink(input.join("deep"), base.path().join("link")).unwrap();
        assert!(paths_overlap(
            &input,
            &base.path().join("link").join("..").join("out")
        ));
    }

    /// A missing directory followed by `..` folds lexically in the missing
    /// tail: `x/../in/new` with `x` absent is `in/new`, overlapping `in`.
    #[test]
    fn a_dotdot_through_a_missing_dir_folds_in_the_tail() {
        let base = TempDir::new().unwrap();
        let input = base.path().join("in");
        std::fs::create_dir_all(&input).unwrap();
        assert!(paths_overlap(
            &input,
            &base.path().join("x").join("..").join("in").join("new")
        ));
        assert!(!paths_overlap(
            &input,
            &base.path().join("x").join("..").join("elsewhere")
        ));
    }

    /// The tool's own `.m3u` under the input root judges by line shape
    /// only: a listed disc that is gone still refreshes, and the derivation
    /// is the bare names in planner order.
    #[test]
    fn own_playlist_derivation_ignores_file_existence_but_not_line_shape() {
        let own = b"Grouped Game (Disc 1).rvz\nGrouped Game (Disc 2).rvz\n";
        assert_eq!(
            own_playlist_derivation("Grouped Game", Some(own)).as_deref(),
            Some("Grouped Game (Disc 1).rvz\nGrouped Game (Disc 2).rvz\n"),
            "no file exists in this test, and the listing is still the tool's own"
        );
        // Absolute, `./`, `..` and subdirectory spellings are user files.
        for user in [
            "/lib/GameCube/Grouped Game (Disc 1).rvz\n/lib/GameCube/Grouped Game (Disc 2).rvz\n",
            "./Grouped Game (Disc 1).rvz\n./Grouped Game (Disc 2).rvz\n",
            "../Other/Grouped Game (Disc 1).rvz\n../Other/Grouped Game (Disc 2).rvz\n",
            "sub/Grouped Game (Disc 1).rvz\nsub/Grouped Game (Disc 2).rvz\n",
        ] {
            assert_eq!(
                own_playlist_derivation("Grouped Game", Some(user.as_bytes())),
                None,
                "{user:?}"
            );
        }
        // A comment-free reordered listing is a user edit.
        assert_eq!(
            own_playlist_derivation(
                "Grouped Game",
                Some(b"Grouped Game (Disc 2).rvz\nGrouped Game (Disc 1).rvz\n")
            ),
            None
        );
        // Another title, or a non-disc extension, is not this .m3u's own.
        assert_eq!(
            own_playlist_derivation(
                "Grouped Game",
                Some(b"Other Game (Disc 1).rvz\nOther Game (Disc 2).rvz\n")
            ),
            None
        );
        assert_eq!(
            own_playlist_derivation("Grouped Game", Some(b"Grouped Game (Disc 1).txt\n")),
            None
        );
    }

    /// End to end in place: a user `.m3u` listing absolute paths in disc
    /// order is kept byte for byte, however the paths read.
    #[tokio::test]
    async fn a_user_playlist_with_absolute_paths_is_kept_in_place() {
        let lib = TempDir::new().unwrap();
        let disc_dir = lib.path().join("GameCube");
        std::fs::create_dir_all(&disc_dir).unwrap();
        for disc in 1..=2 {
            std::fs::write(
                disc_dir.join(format!("Grouped Game (Disc {disc}).rvz")),
                gcm_bytes(),
            )
            .unwrap();
        }
        let m3u = disc_dir.join("Grouped Game.m3u");
        let user = format!(
            "{}\n{}\n",
            disc_dir.join("Grouped Game (Disc 1).rvz").display(),
            disc_dir.join("Grouped Game (Disc 2).rvz").display()
        );
        std::fs::write(&m3u, &user).unwrap();

        let mut req = organize_request(lib.path(), Some(lib.path()), false);
        req.options.playlists = Some(true);
        organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&m3u).unwrap(), user);
    }

    /// The tool's own `.m3u` still refreshes when a listed disc was
    /// removed from disk and another added.
    #[tokio::test]
    async fn an_own_playlist_refreshes_after_a_listed_disc_vanished() {
        let lib = TempDir::new().unwrap();
        let disc_dir = lib.path().join("GameCube");
        std::fs::create_dir_all(&disc_dir).unwrap();
        for disc in [2, 3] {
            std::fs::write(
                disc_dir.join(format!("Grouped Game (Disc {disc}).rvz")),
                gcm_bytes(),
            )
            .unwrap();
        }
        let m3u = disc_dir.join("Grouped Game.m3u");
        // The tool's own earlier output listed Disc 1, which is gone now.
        std::fs::write(
            &m3u,
            "Grouped Game (Disc 1).rvz\nGrouped Game (Disc 2).rvz\n",
        )
        .unwrap();

        let mut req = organize_request(lib.path(), Some(lib.path()), false);
        req.options.playlists = Some(true);
        organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&m3u).unwrap(),
            "Grouped Game (Disc 2).rvz\nGrouped Game (Disc 3).rvz\n"
        );
    }

    /// A rewrite keeps the playlist's existing mode: a 0640 `.m3u` stays
    /// 0640 after the rewrite.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_playlist_rewrite_keeps_its_mode() {
        use std::os::unix::fs::PermissionsExt;

        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        for disc in 1..=2 {
            std::fs::write(
                lib.path().join(format!("Grouped Game (Disc {disc}).rvz")),
                gcm_bytes(),
            )
            .unwrap();
        }
        let disc_dir = out.path().join("GameCube");
        std::fs::create_dir_all(&disc_dir).unwrap();
        let m3u = disc_dir.join("Grouped Game.m3u");
        // A stale differing playlist outside the input tree is rewritten.
        std::fs::write(&m3u, "stale\n").unwrap();
        std::fs::set_permissions(&m3u, std::fs::Permissions::from_mode(0o640)).unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.playlists = Some(true);
        organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        assert!(std::fs::read_to_string(&m3u).unwrap().contains("Disc 2"));
        let mode = std::fs::metadata(&m3u).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640, "{mode:o}");
    }

    /// Clean judges each entry by its own location against the input root
    /// even when the trees do not overlap: `out/GameCube` is a symlink to
    /// `lib/GameCube`, so a file under `out/GameCube/sub` really sits under
    /// the input and is never deleted.
    #[cfg(unix)]
    #[tokio::test]
    async fn clean_keeps_input_files_reached_through_a_symlinked_console_dir() {
        let base = TempDir::new().unwrap();
        let lib = base.path().join("lib");
        let out = base.path().join("out");
        std::fs::create_dir_all(lib.join("GameCube").join("sub")).unwrap();
        std::fs::create_dir_all(lib.join("sub")).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        std::os::unix::fs::symlink(lib.join("GameCube"), out.join("GameCube")).unwrap();
        std::fs::write(lib.join("sub").join("Game.gcm"), gcm_bytes()).unwrap();
        let user_file = lib.join("GameCube").join("sub").join("notes.m3u");
        std::fs::write(&user_file, b"mine").unwrap();

        let mut req = organize_request(lib.as_path(), Some(out.as_path()), false);
        req.options.output_template = Some("{console}/{input_dir}/{basename}.{ext}".to_string());
        req.options.clean = Some(true);
        req.options.max_depth = Some(2);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert!(user_file.exists(), "{:?}", data.rows);
    }

    /// A split set is released all-or-nothing: when the first part is kept
    /// (another scanned unit claims it), no continuation is removed either.
    #[tokio::test]
    async fn a_kept_split_part_keeps_the_whole_set() {
        let dir = TempDir::new().unwrap();
        let parts: Vec<PathBuf> = ["part1", "part2", "part3"]
            .iter()
            .map(|n| dir.path().join(format!("game_{n}.wud")))
            .collect();
        for part in &parts {
            std::fs::write(part, b"part").unwrap();
        }
        let unit = DatUnit::File(parts[0].clone());
        let scanned: Vec<DatUnit> = parts.iter().cloned().map(DatUnit::File).collect();
        let mut claims = source_claim_counts(&scanned);
        // Another scanned unit also claims part1.
        *claims
            .get_mut(&FileIdentity::of(&parts[0]).unwrap())
            .unwrap() += 1;
        let (removed, failures, kept) = remove_sources(
            &unit,
            &unit_source_files(&unit),
            &[],
            &std::fs::canonicalize(dir.path()).unwrap(),
            &mut claims,
            false,
        )
        .await;
        assert!(removed.is_empty(), "{removed:?}");
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert!(
            parts.iter().all(|part| part.exists()),
            "the whole set stays"
        );
    }

    /// A nested clean backup that does not exist yet is resolved through
    /// its deepest existing ancestor: a second written dir's walk never
    /// re-collects what the first dir's clean already backed up. The output
    /// dir is spelled through a symlinked prefix, so the raw and canonical
    /// spellings of the backup differ on every unix host.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_backup_dir_inside_a_written_dir_is_not_recollected_across_dirs() {
        let lib = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        std::fs::create_dir_all(base.path().join("real")).unwrap();
        let out = base.path().join("out");
        std::os::unix::fs::symlink(base.path().join("real"), &out).unwrap();
        std::fs::write(lib.path().join("Game.gba"), gba_bytes()).unwrap();
        std::fs::write(lib.path().join("Game.gcm"), gcm_bytes()).unwrap();
        let gba_dir = out.join("Game Boy Advance");
        let gc_dir = out.join("GameCube");
        std::fs::create_dir_all(&gba_dir).unwrap();
        std::fs::create_dir_all(&gc_dir).unwrap();
        std::fs::write(gba_dir.join("Old.zip"), b"old gba").unwrap();
        std::fs::write(gc_dir.join("Old.rvz"), b"old gc").unwrap();
        let backup = gc_dir.join(".bak");

        let mut req = organize_request(lib.path(), Some(out.as_path()), false);
        req.options.clean = Some(true);
        req.options.clean_backup = Some(backup.clone());
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let clean_rows: Vec<&OrganizeRow> = data
            .rows
            .iter()
            .filter(|row| row.action == "clean")
            .collect();
        // Exactly two stale files, each backed up once and never twice.
        assert_eq!(clean_rows.len(), 2, "{clean_rows:?}");
        for row in &clean_rows {
            let output = row.output.as_ref().expect("a backup row names its output");
            assert!(output.exists(), "{output:?} must exist: {row:?}");
        }
    }

    /// A dry run previews the refusal the real run makes: a corrupt
    /// foreign file under the input, past `--max-depth`, under
    /// overwrite-invalid.
    #[tokio::test]
    async fn a_dry_run_previews_the_under_input_refusal() {
        let lib = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Game.gba"), gba_bytes()).unwrap();
        let out = lib.path().join("out");
        let target = out.join("Game Boy Advance");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("Game.zip"), b"corrupt").unwrap();

        for dry_run in [true, false] {
            let mut req = organize_request(lib.path(), Some(out.as_path()), dry_run);
            req.options.on_conflict = Some("overwrite-invalid".to_string());
            req.options.max_depth = Some(1);
            let response = organize(req, &RecordingProgress::default(), CancelToken::new())
                .await
                .unwrap();
            let Some(RunData::Organize(data)) = response.data else {
                panic!("expected organize data");
            };
            let row = row_for(&data, "Game.gba");
            assert_eq!(row.status, FileStatus::Failed, "dry_run={dry_run} {row:?}");
            assert!(
                row.detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("under the input directory")),
                "dry_run={dry_run} {row:?}"
            );
            assert_eq!(std::fs::read(target.join("Game.zip")).unwrap(), b"corrupt");
        }
    }

    /// A conversion whose desired path this run already realized reports
    /// the same keep row in a dry run and a real run.
    #[tokio::test]
    async fn a_realized_conversion_keep_row_matches_across_dry_and_real() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Game.gcm"), gcm_bytes()).unwrap();
        let plan = UnitPlan {
            index: 0,
            source: lib.path().join("Game.gcm"),
            source_ext: "gcm".to_string(),
            action: Action::Convert {
                op: "dol.compress",
                ext: "rvz".to_string(),
                format: None,
            },
            tokens: crate::util::TemplateTokens::new(None, &lib.path().join("Game.gcm"), "rvz"),
            input_subdir: PathBuf::new(),
            game: None,
            header: None,
            strip: 0,
            trim: None,
            pad: 0,
            pad_fill: 0,
            patch: None,
            desired: Some(out.path().join("Game.rvz")),
            decision: Decision::Keep,
            staging_error: None,
            output_error: None,
            match_error: None,
            member: None,
        };
        let mut rows = Vec::new();
        for dry_run in [true, false] {
            let req = organize_request(lib.path(), Some(out.path()), dry_run);
            let progress = RecordingProgress::default();
            let row = execute_unit(
                &req,
                &plan,
                None,
                &DatUnit::File(lib.path().join("Game.gcm")),
                &unit_source_files(&DatUnit::File(lib.path().join("Game.gcm"))),
                ZipFormat::TorrentZip,
                None,
                &WriteGuards::never(out.path().to_path_buf()),
                Some(RefusalCause::Realized),
                &progress,
                &progress,
                &CancelToken::new(),
            )
            .await
            .unwrap();
            rows.push(row);
        }
        for row in &rows {
            assert_eq!(row.status, FileStatus::Skipped, "{row:?}");
            assert_eq!(
                row.detail.as_deref(),
                Some("kept: this run already wrote the output")
            );
        }
        assert!(!out.path().join("Game.rvz").exists(), "nothing was written");
    }
    /// True when this scratch dir's volume folds case: `Probe.tmp` is
    /// reachable as `probe.tmp`. The probe file is removed again.
    fn case_insensitive_volume(dir: &Path) -> bool {
        std::fs::write(dir.join("Probe.tmp"), b"").unwrap();
        let folds = dir.join("probe.tmp").exists();
        std::fs::remove_file(dir.join("Probe.tmp")).unwrap();
        folds
    }

    /// On a case-insensitive volume the converter's lowercase names reach
    /// an uppercase set, so the set keeps every part in its real on-disk
    /// spelling and release and exclusion name the real files. A
    /// case-sensitive volume has no such set: the converter reads only
    /// lowercase names there.
    #[test]
    fn a_split_set_keeps_its_on_disk_spelling() {
        let dir = TempDir::new().unwrap();
        if !case_insensitive_volume(dir.path()) {
            eprintln!("skipped: this volume is case-sensitive");
            return;
        }
        for name in ["GAME_PART1.WUD", "GAME_PART2.WUD", "GAME_PART3.WUD"] {
            std::fs::write(dir.path().join(name), b"part").unwrap();
        }
        let files = unit_source_files(&DatUnit::File(dir.path().join("GAME_PART1.WUD")));
        let names: Vec<String> = files
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            ["GAME_PART1.WUD", "GAME_PART2.WUD", "GAME_PART3.WUD"]
        );
    }

    /// The set is exactly what the converter opens: a case twin of a
    /// continuation (`GAME_PART2.WUD` beside `game_part2.wud`) is a
    /// different file the converter never reads, so it is never part of
    /// the set and `--move` never deletes it. Applies to case-sensitive
    /// volumes only, where the twin can exist.
    #[tokio::test]
    async fn a_case_twin_of_a_continuation_is_never_released() {
        let lib = TempDir::new().unwrap();
        if case_insensitive_volume(lib.path()) {
            eprintln!("skipped: this volume is case-insensitive, twins cannot exist");
            return;
        }
        for name in [
            "game_part1.wud",
            "game_part2.wud",
            "game_part3.wud",
            "GAME_PART2.WUD",
            "Game_Part2.Wud",
            "game_PART2.wud",
        ] {
            std::fs::write(lib.path().join(name), name.as_bytes()).unwrap();
        }
        let part1 = lib.path().join("game_part1.wud");
        let files = unit_source_files(&DatUnit::File(part1.clone()));
        assert_eq!(
            files,
            vec![
                part1.clone(),
                lib.path().join("game_part2.wud"),
                lib.path().join("game_part3.wud"),
            ],
            "the set holds the exact names the converter opens"
        );
        let mut claims = HashMap::new();
        let (removed, failures, kept) = remove_sources(
            &DatUnit::File(part1.clone()),
            &files,
            &[],
            &std::fs::canonicalize(lib.path()).unwrap(),
            &mut claims,
            false,
        )
        .await;
        assert!(
            failures.is_empty() && kept.is_empty(),
            "{failures:?} {kept:?}"
        );
        assert_eq!(removed.len(), 3);
        for twin in ["GAME_PART2.WUD", "Game_Part2.Wud", "game_PART2.wud"] {
            assert!(
                lib.path().join(twin).exists(),
                "{twin} was never read, so it is never deleted"
            );
        }
    }

    /// An excluded continuation excludes the whole set, so `--move` never
    /// deletes it: nothing runs, the run finishes with a note and exit ok,
    /// and every part stays.
    #[tokio::test]
    async fn moving_never_deletes_an_excluded_split_part() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        for name in ["game_part1.wud", "game_part2.wud", "game_part3.wud"] {
            std::fs::write(lib.path().join(name), b"part").unwrap();
        }
        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.move_source = Some(true);
        req.options.input_exclude = Some(vec!["**/game_part2.wud".to_string()]);
        let progress = RecordingProgress::default();
        let response = organize(req, &progress, CancelToken::new()).await.unwrap();
        assert!(response.ok, "{response:?}");
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert!(data.rows.is_empty(), "{:?}", data.rows);
        assert!(
            warnings(&progress)
                .iter()
                .any(|message| message.contains("matches input_exclude")),
            "{:?}",
            warnings(&progress)
        );
        for name in ["game_part1.wud", "game_part2.wud", "game_part3.wud"] {
            assert!(lib.path().join(name).exists(), "{name} must survive");
        }
    }

    /// A dry run under `--move` applies every release gate and touches
    /// nothing: a cue set (cue plus bin) keeps both files, and its planned
    /// row is a conversion.
    #[tokio::test]
    async fn a_dry_run_move_never_deletes_a_cue_set() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Game.bin"), vec![0u8; 2048 * 4]).unwrap();
        std::fs::write(
            lib.path().join("Game.cue"),
            "FILE \"Game.bin\" BINARY\n  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n",
        )
        .unwrap();
        let mut req = organize_request(lib.path(), Some(out.path()), true);
        req.options.move_source = Some(true);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        let row = row_for(&data, "Game.cue");
        assert_eq!(row.status, FileStatus::Ok, "{row:?}");
        assert!(row.planned, "{row:?}");
        assert!(lib.path().join("Game.cue").exists(), "the cue survives");
        assert!(lib.path().join("Game.bin").exists(), "the bin survives");
    }

    /// The dry run reports what the real run reports for a move: a plain
    /// copy is relabelled `move`, and a kept multi-entry archive carries
    /// its note and stays a `copy`.
    #[tokio::test]
    async fn a_dry_run_move_reports_the_real_relabel_and_notes() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Game.gba"), gba_bytes()).unwrap();
        let mut zip =
            zip::ZipWriter::new(std::fs::File::create(lib.path().join("Pack.zip")).unwrap());
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("Other.gba", opts).unwrap();
        zip.write_all(&gba_bytes()).unwrap();
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("notes.txt", opts).unwrap();
        zip.write_all(b"sidecar").unwrap();
        zip.finish().unwrap();

        let mut rows = Vec::new();
        for dry_run in [true, false] {
            let mut req = organize_request(lib.path(), Some(out.path()), dry_run);
            req.options.move_source = Some(true);
            req.options.zip_exclude = Some("**/*.zip".to_string());
            let response = organize(req, &RecordingProgress::default(), CancelToken::new())
                .await
                .unwrap();
            let Some(RunData::Organize(data)) = response.data else {
                panic!("expected organize data");
            };
            rows.push((
                row_for(&data, "Game.gba").clone(),
                row_for(&data, "Pack.zip").clone(),
            ));
        }
        let ((dry_game, dry_pack), (real_game, real_pack)) = (&rows[0], &rows[1]);
        assert_eq!(dry_game.action, "move", "{dry_game:?}");
        assert_eq!(dry_game.action, real_game.action);
        assert_eq!(dry_pack.action, "copy", "{dry_pack:?}");
        assert_eq!(dry_pack.action, real_pack.action);
        for pack in [dry_pack, real_pack] {
            assert!(
                pack.detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("holds 2 entries")),
                "{pack:?}"
            );
        }
    }

    /// The dry run matches the real run row for row under every conflict
    /// policy where an earlier unit releases the path a later unit
    /// desires: `lib/Game.gba` writes `Game Boy Advance/Game.zip`, which
    /// the one-member `Game.zip` unit frees under `--move`.
    #[tokio::test]
    async fn a_dry_run_matches_the_real_run_across_policies_for_a_released_path() {
        for policy in [
            None,
            Some("skip"),
            Some("rename"),
            Some("overwrite-invalid"),
        ] {
            let lib = TempDir::new().unwrap();
            std::fs::write(lib.path().join("Game.gba"), gba_bytes()).unwrap();
            let console = lib.path().join("Game Boy Advance");
            std::fs::create_dir_all(&console).unwrap();
            let mut zip =
                zip::ZipWriter::new(std::fs::File::create(console.join("Game.zip")).unwrap());
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("Other.gba", opts).unwrap();
            zip.write_all(&gba_bytes()).unwrap();
            zip.finish().unwrap();

            let mut outcomes = Vec::new();
            for dry_run in [true, false] {
                let mut req = organize_request(lib.path(), Some(lib.path()), dry_run);
                req.options.move_source = Some(true);
                req.options.on_conflict = policy.map(str::to_string);
                let response = organize(req, &RecordingProgress::default(), CancelToken::new())
                    .await
                    .unwrap();
                let Some(RunData::Organize(data)) = response.data else {
                    panic!("expected organize data");
                };
                let row = row_for(&data, "Game.gba");
                outcomes.push((row.status, row.output.clone(), data.failed));
            }
            assert_eq!(
                outcomes[0].0, outcomes[1].0,
                "policy {policy:?}: {outcomes:?}"
            );
            assert_eq!(outcomes[0].1, outcomes[1].1, "policy {policy:?}");
            assert_eq!(outcomes[0].2, outcomes[1].2, "policy {policy:?}");
        }
    }

    /// A folded tail can land on an existing symlink:
    /// `missing/../link/out` with `link -> in/deep` is `in/deep/out` once
    /// the link is resolved, so it overlaps `in`.
    #[cfg(unix)]
    #[test]
    fn a_link_reached_again_through_a_dotdot_tail_is_resolved() {
        let base = TempDir::new().unwrap();
        let input = base.path().join("in");
        std::fs::create_dir_all(input.join("deep")).unwrap();
        std::os::unix::fs::symlink(input.join("deep"), base.path().join("link")).unwrap();
        let spelled = base
            .path()
            .join("missing")
            .join("..")
            .join("link")
            .join("out");
        assert!(paths_overlap(&input, &spelled));
    }

    /// A dry run plans a conversion whose desired path is a source this run
    /// released as a new output (a real run finds nothing there); the same
    /// path not released is what the child sees on disk.
    #[tokio::test]
    async fn a_dry_run_converts_onto_a_released_path_as_new() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let source = lib.path().join("Game.gcm");
        std::fs::write(&source, gcm_bytes()).unwrap();
        let desired = out.path().join("Game.rvz");
        std::fs::write(&desired, b"an old source").unwrap();
        let plan = UnitPlan {
            index: 0,
            source: source.clone(),
            source_ext: "gcm".to_string(),
            action: Action::Convert {
                op: "dol.compress",
                ext: "rvz".to_string(),
                format: None,
            },
            tokens: crate::util::TemplateTokens::new(None, &source, "rvz"),
            input_subdir: PathBuf::new(),
            game: None,
            header: None,
            strip: 0,
            trim: None,
            pad: 0,
            pad_fill: 0,
            patch: None,
            desired: Some(desired.clone()),
            decision: Decision::Keep,
            staging_error: None,
            output_error: None,
            match_error: None,
            member: None,
        };
        let req = organize_request(lib.path(), Some(out.path()), true);
        let unit = DatUnit::File(source.clone());
        let mut rows = Vec::new();
        for release in [false, true] {
            let mut guards = WriteGuards::never(lib.path().to_path_buf());
            if release {
                guards.release(vec![desired.clone()]);
            }
            let progress = RecordingProgress::default();
            let row = execute_unit(
                &req,
                &plan,
                None,
                &unit,
                &unit_source_files(&unit),
                ZipFormat::TorrentZip,
                None,
                &guards,
                None,
                &progress,
                &progress,
                &CancelToken::new(),
            )
            .await
            .unwrap();
            rows.push(row);
        }
        assert_ne!(
            rows[0].detail.as_deref(),
            Some("new"),
            "unreleased, the child sees the file on disk: {:?}",
            rows[0]
        );
        assert_eq!(rows[1].status, FileStatus::Ok, "{:?}", rows[1]);
        assert_eq!(rows[1].detail.as_deref(), Some("new"), "{:?}", rows[1]);
        assert_eq!(rows[1].output.as_deref(), Some(desired.as_path()));
        assert_eq!(
            std::fs::read(&desired).unwrap(),
            b"an old source",
            "a dry run writes nothing"
        );
    }

    /// A DAT-degraded unit with a matching patch keeps its source under
    /// `--move` and says so on every row, base and patched alike.
    #[tokio::test]
    async fn a_degraded_unit_with_a_patch_keeps_its_source_with_a_note() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let patches = TempDir::new().unwrap();
        let bytes = gba_bytes();
        std::fs::write(lib.path().join("Game.gba"), &bytes).unwrap();
        ips_for(&patches, "Hack", &bytes);

        let server = crate::dat::client::mock_api::spawn(move |_request_line, _request_body| {
            "not json".to_string()
        });
        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.dat = Some(true);
        req.options.api_base = Some(server.url().to_string());
        req.options.move_source = Some(true);
        req.options.patch = Some(vec![patches.path().to_path_buf()]);
        let response = organize(req, &RecordingProgress::default(), CancelToken::new())
            .await
            .unwrap();
        drop(server);
        let Some(RunData::Organize(data)) = response.data else {
            panic!("expected organize data");
        };
        assert!(lib.path().join("Game.gba").exists(), "the source is kept");
        let unit_rows: Vec<&OrganizeRow> = data
            .rows
            .iter()
            .filter(|row| row.action != "clean" && row.action != "playlist")
            .collect();
        assert!(unit_rows.len() >= 2, "base and patched rows: {unit_rows:?}");
        for row in unit_rows {
            assert!(
                row.detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("source kept: DAT match failed")),
                "{row:?}"
            );
        }
    }
}
