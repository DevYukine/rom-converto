//! Shared JSON command runner used by embedding frontends.
//!
//! The C ABI passes JSON through this module so the ABI stays small while
//! Rust-side request and response schemas can evolve behind a versioned
//! payload.

use crate::util::{
    CancelToken, Cancelled, NoProgress, ProgressReporter, ReportFormat, write_report,
};
use anyhow::Result;
use serde_json::Value;
use std::sync::Mutex;

/// CLI-invocation metadata for a request, for frontends that echo commands.
pub mod cli_echo;
mod dat;
mod defaults;
pub(crate) mod ops;
mod ops_misc;
mod ops_ms;
mod ops_sony;

/// Request, response, and progress-event types for [`run_json`] and friends.
pub mod models;

use defaults::apply_config_defaults;
use models::{ProgressEvent, RunRequest, RunResponse, RunSchemaManifest, RunStatus};
pub use ops::batch_exts;
use ops::{run_batch_request, run_single_request};

pub const RUN_SCHEMA: &str = "rom-converto.run.v1";

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct InvalidArgument(String);

fn invalid_arg(message: impl Into<String>) -> anyhow::Error {
    InvalidArgument(message.into()).into()
}

/// Returns the runner's request/response schema manifest as JSON.
pub fn schema_json() -> Value {
    serde_json::to_value(RunSchemaManifest::current()).expect("runner schema serializes")
}

/// Tracks total/done across a `start()..inc()*` sequence to compute the
/// cumulative completion fraction reported on each `Advance` event. `start()`
/// resets the tally.
#[derive(Default)]
pub struct ProgressTally {
    total: u64,
    done: u64,
}

impl ProgressTally {
    /// Resets the tally for a new run of `total` units.
    pub fn start(&mut self, total: u64) {
        self.total = total;
        self.done = 0;
    }

    /// Records `delta` more units done and returns the cumulative fraction,
    /// or `None` when the total is 0 or unknown.
    pub fn advance(&mut self, delta: u64) -> Option<f64> {
        self.done = self.done.saturating_add(delta);
        (self.total > 0).then(|| (self.done as f64 / self.total as f64).min(1.0))
    }
}

/// [`ProgressReporter`] that buffers events instead of emitting them live,
/// for callers that read them back after the run completes.
#[derive(Default)]
pub struct RecordingProgress {
    state: Mutex<RecordingProgressState>,
}

#[derive(Default)]
struct RecordingProgressState {
    events: Vec<ProgressEvent>,
    tally: ProgressTally,
}

impl RecordingProgress {
    /// Drains and returns every event recorded so far.
    pub fn take_events(&self) -> Vec<ProgressEvent> {
        std::mem::take(&mut self.lock().events)
    }

    fn push(&self, event: ProgressEvent) {
        self.lock().events.push(event);
    }

    /// A poisoned mutex only means a reporting caller panicked; the recorded
    /// events stay well formed, so the run keeps appending to them.
    fn lock(&self) -> std::sync::MutexGuard<'_, RecordingProgressState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl ProgressReporter for RecordingProgress {
    fn start(&self, total: u64, msg: &str) {
        let mut state = self.lock();
        state.tally.start(total);
        state.events.push(ProgressEvent::Start {
            total,
            message: msg.to_string(),
        });
    }

    fn inc(&self, delta: u64) {
        let mut state = self.lock();
        let fraction = state.tally.advance(delta);
        state
            .events
            .push(ProgressEvent::Advance { delta, fraction });
    }

    fn finish(&self) {
        self.push(ProgressEvent::Finish);
    }

    fn set_phase(&self, label: &str) {
        self.push(ProgressEvent::Phase {
            message: label.to_string(),
        });
    }

    fn warn(&self, message: &str) {
        self.push(ProgressEvent::Warn {
            message: message.to_string(),
        });
    }
}

/// Runs one JSON-encoded [`RunRequest`] and returns the JSON-encoded
/// [`RunResponse`], with progress events collected into the response.
pub async fn run_json(request_json: &str, cancel: CancelToken) -> RunResponse {
    let progress = RecordingProgress::default();
    let mut response = run_json_with_progress(request_json, &progress, cancel).await;
    response.events = progress.take_events();
    response
}

