use crate::dat::model::DatFileSummary;
pub use crate::dat::run::{DatMatchData, DatTrackCheck, ExternalId};
pub use crate::dat::scan::{DatScanData, DatScanRow};
use crate::util::report::ser_status;
use crate::util::{FileDigests, FileStatus, PlanLine, ReportRecord, ReportTotals};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::RUN_SCHEMA;
use super::cli_echo::{CliEchoManifest, manifest as cli_echo_manifest};
use super::ops::{operation_names, totals_for};

/// The runner's request/response schema: required and optional request
/// fields, response fields, supported operations, and common option names.
#[derive(Debug, Serialize)]
pub struct RunSchemaManifest {
    pub schema: &'static str,
    pub request: RequestSchema,
    pub response: ResponseSchema,
    pub operations: &'static [&'static str],
    pub common_options: CommonOptionsSchema,
    pub cli: CliEchoManifest,
}

/// Version and schema manifest returned by the C ABI's version query.
#[derive(Debug, Serialize)]
pub struct FfiVersionManifest {
    pub schema: &'static str,
    pub abi_version: u32,
    pub library_version: &'static str,
    pub runner_schema: RunSchemaManifest,
    pub status_codes: StatusCodeSchema,
}

impl FfiVersionManifest {
    /// Builds the manifest for the given ABI version and library version.
    pub fn current(schema: &'static str, abi_version: u32, library_version: &'static str) -> Self {
        Self {
            schema,
            abi_version,
            library_version,
            runner_schema: RunSchemaManifest::current(),
            status_codes: StatusCodeSchema::current(),
        }
    }
}

impl RunSchemaManifest {
    /// Builds the current runner schema manifest.
    pub fn current() -> Self {
        Self {
            schema: RUN_SCHEMA,
            request: RequestSchema {
                required: &["operation"],
                fields: RequestFieldsSchema {
                    schema: "string",
                    operation: "string",
                    input: "path",
                    output: "path",
                    config: "path",
                    preset: "string",
                    dry_run: "bool",
                    options: "object",
                },
            },
            response: ResponseSchema {
                status_codes: StatusCodeSchema::current(),
                fields: &[
                    "schema", "ok", "status", "code", "message", "details", "totals", "records",
                    "events", "data",
                ],
            },
            operations: operation_names(),
            common_options: CommonOptionsSchema {
                on_conflict: &["error", "overwrite", "skip", "rename", "overwrite_invalid"],
                recursive: "bool",
                output_dir: "path",
                output_template: "string",
                max_depth: "usize",
                report: "path",
                dat: "bool",
                move_source: "bool",
                playlists: "bool",
                skip_space_check: "bool",
                verify_after: "bool",
                quick: "bool",
                skip_probe: "bool",
                media_patch: "bool",
                title: "string",
            },
            cli: cli_echo_manifest(),
        }
    }
}

/// Schema of a [`RunRequest`]: which fields are required and what type
/// each field is.
#[derive(Debug, Serialize)]
pub struct RequestSchema {
    pub required: &'static [&'static str],
    pub fields: RequestFieldsSchema,
}

/// Type name of each [`RunRequest`] field, for the schema manifest.
#[derive(Debug, Serialize)]
pub struct RequestFieldsSchema {
    pub schema: &'static str,
    pub operation: &'static str,
    pub input: &'static str,
    pub output: &'static str,
    pub config: &'static str,
    pub preset: &'static str,
    pub dry_run: &'static str,
    pub options: &'static str,
}

/// Schema of a [`RunResponse`]: status codes and the set of possible fields.
#[derive(Debug, Serialize)]
pub struct ResponseSchema {
    pub status_codes: StatusCodeSchema,
    pub fields: &'static [&'static str],
}

/// Numeric status codes for each [`RunStatus`] variant.
#[derive(Debug, Serialize)]
pub struct StatusCodeSchema {
    pub ok: i32,
    pub failed: i32,
    pub invalid_argument: i32,
    pub partial_failure: i32,
    pub cancelled: i32,
    pub internal_error: i32,
}

impl StatusCodeSchema {
    fn current() -> Self {
        Self {
            ok: RunStatus::Ok.as_i32(),
            failed: RunStatus::Failed.as_i32(),
            invalid_argument: RunStatus::InvalidArgument.as_i32(),
            partial_failure: RunStatus::PartialFailure.as_i32(),
            cancelled: RunStatus::Cancelled.as_i32(),
            internal_error: RunStatus::InternalError.as_i32(),
        }
    }
}

