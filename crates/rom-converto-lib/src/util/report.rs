//! Run reports: per-file records and run totals written to CSV, JSON, or
//! HTML at the end of a batch run, via `--report`.

use crate::util::CancelToken;
use crate::util::tally::{FileStatus, format_bytes};
use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::borrow::Cow;
use std::io::{BufWriter, Write};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReportRecord {
    pub input_path: String,
    pub output_path: String,
    pub operation: String,
    #[serde(serialize_with = "ser_status", deserialize_with = "de_status")]
    pub status: FileStatus,
    pub input_bytes: u64,
    pub output_bytes: u64,
    /// Space saved as a percentage of `input_bytes`, rounded to one decimal
    /// place; negative when the output is larger. `None` for skipped or
    /// failed files, where there is no meaningful output size to compare.
    pub ratio_pct: Option<f64>,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReportTotals {
    pub total_files: usize,
    /// Files converted successfully.
    pub ok: usize,
    /// Files not converted because a valid output already existed
    /// (`--on-conflict skip`, or `overwrite-invalid` finding it valid).
    pub skipped: usize,
    /// Files that returned an error during conversion.
    pub failed: usize,
    pub total_input_bytes: u64,
    pub total_output_bytes: u64,
    pub elapsed_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportFormat {
    Csv,
    Json,
    Html,
}

impl ReportFormat {
    /// Unknown or missing extensions default to JSON.
    pub fn from_path(path: &Path) -> Self {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("csv") => Self::Csv,
            Some("html") | Some("htm") => Self::Html,
            _ => Self::Json,
        }
    }
}

impl ReportRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        input_path: String,
        output_path: String,
        operation: impl Into<String>,
        status: FileStatus,
        input_bytes: u64,
        output_bytes: u64,
        elapsed_ms: u64,
        error: Option<String>,
    ) -> Self {
        let ratio_pct = match status {
            FileStatus::Ok if input_bytes > 0 => {
                let saved = (1.0 - output_bytes as f64 / input_bytes as f64) * 100.0;
                Some((saved * 10.0).round() / 10.0)
            }
            _ => None,
        };
        Self {
            input_path,
            output_path,
            operation: operation.into(),
            status,
            input_bytes,
            output_bytes,
            ratio_pct,
            elapsed_ms,
            error,
        }
    }
}

fn status_str(status: FileStatus) -> &'static str {
    match status {
        FileStatus::Ok => "ok",
        FileStatus::Skipped => "skipped",
        FileStatus::Failed => "failed",
    }
}

fn ser_status<S: Serializer>(status: &FileStatus, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(status_str(*status))
}

fn de_status<'de, D: Deserializer<'de>>(d: D) -> Result<FileStatus, D::Error> {
    let s = String::deserialize(d)?;
    match s.as_str() {
        "ok" => Ok(FileStatus::Ok),
        "skipped" => Ok(FileStatus::Skipped),
        "failed" => Ok(FileStatus::Failed),
        other => Err(serde::de::Error::custom(format!(
            "unknown status {other:?}"
        ))),
    }
}

/// Write a run report to `path`. The file is created and truncated directly,
/// bypassing the ROM on-conflict machinery: the report path is an output the
/// user named explicitly, not a converted ROM.
pub fn write_report(
    path: &Path,
    records: &[ReportRecord],
    totals: &ReportTotals,
    format: ReportFormat,
) -> Result<()> {
    write_report_cancellable(path, records, totals, format, &CancelToken::new())
}

pub fn write_report_cancellable(
    path: &Path,
    records: &[ReportRecord],
    totals: &ReportTotals,
    format: ReportFormat,
    cancel: &CancelToken,
) -> Result<()> {
    check_cancel(cancel)?;
    let mut tmp = report_temp(path)?;
    {
        let mut w = CancelWriter::new(BufWriter::new(tmp.as_file_mut()), cancel);
        match format {
            ReportFormat::Csv => write_csv(&mut w, records, totals)?,
            ReportFormat::Json => write_json(&mut w, records, totals)?,
            ReportFormat::Html => write_html(&mut w, records, totals)?,
        }
        w.flush()?;
    }
    persist_report(tmp, path, cancel)?;
    Ok(())
}

const CSV_HEADER: &str =
    "input_path,output_path,operation,status,input_bytes,output_bytes,ratio_pct,elapsed_ms,error";

fn csv_field(s: &str) -> Cow<'_, str> {
    if s.contains([',', '"', '\n', '\r']) {
        Cow::Owned(format!("\"{}\"", s.replace('"', "\"\"")))
    } else {
        Cow::Borrowed(s)
    }
}

fn write_csv<W: Write>(w: &mut W, records: &[ReportRecord], _totals: &ReportTotals) -> Result<()> {
    writeln!(w, "{CSV_HEADER}")?;
    for r in records {
        let ratio = r.ratio_pct.map(|v| format!("{v:.1}")).unwrap_or_default();
        let error = r.error.as_deref().unwrap_or("");
        writeln!(
            w,
            "{},{},{},{},{},{},{},{},{}",
            csv_field(&r.input_path),
            csv_field(&r.output_path),
            csv_field(&r.operation),
            status_str(r.status),
            r.input_bytes,
            r.output_bytes,
            ratio,
            r.elapsed_ms,
            csv_field(error),
        )?;
    }
    Ok(())
}

