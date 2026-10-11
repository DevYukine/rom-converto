//! Cue, NTR, NX and CTR merge, split and bundle handlers.

use super::invalid_arg;
use super::models::{RunData, RunRequest, RunResponse, WupTitleInputOption};
use super::ops::{
    ConvertTarget, convert_op, cso_format, dir_op, nx_keys_for_run, preflight_space,
    required_input, skipped_already_done,
};
use crate::nintendo::ntr::NtrError;
use crate::nintendo::nx::{NxError, NxMergeFormat};
use crate::util::fs::file_len;
use crate::util::{CancelToken, Cancelled, OutputVerify, ProgressReporter};
use anyhow::Result;
use std::path::PathBuf;

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
            crate::util::resolve_input(path, &["nsp", "xci"])
        })
        .collect()
}

pub(crate) async fn nx_merge(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let inputs = option_inputs(&req)?;
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
            input: &first,
            derive: &|basis, _| crate::nintendo::nx::derive_merged_path(basis, ext),
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
        |source, output_dir| async move {
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
        |source, output_dir| async move {
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
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
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
        let cases = [
            ("cue.to_iso", &cue, json!({}), dir.path().join("game.iso")),
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

    #[tokio::test]
    async fn unknown_op_stays_invalid_after_registration() {
        let res = run_json(r#"{"operation":"ntr.compress"}"#, CancelToken::new()).await;
        assert!(!res.ok);
        assert_eq!(res.status, 2);
        assert!(res.message.contains("unknown operation"));
    }
}
