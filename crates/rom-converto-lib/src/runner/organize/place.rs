//! The organize placement writers: zip, copy, and link. Everything funnels
//! through `prepare_output` for conflict handling, then writes the payload.

#[cfg(test)]
use super::WriteGuards;
use super::ext_of;
use super::plan::{Action, UnitPlan, action_label};
use super::skip_row;
use crate::dat::units::DatUnit;
use crate::runner::models::{OrganizeRow, RunOptions, RunRequest, VerifyVerdict};
use crate::runner::ops::{conflict_policy, elapsed_ms, preflight_space};
use crate::runner::{invalid_arg, is_cancelled_error};
use crate::util::{
    CancelToken, Cancelled, ConflictPolicy, FileStatus, OutputExists, OutputVerify, PlanDecision,
    ProgressReporter, VerifyOutcome, ZipFormat, ZipMember, atomic_write, scratch_output_path,
    validate_torrentzip, verify_existing_output, write_torrentzip,
};
use anyhow::{Context, Result};
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

/// How `link_mode` places copy-action outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LinkMode {
    Hard,
    Sym { relative: bool },
    Ref,
}

/// Parses the `link_mode` option; `symlink` cannot be combined with
/// `move_source` (the link would outlive its deleted target).
pub(super) fn link_mode(options: &RunOptions) -> Result<Option<LinkMode>> {
    let Some(mode) = options.link_mode.as_deref() else {
        return Ok(None);
    };
    let mode = match mode {
        "hardlink" => LinkMode::Hard,
        "symlink" => {
            if options.move_source == Some(true) {
                return Err(invalid_arg(
                    "link_mode \"symlink\" cannot be combined with move_source",
                ));
            }
            LinkMode::Sym {
                relative: options.symlink_relative == Some(true),
            }
        }
        "reflink" => LinkMode::Ref,
        other => {
            return Err(invalid_arg(format!(
                "invalid link_mode {other:?}; expected \"hardlink\", \"symlink\" or \"reflink\""
            )));
        }
    };
    Ok(Some(mode))
}

/// Parses the `zip_format` option; TorrentZip is the default.
pub(super) fn zip_format(options: &RunOptions) -> Result<ZipFormat> {
    match options.zip_format.as_deref() {
        None => Ok(ZipFormat::TorrentZip),
        Some(value) => ZipFormat::parse(value).ok_or_else(|| {
            invalid_arg(format!(
                "invalid zip_format {value:?}; expected \"torrentzip\" or \"rvzstd\""
            ))
        }),
    }
}

