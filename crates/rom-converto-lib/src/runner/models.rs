use crate::dat::model::{DatFileSummary, GameAndRelationMatchResult};
use crate::util::{FileDigests, PlanLine, ReportRecord, ReportTotals};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::{RUN_SCHEMA, totals_for};

#[derive(Debug, Serialize)]
pub struct RunSchemaManifest {
    pub schema: &'static str,
    pub request: RequestSchema,
    pub response: ResponseSchema,
    pub operations: &'static [&'static str],
    pub common_options: CommonOptionsSchema,
}

#[derive(Debug, Serialize)]
pub struct FfiVersionManifest {
    pub schema: &'static str,
    pub abi_version: u32,
    pub library_version: &'static str,
    pub runner_schema: RunSchemaManifest,
    pub status_codes: StatusCodeSchema,
}

impl FfiVersionManifest {
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
            operations: &[
                "cso.compress",
                "cso.decompress",
                "cso.verify",
                "cso.to_chd",
                "cso.to-chd",
                "chd.compress",
                "chd.extract",
                "chd.verify",
                "chd.to_cso",
                "chd.to-cso",
                "dol.compress",
                "dol.decompress",
                "dol.migrate",
                "dol.verify",
                "rvl.compress",
                "rvl.decompress",
                "rvl.migrate",
                "rvl.verify",
                "rvz.compress",
                "rvz.decompress",
                "rvz.migrate",
                "ctr.cdn_to_cia",
                "ctr.cdn-to-cia",
                "ctr.generate_cdn_ticket",
                "ctr.generate-cdn-ticket",
                "ctr.decrypt",
                "ctr.encrypt",
                "ctr.compress",
                "ctr.decompress",
                "ctr.convert",
                "ctr.verify",
                "nx.compress",
                "nx.decompress",
                "nx.verify",
                "wup.compress",
                "wup.decrypt",
                "wup.verify",
                "cue.merge",
                "playlist.write",
                "playlist",
                "dat.verify",
                "dat.scan",
                "dat.rename",
                "dat.identify",
                "dat.fixdat",
                "hash",
                "info",
                "info.read",
            ],
            common_options: CommonOptionsSchema {
                on_conflict: &["error", "overwrite", "skip", "rename", "overwrite_invalid"],
                recursive: "bool",
                output_dir: "path",
                output_template: "string",
                max_depth: "usize",
                report: "path",
            },
        }
    }
}

#[derive(Debug, Serialize)]
pub struct RequestSchema {
    pub required: &'static [&'static str],
    pub fields: RequestFieldsSchema,
}

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

#[derive(Debug, Serialize)]
pub struct ResponseSchema {
    pub status_codes: StatusCodeSchema,
    pub fields: &'static [&'static str],
}

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

#[derive(Debug, Serialize)]
pub struct CommonOptionsSchema {
    pub on_conflict: &'static [&'static str],
    pub recursive: &'static str,
    pub output_dir: &'static str,
    pub output_template: &'static str,
    pub max_depth: &'static str,
    pub report: &'static str,
}

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

#[derive(Debug, Serialize)]
pub struct RunResponse {
    pub schema: &'static str,
    pub ok: bool,
    pub status: i32,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub totals: Option<ReportTotals>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<ReportRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<ProgressEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<RunData>,
}

impl RunResponse {
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

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum RunData {
    Plan(PlanLine),
    Plans(RunPlansData),
    Hash(FileDigests),
    Comparison(RunComparisonData),
    BasicPlan(BasicPlanData),
    CtrVerify(crate::nintendo::ctr::verify::CtrVerifyResult),
    DolVerify(crate::nintendo::dol::verify::DolVerifyResult),
    RvlVerify(crate::nintendo::rvl::verify::RvlVerifyResult),
    NxVerify(crate::nintendo::nx::NxVerifyResult),
    WupVerify(crate::nintendo::wup::WupVerifyResult),
    Info(crate::info::InfoResult),
    Playlists(PlaylistsData),
    DatMatch(DatMatchData),
    DatScan(DatScanData),
    DatRename(DatRenameData),
    FixdatPlan(FixdatPlanData),
    FixdatWritten(FixdatWrittenData),
}

#[derive(Debug, Serialize)]
pub struct RunPlansData {
    pub plans: Vec<PlanLine>,
}

#[derive(Debug, Serialize)]
pub struct RunComparisonData {
    pub comparison: ComparisonData,
}

#[derive(Debug, Serialize)]
pub struct ComparisonData {
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub ratio_pct: Option<f64>,
    pub input_format: String,
    pub output_format: String,
}

#[derive(Debug, Serialize)]
pub struct BasicPlanData {
    pub operation: &'static str,
    pub input: PathBuf,
    pub output: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct PlaylistsData {
    pub playlists: Vec<PlaylistPlanData>,
}

#[derive(Debug, Serialize)]
pub struct PlaylistPlanData {
    pub base_title: String,
    pub output: PathBuf,
    pub contents: String,
    pub disc_count: usize,
    pub has_duplicate_numbers: bool,
}

#[derive(Debug, Serialize)]
pub struct DatMatchData {
    pub kind: &'static str,
    pub path: PathBuf,
    pub verdict: String,
    pub match_algo: Option<String>,
    pub game_name: Option<String>,
    pub platform: Option<String>,
    pub signature_group: Option<String>,
    pub dat_file: Option<String>,
    pub dat_file_id: Option<String>,
    pub dat_version: Option<String>,
    #[serde(rename = "match")]
    pub matched: Option<GameAndRelationMatchResult>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DatScanData {
    pub rows: Vec<DatMatchData>,
}

#[derive(Debug, Serialize)]
pub struct DatRenameData {
    pub rows: Vec<DatRenameRowData>,
    pub dry_run: bool,
}

#[derive(Debug, Serialize)]
pub struct DatRenameRowData {
    pub from: PathBuf,
    pub to: Option<PathBuf>,
    pub action: &'static str,
    pub detail: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FixdatPlanData {
    pub dat_file: DatFileSummary,
    pub missing_count: usize,
}

#[derive(Debug, Serialize)]
pub struct FixdatWrittenData {
    pub dat_file: DatFileSummary,
    pub missing_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProgressEvent {
    Start { total: u64, message: String },
    Advance { delta: u64 },
    Finish,
    Phase { message: String },
    Warn { message: String },
}

#[derive(Clone, Debug, Deserialize)]
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
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
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
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum WupTitleInputOption {
    Path(PathBuf),
    Object {
        path: PathBuf,
        format: Option<String>,
        key: Option<PathBuf>,
        key_path: Option<PathBuf>,
    },
}
