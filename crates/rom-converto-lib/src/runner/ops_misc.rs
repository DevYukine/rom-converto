//! Cue, NTR, NX, CTR and ROM patch handlers.

use super::invalid_arg;
use super::models::{RunData, RunRequest, RunResponse, WupTitleInputOption};
use super::ops::{
    ConvertTarget, convert_op, cso_format, dir_op, nx_keys_for_run, output_or, preflight_space,
    required_input, skipped_already_done,
};
use super::organize::{FileIdentity, location_under_root};
use crate::nintendo::ntr::NtrError;
use crate::nintendo::nx::{NxError, NxMergeFormat};
use crate::util::fs::file_len;
use crate::util::{CancelToken, Cancelled, OutputVerify, ProgressReporter};
use anyhow::{Context, Result};
use std::path::PathBuf;

/// Containers a merge reads, plain or as an archive member.
const MERGE_EXTS: &[&str] = &["nsp", "xci"];

/// The scan that collects one game folder's inputs, shared with the
/// recursive run so both agree on what a game is.
pub(crate) fn game_scan(max_depth: Option<usize>) -> crate::util::InputScan<'static> {
    crate::util::InputScan {
        exts: MERGE_EXTS,
        member_exts: None,
        max_depth,
        skip_unpacked_twins: true,
    }
}

pub(crate) async fn cue_to_iso(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            skip_if_already: None,
            input: &input,
            derive: &|basis, _| basis.with_extension("iso"),
            operation: "cue.to_iso",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::disc::cue::to_iso::cue_to_iso(progress, input, output, true, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn cue_to_cso(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let format = cso_format(req.options.format.as_deref().unwrap_or("cso"))?;
    let input = required_input(&req)?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            skip_if_already: None,
            input: &input,
            derive: &|basis, _| basis.with_extension(format.extension()),
            operation: "cue.to_cso",
            verify: OutputVerify::Cso,
        },
        cancel,
        |input, output, cancel| async move {
            crate::pipeline::cue_to_cso(progress, input, output, format, true, cancel).await
        },
    )
    .await
}

