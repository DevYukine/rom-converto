//! Xbox and Xbox 360 handlers.

use super::models::{RunData, RunRequest, RunResponse, XenonConvertData, XenonVerifyData};
use super::ops::{
    ConvertTarget, convert_op, dir_op, required_input, required_output_dir, stage_input,
    staged_path,
};
use crate::microsoft::xbox::XisoCreateOptions;
use crate::util::fs::file_len;
use crate::util::{CancelToken, OutputVerify, ProgressReporter};
use anyhow::Result;

pub(crate) async fn xbox_convert(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let opts = XisoCreateOptions {
        media_patch: req
            .options
            .media_patch
            .unwrap_or(XisoCreateOptions::default().media_patch),
    };
    let mut response = convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| basis.with_extension("xiso"),
            operation: "xbox.convert",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::microsoft::xbox::convert_to_xiso(&input, &output, opts, progress, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await?;
    if let Some(RunData::Plan(line)) = &mut response.data {
        line.media = Some("XISO".to_string());
    }
    Ok(response)
}

pub(crate) async fn xbox_extract(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = required_output_dir(&req)?;
    dir_op(
        &req,
        &input,
        &desired,
        "xbox.extract",
        |source| {
            crate::microsoft::xbox::read_info(source)
                .map(|info| info.total_file_bytes)
                .unwrap_or_else(|_| file_len(source))
        },
        |source, output| async move {
            crate::microsoft::xbox::extract_xiso(&source, &output, progress, cancel)
                .await
                .map(|_| (0, None))
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn xenon_compress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let mut response = convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| basis.with_extension("zar"),
            operation: "xenon.compress",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::microsoft::xenon::pack_zar(&input, &output, progress, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await?;
    if let Some(RunData::Plan(line)) = &mut response.data {
        line.media = Some("ZAR".to_string());
    }
    Ok(response)
}

pub(crate) async fn xenon_extract(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = required_output_dir(&req)?;
    dir_op(
        &req,
        &input,
        &desired,
        "xenon.extract",
        |source| {
            crate::microsoft::xenon::read_info(source)
                .map(|info| info.logical_size)
                .unwrap_or_else(|_| file_len(source))
        },
        |source, output| async move {
            crate::microsoft::xenon::extract_zar(&source, &output, progress, cancel)
                .await
                .map(|_| (0, None))
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn xenon_convert(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = required_output_dir(&req)?;
    let title = req.options.title.clone();
    dir_op(
        &req,
        &input,
        &desired,
        "xenon.convert",
        // On top of the payload: one hash block per 0xCC data blocks, plus
        // the container header.
        |source| {
            let len = file_len(source);
            len + len / 0xCC + 0xB000
        },
        |source, output| async move {
            let summary = crate::microsoft::xenon::convert_to_god(
                &source,
                &output,
                title.as_deref(),
                progress,
                cancel,
            )
            .await?;
            let data = RunData::XenonConvert(XenonConvertData {
                title_id: summary.title_id,
                media_id: summary.media_id,
                part_count: summary.part_count,
                total_bytes: summary.total_bytes,
            });
            Ok((summary.total_bytes, Some(data)))
        },
    )
    .await
}

pub(crate) async fn xenon_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let resolved = stage_input(&input, "xenon.verify").await?;
    let result =
        crate::microsoft::xenon::verify_zar(staged_path(&resolved, &input), progress, cancel)
            .await?;
    Ok(RunResponse::ok(
        "ZAR verification complete.",
        Some(RunData::XenonVerify(XenonVerifyData {
            blocks: result.blocks,
            logical_bytes: result.logical_bytes,
            hash_ok: result.hash_ok,
        })),
    ))
}

#[cfg(test)]
mod tests {
    use super::super::run_json;
    use crate::util::CancelToken;
    use serde_json::json;

    #[tokio::test]
    async fn ms_ops_dry_run_plans() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        let zar = dir.path().join("packed.zar");
        for path in [&iso, &zar] {
            std::fs::write(path, b"x").unwrap();
        }
        let out_dir = dir.path().join("out");
        let cases = [
            (
                "xbox.convert",
                &iso,
                None,
                dir.path().join("game.xiso"),
                Some("XISO"),
            ),
            ("xbox.extract", &iso, Some(&out_dir), out_dir.clone(), None),
            (
                "xenon.compress",
                &iso,
                None,
                dir.path().join("game.zar"),
                Some("ZAR"),
            ),
            ("xenon.extract", &zar, Some(&out_dir), out_dir.clone(), None),
            ("xenon.convert", &iso, Some(&out_dir), out_dir.clone(), None),
        ];
        for (operation, input, output, expected, media) in cases {
            let req = json!({
                "operation": operation,
                "input": input,
                "output": output,
                "dry_run": true,
                "options": { "title": "Game" }
            });
            let res = run_json(&req.to_string(), CancelToken::new()).await;
            assert!(res.ok, "{operation}: {res:?}");
            let data = serde_json::to_value(res.data.unwrap()).unwrap();
            assert_eq!(data["operation"], operation);
            assert_eq!(data["decision"], "New", "{operation}");
            assert_eq!(data["output"].as_str(), expected.to_str(), "{operation}");
            assert_eq!(data["media"].as_str(), media, "{operation}");
        }
    }
}
