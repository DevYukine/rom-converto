//! PS3, PSP and Vita handlers.

use super::models::{RunRequest, RunResponse};
use super::ops::{
    ConvertTarget, convert_op, dir_op, required_input, required_output_dir, skipped_already_done,
};
use crate::sony::ps3::Ps3Error;
use crate::util::fs::file_len;
use crate::util::{CancelToken, OutputVerify, ProgressReporter, spawn_blocking_with_progress};
use anyhow::Result;

pub(crate) async fn ps3_decrypt(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let req = &req;
    let input = required_input(req)?;
    let staged = input.clone();
    let result = convert_op(
        progress,
        req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| crate::sony::ps3::derive_decrypted_path(basis),
            operation: "ps3.decrypt",
            verify: OutputVerify::None,
        },
        cancel,
        |source, output, cancel| async move {
            // The sibling .dkey lives next to the staged input, named after
            // the member when that input is an archive.
            let basis = staged.with_file_name(source.file_name().unwrap_or_default());
            let key =
                crate::sony::ps3::resolve_ps3_key(&source, &basis, req.options.key.as_deref())?;
            crate::sony::ps3::decrypt_ps3_iso(
                progress,
                source,
                output,
                key,
                true,
                req.options.skip_probe.unwrap_or(false),
                cancel,
            )
            .await
            .map_err(anyhow::Error::from)
        },
    )
    .await;
    match result {
        Err(err)
            if matches!(
                err.downcast_ref::<Ps3Error>(),
                Some(Ps3Error::AlreadyDecrypted)
            ) =>
        {
            Ok(skipped_already_done(
                &input,
                "ps3.decrypt",
                "already decrypted",
                &err,
            ))
        }
        other => other,
    }
}

pub(crate) async fn psp_to_iso(
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
            operation: "psp.to_iso",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, _cancel| async move {
            spawn_blocking_with_progress(progress, move |progress| {
                crate::sony::psp::to_iso(progress, &input, &output)
            })
            .await
        },
    )
    .await
}

pub(crate) async fn psp_extract(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    _cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = required_output_dir(&req)?;
    dir_op(
        &req,
        &input,
        &desired,
        "psp.extract",
        file_len,
        |source, output| async move {
            spawn_blocking_with_progress(progress, move |progress| {
                crate::sony::psp::extract_segments(progress, &source, &output)
            })
            .await
            .map(|_| (0, None))
        },
    )
    .await
}

pub(crate) async fn vita_extract(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    _cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = required_output_dir(&req)?;
    dir_op(
        &req,
        &input,
        &desired,
        "vita.extract",
        file_len,
        |source, output| async move {
            spawn_blocking_with_progress(progress, move |progress| {
                crate::sony::vita::pkg::extract(&source, &output, progress)
            })
            .await
            .map(|_| (0, None))
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::super::run_json;
    use crate::util::CancelToken;
    use serde_json::json;

    #[tokio::test]
    async fn sony_ops_dry_run_plans() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        let pbp = dir.path().join("EBOOT.PBP");
        let pkg = dir.path().join("game.pkg");
        for path in [&iso, &pbp, &pkg] {
            std::fs::write(path, b"x").unwrap();
        }
        let out_dir = dir.path().join("out");
        let cases = [
            (
                "ps3.decrypt",
                &iso,
                None,
                dir.path().join("game.decrypted.iso"),
            ),
            ("psp.to_iso", &pbp, None, dir.path().join("EBOOT.iso")),
            ("psp.extract", &pbp, Some(&out_dir), out_dir.clone()),
            ("vita.extract", &pkg, Some(&out_dir), out_dir.clone()),
        ];
        for (operation, input, output, expected) in cases {
            let req = json!({
                "operation": operation,
                "input": input,
                "output": output,
                "dry_run": true
            });
            let res = run_json(&req.to_string(), CancelToken::new()).await;
            assert!(res.ok, "{operation}: {res:?}");
            let data = serde_json::to_value(res.data.unwrap()).unwrap();
            assert_eq!(data["operation"], operation);
            assert_eq!(data["decision"], "New", "{operation}");
            assert_eq!(data["output"].as_str(), expected.to_str(), "{operation}");
        }
    }

    #[tokio::test]
    async fn dir_ops_require_output() {
        let res = run_json(
            r#"{"operation":"psp.extract","input":"EBOOT.PBP"}"#,
            CancelToken::new(),
        )
        .await;
        assert_eq!(res.status, 2);
        assert!(res.message.contains("output path is required"));
    }
}