pub(crate) async fn ntr_encrypt(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let result = convert_op(
        progress,
        &req,
        ConvertTarget {
            skip_if_already: None,
            input: &input,
            derive: &|basis, _| crate::nintendo::ntr::derive_encrypted_path(basis),
            operation: "ntr.encrypt",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::nintendo::ntr::encrypt_ntr_rom(progress, input, output, true, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await;
    ntr_already_done(result, &input, "ntr.encrypt", "already encrypted")
}

pub(crate) async fn ntr_decrypt(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let result = convert_op(
        progress,
        &req,
        ConvertTarget {
            skip_if_already: None,
            input: &input,
            derive: &|basis, _| crate::nintendo::ntr::derive_decrypted_path(basis),
            operation: "ntr.decrypt",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::nintendo::ntr::decrypt_ntr_rom(progress, input, output, true, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await;
    ntr_already_done(result, &input, "ntr.decrypt", "already decrypted")
}

/// Applies one patch to a ROM while keeping both inputs untouched.
pub(crate) async fn patch_apply(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let patch_path = req
        .options
        .patch_file
        .clone()
        .ok_or_else(|| invalid_arg("patch file is required"))?;
    if crate::util::is_archive_path(&input) {
        return Err(invalid_arg(
            "archive inputs are not supported, extract the ROM first",
        ));
    }
    let open_path = patch_path.clone();
    let open_cancel = cancel.clone();
    let patch =
        tokio::task::spawn_blocking(move || crate::patch::Patch::open(&open_path, &open_cancel))
            .await??;
    let desired = output_or(&req, &input, || {
        crate::patch::patched_output_path(&input, &patch_path)
    })?;
    if let Ok(identity) = FileIdentity::probe(&desired) {
        for (path, kind) in [(&input, "input ROM"), (&patch_path, "patch file")] {
            if FileIdentity::probe(path).is_ok_and(|source_identity| source_identity == identity) {
                return Err(invalid_arg(format!(
                    "{} is the {kind}; writing the output there would destroy it",
                    desired.display()
                )));
            }
        }
    }
    convert_op(
        progress,
        &req,
        ConvertTarget {
            skip_if_already: None,
            input: &input,
            derive: &|basis, _| crate::patch::patched_output_path(basis, &patch_path),
            operation: "patch.apply",
            verify: OutputVerify::None,
        },
        cancel,
        |source, output, cancel| async move {
            tokio::task::spawn_blocking(move || patch.apply(&source, &output, &cancel)).await?
        },
    )
    .await
}

/// A ROM that is already in the target state, or has no secure area to work
/// on, is a skip rather than a failure.
fn ntr_already_done(
    result: Result<RunResponse>,
    input: &std::path::Path,
    operation: &str,
    already: &str,
) -> Result<RunResponse> {
    let Err(err) = result else {
        return result;
    };
    let reason = match err.downcast_ref::<NtrError>() {
        Some(NtrError::AlreadyEncrypted | NtrError::AlreadyDecrypted) => already,
        Some(NtrError::NoSecureArea) => "no secure area",
        Some(NtrError::TooSmall) => "too small for a secure area",
        _ => return Err(err),
    };
    Ok(skipped_already_done(input, operation, reason, &err))
}

/// Reads `options.inputs` as a path list, rejecting an absent or empty one.
fn option_inputs(req: &RunRequest) -> Result<Vec<PathBuf>> {
    req.options
        .inputs
        .as_ref()
        .map(|inputs| {
            inputs
                .iter()
                .map(|input| match input {
                    WupTitleInputOption::Path(path) | WupTitleInputOption::Object { path, .. } => {
                        path.clone()
                    }
                })
                .collect::<Vec<PathBuf>>()
        })
        .filter(|inputs| !inputs.is_empty())
        .ok_or_else(|| invalid_arg("options.inputs must not be empty"))
}

/// Stages every merge input, stopping before the next extraction once
/// `cancel` fires so Ctrl-C does not wait for each remaining archive.
fn stage_merge_inputs(
    inputs: &[PathBuf],
    cancel: &CancelToken,
) -> Result<Vec<crate::util::ResolvedInput>> {
    inputs
        .iter()
        .map(|path| {
            if cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            crate::util::resolve_input(path, MERGE_EXTS, cancel)
        })
        .collect()
}

pub(crate) async fn nx_merge(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    // A folder without an explicit list is one game: every NSP or XCI, or
    // archive holding one, under it. Its absolute path names the output,
    // since `.` has no file name.
    let (inputs, game) = match req.input.clone() {
        Some(folder) if req.options.inputs.is_none() && folder.is_dir() => {
            // Earlier merges in the output directory are not part of a game.
            let output_dir = req.options.output_dir.as_deref();
            if output_dir
                .and_then(|dir| FileIdentity::probe(dir).ok())
                .is_some_and(|out| FileIdentity::probe(&folder).is_ok_and(|id| id == out))
            {
                return Err(invalid_arg(format!(
                    "{} is the output directory, not a game folder",
                    folder.display()
                )));
            }
            let max_depth = req.options.max_depth;
            let scan_folder = folder.clone();
            let scan_cancel = cancel.clone();
            let mut inputs = tokio::task::spawn_blocking(move || {
                crate::util::collect_inputs(&scan_folder, &game_scan(max_depth), &scan_cancel)
            })
            .await?
            .with_context(|| format!("scanning {}", folder.display()))?;
            if let Some(out) = output_dir.and_then(|dir| dir.canonicalize().ok()) {
                inputs.retain(|input| !location_under_root(input, &out));
            }
            if inputs.is_empty() {
                return Err(invalid_arg(format!(
                    "no NSP or XCI files found in {}",
                    folder.display()
                )));
            }
            // `absolute` keeps a trailing `..`, which has no file name either.
            let named = std::path::absolute(&folder)?;
            let named = if named.file_name().is_some() {
                named
            } else {
                folder.canonicalize()?
            };
            (inputs, Some(named))
        }
        _ => (option_inputs(&req)?, None),
    };
    let format = match req.options.format.as_deref().unwrap_or("nsp") {
        "nsp" => NxMergeFormat::Nsp,
        "xci" => NxMergeFormat::Xci,
        other => return Err(invalid_arg(format!("invalid NX merge format {other:?}"))),
    };
    let (ext, media) = match format {
        NxMergeFormat::Nsp => ("nsp", "NSP"),
        NxMergeFormat::Xci => ("xci", "XCI"),
    };
    let (keys, missing_keys) = nx_keys_for_run(&req)?;
    let skip_space_check = req.options.skip_space_check.unwrap_or(false);
    // The merge has many inputs but one record; the first one names it.
    let mut req = req;
    let first = req.input.clone().unwrap_or_else(|| inputs[0].clone());
    req.input = Some(first.clone());
    // The staged total is the only honest preflight: convert_op would only
    // size the first input.
    req.options.skip_space_check = Some(true);
    let mut response = convert_op(
        progress,
        &req,
        ConvertTarget {
            skip_if_already: None,
            input: &first,
            derive: &|basis, _| {
                crate::nintendo::nx::derive_merged_path(game.as_deref().unwrap_or(basis), ext)
            },
            operation: "nx.merge",
            verify: OutputVerify::None,
        },
        cancel,
        |_input, output, cancel| async move {
            let stage_cancel = cancel.clone();
            let staged =
                tokio::task::spawn_blocking(move || stage_merge_inputs(&inputs, &stage_cancel))
                    .await??;
            if !skip_space_check {
                preflight_space(
                    output.parent().unwrap_or(&output),
                    staged.iter().map(|r| file_len(r.path())).sum(),
                )?;
            }
            // `staged` owns the temp dirs the merge reads from; it must
            // outlive the merge future.
            let paths = staged
                .iter()
                .map(|resolved| resolved.path().to_path_buf())
                .collect();
            crate::nintendo::nx::merge_containers_async(
                paths, output, format, keys, progress, cancel,
            )
            .await
            .map_err(anyhow::Error::from)
        },
    )
    .await?;
    if let Some(RunData::Plan(line)) = &mut response.data {
        line.media = Some(media.to_string());
        line.missing_keys = missing_keys;
    }
    Ok(response)
}

pub(crate) async fn nx_split(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = req
        .output
        .clone()
        .or_else(|| req.options.output_dir.clone())
        .unwrap_or_else(|| crate::nintendo::nx::derive_split_dir(&input));
    let (keys, missing_keys) = nx_keys_for_run(&req)?;
    // A single-title container is a skip, not a conversion. Counting titles
    // before staging and the space preflight keeps a plain NSP from being
    // extracted, or a full disk from turning the skip into an error; the
    // dry run settles it as a skipped record too. Undecidable inputs fall
    // through to the normal path, which surfaces the same error.
    if !crate::util::is_archive_path(&input) {
        match tokio::task::spawn_blocking({
            let input = input.clone();
            let keys = keys.clone();
            move || crate::nintendo::nx::plan_split(&input, &keys)
        })
        .await
        {
            // A split that proceeds replays the warnings itself; only the
            // skip path, which never reaches it, reports them here.
            Ok(Ok(plan)) if plan.is_single_title_skip() => {
                for warning in plan.warnings() {
                    progress.warn(warning);
                }
                return Ok(skipped_already_done(
                    &input,
                    "nx.split",
                    "single title",
                    &NxError::SingleTitle,
                ));
            }
            Ok(Ok(_)) => {}
            // Planning errors are re-raised by the split itself.
            Ok(Err(_)) => {}
            Err(join) => return Err(join.into()),
        }
    }
    let mut response = match dir_op(
        &req,
        &input,
        &desired,
        "nx.split",
        file_len,
        cancel,
        |source, output_dir, cancel| async move {
            let files = crate::nintendo::nx::split_container_async(
                source, output_dir, keys, progress, cancel,
            )
            .await?;
            Ok((files.iter().map(|p| file_len(p)).sum(), None))
        },
    )
    .await
    {
        Ok(response) => response,
        Err(err) => match err.downcast_ref::<NxError>() {
            Some(NxError::SingleTitle) => {
                return Ok(skipped_already_done(
                    &input,
                    "nx.split",
                    "single title",
                    &err,
                ));
            }
            _ => return Err(err),
        },
    };
    if let Some(RunData::Plan(line)) = &mut response.data {
        line.missing_keys = missing_keys;
    }
    Ok(response)
}

pub(crate) async fn ctr_bundle(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let inputs = option_inputs(&req)?;
    let plan = crate::nintendo::ctr::bundle::plan_bundle(&inputs)
        .map_err(|e| invalid_arg(e.to_string()))?;
    // Check the extension before planning so a dry run and the conflict
    // handling see the rejection too, not only the real write.
    if let Some(output) = &req.output {
        plan.check_output_extension(output)
            .map_err(|e| invalid_arg(e.to_string()))?;
    }
    // The bundle has many inputs but one record; the first one names it.
    let mut req = req;
    let first = req.input.clone().unwrap_or_else(|| inputs[0].clone());
    req.input = Some(first.clone());
    let (basis, kind) = (plan.members[0].clone(), plan.kind);
    let mut response = convert_op(
        progress,
        &req,
        ConvertTarget {
            skip_if_already: None,
            input: &first,
            derive: &|_, _| crate::nintendo::ctr::bundle::derive_bundle_path(&basis, kind),
            operation: "ctr.bundle",
            verify: OutputVerify::None,
        },
        cancel,
        |_, output, cancel| async move {
            crate::nintendo::ctr::bundle::bundle_async(plan, output, progress, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await?;
    if let Some(RunData::Plan(line)) = &mut response.data {
        line.media = Some(kind.ext().to_uppercase());
    }
    Ok(response)
}

pub(crate) async fn ctr_unbundle(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = req
        .output
        .clone()
        .or_else(|| req.options.output_dir.clone())
        .unwrap_or_else(|| crate::nintendo::ctr::bundle::derive_unbundle_dir(&input));
    let user_input = input.clone();
    dir_op(
        &req,
        &input,
        &desired,
        "ctr.unbundle",
        file_len,
        cancel,
        |source, output_dir, cancel| async move {
            let files = crate::nintendo::ctr::bundle::unbundle_async(
                source, user_input, output_dir, progress, cancel,
            )
            .await?;
            Ok((files.iter().map(|p| file_len(p)).sum(), None))
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::super::run_json;
    use crate::nintendo::nx::test_fixtures::{TEST_HEADER_KEY, TEST_KAK_APPLICATION_00};
    use crate::runner::models::ProgressEvent;
    use crate::util::CancelToken;
    use serde_json::json;

    /// The synthetic key set the test containers are built with, written as
    /// a keys file so the runner loads it like a user's `prod.keys`.
    fn write_keys_file(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("prod.keys");
        std::fs::write(
            &path,
            format!(
                "header_key = {}\nkey_area_key_application_00 = {}\n",
                hex::encode(TEST_HEADER_KEY),
                hex::encode(TEST_KAK_APPLICATION_00)
            ),
        )
        .unwrap();
        path
    }

    #[tokio::test]
    async fn single_title_nsp_is_a_skipped_record_with_plan_warnings() {
        use crate::nintendo::nx::models::cnmt::CNMT_TYPE_APPLICATION;
        use crate::nintendo::nx::test_fixtures::{build_meta_nca, build_test_nsp};

        let meta = build_meta_nca(
            0x0100_5151_0000_0000,
            0,
            CNMT_TYPE_APPLICATION,
            &[[0x77; 16]],
        );
        let files = vec![
            (format!("{}.cnmt.nca", hex::encode([0x66; 16])), meta),
            (
                format!("{}.nca", hex::encode([0x77; 16])),
                vec![0xAB; 0x800],
            ),
            // No CNMT references this NCA and its header is unreadable, so
            // planning warns about it; the warning must reach the run even
            // though the split itself is skipped.
            (format!("{}.nca", hex::encode([0x99; 16])), vec![0u8; 0x400]),
        ];
        let dir = tempfile::tempdir().unwrap();
        let nsp_path = dir.path().join("single.nsp");
        std::fs::write(&nsp_path, build_test_nsp(&files)).unwrap();
        let keys = write_keys_file(dir.path());
        let req = json!({
            "operation": "nx.split",
            "input": nsp_path,
            "options": { "keys": keys }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        assert_eq!(res.records.len(), 1, "{res:?}");
        assert_eq!(res.records[0].status, crate::util::FileStatus::Skipped);
        assert!(
            res.records[0]
                .error
                .as_deref()
                .is_some_and(|error| error.contains("single title")),
            "{res:?}"
        );
        assert!(
            res.events.iter().any(|event| matches!(
                event,
                ProgressEvent::Warn { message } if message.contains(&hex::encode([0x99; 16]))
            )),
            "{res:?}"
        );
        // Nothing was written: the skip leaves no split directory behind.
        assert!(!dir.path().join("single_split").exists());
    }

    #[tokio::test]
    async fn multi_title_split_reports_each_plan_warning_once() {
        use crate::nintendo::nx::models::cnmt::CNMT_TYPE_APPLICATION;
        use crate::nintendo::nx::test_fixtures::{build_meta_nca, build_test_nsp};

        let mut files = Vec::new();
        for n in [1u8, 2] {
            files.push((
                format!("{}.cnmt.nca", hex::encode([n; 16])),
                build_meta_nca(
                    0x0100_0000_0000_0000 + u64::from(n) * 0x1000,
                    0,
                    CNMT_TYPE_APPLICATION,
                    &[[0x10 + n; 16]],
                ),
            ));
            files.push((
                format!("{}.nca", hex::encode([0x10 + n; 16])),
                vec![0xAB; 0x800],
            ));
        }
        files.push((format!("{}.nca", hex::encode([0x99; 16])), vec![0u8; 0x400]));
        let dir = tempfile::tempdir().unwrap();
        let nsp = dir.path().join("multi.nsp");
        std::fs::write(&nsp, build_test_nsp(&files)).unwrap();
        let keys = write_keys_file(dir.path());
        let req = json!({
            "operation": "nx.split",
            "input": nsp,
            "options": { "keys": keys }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        let warned = res
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    ProgressEvent::Warn { message } if message.contains(&hex::encode([0x99; 16]))
                )
            })
            .count();
        assert_eq!(warned, 1, "{res:?}");
    }

    #[test]
    fn merge_staging_stops_once_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let inputs: Vec<std::path::PathBuf> = ["a.nsp", "b.nsp"]
            .iter()
            .map(|name| {
                let path = dir.path().join(name);
                std::fs::write(&path, b"nsp").unwrap();
                path
            })
            .collect();
        let token = CancelToken::new();
        assert_eq!(super::stage_merge_inputs(&inputs, &token).unwrap().len(), 2);
        token.cancel();
        let err = super::stage_merge_inputs(&inputs, &token).unwrap_err();
        assert!(crate::runner::is_cancelled_error(&err));
    }

    #[tokio::test]
    async fn nx_merge_stages_archive_inputs() {
        use crate::nintendo::nx::models::cnmt::CNMT_TYPE_APPLICATION;
        use crate::nintendo::nx::test_fixtures::{build_meta_nca, build_test_nsp};
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let keys = write_keys_file(dir.path());
        let mut inputs = Vec::new();
        for (n, title) in [(1u8, 0x0100_AAAA_0000_0000u64), (2, 0x0100_BBBB_0000_0000)] {
            let meta = build_meta_nca(title, 0, CNMT_TYPE_APPLICATION, &[[0x10 + n; 16]]);
            let nsp = build_test_nsp(&[
                (format!("{}.cnmt.nca", hex::encode([n; 16])), meta),
                (
                    format!("{}.nca", hex::encode([0x10 + n; 16])),
                    vec![0xAB; 0x800],
                ),
            ]);
            let archive = dir.path().join(format!("title{n}.zip"));
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
            let opts: zip::write::SimpleFileOptions = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file(format!("title{n}.nsp"), opts).unwrap();
            zip.write_all(&nsp).unwrap();
            zip.finish().unwrap();
            inputs.push(archive);
        }
        let output = dir.path().join("merged.nsp");
        let req = json!({
            "operation": "nx.merge",
            "input": inputs[0],
            "output": output,
            "options": { "inputs": inputs, "keys": keys }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        let merged = std::fs::read(&output).unwrap();
        let names = crate::nintendo::nx::models::Pfs0::read(&mut std::io::Cursor::new(&merged))
            .unwrap()
            .files
            .into_iter()
            .map(|file| file.name)
            .collect::<Vec<_>>();
        // Both archives' contents were staged and merged.
        assert_eq!(names.len(), 4, "{names:?}");
    }

    /// A one-title NSP: a meta NCA plus the program NCA it references.
    fn merge_fixture_nsp(
        meta_id: u8,
        content_id: u8,
        title_id: u64,
        title_type: u8,
        version: u32,
    ) -> Vec<u8> {
        use crate::nintendo::nx::test_fixtures::{build_meta_nca, build_test_nsp};
        build_test_nsp(&[
            (
                format!("{}.cnmt.nca", hex::encode([meta_id; 16])),
                build_meta_nca(title_id, version, title_type, &[[content_id; 16]]),
            ),
            (
                format!("{}.nca", hex::encode([content_id; 16])),
                vec![0xAB; 0x800],
            ),
        ])
    }

    /// `base/` with `Game A` (a valid base plus update), `Game B` (an NSP of
    /// garbage) and a valid loose `loose.nsp` that is not part of any game.
    fn merge_batch_base(dir: &std::path::Path) -> std::path::PathBuf {
        use crate::nintendo::nx::models::cnmt::{CNMT_TYPE_APPLICATION, CNMT_TYPE_PATCH};

        let base = dir.join("base");
        let game_a = base.join("Game A");
        let game_b = base.join("Game B");
        std::fs::create_dir_all(&game_a).unwrap();
        std::fs::create_dir_all(&game_b).unwrap();
        std::fs::write(
            game_a.join("base.nsp"),
            merge_fixture_nsp(1, 0x11, 0x0100_AAAA_0000_0000, CNMT_TYPE_APPLICATION, 0),
        )
        .unwrap();
        std::fs::write(
            game_a.join("update.nsp"),
            merge_fixture_nsp(2, 0x22, 0x0100_AAAA_0000_0800, CNMT_TYPE_PATCH, 1),
        )
        .unwrap();
        std::fs::write(game_b.join("broken.nsp"), vec![0xEE; 0x400]).unwrap();
        std::fs::write(
            base.join("loose.nsp"),
            merge_fixture_nsp(3, 0x33, 0x0100_CCCC_0000_0000, CNMT_TYPE_APPLICATION, 0),
        )
        .unwrap();
        base
    }

    fn all_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(all_files(&path));
            } else {
                out.push(path);
            }
        }
        out
    }

    fn assert_is_pfs0(path: &std::path::Path) {
        let blob = std::fs::read(path).unwrap();
        crate::nintendo::nx::models::Pfs0::read(&mut std::io::Cursor::new(&blob)).unwrap();
    }

    fn pfs0_entry_count(path: &std::path::Path) -> usize {
        let blob = std::fs::read(path).unwrap();
        crate::nintendo::nx::models::Pfs0::read(&mut std::io::Cursor::new(&blob))
            .unwrap()
            .files
            .len()
    }

    fn record_for<'a>(
        res: &'a crate::runner::models::RunResponse,
        game: &str,
    ) -> &'a crate::util::ReportRecord {
        let hits: Vec<_> = res
            .records
            .iter()
            .filter(|r| r.input_path.contains(game) || r.output_path.contains(game))
            .collect();
        assert_eq!(hits.len(), 1, "{game}: {res:?}");
        hits[0]
    }

    async fn run_merge_batch(
        base: &std::path::Path,
        keys: &std::path::Path,
        extra: serde_json::Value,
    ) -> crate::runner::models::RunResponse {
        let mut options = json!({ "recursive": true, "keys": keys });
        options
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let req = json!({ "operation": "nx.merge", "input": base, "options": options });
        run_json(&req.to_string(), CancelToken::new()).await
    }

    #[tokio::test]
    async fn nx_merge_recursive_merges_each_game_folder_and_isolates_failures() {
        let dir = tempfile::tempdir().unwrap();
        let keys = write_keys_file(dir.path());
        let base = merge_batch_base(dir.path());

        let res = run_merge_batch(&base, &keys, json!({})).await;

        assert_eq!(res.code, "partial_failure", "{res:?}");
        assert_eq!(res.records.len(), 2, "{res:?}");
        let merged = base.join("Game A (Merged).nsp");
        assert_is_pfs0(&merged);
        let a = record_for(&res, "Game A");
        assert_eq!(a.status, crate::util::FileStatus::Ok, "{res:?}");
        assert_eq!(
            record_for(&res, "Game B").status,
            crate::util::FileStatus::Failed,
            "{res:?}"
        );
        assert!(!base.join("Game B (Merged).nsp").exists());
        assert!(!base.join("loose (Merged).nsp").exists());
        assert!(
            all_files(dir.path())
                .iter()
                .all(|p| !p.to_string_lossy().contains("loose (Merged)")),
            "loose.nsp is not a game"
        );
    }

    #[tokio::test]
    async fn nx_merge_recursive_rerun_skips_games_already_merged() {
        let dir = tempfile::tempdir().unwrap();
        let keys = write_keys_file(dir.path());
        let base = merge_batch_base(dir.path());
        let first = run_merge_batch(&base, &keys, json!({})).await;
        let merged = base.join("Game A (Merged).nsp");
        assert_eq!(
            record_for(&first, "Game A").status,
            crate::util::FileStatus::Ok,
            "{first:?}"
        );
        let before = std::fs::read(&merged).unwrap();

        let second = run_merge_batch(&base, &keys, json!({})).await;

        assert_eq!(
            record_for(&second, "Game A").status,
            crate::util::FileStatus::Skipped,
            "{second:?}"
        );
        assert_eq!(std::fs::read(&merged).unwrap(), before);

        // Without the broken game, a rerun that only skips passes.
        std::fs::remove_dir_all(base.join("Game B")).unwrap();
        let third = run_merge_batch(&base, &keys, json!({})).await;

        assert!(third.ok, "{third:?}");
        assert_eq!(
            record_for(&third, "Game A").status,
            crate::util::FileStatus::Skipped,
            "{third:?}"
        );
        assert_eq!(std::fs::read(&merged).unwrap(), before);
    }

    #[tokio::test]
    async fn nx_merge_recursive_writes_into_output_dir_and_does_not_treat_it_as_a_game() {
        let dir = tempfile::tempdir().unwrap();
        let keys = write_keys_file(dir.path());
        let base = merge_batch_base(dir.path());
        let out = base.join("merged");
        std::fs::create_dir(&out).unwrap();
        std::fs::write(out.join("X (Merged).nsp"), b"earlier output").unwrap();

        let res = run_merge_batch(&base, &keys, json!({ "output_dir": out })).await;

        // Game A and Game B only: the output directory ran as no game.
        assert_eq!(res.records.len(), 2, "{res:?}");
        assert!(
            res.records
                .iter()
                .all(|r| !std::path::Path::new(&r.input_path).starts_with(&out)),
            "{res:?}"
        );
        assert_is_pfs0(&out.join("Game A (Merged).nsp"));
        assert!(!base.join("Game A (Merged).nsp").exists());
        assert!(
            all_files(dir.path())
                .iter()
                .all(|p| !p.to_string_lossy().contains("merged (Merged)")),
            "{res:?}"
        );
        assert_eq!(
            std::fs::read(out.join("X (Merged).nsp")).unwrap(),
            b"earlier output"
        );
    }

    #[tokio::test]
    async fn nx_merge_game_folder_input_merges_it_as_one_game() {
        use crate::nintendo::nx::models::cnmt::CNMT_TYPE_ADD_ON_CONTENT;

        let dir = tempfile::tempdir().unwrap();
        let keys = write_keys_file(dir.path());
        let base = merge_batch_base(dir.path());
        let dlc = base.join("Game A").join("DLC");
        std::fs::create_dir(&dlc).unwrap();
        std::fs::write(
            dlc.join("dlc.nsp"),
            merge_fixture_nsp(4, 0x44, 0x0100_AAAA_0000_1001, CNMT_TYPE_ADD_ON_CONTENT, 0),
        )
        .unwrap();
        let req = json!({
            "operation": "nx.merge",
            "input": base.join("Game A"),
            "options": { "keys": keys }
        });

        let res = run_json(&req.to_string(), CancelToken::new()).await;

        assert!(res.ok, "{res:?}");
        // Base, update and the nested DLC: two NCAs each.
        assert_eq!(pfs0_entry_count(&base.join("Game A (Merged).nsp")), 6);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn nx_merge_recursive_does_not_follow_a_linked_subfolder() {
        use crate::nintendo::nx::models::cnmt::CNMT_TYPE_APPLICATION;

        let dir = tempfile::tempdir().unwrap();
        let keys = write_keys_file(dir.path());
        let base = merge_batch_base(dir.path());
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::write(
            elsewhere.join("game.nsp"),
            merge_fixture_nsp(5, 0x55, 0x0100_DDDD_0000_0000, CNMT_TYPE_APPLICATION, 0),
        )
        .unwrap();
        std::os::unix::fs::symlink(&elsewhere, base.join("Linked")).unwrap();

        let res = run_merge_batch(&base, &keys, json!({})).await;

        assert_eq!(res.records.len(), 2, "{res:?}");
        assert!(
            res.records
                .iter()
                .all(|record| !record.input_path.contains("Linked")),
            "{res:?}"
        );
        assert!(!base.join("Linked (Merged).nsp").exists());
    }

    #[tokio::test]
    async fn nx_merge_recursive_report_has_one_row_per_game() {
        let dir = tempfile::tempdir().unwrap();
        let keys = write_keys_file(dir.path());
        let base = merge_batch_base(dir.path());
        let report = dir.path().join("report.csv");

        run_merge_batch(&base, &keys, json!({ "report": report })).await;

        let csv = std::fs::read_to_string(&report).unwrap();
        let rows: Vec<&str> = csv
            .lines()
            .filter(|line| line.contains(&*base.to_string_lossy()))
            .collect();
        assert_eq!(rows.len(), 2, "{csv}");
        for game in ["Game A", "Game B"] {
            assert!(rows.iter().any(|row| row.contains(game)), "{game}: {csv}");
        }
    }

    fn write_zip_member(path: &std::path::Path, name: &str, data: &[u8]) {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let opts: zip::write::SimpleFileOptions =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file(name, opts).unwrap();
        zip.write_all(data).unwrap();
        zip.finish().unwrap();
    }

    #[tokio::test]
    async fn nx_merge_recursive_ignores_a_folder_of_unrelated_archives() {
        let dir = tempfile::tempdir().unwrap();
        let keys = write_keys_file(dir.path());
        let base = merge_batch_base(dir.path());
        std::fs::remove_dir_all(base.join("Game B")).unwrap();
        let extras = base.join("Extras");
        std::fs::create_dir(&extras).unwrap();
        write_zip_member(&extras.join("manual.zip"), "manual.txt", b"no container");

        let res = run_merge_batch(&base, &keys, json!({})).await;

        assert!(res.ok, "{res:?}");
        assert_eq!(res.records.len(), 1, "{res:?}");
        assert_eq!(
            record_for(&res, "Game A").status,
            crate::util::FileStatus::Ok,
            "{res:?}"
        );
    }

    #[tokio::test]
    async fn nx_merge_recursive_merges_a_game_folder_holding_only_an_archive() {
        use crate::nintendo::nx::models::cnmt::CNMT_TYPE_APPLICATION;

        let dir = tempfile::tempdir().unwrap();
        let keys = write_keys_file(dir.path());
        let base = merge_batch_base(dir.path());
        std::fs::remove_dir_all(base.join("Game B")).unwrap();
        let zipped = base.join("Game Z");
        std::fs::create_dir(&zipped).unwrap();
        write_zip_member(
            &zipped.join("game.zip"),
            "game.nsp",
            &merge_fixture_nsp(6, 0x66, 0x0100_FFFF_0000_0000, CNMT_TYPE_APPLICATION, 0),
        );

        let res = run_merge_batch(&base, &keys, json!({})).await;

        assert!(res.ok, "{res:?}");
        assert_eq!(
            record_for(&res, "Game Z").status,
            crate::util::FileStatus::Ok,
            "{res:?}"
        );
        assert_eq!(pfs0_entry_count(&base.join("Game Z (Merged).nsp")), 2);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn nx_merge_recursive_skips_an_unreadable_folder() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let keys = write_keys_file(dir.path());
        let base = merge_batch_base(dir.path());
        std::fs::remove_dir_all(base.join("Game B")).unwrap();
        let locked = base.join("Locked");
        std::fs::create_dir(&locked).unwrap();
        let set_mode = |mode| {
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        set_mode(0o000);
        // Root reads it anyway, which leaves nothing to test.
        if std::fs::read_dir(&locked).is_ok() {
            set_mode(0o755);
            return;
        }

        let res = run_merge_batch(&base, &keys, json!({})).await;
        set_mode(0o755);

        assert!(res.ok, "{res:?}");
        assert_eq!(
            record_for(&res, "Locked").status,
            crate::util::FileStatus::Skipped,
            "{res:?}"
        );
        assert_eq!(
            record_for(&res, "Game A").status,
            crate::util::FileStatus::Ok,
            "{res:?}"
        );
    }

    #[tokio::test]
    async fn nx_merge_game_folder_that_is_the_output_dir_fails_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let keys = write_keys_file(dir.path());
        let base = merge_batch_base(dir.path());
        let game = base.join("Game A");
        let sorted_files = || {
            let mut files = all_files(dir.path());
            files.sort();
            files
        };
        let before = sorted_files();
        let req = json!({
            "operation": "nx.merge",
            "input": game,
            "options": { "keys": keys, "output_dir": game }
        });

        let res = run_json(&req.to_string(), CancelToken::new()).await;

        assert_eq!(res.code, "invalid_argument", "{res:?}");
        assert_eq!(sorted_files(), before);
    }

    #[tokio::test]
    async fn nx_merge_game_folder_does_not_read_an_output_dir_nested_in_it() {
        use crate::nintendo::nx::models::cnmt::CNMT_TYPE_APPLICATION;

        let dir = tempfile::tempdir().unwrap();
        let keys = write_keys_file(dir.path());
        let base = merge_batch_base(dir.path());
        let game = base.join("Game A");
        let out = game.join("merged");
        std::fs::create_dir(&out).unwrap();
        std::fs::write(
            out.join("Earlier (Merged).nsp"),
            merge_fixture_nsp(7, 0x77, 0x0100_EEEE_0000_0000, CNMT_TYPE_APPLICATION, 0),
        )
        .unwrap();
        let req = json!({
            "operation": "nx.merge",
            "input": game,
            "options": { "keys": keys, "output_dir": out }
        });

        let res = run_json(&req.to_string(), CancelToken::new()).await;

        assert!(res.ok, "{res:?}");
        // Base and update only: the earlier merge is not an input.
        assert_eq!(pfs0_entry_count(&out.join("Game A (Merged).nsp")), 4);
    }

    #[tokio::test]
    async fn nx_merge_game_folder_max_depth_one_reads_only_top_level_files() {
        use crate::nintendo::nx::models::cnmt::{CNMT_TYPE_APPLICATION, CNMT_TYPE_PATCH};

        let dir = tempfile::tempdir().unwrap();
        let keys = write_keys_file(dir.path());
        let game = dir.path().join("Game");
        std::fs::create_dir_all(game.join("Updates")).unwrap();
        std::fs::write(
            game.join("base.nsp"),
            merge_fixture_nsp(1, 0x11, 0x0100_AAAA_0000_0000, CNMT_TYPE_APPLICATION, 0),
        )
        .unwrap();
        std::fs::write(
            game.join("Updates").join("update.nsp"),
            merge_fixture_nsp(2, 0x22, 0x0100_AAAA_0000_0800, CNMT_TYPE_PATCH, 1),
        )
        .unwrap();
        let req = json!({
            "operation": "nx.merge",
            "input": game,
            "options": { "keys": keys, "max_depth": 1 }
        });

        let res = run_json(&req.to_string(), CancelToken::new()).await;

        assert!(res.ok, "{res:?}");
        // Only the base's meta and program NCA: the update one level down is
        // past the limit.
        assert_eq!(pfs0_entry_count(&dir.path().join("Game (Merged).nsp")), 2);
    }

    #[tokio::test]
    async fn misc_ops_dry_run_plans() {
        let dir = tempfile::tempdir().unwrap();
        let cue = dir.path().join("game.cue");
        let nds = dir.path().join("game.nds");
        let nsp = dir.path().join("game.nsp");
        let cci = dir.path().join("game.cci");
        let cia = dir.path().join("update.cia");
        let bcci = dir.path().join("pack.bcci");
        for path in [&cue, &nds, &nsp, &cci, &cia, &bcci] {
            std::fs::write(path, b"x").unwrap();
        }
        let missing_keys = dir.path().join("missing.keys");
        let (rom, ips) = write_patch_fixture(dir.path());
        let cases = [
            ("cue.to_iso", &cue, json!({}), dir.path().join("game.iso")),
            (
                "patch.apply",
                &rom,
                json!({ "patch_file": ips }),
                dir.path().join("hack.sfc"),
            ),
            (
                "cue.to_cso",
                &cue,
                json!({ "format": "zso" }),
                dir.path().join("game.zso"),
            ),
            (
                "ntr.encrypt",
                &nds,
                json!({}),
                dir.path().join("game.encrypted.nds"),
            ),
            (
                "ntr.decrypt",
                &nds,
                json!({}),
                dir.path().join("game.decrypted.nds"),
            ),
            (
                "nx.merge",
                &nsp,
                json!({ "inputs": [nsp], "keys": missing_keys }),
                dir.path().join("game (Merged).nsp"),
            ),
            (
                "nx.split",
                &nsp,
                json!({ "keys": missing_keys }),
                dir.path().join("game_split"),
            ),
            (
                "nx.decrypt",
                &nsp,
                json!({ "keys": missing_keys }),
                dir.path().join("game.dnsp"),
            ),
            (
                "ctr.bundle",
                &cci,
                json!({ "inputs": [cci, cia] }),
                dir.path().join("game.bcci"),
            ),
            (
                "ctr.unbundle",
                &bcci,
                json!({}),
                dir.path().join("pack_unbundled"),
            ),
        ];
        for (operation, input, options, output) in cases {
            let req = json!({
                "operation": operation,
                "input": input,
                "dry_run": true,
                "options": options
            });
            let res = run_json(&req.to_string(), CancelToken::new()).await;
            assert!(res.ok, "{operation}: {res:?}");
            let data = serde_json::to_value(res.data.unwrap()).unwrap();
            assert_eq!(data["operation"], operation);
            assert_eq!(data["decision"], "New", "{operation}");
            assert_eq!(data["output"].as_str(), output.to_str(), "{operation}");
            if operation.starts_with("nx.") {
                assert!(data["missing_keys"].is_string(), "{operation}: {data}");
            }
            if operation == "ctr.bundle" {
                assert_eq!(data["media"], "BCCI", "{operation}: {data}");
            }
        }
    }

    #[tokio::test]
    async fn ctr_bundle_then_unbundle_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let cci = dir.path().join("game.cci");
        let cia = dir.path().join("update.cia");
        std::fs::write(&cci, b"cci-bytes").unwrap();
        std::fs::write(&cia, b"cia-bytes").unwrap();

        let bundle_req = json!({
            "operation": "ctr.bundle",
            "input": cci,
            "options": { "inputs": [cci, cia] }
        });
        let res = run_json(&bundle_req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        let bundle = dir.path().join("game.bcci");
        assert!(bundle.exists());

        let unbundle_req = json!({ "operation": "ctr.unbundle", "input": bundle });
        let res = run_json(&unbundle_req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        let members = dir.path().join("game_unbundled");
        assert_eq!(
            std::fs::read(members.join("game.cci")).unwrap(),
            b"cci-bytes"
        );
        assert_eq!(
            std::fs::read(members.join("update.cia")).unwrap(),
            b"cia-bytes"
        );
    }

    #[tokio::test]
    async fn ctr_bundle_rejects_a_mismatched_output_extension_even_on_dry_run() {
        let dir = tempfile::tempdir().unwrap();
        let cci = dir.path().join("game.cci");
        std::fs::write(&cci, b"cci-bytes").unwrap();
        let out = dir.path().join("out.zip");
        let req = json!({
            "operation": "ctr.bundle",
            "input": cci,
            "output": out,
            "dry_run": true,
            "options": { "inputs": [cci] }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(!res.ok, "{res:?}");
        assert!(!out.exists());
    }

    #[tokio::test]
    async fn ctr_bundle_without_input_names_the_record_after_the_first_input() {
        let dir = tempfile::tempdir().unwrap();
        let cia = dir.path().join("update.cia");
        let cci = dir.path().join("game.cci");
        std::fs::write(&cia, b"cia-bytes").unwrap();
        std::fs::write(&cci, b"cci-bytes").unwrap();
        // The CIA is listed first; the bundle itself is named after the main.
        let req = json!({
            "operation": "ctr.bundle",
            "dry_run": true,
            "options": { "inputs": [cia, cci] }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        let data = serde_json::to_value(res.data.unwrap()).unwrap();
        assert_eq!(data["input"].as_str(), cia.to_str());
        assert_eq!(
            data["output"].as_str(),
            dir.path().join("game.bcci").to_str()
        );
    }

    fn write_patch_fixture(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let rom = dir.join("game.sfc");
        let patch = dir.join("hack.ips");
        std::fs::write(&rom, [0u8; 16]).unwrap();
        std::fs::write(&patch, b"PATCH\0\0\x04\0\x02\xaa\xbbEOF").unwrap();
        (rom, patch)
    }

    #[tokio::test]
    async fn patch_apply_writes_patched_copy() {
        let dir = tempfile::tempdir().unwrap();
        let (rom, patch) = write_patch_fixture(dir.path());
        let req = json!({
            "operation": "patch.apply",
            "input": rom,
            "options": { "patch_file": patch }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        let mut expected = [0u8; 16];
        expected[4..6].copy_from_slice(&[0xaa, 0xbb]);
        assert_eq!(
            std::fs::read(dir.path().join("hack.sfc")).unwrap(),
            expected
        );
        assert_eq!(std::fs::read(&rom).unwrap(), [0u8; 16]);
    }

    #[tokio::test]
    async fn patch_apply_places_the_copy_in_the_output_dir() {
        let dir = tempfile::tempdir().unwrap();
        let (rom, patch) = write_patch_fixture(dir.path());
        let out = dir.path().join("patched");
        let req = json!({
            "operation": "patch.apply",
            "input": rom,
            "options": { "patch_file": patch, "output_dir": out }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        assert!(out.join("hack.sfc").is_file());
        assert!(!dir.path().join("hack.sfc").exists());
    }

    /// Template tokens describe the ROM, so a plain template resolves onto
    /// the ROM itself and is refused; the same template with another
    /// output directory writes the copy there.
    #[tokio::test]
    async fn patch_apply_template_refuses_the_rom_but_works_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let (rom, patch) = write_patch_fixture(dir.path());
        let req = json!({
            "operation": "patch.apply",
            "input": rom,
            "options": {
                "patch_file": patch,
                "output_template": "{basename}.{ext}",
                "on_conflict": "overwrite"
            }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert_eq!(res.status, 2, "{res:?}");
        assert_eq!(std::fs::read(&rom).unwrap(), [0u8; 16]);

        let out = dir.path().join("patched");
        let req = json!({
            "operation": "patch.apply",
            "input": rom,
            "options": {
                "patch_file": patch,
                "output_template": "{basename}.{ext}",
                "output_dir": out
            }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        assert!(out.join("game.sfc").is_file());
    }

    #[tokio::test]
    async fn patch_apply_without_patch_file_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let (rom, _) = write_patch_fixture(dir.path());
        let req = json!({ "operation": "patch.apply", "input": rom });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(!res.ok, "{res:?}");
        assert_eq!(res.status, 2);
        assert_eq!(std::fs::read(&rom).unwrap(), [0u8; 16]);
        assert!(!dir.path().join("hack.sfc").exists());
    }

    #[tokio::test]
    async fn patch_apply_refuses_output_on_the_rom() {
        let dir = tempfile::tempdir().unwrap();
        let (rom, patch) = write_patch_fixture(dir.path());
        for dry_run in [false, true] {
            let req = json!({
                "operation": "patch.apply",
                "input": rom,
                "output": rom,
                "dry_run": dry_run,
                "options": { "patch_file": patch, "on_conflict": "overwrite" }
            });
            let res = run_json(&req.to_string(), CancelToken::new()).await;
            assert!(!res.ok, "{res:?}");
            assert_eq!(res.status, 2);
            assert_eq!(std::fs::read(&rom).unwrap(), [0u8; 16]);
            assert!(!dir.path().join("hack.sfc").exists());
        }
    }

    #[tokio::test]
    async fn patch_apply_refuses_output_on_the_patch() {
        let dir = tempfile::tempdir().unwrap();
        let (rom, patch) = write_patch_fixture(dir.path());
        let original = std::fs::read(&patch).unwrap();
        for dry_run in [false, true] {
            let req = json!({
                "operation": "patch.apply",
                "input": rom,
                "output": patch,
                "dry_run": dry_run,
                "options": { "patch_file": patch, "on_conflict": "overwrite" }
            });
            let res = run_json(&req.to_string(), CancelToken::new()).await;
            assert!(!res.ok, "{res:?}");
            assert_eq!(res.status, 2);
            assert_eq!(std::fs::read(&patch).unwrap(), original);
            assert_eq!(std::fs::read(&rom).unwrap(), [0u8; 16]);
            assert!(!dir.path().join("hack.sfc").exists());
        }
    }

    #[tokio::test]
    async fn patch_apply_refuses_archive_input() {
        let dir = tempfile::tempdir().unwrap();
        let (rom, patch) = write_patch_fixture(dir.path());
        let archive = dir.path().join("rom.zip");
        std::fs::write(&archive, b"archive").unwrap();
        let req = json!({
            "operation": "patch.apply",
            "input": archive,
            "options": { "patch_file": patch }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(!res.ok, "{res:?}");
        assert_eq!(res.status, 2);
        assert_eq!(std::fs::read(&archive).unwrap(), b"archive");
        assert_eq!(std::fs::read(&rom).unwrap(), [0u8; 16]);
        assert!(!dir.path().join("hack.zip").exists());
    }

    #[tokio::test]
    async fn patch_apply_opens_the_patch_even_on_dry_run() {
        let dir = tempfile::tempdir().unwrap();
        let (rom, _) = write_patch_fixture(dir.path());
        let unsupported = dir.path().join("hack.txt");
        std::fs::write(&unsupported, b"PATCH\0\0\x04\0\x02\xaa\xbbEOF").unwrap();
        for patch in [dir.path().join("missing.ips"), unsupported] {
            let req = json!({
                "operation": "patch.apply",
                "input": rom,
                "dry_run": true,
                "options": { "patch_file": patch }
            });
            let res = run_json(&req.to_string(), CancelToken::new()).await;
            assert!(!res.ok, "{res:?}");
            assert_eq!(std::fs::read(&rom).unwrap(), [0u8; 16]);
            assert!(!dir.path().join("missing.sfc").exists());
        }
        assert!(!dir.path().join("hack.sfc").exists());
    }

    #[tokio::test]
    async fn unknown_op_stays_invalid_after_registration() {
        let res = run_json(r#"{"operation":"ntr.compress"}"#, CancelToken::new()).await;
        assert!(!res.ok);
        assert_eq!(res.status, 2);
        assert!(res.message.contains("unknown operation"));
    }
}
