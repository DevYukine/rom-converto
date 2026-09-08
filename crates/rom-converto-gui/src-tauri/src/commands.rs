use crate::info_cache::InfoCache;
use crate::progress::TauriProgress;
use rom_converto_lib::info::{DiscContent, InfoOptions, InfoResult, read_info};
use rom_converto_lib::nintendo::nx::find_keys_file;
use rom_converto_lib::runner::models::{ComparisonData, RunData, RunRequest, RunStatus};
use rom_converto_lib::runner::{is_cancelled_error, run_request};
use rom_converto_lib::util::HashCache;
use rom_converto_lib::util::fs::{collect_all_files, collect_files_with_exts};
use rom_converto_lib::util::{
    CancelToken, ConflictPolicy, PlanLine, ProgressReporter, ReportFormat, ReportRecord,
    ReportTotals, write_report,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, State};

use crate::err_to_string;

/// Result of a report-capable command. The message drives the operation log;
/// the records are accumulated client-side and handed back to
/// `cmd_write_report` once the run finishes, matching how the CLI collects
/// records and writes a single report at the end. A recursive run returns one
/// record per file, so the report keeps every row. `input_bytes`/`output_bytes`
/// are always populated (independent of the report toggle) so the batch
/// completion notification can total up space saved without requiring the
/// user to turn reporting on.
#[derive(serde::Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct RunOutcome {
    message: String,
    /// The runner's [`RunStatus`] as an integer: a partial failure completes
    /// the command but did not convert every file, so the frontend marks the
    /// job failed rather than done.
    status: i32,
    records: Vec<ReportRecord>,
    input_bytes: u64,
    output_bytes: u64,
    comparison: Option<ComparisonData>,
    /// The runner's operation-specific payload, handed to the frontend as is
    /// so result cards read structured data instead of parsing a message.
    #[cfg_attr(feature = "ts-export", ts(type = "unknown"))]
    data: Option<serde_json::Value>,
}

/// Registry of the cancel tokens for every operation currently running,
/// keyed by task id. Concurrent batch jobs run in their own slot (one
/// task id per slot), so cancelling one must not touch the others.
pub type ActiveCancel = Arc<tokio::sync::Mutex<HashMap<String, CancelToken>>>;

async fn begin(state: &ActiveCancel, key: &str) -> CancelToken {
    let token = CancelToken::new();
    state.lock().await.insert(key.to_string(), token.clone());
    token
}

async fn finish(state: &ActiveCancel, key: &str) {
    state.lock().await.remove(key);
}

/// Cancels one in-flight task by id, or every in-flight task when `task_id`
/// is `None` (the frontend's "abort everything" fallback).
#[tauri::command]
pub async fn cmd_cancel(
    state: State<'_, ActiveCancel>,
    task_id: Option<String>,
) -> Result<(), String> {
    let map = state.lock().await;
    match task_id {
        Some(id) => {
            if let Some(token) = map.get(&id) {
                token.cancel();
            }
        }
        None => {
            for token in map.values() {
                token.cancel();
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod cancel_registry_tests {
    use super::*;

    #[tokio::test]
    async fn cancel_one_leaves_others_live() {
        let state: ActiveCancel = Arc::default();
        let a = {
            let token = CancelToken::new();
            state.lock().await.insert("a".to_string(), token.clone());
            token
        };
        let b = {
            let token = CancelToken::new();
            state.lock().await.insert("b".to_string(), token.clone());
            token
        };
        {
            let map = state.lock().await;
            if let Some(token) = map.get("a") {
                token.cancel();
            }
        }
        assert!(a.is_cancelled());
        assert!(!b.is_cancelled());
    }

    #[tokio::test]
    async fn cancel_all_drains_every_token() {
        let state: ActiveCancel = Arc::default();
        let a = {
            let token = CancelToken::new();
            state.lock().await.insert("a".to_string(), token.clone());
            token
        };
        let b = {
            let token = CancelToken::new();
            state.lock().await.insert("b".to_string(), token.clone());
            token
        };
        {
            let map = state.lock().await;
            for token in map.values() {
                token.cancel();
            }
        }
        assert!(a.is_cancelled());
        assert!(b.is_cancelled());
    }

    #[tokio::test]
    async fn begin_and_finish_track_by_key() {
        let state: ActiveCancel = Arc::default();
        let token = begin(&state, "slot-0").await;
        assert!(state.lock().await.contains_key("slot-0"));
        finish(&state, "slot-0").await;
        assert!(!state.lock().await.contains_key("slot-0"));
        assert!(!token.is_cancelled());
    }
}

/// Runs `body` on a dedicated thread with its own current-thread runtime, for
/// conversions whose futures are not `Send`: the streaming crypt pipelines hold
/// their worker-pool receiver across await points, and `ChdReader`'s nested
/// async types exceed the compiler's Send-inference recursion limit.
///
/// The join blocks, so it waits on the blocking pool: Tauri's async runtime
/// stays free to service `cmd_cancel` while the conversion runs.
async fn on_dedicated_runtime<T, F>(body: F) -> Result<T, String>
where
    F: FnOnce(&tokio::runtime::Runtime) -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(err_to_string)?;
            body(&rt)
        })
        .join()
        .map_err(|_| {
            "The operation failed unexpectedly. Try again, and report a bug if it keeps happening."
                .to_string()
        })?
    })
    .await
    .map_err(err_to_string)?
}