/// Places a unit by zipping, copying, or linking it, with the same conflict
/// and dry-run semantics as `prepare_output`. Source deletion is the
/// execute loop's job: it fires only once every plan of a unit succeeded.
/// `consume_source` marks `source` as an extraction only this plan reads, so
/// a plain copy moves it into place instead of copying its bytes.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_place(
    req: &RunRequest,
    plan: &UnitPlan,
    unit: &DatUnit,
    own_sources: &[PathBuf],
    source: &Path,
    consume_source: bool,
    demote_reason: Option<&'static str>,
    zip_format: ZipFormat,
    link_mode: Option<LinkMode>,
    guards: &super::WriteGuards<'_>,
    progress: &dyn ProgressReporter,
    file_progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<OrganizeRow> {
    let unit_started = Instant::now();
    let dry_run = req.dry_run;
    let primary = unit.display_path().to_path_buf();
    let console = plan.tokens.console.clone();
    let input_bytes = unit.size_bytes();
    let policy = conflict_policy(req)?;

    // `zip_exclude` demotions already happened at
    // plan time in `resolve_paths`: `plan.action` and `plan.desired` carry
    // the final verb and output path.
    let desired = plan
        .desired
        .as_ref()
        .expect("Keep plan without a resolved output path");
    // A demoted link places as a copy: linked sources must be plain files.
    let action = if demote_reason.is_some() {
        &Action::Copy
    } else {
        &plan.action
    };
    let action_str = action_label(action);

    // Dry runs need the digest for `overwrite-invalid` only when an existing
    // unreleased output will be compared; real runs keep the existing behavior.
    let verify_after = !dry_run && req.options.verify_after == Some(true);
    let conflict_digest = policy == ConflictPolicy::OverwriteInvalid
        && (!dry_run || (desired.exists() && !guards.is_released(desired)));
    let needs_digest = match action {
        Action::Link => conflict_digest,
        _ => verify_after || conflict_digest,
    };
    let (source_crc, source_size) = if needs_digest {
        match source_digest(source, plan.strip, plan.pad, plan.pad_fill, cancel).await {
            Ok((crc, size)) => (Some(crc), Some(size)),
            Err(err) if is_cancelled_error(&err) => return Err(err),
            Err(err) => {
                return Ok(super::failed_row(
                    &primary,
                    console,
                    action_str,
                    super::plan::error_detail(&err),
                    input_bytes,
                    unit_started,
                    dry_run,
                ));
            }
        }
    } else {
        (None, None)
    };
    let verify_target = match (action, source_crc, source_size) {
        (_, None, _) | (_, _, None) => OutputVerify::None,
        (Action::Zip, Some(crc), Some(size)) => OutputVerify::Zip {
            format: zip_format,
            crc,
            size,
            member: zip_member_name(plan, desired),
        },
        (_, Some(crc), Some(size)) => OutputVerify::Raw { crc, size },
    };

    // `prepare_output` owns the conflict handling end to end: under
    // `overwrite-invalid` it verifies the existing output against this
    // payload and reports a kept valid output through the flag, so the row
    // below can name the verified output the way a dry run's plan does.
    let mut detail = demote_reason.map(str::to_string);
    let prepared = crate::runner::ops::prepare_output_quiet(
        progress,
        req,
        &primary,
        desired,
        &action_str,
        verify_target.clone(),
        cancel,
        guards.is_released(desired),
    )
    .await;
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(err) if is_cancelled_error(&err) => return Err(err),
        Err(err) if OutputExists::in_chain(&err) => {
            progress.warn(&err.to_string());
            return Ok(skip_row(
                &primary,
                console,
                "output exists",
                Some(desired.clone()),
                input_bytes,
                unit_started,
                dry_run,
            ));
        }
        Err(err) => {
            return Ok(super::failed_row(
                &primary,
                console,
                action_str,
                super::plan::error_detail(&err),
                input_bytes,
                unit_started,
                dry_run,
            ));
        }
    };
    if let Some(mut line) = prepared.line {
        // A dry run shows the refusal the real run would make: a write the
        // guards would refuse (a corrupt foreign file under the input, a
        // path this run already realized) previews as the failure, never
        // as a rewrite or a new file.
        if matches!(
            line.decision,
            PlanDecision::RewriteInvalid | PlanDecision::New
        ) && let Some(row) = guards.refuse(plan, own_sources, input_bytes, unit_started, dry_run)
        {
            return Ok(row);
        }
        line.input = primary.clone();
        let (status, detail) = super::plan_outcome(&line.decision);
        return Ok(OrganizeRow {
            input: primary,
            output: Some(line.output),
            console,
            action: action_str,
            status,
            planned: dry_run,
            verify: None,
            detail,
            input_bytes,
            output_bytes: 0,
            elapsed_ms: elapsed_ms(unit_started),
        });
    }
    if prepared.kept_valid {
        // The existing output verified valid: the row is a skip naming the
        // kept output, so playlists re-plan and clean keeps the file.
        return Ok(skip_row(
            &primary,
            console,
            super::VERIFIED_VALID,
            Some(desired.clone()),
            input_bytes,
            unit_started,
            dry_run,
        ));
    }
    let Some(output) = prepared.output else {
        return Ok(skip_row(
            &primary,
            console,
            "output exists",
            Some(desired.clone()),
            input_bytes,
            unit_started,
            dry_run,
        ));
    };
    // Under `overwrite-invalid` a valid existing output was kept above, so
    // a write this far down is a rewrite of an invalid or unrelated file:
    // the write guards still get the final say before it lands.
    if let Some(row) = guards.refuse(plan, own_sources, input_bytes, unit_started, dry_run) {
        return Ok(row);
    }
    // The guards passed: announce the rewrite the quiet `prepare_output`
    // withheld, only when an existing output verified invalid.
    if prepared.rewrite_invalid {
        log::info!(
            "Rewriting, output failed verification: {}",
            desired.display()
        );
    }
    // Conversions and copies can inflate a file well past its input size;
    // refuse the write when the output volume lacks room, per unit, unless
    // the run skipped the check. The payload is what lands: the source's
    // size minus a stripped header plus trim padding. Links write nothing.
    if *action != Action::Link && req.options.skip_space_check != Some(true) {
        let payload_bytes = std::fs::metadata(source)
            .map(|meta| meta.len())
            .unwrap_or(input_bytes)
            .saturating_sub(plan.strip)
            .saturating_add(plan.pad);
        if let Err(err) = preflight_space(output.parent().unwrap_or(&output), payload_bytes) {
            return Ok(super::failed_row(
                &primary,
                console,
                action_str,
                super::plan::error_detail(&err),
                input_bytes,
                unit_started,
                dry_run,
            ));
        }
    }
    let written = match action {
        Action::Zip => {
            if torrentzip_passthrough(plan, zip_format, &primary, desired).await {
                let copied = copy_passthrough(&primary, &output, cancel).await;
                if copied.is_ok() {
                    detail = Some("already torrentzip".to_string());
                }
                copied
            } else {
                write_zip_output(
                    plan,
                    zip_format,
                    source,
                    desired,
                    &output,
                    file_progress,
                    cancel,
                )
                .await
            }
        }
        Action::Copy if consume_source && plan.strip == 0 && plan.pad == 0 => {
            move_output(source, &output, cancel).await
        }
        Action::Copy => {
            copy_output(source, &output, plan.strip, plan.pad, plan.pad_fill, cancel).await
        }
        Action::Link => {
            let mode = link_mode.expect("Link plans imply a parsed link_mode");
            link_output(source, &output, mode).await
        }
        Action::Convert { .. } => unreachable!("conversions are dispatched to run_conversion"),
    };
    if let Err(err) = written {
        if is_cancelled_error(&err) {
            return Err(err);
        }
        return Ok(super::failed_row(
            &primary,
            console,
            action_str,
            super::plan::error_detail(&err),
            input_bytes,
            unit_started,
            dry_run,
        ));
    }
    // `verify_after` re-checks the freshly written output with the same
    // integrity check `overwrite-invalid` uses; a mismatch fails the row.
    // Links write nothing of their own, so there is nothing to verify.
    if verify_after && !matches!(action, Action::Link) {
        match verify_existing_output(progress, &output, verify_target, cancel.clone()).await {
            Ok(VerifyOutcome::Valid) => {}
            Ok(VerifyOutcome::Invalid) => {
                return Ok(OrganizeRow {
                    input: primary,
                    output: Some(output.clone()),
                    console,
                    action: action_str,
                    status: FileStatus::Failed,
                    planned: dry_run,
                    verify: Some(VerifyVerdict::Failed),
                    detail: Some(format!(
                        "{}: written output does not match the source",
                        super::VERIFY_FAILED_PREFIX
                    )),
                    input_bytes,
                    output_bytes: crate::util::fs::file_len(&output),
                    elapsed_ms: elapsed_ms(unit_started),
                });
            }
            // The check never produced a verdict: the freshly written
            // output could not be read back. The row fails so the source is
            // never released, and the detail names the cause.
            Ok(VerifyOutcome::Unverified(cause)) => {
                return Ok(OrganizeRow {
                    input: primary,
                    output: Some(output.clone()),
                    console,
                    action: action_str,
                    status: FileStatus::Failed,
                    planned: dry_run,
                    verify: Some(VerifyVerdict::Unverified),
                    detail: Some(format!("verify error: {cause}")),
                    input_bytes,
                    output_bytes: crate::util::fs::file_len(&output),
                    elapsed_ms: elapsed_ms(unit_started),
                });
            }
            Err(err) if is_cancelled_error(&err) || cancel.is_cancelled() => return Err(err),
            Err(err) => {
                return Ok(OrganizeRow {
                    input: primary,
                    output: Some(output.clone()),
                    console,
                    action: action_str,
                    status: FileStatus::Failed,
                    planned: dry_run,
                    verify: Some(VerifyVerdict::Unverified),
                    // The verify could not run: a transient error, not a bad
                    // output, so the path keeps its clean protection.
                    detail: Some(format!("verify error: {err:#}")),
                    input_bytes,
                    output_bytes: crate::util::fs::file_len(&output),
                    elapsed_ms: elapsed_ms(unit_started),
                });
            }
        }
    }
    // A stripped console header is worth naming on the row: the output's
    // payload differs from the source file by the skipped header bytes.
    if plan.strip > 0
        && let Some(header) = &plan.header
    {
        let note = format!("header stripped: {}", header.kind);
        detail = Some(match detail.take() {
            Some(existing) => format!("{existing}; {note}"),
            None => note,
        });
    }
    Ok(OrganizeRow {
        input: primary,
        output: Some(output.clone()),
        console,
        action: action_str,
        status: FileStatus::Ok,
        planned: dry_run,
        // A link writes nothing of its own, so verify_after checks nothing
        // for it; every other placement that passed above was verified.
        verify: (verify_after && !matches!(action, Action::Link))
            .then_some(VerifyVerdict::Verified),
        detail,
        input_bytes,
        output_bytes: crate::util::fs::file_len(&output),
        elapsed_ms: elapsed_ms(unit_started),
    })
}