#[derive(Serialize)]
struct ReportDoc<'a> {
    files: &'a [ReportRecord],
    totals: &'a ReportTotals,
}

fn write_json<W: Write>(w: &mut W, records: &[ReportRecord], totals: &ReportTotals) -> Result<()> {
    let doc = ReportDoc {
        files: records,
        totals,
    };
    serde_json::to_writer_pretty(&mut *w, &doc)?;
    writeln!(w)?;
    Ok(())
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn write_html<W: Write>(w: &mut W, records: &[ReportRecord], totals: &ReportTotals) -> Result<()> {
    writeln!(w, "<!DOCTYPE html>")?;
    writeln!(w, "<html lang=\"en\">")?;
    writeln!(w, "<head>")?;
    writeln!(w, "<meta charset=\"utf-8\">")?;
    writeln!(w, "<title>rom-converto run report</title>")?;
    writeln!(
        w,
        "<style>body{{font-family:sans-serif;margin:1.5rem}}\
table{{border-collapse:collapse;width:100%}}\
th,td{{border:1px solid #ccc;padding:4px 8px;text-align:left;font-size:14px}}\
thead th{{background:#f0f0f0}}\
tfoot td{{font-weight:bold;background:#f7f7f7}}\
td.num{{text-align:right;font-variant-numeric:tabular-nums}}</style>"
    )?;
    writeln!(w, "</head>")?;
    writeln!(w, "<body>")?;
    writeln!(w, "<h1>Run report</h1>")?;
    writeln!(w, "<table>")?;
    writeln!(w, "<thead><tr>")?;
    for col in [
        "Input",
        "Output",
        "Operation",
        "Status",
        "Input size",
        "Output size",
        "Ratio",
        "Elapsed",
        "Error",
    ] {
        write!(w, "<th>{col}</th>")?;
    }
    writeln!(w, "</tr></thead>")?;
    writeln!(w, "<tbody>")?;
    for r in records {
        let ratio = r.ratio_pct.map(|v| format!("{v:.1}%")).unwrap_or_default();
        let error = r.error.as_deref().unwrap_or("");
        write!(w, "<tr>")?;
        write!(w, "<td>{}</td>", html_escape(&r.input_path))?;
        write!(w, "<td>{}</td>", html_escape(&r.output_path))?;
        write!(w, "<td>{}</td>", html_escape(&r.operation))?;
        write!(w, "<td>{}</td>", status_str(r.status))?;
        write!(w, "<td class=\"num\">{}</td>", format_bytes(r.input_bytes))?;
        write!(w, "<td class=\"num\">{}</td>", format_bytes(r.output_bytes))?;
        write!(w, "<td class=\"num\">{}</td>", html_escape(&ratio))?;
        write!(w, "<td class=\"num\">{} ms</td>", r.elapsed_ms)?;
        write!(w, "<td>{}</td>", html_escape(error))?;
        writeln!(w, "</tr>")?;
    }
    writeln!(w, "</tbody>")?;
    writeln!(w, "<tfoot><tr>")?;
    write!(
        w,
        "<td>{} files ({} ok, {} skipped, {} failed)</td>",
        totals.total_files, totals.ok, totals.skipped, totals.failed
    )?;
    write!(w, "<td></td><td></td><td>totals</td>")?;
    write!(
        w,
        "<td class=\"num\">{}</td>",
        format_bytes(totals.total_input_bytes)
    )?;
    write!(
        w,
        "<td class=\"num\">{}</td>",
        format_bytes(totals.total_output_bytes)
    )?;
    write!(w, "<td></td>")?;
    write!(w, "<td class=\"num\">{} ms</td>", totals.elapsed_ms)?;
    write!(w, "<td></td>")?;
    writeln!(w, "</tr></tfoot>")?;
    writeln!(w, "</table>")?;
    writeln!(w, "</body>")?;
    writeln!(w, "</html>")?;
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
pub struct HashReportRecord {
    pub path: String,
    pub crc32: Option<String>,
    pub sha1: Option<String>,
    pub md5: Option<String>,
    pub sha256: Option<String>,
    pub size_bytes: u64,
    #[serde(serialize_with = "ser_status")]
    pub status: FileStatus,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

/// Write a hash run report to `path`, reusing the same CSV/JSON/HTML
/// infrastructure as `write_report`. Digest columns replace the
/// conversion-shaped output/ratio columns, since hashing has no output file.
pub fn write_hash_report(
    path: &Path,
    records: &[HashReportRecord],
    totals: &ReportTotals,
    format: ReportFormat,
) -> Result<()> {
    write_hash_report_cancellable(path, records, totals, format, &CancelToken::new())
}

pub fn write_hash_report_cancellable(
    path: &Path,
    records: &[HashReportRecord],
    totals: &ReportTotals,
    format: ReportFormat,
    cancel: &CancelToken,
) -> Result<()> {
    check_cancel(cancel)?;
    let mut tmp = report_temp(path)?;
    {
        let mut w = CancelWriter::new(BufWriter::new(tmp.as_file_mut()), cancel);
        match format {
            ReportFormat::Csv => write_hash_csv(&mut w, records)?,
            ReportFormat::Json => write_hash_json(&mut w, records, totals)?,
            ReportFormat::Html => write_hash_html(&mut w, records, totals)?,
        }
        w.flush()?;
    }
    persist_report(tmp, path, cancel)?;
    Ok(())
}

const HASH_CSV_HEADER: &str = "path,crc32,sha1,md5,sha256,size_bytes,status,elapsed_ms,error";

fn write_hash_csv<W: Write>(w: &mut W, records: &[HashReportRecord]) -> Result<()> {
    writeln!(w, "{HASH_CSV_HEADER}")?;
    for r in records {
        writeln!(
            w,
            "{},{},{},{},{},{},{},{},{}",
            csv_field(&r.path),
            r.crc32.as_deref().unwrap_or(""),
            r.sha1.as_deref().unwrap_or(""),
            r.md5.as_deref().unwrap_or(""),
            r.sha256.as_deref().unwrap_or(""),
            r.size_bytes,
            status_str(r.status),
            r.elapsed_ms,
            csv_field(r.error.as_deref().unwrap_or("")),
        )?;
    }
    Ok(())
}

#[derive(Serialize)]
struct HashReportDoc<'a> {
    files: &'a [HashReportRecord],
    totals: &'a ReportTotals,
}

fn write_hash_json<W: Write>(
    w: &mut W,
    records: &[HashReportRecord],
    totals: &ReportTotals,
) -> Result<()> {
    let doc = HashReportDoc {
        files: records,
        totals,
    };
    serde_json::to_writer_pretty(&mut *w, &doc)?;
    writeln!(w)?;
    Ok(())
}

fn write_hash_html<W: Write>(
    w: &mut W,
    records: &[HashReportRecord],
    totals: &ReportTotals,
) -> Result<()> {
    writeln!(w, "<!DOCTYPE html>")?;
    writeln!(w, "<html lang=\"en\">")?;
    writeln!(w, "<head>")?;
    writeln!(w, "<meta charset=\"utf-8\">")?;
    writeln!(w, "<title>rom-converto hash report</title>")?;
    writeln!(
        w,
        "<style>body{{font-family:sans-serif;margin:1.5rem}}\
table{{border-collapse:collapse;width:100%}}\
th,td{{border:1px solid #ccc;padding:4px 8px;text-align:left;font-size:14px}}\
thead th{{background:#f0f0f0}}\
tfoot td{{font-weight:bold;background:#f7f7f7}}\
td.num{{text-align:right;font-variant-numeric:tabular-nums}}</style>"
    )?;
    writeln!(w, "</head>")?;
    writeln!(w, "<body>")?;
    writeln!(w, "<h1>Hash report</h1>")?;
    writeln!(w, "<table>")?;
    writeln!(w, "<thead><tr>")?;
    for col in [
        "Path", "CRC32", "SHA1", "MD5", "SHA256", "Size", "Status", "Elapsed", "Error",
    ] {
        write!(w, "<th>{col}</th>")?;
    }
    writeln!(w, "</tr></thead>")?;
    writeln!(w, "<tbody>")?;
    for r in records {
        write!(w, "<tr>")?;
        write!(w, "<td>{}</td>", html_escape(&r.path))?;
        write!(w, "<td>{}</td>", r.crc32.as_deref().unwrap_or(""))?;
        write!(w, "<td>{}</td>", r.sha1.as_deref().unwrap_or(""))?;
        write!(w, "<td>{}</td>", r.md5.as_deref().unwrap_or(""))?;
        write!(w, "<td>{}</td>", r.sha256.as_deref().unwrap_or(""))?;
        write!(w, "<td class=\"num\">{}</td>", format_bytes(r.size_bytes))?;
        write!(w, "<td>{}</td>", status_str(r.status))?;
        write!(w, "<td class=\"num\">{} ms</td>", r.elapsed_ms)?;
        write!(
            w,
            "<td>{}</td>",
            html_escape(r.error.as_deref().unwrap_or(""))
        )?;
        writeln!(w, "</tr>")?;
    }
    writeln!(w, "</tbody>")?;
    writeln!(w, "<tfoot><tr>")?;
    write!(
        w,
        "<td>{} files ({} ok, {} failed)</td>",
        totals.total_files, totals.ok, totals.failed
    )?;
    write!(w, "<td></td><td></td><td></td><td>totals</td>")?;
    write!(
        w,
        "<td class=\"num\">{}</td>",
        format_bytes(totals.total_input_bytes)
    )?;
    write!(w, "<td></td>")?;
    write!(w, "<td class=\"num\">{} ms</td>", totals.elapsed_ms)?;
    write!(w, "<td></td>")?;
    writeln!(w, "</tr></tfoot>")?;
    writeln!(w, "</table>")?;
    writeln!(w, "</body>")?;
    writeln!(w, "</html>")?;
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
pub struct DatReportRecord {
    pub path: String,
    pub verdict: String,
    pub game_name: Option<String>,
    pub game_id: Option<String>,
    pub platform: Option<String>,
    pub signature_group: Option<String>,
    pub dat_file_name: Option<String>,
    pub dat_file_id: Option<String>,
    pub dat_version: Option<String>,
    pub match_algo: Option<String>,
    pub detail: Option<String>,
    pub size_bytes: u64,
    #[serde(serialize_with = "ser_status")]
    pub status: FileStatus,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

/// Write a dat run report to `path`, reusing the same CSV/JSON/HTML
/// infrastructure as `write_report`. Verdict and game metadata columns
/// replace the conversion-shaped output/ratio columns, since a dat run
/// identifies files rather than converting them.
pub fn write_dat_report(
    path: &Path,
    records: &[DatReportRecord],
    totals: &ReportTotals,
    format: ReportFormat,
) -> Result<()> {
    write_dat_report_cancellable(path, records, totals, format, &CancelToken::new())
}

pub fn write_dat_report_cancellable(
    path: &Path,
    records: &[DatReportRecord],
    totals: &ReportTotals,
    format: ReportFormat,
    cancel: &CancelToken,
) -> Result<()> {
    check_cancel(cancel)?;
    let mut tmp = report_temp(path)?;
    {
        let mut w = CancelWriter::new(BufWriter::new(tmp.as_file_mut()), cancel);
        match format {
            ReportFormat::Csv => write_dat_csv(&mut w, records)?,
            ReportFormat::Json => write_dat_json(&mut w, records, totals)?,
            ReportFormat::Html => write_dat_html(&mut w, records, totals)?,
        }
        w.flush()?;
    }
    persist_report(tmp, path, cancel)?;
    Ok(())
}

fn report_temp(path: &Path) -> Result<tempfile::NamedTempFile> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    tempfile::Builder::new()
        .prefix(".rom-converto-report-")
        .suffix(".tmp")
        .tempfile_in(parent)
        .with_context(|| format!("creating report temp file in {}", parent.display()))
}

fn persist_report(tmp: tempfile::NamedTempFile, path: &Path, cancel: &CancelToken) -> Result<()> {
    check_cancel(cancel)?;
    tmp.as_file().sync_all()?;
    check_cancel(cancel)?;
    tmp.persist(path)
        .with_context(|| format!("replacing report file {}", path.display()))?;
    Ok(())
}

fn check_cancel(cancel: &CancelToken) -> std::io::Result<()> {
    if cancel.is_cancelled() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "cancelled",
        ));
    }
    Ok(())
}

