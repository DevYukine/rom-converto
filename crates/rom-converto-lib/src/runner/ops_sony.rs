//! PS1, PS3, PSP and Vita handlers.

use super::invalid_arg;
use super::models::{RunRequest, RunResponse};
use super::ops::{
    ConvertTarget, conflict_policy, convert_op, dir_op, dir_op_record, output_exists, planned,
    preflight_space, required_input, required_output_dir, skipped, skipped_already_done,
    stage_input, staged_path,
};
use crate::util::ConflictPolicy;
use std::path::{Path, PathBuf};

use crate::sony::ps3::Ps3Error;
use crate::util::fs::{file_len, has_ext};
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
            skip_if_already: None,
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
            skip_if_already: None,
            input: &input,
            derive: &|basis, _| basis.with_extension("iso"),
            operation: "psp.to_iso",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            spawn_blocking_with_progress(progress, move |progress| {
                crate::sony::psp::to_iso(progress, &input, &output, &cancel)
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

pub(crate) async fn psx_to_pbp(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    use crate::sony::psx::pbp::{
        BlockEncoding, ExistingEboot, PbpOptions, PsxPbpError, derive_output_dir,
        find_existing_eboot, required_bytes,
    };
    use crate::util::{PlanDecision, PlanLine};

    const OPERATION: &str = "psx.to_pbp";
    let input = required_input(&req)?;
    refuse_archived_playlist(&input).await?;
    let desired = req
        .output
        .clone()
        .or_else(|| req.options.output_dir.clone())
        .unwrap_or_else(|| derive_output_dir(&input));
    // The output folder holds one folder per game and may hold other games
    // (PSP/GAME), so the conflict policy applies to this game's EBOOTs, not
    // to the folder being non-empty.
    let existing = match conflict_policy(&req)? {
        ConflictPolicy::Overwrite => ExistingEboot::Replace,
        ConflictPolicy::Skip | ConflictPolicy::OverwriteInvalid => ExistingEboot::Keep,
        ConflictPolicy::Error => ExistingEboot::Refuse,
        ConflictPolicy::Rename => {
            return Err(invalid_arg(format!(
                "rename is not supported for directory outputs, use overwrite/skip/error: {}",
                desired.display()
            )));
        }
    };
    if desired.is_file() {
        return Err(invalid_arg(format!(
            "output path is a file, expected a folder: {}",
            desired.display()
        )));
    }
    let options = PbpOptions {
        existing,
        encoding: match req.options.compression.as_deref() {
            Some(name) => name
                .parse()
                .map_err(|err: PsxPbpError| invalid_arg(err.to_string()))?,
            None => BlockEncoding::default(),
        },
        title: req.options.title.clone(),
        disc_id: req.options.disc_id.clone(),
        splash: req.options.splash.clone(),
        icon0: req.options.icon0.clone(),
        pic1: req.options.pic1.clone(),
    };
    let find = |source: PathBuf| {
        let (output_dir, disc_id) = (desired.clone(), options.disc_id.clone());
        tokio::task::spawn_blocking(move || {
            find_existing_eboot(&source, &output_dir, disc_id.as_deref())
        })
    };
    let refuse = |path: PathBuf| {
        let message = PsxPbpError::EbootExists(path.clone()).to_string();
        output_exists(&path, message)
    };

    if req.dry_run {
        // A preview does not extract an archive, and an input it cannot read
        // plans as new; the real run reports why it fails.
        let present = if crate::util::is_archive_path(&input) {
            None
        } else {
            find(input.clone()).await?.ok().flatten()
        };
        let decision = match (present, existing) {
            (None, _) => PlanDecision::New,
            (Some(_), ExistingEboot::Replace) => PlanDecision::Overwrite,
            (Some(_), ExistingEboot::Keep) => PlanDecision::Skip,
            (Some(path), ExistingEboot::Refuse) => return Err(refuse(path)),
        };
        return Ok(planned(PlanLine {
            operation: OPERATION.to_string(),
            input: input.clone(),
            output: desired,
            decision,
            media: None,
            missing_keys: None,
        }));
    }

    let resolved = stage_input(&input, OPERATION).await?;
    let source = staged_path(&resolved, &input).to_path_buf();
    if let Some(path) = find(source.clone()).await?? {
        match existing {
            ExistingEboot::Replace => {}
            ExistingEboot::Keep => {
                log::info!("Skipped, EBOOT exists: {}", path.display());
                return Ok(skipped(&input, &desired, OPERATION, None));
            }
            ExistingEboot::Refuse => return Err(refuse(path)),
        }
    }
    if !req.options.skip_space_check.unwrap_or(false) {
        preflight_space(&desired, required_bytes(&source))?;
    }
    let started = std::time::Instant::now();
    let written = crate::sony::psx::pbp::to_pbp(progress, &source, &desired, &options, cancel)
        .await
        .map_err(|err| match err {
            PsxPbpError::EbootExists(path) => refuse(path),
            other => other.into(),
        })?;
    if written.is_empty() {
        return Ok(skipped(&input, &desired, OPERATION, None));
    }
    let bytes = written.iter().map(|path| file_len(path)).sum();
    Ok(
        RunResponse::ok(format!("Wrote {}", desired.display()), None)
            .with_record(dir_op_record(&input, &desired, OPERATION, bytes, started)),
    )
}

/// Refuses an archive holding an `.m3u`: staging extracts one member and
/// the sidecars of a cue sheet, never the discs a playlist names.
async fn refuse_archived_playlist(input: &Path) -> Result<()> {
    if !crate::util::is_archive_path(input) {
        return Ok(());
    }
    let archive = input.to_path_buf();
    let members =
        tokio::task::spawn_blocking(move || crate::util::archive::list_members(&archive)).await??;
    if let Some(playlist) = members
        .iter()
        .find(|member| has_ext(Path::new(&member.name), "m3u"))
    {
        return Err(invalid_arg(format!(
            "{}: {} is a playlist inside an archive; extract the archive and pass the .m3u",
            input.display(),
            playlist.name
        )));
    }
    Ok(())
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
        let cue = dir.path().join("game.cue");
        for path in [&iso, &pbp, &pkg, &cue] {
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
            ("psx.to_pbp", &cue, None, dir.path().join("game_psp")),
            ("psx.to_pbp", &cue, Some(&out_dir), out_dir.clone()),
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
    async fn psx_to_pbp_rejects_an_unknown_block_encoding() {
        let res = run_json(
            r#"{"operation":"psx.to_pbp","input":"game.cue","options":{"compression":"zstd"}}"#,
            CancelToken::new(),
        )
        .await;
        assert_eq!(res.status, 2);
        assert!(res.message.contains("unknown block encoding"), "{res:?}");
    }

    #[tokio::test]
    async fn psx_to_pbp_refuses_a_playlist_inside_an_archive() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("game.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        let opts: zip::write::SimpleFileOptions =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, body) in [
            ("game (Disc 1).cue", "FILE \"game (Disc 1).bin\" BINARY\n"),
            ("game.m3u", "game (Disc 1).cue\n"),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();

        let req = json!({ "operation": "psx.to_pbp", "input": archive });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert_eq!(res.status, 2, "{res:?}");
        assert!(
            res.message.contains("game.m3u is a playlist inside an archive"),
            "{res:?}"
        );
    }

    /// PSP/GAME holds other games, so the policy applies to each game's
    /// EBOOT, not to the folder being non-empty.
    #[tokio::test]
    async fn psx_to_pbp_conflicts_are_per_game_in_a_shared_folder() {
        use crate::sony::psx::pbp::test_fixtures::{ps1_data_track, write_disc};

        let dir = tempfile::tempdir().unwrap();
        let game = dir.path().join("GAME");
        let run = |cue: std::path::PathBuf, policy: &str| {
            let req = json!({
                "operation": "psx.to_pbp",
                "input": cue,
                "output": game,
                "options": { "on_conflict": policy },
            });
            async move { run_json(&req.to_string(), CancelToken::new()).await }
        };
        let first = write_disc(dir.path(), "first", &ps1_data_track("SLUS_999.99", 40));
        let second = write_disc(dir.path(), "second", &ps1_data_track("SLES_999.95", 40));

        for cue in [&first, &second] {
            let res = run(cue.clone(), "error").await;
            assert!(res.ok, "{res:?}");
        }
        assert!(game.join("SLUS99999/EBOOT.PBP").is_file());
        assert!(game.join("SLES99995/EBOOT.PBP").is_file());

        let res = run(first.clone(), "skip").await;
        assert!(res.ok && res.message.starts_with("Skipped existing"), "{res:?}");
        let res = run(first.clone(), "error").await;
        assert!(!res.ok, "{res:?}");
        assert!(res.message.contains("SLUS99999"), "{res:?}");

        let plan = |cue: &std::path::Path, policy: &str| {
            let req = json!({
                "operation": "psx.to_pbp",
                "input": cue,
                "output": game,
                "dry_run": true,
                "options": { "on_conflict": policy },
            });
            async move { run_json(&req.to_string(), CancelToken::new()).await }
        };
        let third = write_disc(dir.path(), "third", &ps1_data_track("SCES_999.96", 40));
        for (cue, policy, decision) in [
            (&third, "error", "New"),
            (&first, "skip", "Skip"),
            (&first, "overwrite", "Overwrite"),
        ] {
            let res = plan(cue, policy).await;
            assert!(res.ok, "{policy}: {res:?}");
            let data = serde_json::to_value(res.data.unwrap()).unwrap();
            assert_eq!(data["decision"], decision, "{policy}");
        }
        assert!(!plan(&first, "error").await.ok);
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
