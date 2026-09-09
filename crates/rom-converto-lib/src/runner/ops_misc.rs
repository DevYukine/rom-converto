//! Cue, NTR and NX merge/split handlers.

use super::invalid_arg;
use super::models::{RunData, RunRequest, RunResponse, WupTitleInputOption};
use super::ops::{
    ConvertTarget, convert_op, cso_format, dir_op, nx_keys_for_run, required_input,
    skipped_already_done,
};
use crate::nintendo::ntr::NtrError;
use crate::nintendo::nx::NxMergeFormat;
use crate::util::fs::file_len;
use crate::util::{CancelToken, OutputVerify, ProgressReporter};
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
        |input, output, _cancel| async move {
            crate::disc::cue::to_iso::cue_to_iso(progress, input, output, true)
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
        |input, output, _cancel| async move {
            crate::pipeline::cue_to_cso(progress, input, output, format, true).await
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

pub(crate) async fn nx_merge(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let inputs =
        req.options
            .inputs
            .as_ref()
            .map(|inputs| {
                inputs
                    .iter()
                    .map(|input| match input {
                        WupTitleInputOption::Path(path)
                        | WupTitleInputOption::Object { path, .. } => path.clone(),
                    })
                    .collect::<Vec<PathBuf>>()
            })
            .filter(|inputs| !inputs.is_empty())
            .ok_or_else(|| invalid_arg("options.inputs must not be empty"))?;
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
    // The merge has many inputs but one record; the first one names it.
    let mut req = req;
    let first = req.input.clone().unwrap_or_else(|| inputs[0].clone());
    req.input = Some(first.clone());
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
            crate::nintendo::nx::merge_containers_async(
                inputs, output, format, keys, progress, cancel,
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
    let mut response = dir_op(
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
    .await?;
    if let Some(RunData::Plan(line)) = &mut response.data {
        line.missing_keys = missing_keys;
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::super::run_json;
    use crate::util::CancelToken;
    use serde_json::json;

    #[tokio::test]
    async fn misc_ops_dry_run_plans() {
        let dir = tempfile::tempdir().unwrap();
        let cue = dir.path().join("game.cue");
        let nds = dir.path().join("game.nds");
        let nsp = dir.path().join("game.nsp");
        for path in [&cue, &nds, &nsp] {
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
        }
    }

    #[tokio::test]
    async fn unknown_op_stays_invalid_after_registration() {
        let res = run_json(r#"{"operation":"ntr.compress"}"#, CancelToken::new()).await;
        assert!(!res.ok);
        assert_eq!(res.status, 2);
        assert!(res.message.contains("unknown operation"));
    }
}