/// Runs one JSON-encoded [`RunRequest`], reporting progress live to
/// `progress` instead of buffering it.
pub async fn run_json_with_progress(
    request_json: &str,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> RunResponse {
    let req = match serde_json::from_str::<RunRequest>(request_json) {
        Ok(req) => req,
        Err(err) => {
            return RunResponse::error(
                RunStatus::InvalidArgument,
                "Request JSON is invalid.",
                Some(err.to_string()),
            );
        }
    };

    match run_request(req, progress, cancel).await {
        Ok(response) => response,
        Err(err) if is_cancelled_error(&err) => RunResponse::error(
            RunStatus::Cancelled,
            "Operation cancelled.",
            Some(err.to_string()),
        ),
        Err(err) if err.downcast_ref::<InvalidArgument>().is_some() => RunResponse::error(
            RunStatus::InvalidArgument,
            err.to_string(),
            error_chain(&err),
        ),
        Err(err) => RunResponse::error(RunStatus::Failed, err.to_string(), error_chain(&err)),
    }
}

/// Parses and dispatches one [`RunRequest`] to its operation handler.
pub async fn run_request(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    if let Some(schema) = req.schema.as_deref()
        && schema != RUN_SCHEMA
    {
        return Err(invalid_arg(format!(
            "unsupported schema {schema:?}; expected {RUN_SCHEMA}"
        )));
    }

    let req = apply_config_defaults(req)?;
    let report = req.options.report.clone();
    let response = if req.options.recursive.unwrap_or(false) {
        run_batch_request(req.clone(), progress, cancel.clone()).await?
    } else {
        run_single_request(req.clone(), progress, cancel.clone()).await?
    };
    if !req.operation.starts_with("dat.")
        && let (Some(path), Some(totals)) = (report.as_deref(), response.totals.as_ref())
        && !response.records.is_empty()
    {
        write_report(
            path,
            &response.records,
            totals,
            ReportFormat::from_path(path),
            &cancel,
        )?;
    }
    Ok(response)
}

/// Short report verb for a dotted operation name: `chd.compress` becomes
/// `compress`, `cso.to_chd` becomes `to-chd`. Report records carry the verb
/// the CLI and GUI already write.
pub(crate) fn record_verb(op: &str) -> String {
    op.rsplit('.').next().unwrap_or(op).replace('_', "-")
}

/// [`record_verb`] with the suffix the CLI writes under a dry run, so an
/// exported plan is distinguishable from a report of a real run.
pub(crate) fn planned_verb(op: &str, dry_run: bool) -> String {
    let verb = record_verb(op);
    if dry_run {
        format!("{verb} (dry run)")
    } else {
        verb
    }
}

fn error_chain(err: &anyhow::Error) -> Option<String> {
    let details = err
        .chain()
        .skip(1)
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    (!details.is_empty()).then(|| details.join(": "))
}

/// True when `err`'s chain carries a cancellation, whichever module raised it.
pub fn is_cancelled_error(err: &anyhow::Error) -> bool {
    Cancelled::in_chain(err)
}

/// Runs one JSON-encoded [`RunRequest`], discarding progress events.
pub async fn run_json_no_progress(request_json: &str) -> RunResponse {
    run_json_with_progress(request_json, &NoProgress, CancelToken::new()).await
}

#[cfg(test)]
mod tests {
    use super::dat::dat_checksum_bounds;
    use super::models::RunOptions;
    use super::ops::disc_mode;
    use super::*;
    use crate::disc::chd::DiscMode;
    use crate::util::{ChecksumBounds, FileStatus, HashAlgo};
    use serde_json::{Value, json};

    #[test]
    fn status_codes_are_stable() {
        assert_eq!(RunStatus::Ok.as_i32(), 0);
        assert_eq!(RunStatus::Failed.as_i32(), 1);
        assert_eq!(RunStatus::InvalidArgument.as_i32(), 2);
        assert_eq!(RunStatus::PartialFailure.as_i32(), 3);
        assert_eq!(RunStatus::Cancelled.as_i32(), 130);
        assert_eq!(RunStatus::InternalError.as_i32(), 255);
    }

    #[test]
    fn disc_mode_parses_ld() {
        assert_eq!(disc_mode(Some("ld")).unwrap(), Some(DiscMode::Ld));
    }

    #[test]
    fn progress_tally_computes_cumulative_fraction() {
        let mut tally = ProgressTally::default();
        tally.start(100);
        assert_eq!(tally.advance(25), Some(0.25));
        assert_eq!(tally.advance(25), Some(0.5));
    }

    #[test]
    fn progress_tally_with_zero_total_is_none() {
        let mut tally = ProgressTally::default();
        tally.start(0);
        assert_eq!(tally.advance(1), None);
    }

    #[test]
    fn progress_tally_start_resets() {
        let mut tally = ProgressTally::default();
        tally.start(100);
        tally.advance(50);
        tally.start(10);
        assert_eq!(tally.advance(5), Some(0.5));
    }

    #[tokio::test]
    async fn invalid_json_returns_user_safe_error() {
        let res = run_json("{", CancelToken::new()).await;
        assert!(!res.ok);
        assert_eq!(res.status, 2);
        assert_eq!(res.code, "invalid_argument");
        assert_eq!(res.message, "Request JSON is invalid.");
    }

    #[tokio::test]
    async fn dry_run_cso_compress_returns_plan() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.iso");
        std::fs::write(&input, b"x").unwrap();
        let req = json!({
            "schema": RUN_SCHEMA,
            "operation": "cso.compress",
            "input": input,
            "dry_run": true
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        let data = serde_json::to_value(res.data.unwrap()).unwrap();
        assert_eq!(data["operation"], "cso.compress");
        assert_eq!(data["decision"], "New");
    }

    #[tokio::test]
    async fn recursive_dry_run_returns_plans_and_writes_report() {
        let dir = tempfile::tempdir().unwrap();
        let input_dir = dir.path().join("roms");
        let output_dir = dir.path().join("out");
        let report = dir.path().join("report.json");
        std::fs::create_dir(&input_dir).unwrap();
        std::fs::write(input_dir.join("a.iso"), b"a").unwrap();
        std::fs::write(input_dir.join("b.iso"), b"b").unwrap();

        let req = json!({
            "schema": RUN_SCHEMA,
            "operation": "cso.compress",
            "input": input_dir,
            "dry_run": true,
            "options": {
                "recursive": true,
                "output_dir": output_dir,
                "report": report
            }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        assert_eq!(res.totals.as_ref().unwrap().total_files, 2);
        let data = serde_json::to_value(res.data.as_ref().unwrap()).unwrap();
        assert_eq!(data["plans"].as_array().unwrap().len(), 2);
        assert!(report.exists());
    }

    #[tokio::test]
    async fn recursive_output_dir_mirrors_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let input_dir = dir.path().join("roms");
        let output_dir = dir.path().join("out");
        std::fs::create_dir_all(input_dir.join("sub")).unwrap();
        std::fs::write(input_dir.join("top.iso"), b"a").unwrap();
        std::fs::write(input_dir.join("sub").join("nested.iso"), b"b").unwrap();

        let req = json!({
            "operation": "cso.compress",
            "input": input_dir,
            "dry_run": true,
            "options": { "recursive": true, "output_dir": output_dir }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        let data = serde_json::to_value(res.data.as_ref().unwrap()).unwrap();
        let outputs: Vec<&str> = data["plans"]
            .as_array()
            .unwrap()
            .iter()
            .map(|plan| plan["output"].as_str().unwrap())
            .collect();
        assert!(outputs.contains(&output_dir.join("top.cso").to_str().unwrap()));
        assert!(
            outputs.contains(&output_dir.join("sub").join("nested.cso").to_str().unwrap()),
            "{outputs:?}"
        );
    }

    #[tokio::test]
    async fn playlist_dry_run_returns_playlists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Game (Disc 1).cue"), b"").unwrap();
        std::fs::write(dir.path().join("Game (Disc 2).cue"), b"").unwrap();
        let req = json!({
            "schema": RUN_SCHEMA,
            "operation": "playlist.write",
            "input": dir.path(),
            "dry_run": true
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        let data = serde_json::to_value(res.data.as_ref().unwrap()).unwrap();
        assert_eq!(data["playlists"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn schema_manifest_lists_dat_operations() {
        let schema = schema_json();
        let ops = schema["operations"].as_array().unwrap();
        assert!(ops.iter().any(|op| op == "dat.verify"));
        assert!(ops.iter().any(|op| op == "dat.fixdat"));
        for alias in [
            "cso.to-chd",
            "chd.to-cso",
            "ctr.cdn-to-cia",
            "ctr.generate-cdn-ticket",
            "playlist",
            "info.read",
        ] {
            assert!(ops.iter().any(|op| op == alias), "missing {alias}");
        }
        assert!(
            schema["response"]["fields"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field == "schema")
        );
    }

    #[tokio::test]
    async fn disc_aliases_preserve_requested_operation_in_plans() {
        let dir = tempfile::tempdir().unwrap();
        for operation in [
            "dol.compress",
            "rvl.compress",
            "rvz.compress",
            "dol.decompress",
            "rvl.decompress",
            "rvz.decompress",
        ] {
            let input = dir.path().join(if operation.ends_with(".compress") {
                "game.iso"
            } else {
                "game.rvz"
            });
            std::fs::write(&input, b"x").unwrap();
            let req = json!({
                "operation": operation,
                "input": input,
                "output": dir.path().join(format!("{operation}.out")),
                "dry_run": true
            });
            let res = run_json(&req.to_string(), CancelToken::new()).await;
            assert!(res.ok, "{operation}: {res:?}");
            let data = serde_json::to_value(res.data.unwrap()).unwrap();
            assert_eq!(data["operation"], operation);
        }
    }

    #[tokio::test]
    async fn cdn_output_dir_alias_falls_back_and_canonical_wins() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("cdn");
        std::fs::create_dir(&input).unwrap();
        let legacy = dir.path().join("legacy");
        let canonical = dir.path().join("canonical");

        let legacy_req = json!({
            "operation": "ctr.cdn_to_cia",
            "input": input,
            "dry_run": true,
            "options": { "output_dir_cia": legacy }
        });
        let legacy_res = run_json(&legacy_req.to_string(), CancelToken::new()).await;
        let legacy_data = serde_json::to_value(legacy_res.data.unwrap()).unwrap();
        assert_eq!(
            legacy_data["output"].as_str(),
            legacy.join("cdn.cia").to_str()
        );

        let canonical_req = json!({
            "operation": "ctr.cdn_to_cia",
            "input": input,
            "dry_run": true,
            "options": { "output_dir": canonical, "output_dir_cia": legacy }
        });
        let canonical_res = run_json(&canonical_req.to_string(), CancelToken::new()).await;
        let canonical_data = serde_json::to_value(canonical_res.data.unwrap()).unwrap();
        assert_eq!(
            canonical_data["output"].as_str(),
            canonical.join("cdn.cia").to_str()
        );
    }

    #[tokio::test]
    async fn ctr_cdn_to_cia_dsiware_with_ticket_flag() {
        use crate::nintendo::ctr::models::cia::CiaFile;
        use crate::nintendo::ctr::test_fixtures::{append_be, make_cert, make_tmd};
        use binrw::{BinRead, Endian};
        use sha2::{Digest, Sha256};
        use std::io::Cursor;

        let tmp = tempfile::tempdir().unwrap();
        let cdn_dir = tmp.path().join("dsiware");
        std::fs::create_dir_all(&cdn_dir).unwrap();

        let content: Vec<u8> = (0..0x400u32).map(|i| i as u8).collect();
        let mut hasher = Sha256::new();
        hasher.update(&content);
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&hasher.finalize());
        std::fs::write(cdn_dir.join("00000000"), &content).unwrap();

        let title_id = 0x0004800400000000u64;
        let tmd = make_tmd(title_id, vec![(0, 0, content, hash)], false);
        let mut tmd_buf = Vec::new();
        append_be(&mut tmd_buf, &tmd);
        append_be(&mut tmd_buf, &make_cert(b"CP0000000b", 0xBB));
        append_be(&mut tmd_buf, &make_cert(b"CA00000003", 0xAA));
        std::fs::write(cdn_dir.join("tmd"), &tmd_buf).unwrap();

        let req = json!({
            "operation": "ctr.cdn_to_cia",
            "input": cdn_dir,
            "options": { "ensure_ticket_exists": true }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");

        let output = tmp.path().join("dsiware.cia");
        assert!(output.exists(), "missing {}", output.display());
        let bytes = std::fs::read(&output).unwrap();
        assert!(CiaFile::read_options(&mut Cursor::new(&bytes), Endian::Little, ()).is_ok());
    }

    #[tokio::test]
    async fn output_conflict_skip_avoids_fixdat_and_ticket_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("existing.xml");
        std::fs::write(&output, b"existing").unwrap();
        for operation in ["dat.fixdat", "ctr.generate_cdn_ticket"] {
            let req = json!({
                "operation": operation,
                "input": dir.path(),
                "output": output,
                "options": { "on_conflict": "skip" }
            });
            let res = run_json(&req.to_string(), CancelToken::new()).await;
            assert!(res.ok, "{operation}: {res:?}");
            assert_eq!(res.records[0].status, FileStatus::Skipped);
            assert_eq!(std::fs::read(&output).unwrap(), b"existing");
        }
    }

    #[tokio::test]
    async fn unknown_operation_is_invalid_argument_response() {
        let req: Value = json!({ "operation": "wat" });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(!res.ok);
        assert_eq!(res.status, 2);
        assert!(res.message.contains("unknown operation"));
    }

    #[tokio::test]
    async fn bad_schema_is_invalid_argument_response() {
        let req: Value = json!({ "schema": "wrong", "operation": "hash" });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(!res.ok);
        assert_eq!(res.status, 2);
        assert!(res.message.contains("unsupported schema"));
    }

    #[tokio::test]
    async fn recursive_file_input_processes_that_file() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.iso");
        std::fs::write(&input, vec![0u8; 4 * 2048]).unwrap();
        let req: Value = json!({
            "operation": "cso.compress",
            "input": input,
            "options": { "recursive": true }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        assert!(dir.path().join("game.cso").exists());
    }

    #[tokio::test]
    async fn missing_required_output_is_invalid_argument_response() {
        let req: Value = json!({
            "operation": "wup.decrypt",
            "input": "title"
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(!res.ok);
        assert_eq!(res.status, 2);
        assert!(res.message.contains("output path is required"));
    }

    #[tokio::test]
    async fn playlist_file_input_is_invalid_argument_response() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.cue");
        std::fs::write(&input, b"").unwrap();
        let req: Value = json!({
            "operation": "playlist.write",
            "input": input
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(!res.ok);
        assert_eq!(res.status, 2);
        assert!(res.message.contains("playlist input"));
    }

    #[tokio::test]
    async fn dat_scan_file_input_is_invalid_argument_response() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.iso");
        std::fs::write(&input, b"").unwrap();
        let req: Value = json!({
            "operation": "dat.scan",
            "input": input
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(!res.ok);
        assert_eq!(res.status, 2);
        assert!(res.message.contains("dat.scan input"));
    }

    #[test]
    fn config_defaults_fill_missing_options() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("rom-converto.toml");
        std::fs::write(
            &config,
            "[cso]\nblock_size = 32768\n[presets.fast.cso]\nblock_size = 65536\n",
        )
        .unwrap();
        let req = RunRequest {
            schema: Some(RUN_SCHEMA.to_string()),
            operation: "cso.compress".to_string(),
            input: None,
            output: None,
            config: Some(config),
            preset: Some("fast".to_string()),
            options: RunOptions::default(),
            ctx: Default::default(),
            dry_run: false,
        };
        let req = apply_config_defaults(req).unwrap();
        assert_eq!(req.options.block_size, Some(65536));
    }

    #[test]
    fn dat_checksum_bounds_use_defaults_and_validate_range() {
        let mut req = RunRequest {
            schema: None,
            operation: "dat.verify".to_string(),
            input: None,
            output: None,
            config: None,
            preset: None,
            options: RunOptions::default(),
            ctx: Default::default(),
            dry_run: false,
        };
        let algos = [HashAlgo::Crc32, HashAlgo::Sha1];
        assert_eq!(
            dat_checksum_bounds(&req, &algos).unwrap(),
            ChecksumBounds::new(HashAlgo::Crc32, HashAlgo::Sha256).unwrap()
        );

        req.options.input_checksum_min = Some("sha256".to_string());
        req.options.input_checksum_max = Some("crc32".to_string());
        assert!(dat_checksum_bounds(&req, &algos).is_err());
    }
}