/// Runs one request through the shared library runner and maps its response
/// onto the GUI's `RunOutcome`. The GUI defaults conflicts to `overwrite`
/// (the runner's own default is `error`) and attaches the app-wide hash cache
/// so digests are shared with the CLI.
#[tauri::command]
pub async fn cmd_run(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    cache: State<'_, Arc<HashCache>>,
    task_id: Option<String>,
    progress_key: Option<String>,
    mut request: RunRequest,
    report: bool,
) -> Result<RunOutcome, String> {
    // The cancel registry is keyed by the unique task id; progress and row
    // events go out on the op's own channel, which several jobs may share.
    let key = task_id.unwrap_or_else(|| request.operation.clone());
    let progress = Arc::new(TauriProgress::new(
        app,
        progress_key.unwrap_or_else(|| key.clone()),
    ));
    // A default, not an override: an explicit `on_conflict` on the request or
    // in the user's config still wins.
    request.ctx.default_conflict = Some(ConflictPolicy::Overwrite);
    request.ctx.hash_cache = Some(Arc::clone(&cache));
    let token = begin(&state, &key).await;
    let runner_progress = Arc::clone(&progress);
    let outcome = on_dedicated_runtime(move |rt| {
        rt.block_on(run_request(request, runner_progress.as_ref(), token))
            .map_err(|err| {
                if is_cancelled_error(&err) {
                    "operation cancelled".to_string()
                } else {
                    err.to_string()
                }
            })
    })
    .await;
    // Ops whose outer bar counts units (the dat batches) leave it open; the
    // frontend's running flag clears on this.
    progress.finish();
    finish(&state, &key).await;
    let mut response = outcome?;
    // A partial failure is a completed run that carries records for the files
    // that did work; only a hard failure or a rejected request is an error.
    if response.status == RunStatus::Failed.as_i32()
        || response.status == RunStatus::InvalidArgument.as_i32()
    {
        return Err(response.message);
    }
    let data = response.data.take();
    let json = data
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(err_to_string)?;
    let (message, comparison) = match data {
        Some(RunData::Plan(line)) => (line.display_text(), None),
        Some(RunData::Plans(data)) => (
            data.plans
                .iter()
                .map(PlanLine::display_text)
                .collect::<Vec<_>>()
                .join("\n"),
            None,
        ),
        Some(RunData::BasicPlan(plan)) => (
            format!(
                "Would {} {} -> {}",
                plan.operation,
                plan.input.display(),
                plan.output.display()
            ),
            None,
        ),
        Some(RunData::Comparison(data)) => (response.message, Some(data.comparison)),
        _ => (response.message, None),
    };
    let (input_bytes, output_bytes) = response
        .totals
        .as_ref()
        .map(|t| (t.total_input_bytes, t.total_output_bytes))
        .or_else(|| {
            response
                .records
                .first()
                .map(|r| (r.input_bytes, r.output_bytes))
        })
        .or_else(|| comparison.as_ref().map(|c| (c.input_bytes, c.output_bytes)))
        .unwrap_or((0, 0));
    Ok(RunOutcome {
        message,
        status: response.status,
        records: if report { response.records } else { Vec::new() },
        input_bytes,
        output_bytes,
        comparison,
        data: json,
    })
}

#[derive(serde::Deserialize)]
pub struct ReportPayload {
    records: Vec<ReportRecord>,
    totals: ReportTotals,
}