/// Type names of the [`RunOptions`] fields shared across operations.
#[derive(Debug, Serialize)]
pub struct CommonOptionsSchema {
    pub on_conflict: &'static [&'static str],
    pub recursive: &'static str,
    pub output_dir: &'static str,
    pub output_template: &'static str,
    pub max_depth: &'static str,
    pub report: &'static str,
    pub dat: &'static str,
    pub move_source: &'static str,
    pub playlists: &'static str,
    pub skip_space_check: &'static str,
    pub verify_after: &'static str,
    pub quick: &'static str,
    pub skip_probe: &'static str,
    pub media_patch: &'static str,
    pub title: &'static str,
}

/// Outcome category of a run, mapped to a stable numeric exit code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Ok,
    Failed,
    InvalidArgument,
    PartialFailure,
    Cancelled,
    InternalError,
}

impl RunStatus {
    /// Numeric exit code for this status.
    pub fn as_i32(self) -> i32 {
        match self {
            Self::Ok => 0,
            Self::Failed => 1,
            Self::InvalidArgument => 2,
            Self::PartialFailure => 3,
            Self::Cancelled => 130,
            Self::InternalError => 255,
        }
    }

    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::InvalidArgument => "invalid_argument",
            Self::PartialFailure => "partial_failure",
            Self::Cancelled => "cancelled",
            Self::InternalError => "internal_error",
        }
    }
}

/// JSON-encoded result of one run: status, message, records, progress
/// events, and operation-specific data.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct RunResponse {
    pub schema: &'static str,
    pub ok: bool,
    pub status: i32,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub details: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub totals: Option<ReportTotals>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<ReportRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<ProgressEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub data: Option<RunData>,
}

impl RunResponse {
    /// Builds a successful response carrying `data`.
    pub fn ok(message: impl Into<String>, data: Option<RunData>) -> Self {
        Self {
            schema: RUN_SCHEMA,
            ok: true,
            status: RunStatus::Ok.as_i32(),
            code: RunStatus::Ok.code().to_string(),
            message: message.into(),
            details: None,
            totals: None,
            records: Vec::new(),
            events: Vec::new(),
            data,
        }
    }

    /// Builds a failed response with the given status and message.
    pub fn error(status: RunStatus, message: impl Into<String>, details: Option<String>) -> Self {
        Self {
            schema: RUN_SCHEMA,
            ok: false,
            status: status.as_i32(),
            code: status.code().to_string(),
            message: message.into(),
            details,
            totals: None,
            records: Vec::new(),
            events: Vec::new(),
            data: None,
        }
    }

    pub(crate) fn with_record(mut self, record: ReportRecord) -> Self {
        self.totals = Some(totals_for(&record));
        self.records.push(record);
        self
    }
}

/// Operation-specific payload of a [`RunResponse`], one variant per
/// operation family.
#[derive(Debug, Serialize)]
#[serde(untagged)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub enum RunData {
    Plan(PlanLine),
    Plans(RunPlansData),
    Hash(FileDigests),
    Hashes(Vec<HashRow>),
    Comparison(RunComparisonData),
    BasicPlan(BasicPlanData),
    CtrVerify(crate::nintendo::ctr::verify::CtrVerifyResult),
    DolVerify(crate::nintendo::dol::verify::DolVerifyResult),
    RvlVerify(crate::nintendo::rvl::verify::RvlVerifyResult),
    NxVerify(crate::nintendo::nx::NxVerifyResult),
    WupVerify(crate::nintendo::wup::WupVerifyResult),
    XenonVerify(XenonVerifyData),
    XenonConvert(XenonConvertData),
    Info(crate::info::InfoResult),
    Organize(OrganizeData),
    Playlists(PlaylistsData),
    DatMatch(DatMatchData),
    DatVerify(DatVerifyData),
    DatScan(DatScanData),
    DatRename(DatRenameData),
    FixdatPlan(FixdatPlanData),
    FixdatWritten(FixdatWrittenData),
}

/// Dry-run plan lines for an operation that plans to a list of actions.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct RunPlansData {
    pub plans: Vec<PlanLine>,
}

/// One file's digests from a recursive `hash` run.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct HashRow {
    pub path: PathBuf,
    pub digests: FileDigests,
}

/// Wraps a [`ComparisonData`] for the response's `data` field.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct RunComparisonData {
    pub comparison: ComparisonData,
}