struct CancelWriter<'a, W> {
    inner: W,
    cancel: &'a CancelToken,
}

impl<'a, W> CancelWriter<'a, W> {
    fn new(inner: W, cancel: &'a CancelToken) -> Self {
        Self { inner, cancel }
    }
}

impl<W: Write> Write for CancelWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        check_cancel(self.cancel)?;
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        check_cancel(self.cancel)?;
        self.inner.flush()
    }
}

const DAT_CSV_HEADER: &str = "path,verdict,game_name,game_id,platform,signature_group,dat_file_name,dat_file_id,dat_version,match_algo,detail,size_bytes,status,elapsed_ms,error";

fn write_dat_csv<W: Write>(w: &mut W, records: &[DatReportRecord]) -> Result<()> {
    writeln!(w, "{DAT_CSV_HEADER}")?;
    for r in records {
        writeln!(
            w,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            csv_field(&r.path),
            csv_field(&r.verdict),
            csv_field(r.game_name.as_deref().unwrap_or("")),
            csv_field(r.game_id.as_deref().unwrap_or("")),
            csv_field(r.platform.as_deref().unwrap_or("")),
            csv_field(r.signature_group.as_deref().unwrap_or("")),
            csv_field(r.dat_file_name.as_deref().unwrap_or("")),
            csv_field(r.dat_file_id.as_deref().unwrap_or("")),
            csv_field(r.dat_version.as_deref().unwrap_or("")),
            csv_field(r.match_algo.as_deref().unwrap_or("")),
            csv_field(r.detail.as_deref().unwrap_or("")),
            r.size_bytes,
            status_str(r.status),
            r.elapsed_ms,
            csv_field(r.error.as_deref().unwrap_or("")),
        )?;
    }
    Ok(())
}