/// Every image extension `info` reads, used to pick the member to inspect
/// when the input is an archive.
const ALL_IMAGE_EXTS: &[&str] = &[
    "iso", "gcm", "wbfs", "rvz", "gcz", "wia", "nkit", "chd", "cso", "zso", "dax", "cue", "cia",
    "3ds", "cci", "cxi", "3dsx", "zcia", "zcci", "zcxi", "z3dsx", "nsp", "xci", "nca", "nsz",
    "xcz", "ncz", "wud", "wux", "xiso", "zar", "avi",
];

fn input_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Write a run report from records the frontend accumulated during a run. The
/// format is inferred from the path extension and the file is written directly,
/// bypassing the on-conflict machinery, exactly as the CLI does.
#[tauri::command]
pub async fn cmd_write_report(path: PathBuf, payload: ReportPayload) -> Result<(), String> {
    let format = ReportFormat::from_path(&path);
    tokio::task::spawn_blocking(move || {
        write_report(
            &path,
            &payload.records,
            &payload.totals,
            format,
            &CancelToken::new(),
        )
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)
}

/// Size of `path` in bytes, or 0 if it cannot be read. Used to fill the
/// `input_bytes` field of a failed record on the frontend, matching the CLI,
/// whose `failed_record` carries the input file size.
#[tauri::command]
pub fn cmd_file_size(path: PathBuf) -> u64 {
    input_size(&path)
}

/// Which `prod.keys` file nx operations would use right now, for the GUI
/// status row. Home-relative results are shortened to `~` for display.
#[tauri::command]
pub fn cmd_nx_keys_resolve(keys: Option<PathBuf>) -> Option<String> {
    find_keys_file(keys.as_deref()).map(|p| rom_converto_lib::util::contract_tilde(&p))
}

#[tauri::command]
pub async fn cmd_read_info(
    cache: State<'_, Arc<InfoCache>>,
    input: PathBuf,
    keys: Option<PathBuf>,
) -> Result<String, String> {
    let cache_inner = cache.inner().clone();
    let result = tokio::task::spawn_blocking(move || -> Result<Arc<InfoResult>, anyhow::Error> {
        if let Some(key) = InfoCache::key_for(&input, keys.as_deref())
            && let Some(hit) = cache_inner.get(&key)
        {
            return Ok(hit);
        }
        let resolved = rom_converto_lib::util::resolve_input(&input, ALL_IMAGE_EXTS)?;
        let opts = InfoOptions {
            keys_path: keys.clone(),
            parent_path: None,
        };
        let info = read_info(resolved.path(), &opts)?;
        let arc = Arc::new(info);
        if let Some(key) = InfoCache::key_for(&input, keys.as_deref()) {
            cache_inner.insert(key, arc.clone());
        }
        Ok(arc)
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)?;
    serde_json::to_string(result.as_ref()).map_err(err_to_string)
}

/// The frontend posts back the InfoResult JSON it already holds, so the
/// Rust side does not need to redo the extraction.
#[tauri::command]
pub async fn cmd_save_icon(info_json: String, dest: PathBuf) -> Result<String, String> {
    let info: InfoResult = serde_json::from_str(&info_json).map_err(err_to_string)?;
    let bytes =
        extract_icon_png(&info).ok_or_else(|| "This file has no icon to save.".to_string())?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(err_to_string)?;
    }
    std::fs::write(&dest, &bytes).map_err(err_to_string)?;
    Ok(dest.display().to_string())
}

/// Recursively scan `dir` for files matching `exts`, applying the same junk
/// filter and sort as the CLI batch walk. A `"*"` entry in `exts` matches
/// every file; an empty `exts` matches nothing. A non-directory path surfaces
/// as an error so the caller can fall back to treating it as a single file.
#[tauri::command]
pub async fn cmd_scan_dir(
    dir: PathBuf,
    exts: Vec<String>,
    max_depth: Option<usize>,
) -> Result<Vec<PathBuf>, String> {
    tokio::task::spawn_blocking(move || {
        // "*" lists every file; an empty list still matches nothing so folder-input ops stay unexpanded.
        if exts.iter().any(|e| e == "*") {
            return collect_all_files(&dir, max_depth, &CancelToken::new()).map_err(err_to_string);
        }
        let ext_refs: Vec<&str> = exts.iter().map(String::as_str).collect();
        collect_files_with_exts(&dir, &ext_refs, max_depth, &CancelToken::new())
            .map_err(err_to_string)
    })
    .await
    .map_err(err_to_string)?
}

#[tauri::command]
pub fn app_display_version() -> &'static str {
    env!("ROM_CONVERTO_DISPLAY_VERSION")
}