/// Input/output size comparison for a completed conversion.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct ComparisonData {
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub ratio_pct: Option<f64>,
    pub input_format: String,
    pub output_format: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub output_sha1: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub verify: Option<VerifyReport>,
}

/// Outcome of a post-conversion verification pass.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct VerifyReport {
    pub ok: bool,
    pub round_trip: bool,
    pub message: String,
}

/// Result of a `xenon.verify` run.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct XenonVerifyData {
    pub blocks: u64,
    pub logical_bytes: u64,
    pub hash_ok: bool,
}

/// Dry-run plan for a simple one-input-one-output operation. No runner
/// operation emits it any more; every dry run plans a [`PlanLine`].
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct BasicPlanData {
    pub operation: &'static str,
    pub input: PathBuf,
    pub output: PathBuf,
}

/// Result of a `xenon.convert` run: the Games on Demand container written.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct XenonConvertData {
    pub title_id: u32,
    pub media_id: u32,
    pub part_count: u64,
    pub total_bytes: u64,
}

/// Result of a `playlist.write` run: the playlists that were (or would be)
/// written.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct PlaylistsData {
    pub playlists: Vec<PlaylistPlanData>,
}

/// One planned or written `.m3u` playlist.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct PlaylistPlanData {
    pub base_title: String,
    pub output: PathBuf,
    pub contents: String,
    pub disc_count: usize,
    pub has_duplicate_numbers: bool,
}

/// One library item handled by an `organize` run.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct OrganizeRow {
    /// The unit's primary path: the file, the cue of a set, or the archive.
    pub input: PathBuf,
    /// Planned or written target path.
    pub output: Option<PathBuf>,
    /// Console folder label the unit was placed under.
    pub console: Option<String>,
    /// Child op name ("dol.compress", ...) or "zip" | "copy" | "move" | "skip".
    pub action: String,
    #[serde(serialize_with = "ser_status")]
    #[cfg_attr(feature = "ts-export", ts(type = "\"ok\" | \"skipped\" | \"failed\""))]
    pub status: FileStatus,
    /// True for dry-run rows, which plan without writing.
    pub planned: bool,
    /// Skip reason, plan decision text, or error.
    pub detail: Option<String>,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub elapsed_ms: u64,
}

/// Result of an `organize` run: one row per library item.
#[derive(Debug, Default, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct OrganizeData {
    pub rows: Vec<OrganizeRow>,
    pub dry_run: bool,
    pub ok: usize,
    pub skipped: usize,
    pub failed: usize,
    pub playlists: Vec<PlaylistPlanData>,
}

/// Result of a `dat.verify` run over a directory: per-verdict counts and one
/// [`DatMatchData`] per unit in walk order.
#[derive(Debug, Default, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct DatVerifyData {
    pub verified: usize,
    pub hint: usize,
    pub unknown: usize,
    pub unsupported: usize,
    pub failed: usize,
    pub rows: Vec<DatMatchData>,
}

impl DatVerifyData {
    /// Append a row, bumping the count for its verdict.
    pub fn push(&mut self, row: DatMatchData) {
        match row.verdict.as_str() {
            "verified" => self.verified += 1,
            "hint" => self.hint += 1,
            "unknown" => self.unknown += 1,
            "unsupported" => self.unsupported += 1,
            _ => self.failed += 1,
        }
        self.rows.push(row);
    }
}

/// Result of a `dat.rename` run: the planned or applied renames and how many
/// were renamed (or would be), skipped, or failed.
#[derive(Debug, Default, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct DatRenameData {
    pub rows: Vec<DatRenameRowData>,
    pub dry_run: bool,
    pub renamed: usize,
    pub skipped: usize,
    pub failed: usize,
}

/// One planned or applied rename.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct DatRenameRowData {
    pub from: PathBuf,
    pub to: Option<PathBuf>,
    pub action: &'static str,
    pub detail: Option<String>,
}

/// Dry-run result of a `dat.fixdat` run: the DAT file, how many of its games
/// are missing locally, and how many files those games span.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct FixdatPlanData {
    pub dat_file: DatFileSummary,
    pub total_games: usize,
    pub missing_count: usize,
    pub missing_files: usize,
    pub output: PathBuf,
}

/// Result of a `dat.fixdat` run: the DAT file and what was written.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct FixdatWrittenData {
    pub dat_file: DatFileSummary,
    pub total_games: usize,
    pub missing_count: usize,
    pub missing_files: usize,
    pub output: PathBuf,
}