#[derive(Serialize)]
struct DatReportDoc<'a> {
    files: &'a [DatReportRecord],
    totals: &'a ReportTotals,
}

fn write_dat_json<W: Write>(
    w: &mut W,
    records: &[DatReportRecord],
    totals: &ReportTotals,
) -> Result<()> {
    let doc = DatReportDoc {
        files: records,
        totals,
    };
    serde_json::to_writer_pretty(&mut *w, &doc)?;
    writeln!(w)?;
    Ok(())
}

fn write_dat_html<W: Write>(
    w: &mut W,
    records: &[DatReportRecord],
    totals: &ReportTotals,
) -> Result<()> {
    writeln!(w, "<!DOCTYPE html>")?;
    writeln!(w, "<html lang=\"en\">")?;
    writeln!(w, "<head>")?;
    writeln!(w, "<meta charset=\"utf-8\">")?;
    writeln!(w, "<title>rom-converto dat report</title>")?;
    writeln!(
        w,
        "<style>body{{font-family:sans-serif;margin:1.5rem}}\
table{{border-collapse:collapse;width:100%}}\
th,td{{border:1px solid #ccc;padding:4px 8px;text-align:left;font-size:14px}}\
thead th{{background:#f0f0f0}}\
tfoot td{{font-weight:bold;background:#f7f7f7}}\
td.num{{text-align:right;font-variant-numeric:tabular-nums}}</style>"
    )?;
    writeln!(w, "</head>")?;
    writeln!(w, "<body>")?;
    writeln!(w, "<h1>Dat report</h1>")?;
    writeln!(w, "<table>")?;
    writeln!(w, "<thead><tr>")?;
    for col in [
        "Path",
        "Verdict",
        "Game",
        "Game id",
        "Platform",
        "Signature group",
        "DAT file",
        "DAT file id",
        "Dat version",
        "Match algo",
        "Detail",
        "Size",
        "Status",
        "Elapsed",
        "Error",
    ] {
        write!(w, "<th>{col}</th>")?;
    }
    writeln!(w, "</tr></thead>")?;
    writeln!(w, "<tbody>")?;
    for r in records {
        write!(w, "<tr>")?;
        write!(w, "<td>{}</td>", html_escape(&r.path))?;
        write!(w, "<td>{}</td>", html_escape(&r.verdict))?;
        write!(
            w,
            "<td>{}</td>",
            html_escape(r.game_name.as_deref().unwrap_or(""))
        )?;
        write!(
            w,
            "<td>{}</td>",
            html_escape(r.game_id.as_deref().unwrap_or(""))
        )?;
        write!(
            w,
            "<td>{}</td>",
            html_escape(r.platform.as_deref().unwrap_or(""))
        )?;
        write!(
            w,
            "<td>{}</td>",
            html_escape(r.signature_group.as_deref().unwrap_or(""))
        )?;
        write!(
            w,
            "<td>{}</td>",
            html_escape(r.dat_file_name.as_deref().unwrap_or(""))
        )?;
        write!(
            w,
            "<td>{}</td>",
            html_escape(r.dat_file_id.as_deref().unwrap_or(""))
        )?;
        write!(
            w,
            "<td>{}</td>",
            html_escape(r.dat_version.as_deref().unwrap_or(""))
        )?;
        write!(
            w,
            "<td>{}</td>",
            html_escape(r.match_algo.as_deref().unwrap_or(""))
        )?;
        write!(
            w,
            "<td>{}</td>",
            html_escape(r.detail.as_deref().unwrap_or(""))
        )?;
        write!(w, "<td class=\"num\">{}</td>", format_bytes(r.size_bytes))?;
        write!(w, "<td>{}</td>", status_str(r.status))?;
        write!(w, "<td class=\"num\">{} ms</td>", r.elapsed_ms)?;
        write!(
            w,
            "<td>{}</td>",
            html_escape(r.error.as_deref().unwrap_or(""))
        )?;
        writeln!(w, "</tr>")?;
    }
    writeln!(w, "</tbody>")?;
    writeln!(w, "<tfoot><tr>")?;
    write!(
        w,
        "<td>{} files ({} ok, {} skipped, {} failed)</td>",
        totals.total_files, totals.ok, totals.skipped, totals.failed
    )?;
    for _ in 0..10 {
        write!(w, "<td></td>")?;
    }
    write!(
        w,
        "<td class=\"num\">{}</td>",
        format_bytes(totals.total_input_bytes)
    )?;
    write!(w, "<td></td>")?;
    write!(w, "<td class=\"num\">{} ms</td>", totals.elapsed_ms)?;
    write!(w, "<td></td>")?;
    writeln!(w, "</tr></tfoot>")?;
    writeln!(w, "</table>")?;
    writeln!(w, "</body>")?;
    writeln!(w, "</html>")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_record() -> ReportRecord {
        ReportRecord::new(
            "in.iso".into(),
            "out.cso".into(),
            "compress",
            FileStatus::Ok,
            1024 * 1024,
            256 * 1024,
            1500,
            None,
        )
    }

    #[test]
    fn cancelled_report_preserves_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        std::fs::write(&path, b"existing").unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();

        assert!(
            write_report_cancellable(
                &path,
                &[ok_record()],
                &ReportTotals::default(),
                ReportFormat::Json,
                &cancel,
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"existing");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn report_replaces_existing_file_after_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        std::fs::write(&path, b"existing").unwrap();

        write_report_cancellable(
            &path,
            &[ok_record()],
            &ReportTotals::default(),
            ReportFormat::Json,
            &CancelToken::new(),
        )
        .unwrap();
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(report["files"].as_array().unwrap().len(), 1);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    fn render(records: &[ReportRecord], totals: &ReportTotals, format: ReportFormat) -> String {
        let mut buf = Vec::new();
        match format {
            ReportFormat::Csv => write_csv(&mut buf, records, totals).unwrap(),
            ReportFormat::Json => write_json(&mut buf, records, totals).unwrap(),
            ReportFormat::Html => write_html(&mut buf, records, totals).unwrap(),
        }
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn csv_header_and_one_ok_row() {
        let out = render(&[ok_record()], &ReportTotals::default(), ReportFormat::Csv);
        let mut lines = out.lines();
        assert_eq!(lines.next().unwrap(), CSV_HEADER);
        let row = lines.next().unwrap();
        assert_eq!(
            row, "in.iso,out.cso,compress,ok,1048576,262144,75.0,1500,",
            "{row}"
        );
    }

    #[test]
    fn csv_escapes_comma_in_path() {
        let rec = ReportRecord::new(
            "a,b.iso".into(),
            String::new(),
            "compress",
            FileStatus::Ok,
            10,
            5,
            0,
            None,
        );
        let out = render(&[rec], &ReportTotals::default(), ReportFormat::Csv);
        assert!(out.contains("\"a,b.iso\""), "{out}");
    }

    #[test]
    fn csv_escapes_quote_in_path() {
        let rec = ReportRecord::new(
            "a\"b.iso".into(),
            String::new(),
            "compress",
            FileStatus::Ok,
            10,
            5,
            0,
            None,
        );
        let out = render(&[rec], &ReportTotals::default(), ReportFormat::Csv);
        assert!(out.contains("\"a\"\"b.iso\""), "{out}");
    }

    #[test]
    fn csv_escapes_newline_in_path() {
        let rec = ReportRecord::new(
            "a\nb.iso".into(),
            String::new(),
            "compress",
            FileStatus::Ok,
            10,
            5,
            0,
            None,
        );
        let out = render(&[rec], &ReportTotals::default(), ReportFormat::Csv);
        assert!(out.contains("\"a\nb.iso\""), "{out}");
        assert_eq!(out.lines().count(), 3, "embedded newline split the row");
    }

    #[test]
    fn json_stable_schema() {
        let skipped = ReportRecord::new(
            "s.iso".into(),
            String::new(),
            "compress",
            FileStatus::Skipped,
            0,
            0,
            0,
            None,
        );
        let totals = ReportTotals {
            total_files: 2,
            ok: 1,
            skipped: 1,
            ..ReportTotals::default()
        };
        let out = render(&[ok_record(), skipped], &totals, ReportFormat::Json);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["files"].as_array().unwrap().len(), 2);
        assert_eq!(v["totals"]["ok"], 1);
        assert_eq!(v["files"][0]["status"], "ok");
        assert!(v["files"][0]["ratio_pct"].is_number());
        assert_eq!(v["files"][0]["input_bytes"], 1024 * 1024);
        assert!(v["files"][1]["ratio_pct"].is_null());
    }

    #[test]
    fn json_empty_run() {
        let out = render(&[], &ReportTotals::default(), ReportFormat::Json);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v["files"].as_array().unwrap().is_empty());
        assert_eq!(v["totals"]["total_files"], 0);
        assert_eq!(v["totals"]["total_input_bytes"], 0);
    }

    #[test]
    fn html_escapes_angle_brackets() {
        let rec = ReportRecord::new(
            "<game>.iso".into(),
            String::new(),
            "compress",
            FileStatus::Ok,
            10,
            5,
            0,
            None,
        );
        let out = render(&[rec], &ReportTotals::default(), ReportFormat::Html);
        assert!(out.contains("&lt;game&gt;.iso"), "{out}");
        assert!(!out.contains("<game>"), "{out}");
    }

    #[test]
    fn html_totals_row_present() {
        let out = render(&[ok_record()], &ReportTotals::default(), ReportFormat::Html);
        assert!(out.contains("<tfoot"), "{out}");
    }

    #[test]
    fn format_from_extension() {
        assert_eq!(
            ReportFormat::from_path(Path::new("a.csv")),
            ReportFormat::Csv
        );
        assert_eq!(
            ReportFormat::from_path(Path::new("a.json")),
            ReportFormat::Json
        );
        assert_eq!(
            ReportFormat::from_path(Path::new("a.html")),
            ReportFormat::Html
        );
        assert_eq!(
            ReportFormat::from_path(Path::new("a.htm")),
            ReportFormat::Html
        );
        assert_eq!(
            ReportFormat::from_path(Path::new("a.HTML")),
            ReportFormat::Html
        );
        assert_eq!(
            ReportFormat::from_path(Path::new("out.txt")),
            ReportFormat::Json
        );
        assert_eq!(
            ReportFormat::from_path(Path::new("noext")),
            ReportFormat::Json
        );
    }

    #[test]
    fn failed_row_has_no_ratio() {
        let rec = ReportRecord::new(
            "in.iso".into(),
            String::new(),
            "compress",
            FileStatus::Failed,
            1024,
            0,
            10,
            Some("boom".into()),
        );
        assert!(rec.ratio_pct.is_none());
        let csv = render(
            std::slice::from_ref(&rec),
            &ReportTotals::default(),
            ReportFormat::Csv,
        );
        let row = csv.lines().nth(1).unwrap();
        assert!(row.ends_with(",boom"), "{row}");
        assert!(row.contains(",failed,1024,0,,10,"), "{row}");
        let json = render(&[rec], &ReportTotals::default(), ReportFormat::Json);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v["files"][0]["ratio_pct"].is_null());
        assert_eq!(v["files"][0]["error"], "boom");
    }

    #[test]
    fn decompress_ratio_negative() {
        let rec = ReportRecord::new(
            "in.cso".into(),
            "out.iso".into(),
            "decompress",
            FileStatus::Ok,
            256 * 1024,
            1024 * 1024,
            0,
            None,
        );
        assert!(rec.ratio_pct.unwrap() < 0.0, "{:?}", rec.ratio_pct);
    }

    #[test]
    fn no_unicode_dashes_in_output() {
        let totals = ReportTotals {
            total_files: 1,
            ok: 1,
            total_input_bytes: 1024 * 1024,
            total_output_bytes: 256 * 1024,
            elapsed_ms: 1500,
            ..ReportTotals::default()
        };
        for format in [ReportFormat::Csv, ReportFormat::Json, ReportFormat::Html] {
            let out = render(&[ok_record()], &totals, format);
            assert!(!out.contains('\u{2014}'), "em dash in {format:?}");
            assert!(!out.contains('\u{2013}'), "en dash in {format:?}");
        }
    }

    #[test]
    fn report_record_round_trips_through_json() {
        let original = ReportRecord::new(
            "in.iso".into(),
            "out.cso".into(),
            "compress",
            FileStatus::Skipped,
            1024 * 1024,
            256 * 1024,
            1500,
            Some("note".into()),
        );
        let json = serde_json::to_string(&original).unwrap();
        let back: ReportRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.input_path, original.input_path);
        assert_eq!(back.output_path, original.output_path);
        assert_eq!(back.operation, original.operation);
        assert_eq!(back.status, original.status);
        assert_eq!(back.input_bytes, original.input_bytes);
        assert_eq!(back.output_bytes, original.output_bytes);
        assert_eq!(back.ratio_pct, original.ratio_pct);
        assert_eq!(back.elapsed_ms, original.elapsed_ms);
        assert_eq!(back.error, original.error);
    }

    #[test]
    fn report_record_status_deserializes_each_variant() {
        for (text, expected) in [
            ("ok", FileStatus::Ok),
            ("skipped", FileStatus::Skipped),
            ("failed", FileStatus::Failed),
        ] {
            let json = format!(
                r#"{{"input_path":"a","output_path":"b","operation":"compress","status":"{text}","input_bytes":0,"output_bytes":0,"ratio_pct":null,"elapsed_ms":0,"error":null}}"#
            );
            let rec: ReportRecord = serde_json::from_str(&json).unwrap();
            assert_eq!(rec.status, expected);
        }
        let bad = r#"{"input_path":"a","output_path":"b","operation":"compress","status":"bogus","input_bytes":0,"output_bytes":0,"ratio_pct":null,"elapsed_ms":0,"error":null}"#;
        assert!(serde_json::from_str::<ReportRecord>(bad).is_err());
    }

    #[test]
    fn report_totals_round_trips() {
        let totals = ReportTotals {
            total_files: 3,
            ok: 2,
            skipped: 1,
            failed: 0,
            total_input_bytes: 4096,
            total_output_bytes: 2048,
            elapsed_ms: 99,
        };
        let json = serde_json::to_string(&totals).unwrap();
        let back: ReportTotals = serde_json::from_str(&json).unwrap();
        assert_eq!(back.total_files, totals.total_files);
        assert_eq!(back.ok, totals.ok);
        assert_eq!(back.skipped, totals.skipped);
        assert_eq!(back.failed, totals.failed);
        assert_eq!(back.total_input_bytes, totals.total_input_bytes);
        assert_eq!(back.total_output_bytes, totals.total_output_bytes);
        assert_eq!(back.elapsed_ms, totals.elapsed_ms);
    }

    fn hash_ok_record() -> HashReportRecord {
        HashReportRecord {
            path: "game.iso".into(),
            crc32: Some("352441c2".into()),
            sha1: Some("a9993e364706816aba3e25717850c26c9cd0d89d".into()),
            md5: None,
            sha256: None,
            size_bytes: 2048,
            status: FileStatus::Ok,
            elapsed_ms: 12,
            error: None,
        }
    }

    fn render_hash(records: &[HashReportRecord], format: ReportFormat) -> String {
        let totals = ReportTotals::default();
        let mut buf = Vec::new();
        match format {
            ReportFormat::Csv => write_hash_csv(&mut buf, records).unwrap(),
            ReportFormat::Json => write_hash_json(&mut buf, records, &totals).unwrap(),
            ReportFormat::Html => write_hash_html(&mut buf, records, &totals).unwrap(),
        }
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn hash_csv_header_and_empty_cells() {
        let out = render_hash(&[hash_ok_record()], ReportFormat::Csv);
        let mut lines = out.lines();
        assert_eq!(lines.next().unwrap(), HASH_CSV_HEADER);
        let row = lines.next().unwrap();
        assert!(
            row.contains("a9993e364706816aba3e25717850c26c9cd0d89d,,,2048,ok,12,"),
            "{row}"
        );
    }

    #[test]
    fn hash_json_schema() {
        let out = render_hash(&[hash_ok_record()], ReportFormat::Json);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["files"][0]["crc32"], "352441c2");
        assert!(v["files"][0]["md5"].is_null());
        assert_eq!(v["files"][0]["status"], "ok");
    }

    #[test]
    fn hash_html_has_digest_columns() {
        let out = render_hash(&[hash_ok_record()], ReportFormat::Html);
        assert!(out.contains("<th>CRC32</th>"), "{out}");
        assert!(out.contains("<th>SHA256</th>"), "{out}");
        assert!(out.contains("352441c2"), "{out}");
    }

    #[test]
    fn hash_report_has_no_unicode_dashes() {
        for format in [ReportFormat::Csv, ReportFormat::Json, ReportFormat::Html] {
            let out = render_hash(&[hash_ok_record()], format);
            assert!(!out.contains('\u{2014}'), "em dash in {format:?}");
            assert!(!out.contains('\u{2013}'), "en dash in {format:?}");
        }
    }

    fn dat_ok_record() -> DatReportRecord {
        DatReportRecord {
            path: "game.chd".into(),
            verdict: "verified".into(),
            game_name: Some("Some Game (USA)".into()),
            game_id: Some("g-1".into()),
            platform: Some("PlayStation".into()),
            signature_group: Some("Redump".into()),
            dat_file_name: Some("Sony - PlayStation - Games".into()),
            dat_file_id: Some("d-1".into()),
            dat_version: Some("2026-06-01".into()),
            match_algo: Some("sha1".into()),
            detail: None,
            size_bytes: 700_000_000,
            status: FileStatus::Ok,
            elapsed_ms: 850,
            error: None,
        }
    }

    fn render_dat(records: &[DatReportRecord], format: ReportFormat) -> String {
        let totals = ReportTotals::default();
        let mut buf = Vec::new();
        match format {
            ReportFormat::Csv => write_dat_csv(&mut buf, records).unwrap(),
            ReportFormat::Json => write_dat_json(&mut buf, records, &totals).unwrap(),
            ReportFormat::Html => write_dat_html(&mut buf, records, &totals).unwrap(),
        }
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn dat_csv_header_and_one_row() {
        let out = render_dat(&[dat_ok_record()], ReportFormat::Csv);
        let mut lines = out.lines();
        assert_eq!(lines.next().unwrap(), DAT_CSV_HEADER);
        let row = lines.next().unwrap();
        assert_eq!(
            row,
            "game.chd,verified,Some Game (USA),g-1,PlayStation,Redump,Sony - PlayStation - Games,d-1,2026-06-01,sha1,,700000000,ok,850,",
            "{row}"
        );
    }

    #[test]
    fn dat_csv_escapes_comma_in_game_name() {
        let mut rec = dat_ok_record();
        rec.game_name = Some("Some Game, Special Edition (USA)".into());
        let out = render_dat(&[rec], ReportFormat::Csv);
        assert!(
            out.contains("\"Some Game, Special Edition (USA)\""),
            "{out}"
        );
    }

    #[test]
    fn dat_csv_escapes_quote_in_detail() {
        let mut rec = dat_ok_record();
        rec.detail = Some("track \"01\" ok".into());
        let out = render_dat(&[rec], ReportFormat::Csv);
        assert!(out.contains("\"track \"\"01\"\" ok\""), "{out}");
    }

    #[test]
    fn dat_csv_row_with_all_fields_empty() {
        let rec = DatReportRecord {
            path: "unknown.iso".into(),
            verdict: "unknown".into(),
            game_name: None,
            game_id: None,
            platform: None,
            signature_group: None,
            dat_file_name: None,
            dat_file_id: None,
            dat_version: None,
            match_algo: None,
            detail: None,
            size_bytes: 0,
            status: FileStatus::Failed,
            elapsed_ms: 5,
            error: Some("boom".into()),
        };
        let out = render_dat(&[rec], ReportFormat::Csv);
        let row = out.lines().nth(1).unwrap();
        assert_eq!(row, "unknown.iso,unknown,,,,,,,,,,0,failed,5,boom", "{row}");
    }

    #[test]
    fn dat_json_schema() {
        let out = render_dat(&[dat_ok_record()], ReportFormat::Json);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["files"][0]["verdict"], "verified");
        assert_eq!(v["files"][0]["game_name"], "Some Game (USA)");
        assert_eq!(v["files"][0]["dat_file_name"], "Sony - PlayStation - Games");
        assert_eq!(v["files"][0]["match_algo"], "sha1");
        assert_eq!(v["files"][0]["status"], "ok");
        assert!(v["files"][0]["detail"].is_null());
    }

    #[test]
    fn dat_html_has_verdict_and_game_columns() {
        let out = render_dat(&[dat_ok_record()], ReportFormat::Html);
        assert!(out.contains("<th>Verdict</th>"), "{out}");
        assert!(out.contains("<th>Game</th>"), "{out}");
        assert!(out.contains("<th>DAT file</th>"), "{out}");
        assert!(out.contains("Some Game (USA)"), "{out}");
        assert!(out.contains("Sony - PlayStation - Games"), "{out}");
        assert!(out.contains("<tfoot"), "{out}");
    }

    #[test]
    fn dat_html_escapes_angle_brackets_in_game_name() {
        let mut rec = dat_ok_record();
        rec.game_name = Some("<script>".into());
        let out = render_dat(&[rec], ReportFormat::Html);
        assert!(out.contains("&lt;script&gt;"), "{out}");
        assert!(!out.contains("<script>"), "{out}");
    }

    #[test]
    fn dat_report_has_no_unicode_dashes() {
        for format in [ReportFormat::Csv, ReportFormat::Json, ReportFormat::Html] {
            let out = render_dat(&[dat_ok_record()], format);
            assert!(!out.contains('\u{2014}'), "em dash in {format:?}");
            assert!(!out.contains('\u{2013}'), "en dash in {format:?}");
        }
    }
}