fn extract_icon_png(info: &InfoResult) -> Option<Vec<u8>> {
    match info {
        InfoResult::Ctr(c) => c.icon.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::Dol(d) => d.banner_image.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::Rvl(r) => r.image.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::Wup(w) => w.image.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::Nx(n) => n
            .full
            .as_ref()
            .and_then(|f| f.control.as_ref())
            .and_then(|c| c.icon.as_ref())
            .map(|i| i.png_bytes.clone()),
        InfoResult::Chd(c) => c.content.as_ref().and_then(disc_content_icon_png),
        InfoResult::Cso(c) => c.content.as_ref().and_then(disc_content_icon_png),
        InfoResult::Xbox(x) => x
            .xbe
            .as_ref()
            .and_then(|b| b.icon.as_ref())
            .or_else(|| x.xex.as_ref().and_then(|x| x.icon.as_ref()))
            .map(|i| i.png_bytes.clone()),
        InfoResult::Xenon(z) => z
            .xex
            .as_ref()
            .and_then(|x| x.icon.as_ref())
            .map(|i| i.png_bytes.clone()),
        InfoResult::Ps3(p) => p.icon.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::Psx(_) => None,
        InfoResult::Psp(p) => p.icon.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::LaserDisc(_) => None,
        InfoResult::Nds(n) => n.banner.as_ref().map(|b| b.icon.png_bytes.clone()),
        InfoResult::Retro(_) => None,
        InfoResult::Pbp(p) => p.icon.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::Vpk(v) => v.icon.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::Pkg(p) => p.icon.as_ref().map(|i| i.png_bytes.clone()),
    }
}

fn disc_content_icon_png(content: &DiscContent) -> Option<Vec<u8>> {
    match content {
        DiscContent::Psp(p) => p.icon.as_ref().map(|i| i.png_bytes.clone()),
        DiscContent::Psx(_) => None,
    }
}

#[cfg(test)]
mod comparison_tests {
    use super::*;
    use rom_converto_lib::runner::models::VerifyReport;
    use serde_json::Value;

    // Assert an object has exactly `expected` keys, catching a stray
    // camelCase field before it reaches the snake_case TS contract.
    fn assert_keys(v: &Value, expected: &[&str]) {
        let obj = v.as_object().expect("object");
        let mut got: Vec<&str> = obj.keys().map(String::as_str).collect();
        got.sort_unstable();
        let mut want: Vec<&str> = expected.to_vec();
        want.sort_unstable();
        assert_eq!(got, want);
    }

    #[test]
    fn comparison_summary_keys_are_snake_case() {
        let summary = ComparisonData {
            input_bytes: 1024,
            output_bytes: 256,
            ratio_pct: Some(75.0),
            input_format: "ISO".into(),
            output_format: "RVZ".into(),
            output_sha1: Some("abc123".into()),
            verify: Some(VerifyReport {
                ok: true,
                round_trip: false,
                message: "Verified".into(),
            }),
        };
        let v = serde_json::to_value(&summary).unwrap();
        assert_keys(
            &v,
            &[
                "input_bytes",
                "output_bytes",
                "ratio_pct",
                "input_format",
                "output_format",
                "output_sha1",
                "verify",
            ],
        );
        assert_keys(&v["verify"], &["ok", "round_trip", "message"]);
    }

    // The queue reads `status` to tell a partial failure from a clean run.
    #[test]
    fn run_outcome_carries_run_status() {
        let outcome = RunOutcome {
            message: "2 of 3 files failed".into(),
            status: RunStatus::PartialFailure.as_i32(),
            records: Vec::new(),
            input_bytes: 0,
            output_bytes: 0,
            comparison: None,
            data: None,
        };
        let v = serde_json::to_value(&outcome).unwrap();
        assert_keys(
            &v,
            &[
                "message",
                "status",
                "records",
                "input_bytes",
                "output_bytes",
                "comparison",
                "data",
            ],
        );
        assert_eq!(v["status"], RunStatus::PartialFailure.as_i32());
    }
}