/// The member name a zip placement writes: the planned stem with the source's
/// extension, or the headerless target extension when a header is stripped.
fn zip_member_name(plan: &UnitPlan, desired: &Path) -> String {
    format!(
        "{}.{}",
        desired
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output"),
        plan.payload_ext()
    )
}

/// Byte sink counting every chunk it absorbs, so the size a digest pair
/// reports is the size that was actually streamed.
struct CountingSink<S> {
    inner: S,
    count: u64,
}

impl<S: crate::util::hash::PumpSink> crate::util::hash::PumpSink for CountingSink<S> {
    fn absorb(&mut self, chunk: &[u8]) -> std::io::Result<()> {
        self.inner.absorb(chunk)?;
        self.count += chunk.len() as u64;
        Ok(())
    }
}

/// Byte sink writing every chunk through to the output file.
struct WriterSink<'a>(&'a mut dyn std::io::Write);

impl crate::util::hash::PumpSink for WriterSink<'_> {
    fn absorb(&mut self, chunk: &[u8]) -> std::io::Result<()> {
        self.0.write_all(chunk)
    }
}

/// Streams `source` through CRC-32 (ISO-HDLC) with the plan's strip and pad
/// applied (the payload a zip member or copy carries), returning the digest
/// and size an output must match. One pass, no cache.
async fn source_digest(
    source: &Path,
    strip: u64,
    pad: u64,
    fill: u8,
    cancel: &CancelToken,
) -> Result<(u32, u64)> {
    let src = source.to_path_buf();
    let cancel = cancel.clone();
    tokio::task::spawn_blocking(move || {
        let mut input =
            std::fs::File::open(&src).with_context(|| format!("opening {}", src.display()))?;
        let mut sink = CountingSink {
            inner: crate::util::hash::CRC32.digest(),
            count: 0,
        };
        crate::util::hash::pump_skip_pad(
            &mut input,
            &mut sink,
            strip,
            pad,
            fill,
            &crate::util::NoProgress,
            &cancel,
        )?;
        Ok((sink.inner.finalize(), sink.count))
    })
    .await
    .context("hashing source")?
}

/// True when the unit's own `.zip` input can be placed byte-for-byte instead
/// of re-zipping: it validates in the requested format, holds exactly the one
/// member this plan would write under its planned name, and needs no header
/// strip or trim padding. A patched plan never passes through: its
/// output is a different artifact than the input archive.
async fn torrentzip_passthrough(
    plan: &UnitPlan,
    format: ZipFormat,
    primary: &Path,
    desired: &Path,
) -> bool {
    if plan.patch.is_some()
        || plan.strip != 0
        || plan.pad > 0
        || !ext_of(primary).eq_ignore_ascii_case("zip")
    {
        return false;
    }
    let member_name = zip_member_name(plan, desired);
    let path = primary.to_path_buf();
    tokio::task::spawn_blocking(move || {
        matches!(
            validate_torrentzip(&path),
            Ok(Some((fmt, entries)))
                if fmt == format && entries.len() == 1 && entries[0].name == member_name
        )
    })
    .await
    .unwrap_or(false)
}