/// One row emitted to a live consumer during a run: a finished file's
/// report record, or a dry-run plan line.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub enum RunRow {
    Record(ReportRecord),
    Plan(PlanLine),
    Hash(HashRow),
    DatMatch(Box<DatMatchData>),
    DatScan(DatScanRow),
    DatRename(DatRenameRowData),
    Organize(OrganizeRow),
}

/// One progress update emitted during a run.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub enum ProgressEvent {
    Start {
        total: u64,
        message: String,
    },
    Advance {
        delta: u64,
        /// Cumulative fraction done (0.0-1.0) since the preceding `Start`,
        /// clamped to 1.0. `None` when the total was 0 or unknown.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        fraction: Option<f64>,
    },
    Finish,
    Phase {
        message: String,
    },
    Warn {
        message: String,
    },
}

/// Deserialized JSON request: operation name, input/output paths, and
/// per-operation options.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct RunRequest {
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(alias = "op", alias = "command")]
    pub operation: String,
    #[serde(default)]
    pub input: Option<PathBuf>,
    #[serde(default)]
    pub output: Option<PathBuf>,
    #[serde(default)]
    pub config: Option<PathBuf>,
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub options: RunOptions,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(skip)]
    pub ctx: RunContext,
}

/// In-process state threaded into a run that cannot travel through JSON.
#[derive(Clone, Default)]
pub struct RunContext {
    pub hash_cache: Option<std::sync::Arc<crate::util::HashCache>>,
    /// Conflict policy for writing operations when neither the request nor
    /// the config defaults set `on_conflict`; `None` means `error`. Lets a
    /// frontend carry its own default without forcing it onto every request.
    pub default_conflict: Option<crate::util::ConflictPolicy>,
}

impl std::fmt::Debug for RunContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunContext")
            .field("hash_cache", &self.hash_cache.is_some())
            .field("default_conflict", &self.default_conflict)
            .finish()
    }
}

/// Per-operation options accepted in a [`RunRequest`], validated
/// per-operation by the handler that reads them.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct RunOptions {
    pub config: Option<PathBuf>,
    pub preset: Option<String>,
    pub on_conflict: Option<String>,
    pub recursive: Option<bool>,
    pub output_dir: Option<PathBuf>,
    pub output_template: Option<String>,
    pub max_depth: Option<usize>,
    pub report: Option<PathBuf>,
    pub format: Option<String>,
    pub block_size: Option<u32>,
    pub hunk_size: Option<u32>,
    pub codecs: Option<Vec<String>>,
    pub mode: Option<String>,
    pub parent: Option<PathBuf>,
    pub full: Option<bool>,
    pub fix: Option<bool>,
    pub level: Option<i32>,
    pub chunk_size: Option<u32>,
    pub skip_verify: Option<bool>,
    pub deep: Option<bool>,
    pub deep_verify: Option<bool>,
    pub algo: Option<String>,
    pub allow_encrypted: Option<bool>,
    pub content_hashes: Option<bool>,
    pub trim: Option<bool>,
    pub compress: Option<bool>,
    pub cleanup: Option<bool>,
    pub ensure_ticket_exists: Option<bool>,
    pub decrypt: Option<bool>,
    pub output_dir_cia: Option<PathBuf>,
    pub keys: Option<PathBuf>,
    pub block_size_exp: Option<u32>,
    pub key: Option<PathBuf>,
    pub extensions: Option<String>,
    pub playlist_mode: Option<String>,
    pub api_base: Option<String>,
    pub input_checksum_min: Option<String>,
    pub input_checksum_max: Option<String>,
    pub inputs: Option<Vec<WupTitleInputOption>>,
    pub platform: Option<String>,
    pub dat_id: Option<String>,
    pub dat_name: Option<String>,
    pub subset: Option<String>,
    pub skip_space_check: Option<bool>,
    pub verify_after: Option<bool>,
    pub quick: Option<bool>,
    pub skip_probe: Option<bool>,
    pub media_patch: Option<bool>,
    pub title: Option<String>,
    pub dat: Option<bool>,
    pub move_source: Option<bool>,
    pub playlists: Option<bool>,
}

/// One Wii U title input: a bare path, or a path with an explicit format
/// and key.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub enum WupTitleInputOption {
    Path(PathBuf),
    Object {
        path: PathBuf,
        format: Option<String>,
        key: Option<PathBuf>,
        key_path: Option<PathBuf>,
    },
}