/// Places an already-structured zip byte-for-byte with `atomic_write`, so a
/// failed copy leaves any existing output untouched.
async fn copy_passthrough(input: &Path, output: &Path, cancel: &CancelToken) -> Result<()> {
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    let src = input.to_path_buf();
    let dst = output.to_path_buf();
    tokio::task::spawn_blocking(move || {
        atomic_write(&dst, true, |file| {
            let mut input = std::fs::File::open(&src)?;
            std::io::copy(&mut input, file)?;
            Ok::<(), anyhow::Error>(())
        })
    })
    .await
    .context("copying already-torrentzip input")?
    .context("copying already-torrentzip input")?;
    Ok(())
}

/// Writes `source` as the single member of a new structured zip at `output`.
/// The member keeps the planned stem (under the headerless extension when a
/// console header is stripped), even when a conflict renamed the output file.
async fn write_zip_output(
    plan: &UnitPlan,
    format: ZipFormat,
    source: &Path,
    desired: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<()> {
    let member = ZipMember {
        name: zip_member_name(plan, desired),
        path: source.to_path_buf(),
        skip: plan.strip,
        pad: plan.pad,
        fill: plan.pad_fill,
    };
    let dst = output.to_path_buf();
    let cancel = cancel.clone();
    crate::util::spawn_blocking_with_progress(progress, move |progress| {
        write_torrentzip(&member, &dst, format, progress, &cancel)
    })
    .await
    .map(|_entry| ())
}

/// Copies `source` to `output` through a scratch sibling and rename, in
/// chunks so cancellation lands between them; the scratch file is removed on
/// any failure or cancellation. `skip` bytes are omitted from the head of the
/// source and `pad` bytes of `fill` appended, for header stripping and trim
/// padding.
async fn copy_output(
    source: &Path,
    output: &Path,
    skip: u64,
    pad: u64,
    fill: u8,
    cancel: &CancelToken,
) -> Result<()> {
    let src = source.to_path_buf();
    let dst = output.to_path_buf();
    let cancel = cancel.clone();
    tokio::task::spawn_blocking(move || {
        atomic_write(&dst, true, |file| {
            let mut input = std::fs::File::open(&src)?;
            let mut sink = WriterSink(file);
            crate::util::hash::pump_skip_pad(
                &mut input,
                &mut sink,
                skip,
                pad,
                fill,
                &crate::util::NoProgress,
                &cancel,
            )?;
            Ok::<(), anyhow::Error>(())
        })
    })
    .await
    .context("copying output")??;
    Ok(())
}

/// Moves a disposable `source` to `output`: a rename when both sit on one
/// volume, else a [`copy_output`].
async fn move_output(source: &Path, output: &Path, cancel: &CancelToken) -> Result<()> {
    if std::fs::rename(source, output).is_ok() {
        return Ok(());
    }
    copy_output(source, output, 0, 0, 0, cancel).await
}

/// Places a filesystem link at `output` pointing at `source`. The link is
/// created under a scratch sibling name (the same naming `scratch_output_path`
/// uses for writers) and `rename`d over `output`, so an existing output is
/// never removed before the new link is known to succeed; a failed link
/// creation leaves it untouched.
async fn link_output(source: &Path, output: &Path, mode: LinkMode) -> Result<()> {
    let src = source.to_path_buf();
    let dst = output.to_path_buf();
    tokio::task::spawn_blocking(move || {
        // Scratch writers create the output's parent for themselves; links
        // bring their own.
        if let Some(parent) = dst.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        // `scratch_output_path` pre-creates an empty placeholder to reserve
        // the name; link syscalls refuse an existing destination, so it is
        // removed again right before the link takes its place.
        let scratch = scratch_output_path(&dst)?;
        std::fs::remove_file(&scratch)?;
        let created = match mode {
            LinkMode::Hard => {
                // The link must reference the file itself, not a symlink in
                // its place: the source is resolved to its target first, so
                // the output never dangles when the input link goes away.
                let target = std::fs::canonicalize(&src).unwrap_or_else(|_| src.clone());
                std::fs::hard_link(&target, &scratch).context("hard linking output")
            }
            LinkMode::Sym { relative } => {
                let target = if relative {
                    relative_target(&src, &dst)
                } else {
                    // `absolute` keeps the spelling lexical: canonicalizing
                    // would resolve intermediate symlinks the link should
                    // keep pointing through.
                    std::path::absolute(&src).unwrap_or_else(|_| src.clone())
                };
                #[cfg(unix)]
                {
                    std::os::unix::fs::symlink(&target, &scratch).context("symlinking output")
                }
                #[cfg(windows)]
                {
                    std::os::windows::fs::symlink_file(&target, &scratch)
                        .context("symlinking output")
                }
            }
            LinkMode::Ref => reflink_copy::reflink(&src, &scratch).context("reflinking output"),
        };
        if let Err(err) = created {
            let _ = std::fs::remove_file(&scratch);
            return Err(err);
        }
        std::fs::rename(&scratch, &dst).context("placing linked output")?;
        Ok(())
    })
    .await
    .context("linking output")??;
    Ok(())
}

/// A pathdiff-free relative path from the link's directory to `target`,
/// built from common-prefix stripping alone. Both paths are absolutized
/// first (an output dir spelled relatively would otherwise share no prefix
/// with an absolute source and produce a nonsense `../..` chain), and the
/// `..` chain is computed from the link directory's canonical form,
/// falling back to its absolute spelling when it does not exist yet, so a
/// link behind a symlinked output dir still resolves.
fn relative_target(target: &Path, link: &Path) -> PathBuf {
    let target = std::path::absolute(target).unwrap_or_else(|_| target.to_path_buf());
    let link = std::path::absolute(link).unwrap_or_else(|_| link.to_path_buf());
    let base = link.parent().unwrap_or(Path::new(".")).to_path_buf();
    // The chain must count the hops the kernel actually takes, so a
    // symlinked output dir is resolved here. The target's spelling stays
    // lexical: canonicalizing it would resolve intermediate symlinks the
    // link should keep pointing through. Both sides go through
    // `without_verbatim_prefix` so a canonical `\\?\C:\...` (or verbatim
    // UNC) base and a plain target compare in one prefix form.
    let base = without_verbatim_prefix(std::fs::canonicalize(&base).unwrap_or(base));
    let target = without_verbatim_prefix(target);
    let base: Vec<Component> = base.components().collect();
    let target_parts: Vec<Component> = target.components().collect();
    let common = base
        .iter()
        .zip(target_parts.iter())
        .take_while(|(b, t)| b == t)
        .count();
    if common == 0 {
        // Windows only: paths on different drives share no prefix at all,
        // and no `..` chain can cross a drive boundary, so the link keeps
        // the absolute target. Unix absolute paths always share the root.
        return target;
    }
    let mut relative = PathBuf::new();
    for _ in &base[common..] {
        relative.push("..");
    }
    for component in &target_parts[common..] {
        relative.push(component.as_os_str());
    }
    relative
}

/// Rewrites a verbatim path to its plain form so a canonical and a plain
/// spelling of one location share a path prefix: `\\?\C:\dir` (what
/// `canonicalize` returns on Windows) becomes `C:\dir`, and
/// `\\?\UNC\server\share\dir` becomes `\\server\share\dir`. A
/// mapped network drive canonicalizes to its UNC share while the target
/// keeps its drive letter, so such a link stays absolute. Any other path,
/// and every path off Windows, is returned unchanged.
#[cfg(windows)]
fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    let plain = {
        let text = path.to_string_lossy();
        text.strip_prefix(r"\\?\").and_then(|rest| {
            if let Some(unc) = rest.strip_prefix(r"UNC\") {
                Some(PathBuf::from(format!(r"\\{unc}")))
            } else if rest.as_bytes().get(1) == Some(&b':') {
                Some(PathBuf::from(rest))
            } else {
                None
            }
        })
    };
    plain.unwrap_or(path)
}

#[cfg(not(windows))]
fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    path
}

#[cfg(test)]
mod tests {
    use super::super::plan::Decision;
    use super::*;
    use crate::runner::RecordingProgress;
    use crate::runner::models::{RunData, RunResponse};
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

    fn rows_of(response: RunResponse) -> Vec<OrganizeRow> {
        match response.data {
            Some(RunData::Organize(data)) => data.rows,
            other => panic!("expected organize data, got {other:?}"),
        }
    }

    fn row_for<'a>(rows: &'a [OrganizeRow], name: &str) -> &'a OrganizeRow {
        rows.iter()
            .find(|row| {
                row.input
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy() == name)
            })
            .unwrap_or_else(|| panic!("no row for {name}"))
    }

    /// An input zip that is already a torrentzip of exactly the planned member
    /// is placed byte-for-byte instead of re-zipped.
    #[tokio::test]
    async fn torrentzip_passthrough_copies_bytes_identically() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        // The zip's source payload is built outside the library so its plain
        // .gba never competes with the zip for the same output path.
        let scratch = TempDir::new().unwrap();
        let raw = scratch.path().join("Test Game.gba");
        std::fs::write(&raw, gba_bytes()).unwrap();
        let zip_path = lib.path().join("Test Game.zip");
        write_torrentzip(
            &ZipMember {
                name: "Test Game.gba".to_string(),
                path: raw,
                skip: 0,
                pad: 0,
                fill: 0,
            },
            &zip_path,
            ZipFormat::TorrentZip,
            &crate::util::NoProgress,
            &CancelToken::new(),
        )
        .unwrap();
        let input = std::fs::read(&zip_path).unwrap();

        let response = super::super::organize(
            organize_request(lib.path(), Some(out.path()), false),
            &RecordingProgress::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        let rows = rows_of(response);
        let row = row_for(&rows, "Test Game.zip");
        assert_eq!(row.status, FileStatus::Ok, "{:?}", row.detail);
        assert_eq!(row.action, "zip");
        assert_eq!(row.detail.as_deref(), Some("already torrentzip"));
        let output = row.output.clone().unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), input);
    }

    /// `overwrite-invalid` rewrites an existing zip output whose member CRC no
    /// longer matches the source payload.
    #[tokio::test]
    async fn overwrite_invalid_rewrites_a_stale_zip() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Test Game.gba"), gba_bytes()).unwrap();

        let response = super::super::organize(
            organize_request(lib.path(), Some(out.path()), false),
            &RecordingProgress::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        let zip_path = row_for(&rows_of(response), "Test Game.gba")
            .output
            .clone()
            .unwrap();

        // Replace the output with a structured zip of different content.
        let stale = out.path().join("stale.gba");
        std::fs::write(&stale, b"stale payload").unwrap();
        write_torrentzip(
            &ZipMember {
                name: "Test Game.gba".to_string(),
                path: stale,
                skip: 0,
                pad: 0,
                fill: 0,
            },
            &zip_path,
            ZipFormat::TorrentZip,
            &crate::util::NoProgress,
            &CancelToken::new(),
        )
        .unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.on_conflict = Some("overwrite-invalid".to_string());
        let response =
            super::super::organize(req, &RecordingProgress::default(), CancelToken::new())
                .await
                .unwrap();
        let rows = rows_of(response);
        let row = row_for(&rows, "Test Game.gba");
        assert_eq!(row.status, FileStatus::Ok, "{:?}", row.detail);

        // The rewritten output's member matches the source payload again.
        let (_, entries) = validate_torrentzip(&zip_path)
            .unwrap()
            .expect("structured zip");
        let expected = crate::util::hash::CRC32.checksum(&gba_bytes());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].crc, expected);
        assert_eq!(entries[0].size, 0xC0);
    }

    /// `overwrite-invalid` with `move_source` replaces an unrelated file
    /// sitting at the output path with the link, and only then removes the
    /// source: the placement, not the deletion, decides.
    #[cfg(unix)]
    #[tokio::test]
    async fn overwrite_invalid_relinks_an_unrelated_output_and_then_moves() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let bytes = gba_bytes();
        std::fs::write(lib.path().join("Game.xiso"), &bytes).unwrap();
        // A discovery run names the output path, which is then clobbered
        // with an unrelated file.
        let response = super::super::organize(
            organize_request(lib.path(), Some(out.path()), false),
            &RecordingProgress::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        let output = row_for(&rows_of(response), "Game.xiso")
            .output
            .clone()
            .unwrap();
        std::fs::write(&output, b"unrelated").unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.link_mode = Some("hardlink".to_string());
        req.options.move_source = Some(true);
        req.options.on_conflict = Some("overwrite-invalid".to_string());
        let response =
            super::super::organize(req, &RecordingProgress::default(), CancelToken::new())
                .await
                .unwrap();
        let rows = rows_of(response);
        let row = row_for(&rows, "Game.xiso");
        assert_eq!(row.status, FileStatus::Ok, "{:?}", row.detail);
        assert_eq!(row.action, "link");
        let output = row.output.clone().unwrap();
        assert_eq!(
            std::fs::read(&output).unwrap(),
            bytes,
            "the unrelated file must be replaced by the link"
        );
        // The output carries the source's payload while the source itself is
        // gone: the link was placed first, the removal came after.
        assert!(
            !lib.path().join("Game.xiso").exists(),
            "the source moved out once the link took its place"
        );
        assert!(std::fs::read_link(&output).is_err(), "a hardlink is a file");
    }

    /// A plain skip policy keeps both the unrelated output and the source.
    #[cfg(unix)]
    #[tokio::test]
    async fn skip_policy_keeps_the_source_when_the_output_is_unrelated() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Game.xiso"), gba_bytes()).unwrap();
        let response = super::super::organize(
            organize_request(lib.path(), Some(out.path()), false),
            &RecordingProgress::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        let output = row_for(&rows_of(response), "Game.xiso")
            .output
            .clone()
            .unwrap();
        std::fs::write(&output, b"unrelated").unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.link_mode = Some("hardlink".to_string());
        req.options.move_source = Some(true);
        req.options.on_conflict = Some("skip".to_string());
        let response =
            super::super::organize(req, &RecordingProgress::default(), CancelToken::new())
                .await
                .unwrap();
        let rows = rows_of(response);
        let row = row_for(&rows, "Game.xiso");
        assert_eq!(row.status, FileStatus::Skipped, "{:?}", row.detail);
        assert_eq!(row.detail.as_deref(), Some("output exists"));
        assert_eq!(std::fs::read(&output).unwrap(), b"unrelated");
        assert!(
            lib.path().join("Game.xiso").exists(),
            "a skip-policy skip keeps the input"
        );
    }

    /// `verify_after` re-verifies a freshly written zip and copy without a
    /// false failure; links write nothing and stay unverified.
    #[cfg(unix)]
    #[tokio::test]
    async fn verify_after_passes_fresh_writes_and_skips_links() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(lib.path().join("Test Game.gba"), gba_bytes()).unwrap();
        std::fs::write(lib.path().join("Game.xiso"), b"not a real xiso").unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.verify_after = Some(true);
        req.options.link_mode = Some("hardlink".to_string());
        let response =
            super::super::organize(req, &RecordingProgress::default(), CancelToken::new())
                .await
                .unwrap();
        let rows = rows_of(response);
        let zip_row = row_for(&rows, "Test Game.gba");
        assert_eq!(zip_row.status, FileStatus::Ok, "{:?}", zip_row.detail);
        assert_eq!(zip_row.action, "zip");
        assert_eq!(zip_row.detail.as_deref(), None);
        let link_row = row_for(&rows, "Game.xiso");
        assert_eq!(link_row.status, FileStatus::Ok, "{:?}", link_row.detail);
        assert_eq!(link_row.action, "link");
    }

    /// A freshly written output the verify cannot read back fails the row
    /// with the cause: an unverified check must never pass a placement, or
    /// a bad write would release its source.
    #[cfg(unix)]
    #[tokio::test]
    async fn verify_after_fails_an_unreadable_fresh_output() {
        use std::os::unix::fs::PermissionsExt;

        /// Strips read permission from the output the moment the writer's
        /// progress events are relayed back: the writer's own handle keeps
        /// the write working, and the events land before `verify_after`
        /// opens the published file.
        struct Blindfold(PathBuf);
        impl Blindfold {
            fn blind(&self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o000));
            }
        }
        impl ProgressReporter for Blindfold {
            fn start(&self, _: u64, _: &str) {
                self.blind();
            }
            fn inc(&self, _: u64) {
                self.blind();
            }
            fn finish(&self) {
                self.blind();
            }
        }

        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let source = lib.path().join("Test Game.gba");
        std::fs::write(&source, gba_bytes()).unwrap();
        let desired = out.path().join("Test Game.zip");

        let plan = UnitPlan {
            index: 0,
            source: source.clone(),
            source_ext: "gba".to_string(),
            action: Action::Zip,
            tokens: crate::util::template::TemplateTokens::new(None, &source, "zip"),
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
        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.verify_after = Some(true);

        let progress = RecordingProgress::default();
        let guards = WriteGuards::never(out.path().to_path_buf());
        let row = run_place(
            &req,
            &plan,
            &DatUnit::File(source.clone()),
            std::slice::from_ref(&source),
            &source,
            false,
            None,
            ZipFormat::TorrentZip,
            None,
            &guards,
            &progress,
            &Blindfold(desired.clone()),
            &CancelToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(row.status, FileStatus::Failed, "{row:?}");
        let detail = row.detail.expect("the row names the cause");
        assert!(
            detail.starts_with("verify error: "),
            "the detail names the verify cause: {detail}"
        );
        assert_eq!(row.output.as_deref(), Some(desired.as_path()));
        // The unreadable output stays on disk; releasing the source is the
        // execute loop's job and a failed row never triggers it.
        assert!(desired.exists());
    }

    /// `zip_exclude` demotes the zip to a copy that keeps the source's
    /// extension and raw payload.
    #[tokio::test]
    async fn zip_exclude_places_a_copy_instead_of_a_zip() {
        let lib = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let bytes = gba_bytes();
        std::fs::write(lib.path().join("Test Game.gba"), &bytes).unwrap();

        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.zip_exclude = Some("**/*.zip".to_string());
        let response =
            super::super::organize(req, &RecordingProgress::default(), CancelToken::new())
                .await
                .unwrap();
        let rows = rows_of(response);
        let row = row_for(&rows, "Test Game.gba");
        assert_eq!(row.status, FileStatus::Ok, "{:?}", row.detail);
        assert_eq!(row.action, "copy");
        let output = row.output.clone().unwrap();
        assert_eq!(output.extension().and_then(|e| e.to_str()), Some("gba"));
        assert_eq!(std::fs::read(&output).unwrap(), bytes);
    }

    /// A failed link creation (here, a source that no longer exists)
    /// leaves whatever already sat at the output path completely untouched:
    /// the scratch sibling is cleaned up and the destination is never
    /// cleared before the link is known to succeed.
    #[tokio::test]
    async fn link_output_leaves_the_existing_file_untouched_on_failure() {
        let dir = TempDir::new().unwrap();
        let missing_source = dir.path().join("does-not-exist.rom");
        let output = dir.path().join("game.rom");
        std::fs::write(&output, b"existing output").unwrap();

        let err = link_output(&missing_source, &output, LinkMode::Hard)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("hard linking output"), "{err}");
        assert_eq!(std::fs::read(&output).unwrap(), b"existing output");
        // No scratch sibling leaks into the output directory.
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("game.rom")]);
    }

    /// `relative_target` absolutizes both paths first: a link dir spelled
    /// relative to the working directory shares no literal prefix with an
    /// absolute target, and stripping without absolutizing would emit a
    /// nonsense `../..` chain to the filesystem root.
    #[test]
    fn relative_target_absolutizes_both_paths_first() {
        let cwd = std::env::current_dir().unwrap();
        let target = cwd.join("lib").join("Game.gba");
        let relative = relative_target(&target, Path::new("out/sub/link"));
        assert_eq!(relative, PathBuf::from("../../lib/Game.gba"));
    }

    /// The `..` chain starts at the link directory's canonical form, so a
    /// link reached through a symlinked output dir still resolves to the
    /// target.
    #[cfg(unix)]
    #[test]
    fn relative_target_resolves_behind_a_symlinked_output_dir() {
        let lib = TempDir::new().unwrap();
        let real_out = TempDir::new().unwrap();
        let target = lib.path().join("Game.gba");
        std::fs::write(&target, gba_bytes()).unwrap();
        std::fs::create_dir_all(real_out.path().join("sub")).unwrap();
        let out_link = lib.path().join("out");
        std::os::unix::fs::symlink(real_out.path(), &out_link).unwrap();

        let relative = relative_target(&target, &out_link.join("sub").join("Game.gba"));
        assert!(relative.is_relative(), "the chain stays relative");
        // The link's parent resolves to the real output dir, so joining the
        // chain onto it must land on the target.
        let base = std::fs::canonicalize(out_link.join("sub")).unwrap();
        assert_eq!(
            std::fs::canonicalize(base.join(&relative)).unwrap(),
            std::fs::canonicalize(&target).unwrap()
        );
    }

    /// Paths on different drives share no prefix at all: no `..` chain can
    /// cross a drive boundary, so the link keeps the absolute target.
    #[cfg(windows)]
    #[test]
    fn relative_target_falls_back_to_absolute_across_drives() {
        let relative = relative_target(Path::new(r"D:\lib\Game.gba"), Path::new(r"C:\out\link"));
        assert_eq!(relative, PathBuf::from(r"D:\lib\Game.gba"));
    }

    /// An existing link dir canonicalizes to a verbatim (`\\?\C:\...`)
    /// path on Windows while the target keeps its plain spelling: both
    /// sides share the drive, so the link is still relative rather than
    /// falling back to the absolute target.
    #[cfg(windows)]
    #[test]
    fn relative_target_stays_relative_for_an_existing_same_drive_link_dir() {
        let dir = TempDir::new().unwrap();
        let root = without_verbatim_prefix(std::fs::canonicalize(dir.path()).unwrap());
        std::fs::create_dir_all(root.join("out")).unwrap();
        let relative = relative_target(
            &root.join("lib").join("Game.gba"),
            &root.join("out").join("link"),
        );
        assert_eq!(relative, Path::new("..").join("lib").join("Game.gba"));
    }

    /// A patched plan never passes its input zip through byte-for-byte: the
    /// patched ROM is a different artifact than the archived source.
    #[tokio::test]
    async fn torrentzip_passthrough_rejects_patched_plans() {
        let lib = TempDir::new().unwrap();
        let scratch = TempDir::new().unwrap();
        let raw = scratch.path().join("Test Game.gba");
        std::fs::write(&raw, gba_bytes()).unwrap();
        let zip_path = lib.path().join("Test Game.zip");
        write_torrentzip(
            &ZipMember {
                name: "Test Game.gba".to_string(),
                path: raw.clone(),
                skip: 0,
                pad: 0,
                fill: 0,
            },
            &zip_path,
            ZipFormat::TorrentZip,
            &crate::util::NoProgress,
            &CancelToken::new(),
        )
        .unwrap();

        let desired = lib.path().join("Test Game.zip");
        let mut unpatched = place_plan(0);
        unpatched.patch = None;
        assert!(
            torrentzip_passthrough(&unpatched, ZipFormat::TorrentZip, &zip_path, &desired).await
        );

        let mut patched = place_plan(0);
        patched.patch = Some(zip_path.clone());
        assert!(
            !torrentzip_passthrough(&patched, ZipFormat::TorrentZip, &zip_path, &desired).await
        );
    }

    /// A padded placement writes exactly the padding bytes the plan names:
    /// 0xFF filler lands as 0xFF in a copy, in the zip member, and in the
    /// payload `verify_after` checks.
    #[tokio::test]
    async fn padded_placements_write_the_planned_fill_byte() {
        let lib = TempDir::new().unwrap();
        let source = lib.path().join("Test Game.gba");
        std::fs::write(&source, gba_bytes()).unwrap();

        // A copy: the tail is 0xFF and verify_after passes against it.
        let out = TempDir::new().unwrap();
        let desired = out.path().join("Test Game.gba");
        let mut plan = place_plan(0);
        plan.action = Action::Copy;
        plan.pad = 16;
        plan.pad_fill = 0xFF;
        plan.desired = Some(desired.clone());
        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.verify_after = Some(true);
        let progress = RecordingProgress::default();
        let guards = WriteGuards::never(out.path().to_path_buf());
        let row = run_place(
            &req,
            &plan,
            &DatUnit::File(source.clone()),
            std::slice::from_ref(&source),
            &source,
            false,
            None,
            ZipFormat::TorrentZip,
            None,
            &guards,
            &progress,
            &progress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(row.status, FileStatus::Ok, "{row:?}");
        assert_eq!(row.verify, Some(VerifyVerdict::Verified));
        let written = std::fs::read(&desired).unwrap();
        assert_eq!(written.len(), gba_bytes().len() + 16);
        assert!(
            written[gba_bytes().len()..]
                .iter()
                .all(|byte| *byte == 0xFF)
        );

        // A zip member carries the same padded payload, and the archive
        // verifies against it.
        let out = TempDir::new().unwrap();
        let desired = out.path().join("Test Game.zip");
        let mut plan = place_plan(0);
        plan.pad = 16;
        plan.pad_fill = 0xFF;
        plan.desired = Some(desired.clone());
        let mut req = organize_request(lib.path(), Some(out.path()), false);
        req.options.verify_after = Some(true);
        let progress = RecordingProgress::default();
        let guards = WriteGuards::never(out.path().to_path_buf());
        let row = run_place(
            &req,
            &plan,
            &DatUnit::File(source.clone()),
            std::slice::from_ref(&source),
            &source,
            false,
            None,
            ZipFormat::TorrentZip,
            None,
            &guards,
            &progress,
            &progress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(row.status, FileStatus::Ok, "{row:?}");
        assert_eq!(row.verify, Some(VerifyVerdict::Verified));
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&desired).unwrap()).unwrap();
        let mut member = archive.by_name("Test Game.gba").unwrap();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut member, &mut bytes).unwrap();
        assert_eq!(bytes.len(), gba_bytes().len() + 16);
        assert!(bytes[gba_bytes().len()..].iter().all(|byte| *byte == 0xFF));
    }

    /// A minimal zip-action plan for a plain `.gba` source.
    fn place_plan(index: usize) -> UnitPlan {
        UnitPlan {
            index,
            source: PathBuf::from("Test Game.gba"),
            source_ext: "gba".to_string(),
            action: Action::Zip,
            tokens: crate::util::TemplateTokens::new(None, Path::new("Test Game.gba"), "zip"),
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
    /// A dry run under overwrite-invalid previews the refusal the real run
    /// makes for a path this run already wrote (a brand-new path, so the
    /// dry run's own decision is `New`): the row fails with the guard's
    /// reason instead of planning a second write.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dry_run_placement_refuses_a_path_this_run_already_wrote() {
        let lib = TempDir::new().unwrap();
        let source = lib.path().join("Test Game.gba");
        std::fs::write(&source, gba_bytes()).unwrap();
        let out = TempDir::new().unwrap();
        std::fs::create_dir_all(out.path().join("real")).unwrap();
        std::os::unix::fs::symlink(out.path().join("real"), out.path().join("link")).unwrap();

        let mut plan = place_plan(0);
        plan.desired = Some(out.path().join("link").join("Test Game.zip"));
        let mut req = organize_request(lib.path(), Some(out.path()), true);
        req.options.on_conflict = Some("overwrite-invalid".to_string());
        let mut guards = WriteGuards::never(lib.path().to_path_buf());
        guards.policy = ConflictPolicy::OverwriteInvalid;
        guards.realized(&out.path().join("real").join("Test Game.zip"));
        let progress = RecordingProgress::default();
        let row = run_place(
            &req,
            &plan,
            &DatUnit::File(source.clone()),
            std::slice::from_ref(&source),
            &source,
            false,
            None,
            ZipFormat::TorrentZip,
            None,
            &guards,
            &progress,
            &progress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(row.status, FileStatus::Failed, "{row:?}");
        assert!(
            row.detail
                .as_deref()
                .is_some_and(|detail| detail.contains("this run already wrote it")),
            "{row:?}"
        );
    }
}
