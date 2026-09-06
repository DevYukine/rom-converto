//! Run reports: per-file records and run totals written to CSV, JSON, or
//! HTML at the end of a batch run, via `--report`.

use crate::util::tally::{FileStatus, format_bytes};
use crate::util::{CancelToken, Cancelled};
use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::borrow::Cow;
use std::io::{BufWriter, Write};
use std::path::Path;

/// One file's outcome in a conversion run report.
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

/// Aggregate counters for a conversion run report.
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

/// On-disk format for a `--report` file.
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

/// Inputs for [`ReportRecord::new`]. Mirrors [`ReportRecord`] minus
/// `ratio_pct`, which is derived from the byte counts and status.
pub struct ReportRecordInput {
    pub input_path: String,
    pub output_path: String,
    pub operation: String,
    pub status: FileStatus,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

impl ReportRecord {
    /// Builds a record from `input`, deriving `ratio_pct` from the byte
    /// counts and status.
    pub fn new(input: ReportRecordInput) -> Self {
        let ReportRecordInput {
            input_path,
            output_path,
            operation,
            status,
            input_bytes,
            output_bytes,
            elapsed_ms,
            error,
        } = input;
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
            operation,
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

/// Cancellable twin of [`write_report`].
pub fn write_report(
    path: &Path,
    records: &[ReportRecord],
    totals: &ReportTotals,
    format: ReportFormat,
    cancel: &CancelToken,
) -> Result<()> {
    write_rows(path, records, totals, format, cancel)
}

/// One file's outcome in a hash run report.
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

/// Cancellable twin of [`write_hash_report`].
pub fn write_hash_report(
    path: &Path,
    records: &[HashReportRecord],
    totals: &ReportTotals,
    format: ReportFormat,
    cancel: &CancelToken,
) -> Result<()> {
    write_rows(path, records, totals, format, cancel)
}

/// One file's outcome in a dat-matching run report.
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

/// Cancellable twin of [`write_dat_report`].
pub fn write_dat_report(
    path: &Path,
    records: &[DatReportRecord],
    totals: &ReportTotals,
    format: ReportFormat,
    cancel: &CancelToken,
) -> Result<()> {
    write_rows(path, records, totals, format, cancel)
}

/// One report shape: its columns and how a record fills them. Each format
/// has a single writer that renders any implementor, so the three report
/// kinds share one CSV, one JSON, and one HTML renderer.
trait ReportRow: Serialize {
    const CSV_HEADER: &'static str;
    const TITLE: &'static str;
    const HEADING: &'static str;
    const COLUMNS: &'static [&'static str];

    /// This record's cells, in column order.
    fn cells(&self) -> Vec<Cell<'_>>;

    /// The HTML footer row, in the same column order.
    fn totals_cells(totals: &ReportTotals) -> Vec<Cell<'static>>;
}

/// One cell: CSV writes the raw value, HTML the human-readable one,
/// right-aligned for the numeric kinds.
enum Cell<'a> {
    Text(Cow<'a, str>),
    Bytes(u64),
    Millis(u64),
    /// Space saved as a percentage, blank when there is nothing to compare.
    Pct(Option<f64>),
}

impl Cell<'_> {
    fn csv(&self) -> Cow<'_, str> {
        match self {
            Cell::Text(s) => csv_field(s),
            Cell::Bytes(n) | Cell::Millis(n) => Cow::Owned(n.to_string()),
            Cell::Pct(v) => Cow::Owned(v.map(|v| format!("{v:.1}")).unwrap_or_default()),
        }
    }

    /// The cell body and whether it is right-aligned.
    fn html(&self) -> (String, bool) {
        match self {
            Cell::Text(s) => (html_escape(s), false),
            Cell::Bytes(n) => (format_bytes(*n), true),
            Cell::Millis(n) => (format!("{n} ms"), true),
            Cell::Pct(v) => (v.map(|v| format!("{v:.1}%")).unwrap_or_default(), true),
        }
    }
}

fn text(s: &str) -> Cell<'_> {
    Cell::Text(Cow::Borrowed(s))
}

fn opt(s: Option<&str>) -> Cell<'_> {
    text(s.unwrap_or(""))
}

impl ReportRow for ReportRecord {
    const CSV_HEADER: &'static str = "input_path,output_path,operation,status,input_bytes,output_bytes,ratio_pct,elapsed_ms,error";
    const TITLE: &'static str = "rom-converto run report";
    const HEADING: &'static str = "Run report";
    const COLUMNS: &'static [&'static str] = &[
        "Input",
        "Output",
        "Operation",
        "Status",
        "Input size",
        "Output size",
        "Ratio",
        "Elapsed",
        "Error",
    ];

    fn cells(&self) -> Vec<Cell<'_>> {
        vec![
            text(&self.input_path),
            text(&self.output_path),
            text(&self.operation),
            text(status_str(self.status)),
            Cell::Bytes(self.input_bytes),
            Cell::Bytes(self.output_bytes),
            Cell::Pct(self.ratio_pct),
            Cell::Millis(self.elapsed_ms),
            opt(self.error.as_deref()),
        ]
    }

    fn totals_cells(totals: &ReportTotals) -> Vec<Cell<'static>> {
        vec![
            Cell::Text(Cow::Owned(format!(
                "{} files ({} ok, {} skipped, {} failed)",
                totals.total_files, totals.ok, totals.skipped, totals.failed
            ))),
            text(""),
            text(""),
            text("totals"),
            Cell::Bytes(totals.total_input_bytes),
            Cell::Bytes(totals.total_output_bytes),
            text(""),
            Cell::Millis(totals.elapsed_ms),
            text(""),
        ]
    }
}

impl ReportRow for HashReportRecord {
    const CSV_HEADER: &'static str =
        "path,crc32,sha1,md5,sha256,size_bytes,status,elapsed_ms,error";
    const TITLE: &'static str = "rom-converto hash report";
    const HEADING: &'static str = "Hash report";
    const COLUMNS: &'static [&'static str] = &[
        "Path", "CRC32", "SHA1", "MD5", "SHA256", "Size", "Status", "Elapsed", "Error",
    ];

    fn cells(&self) -> Vec<Cell<'_>> {
        vec![
            text(&self.path),
            opt(self.crc32.as_deref()),
            opt(self.sha1.as_deref()),
            opt(self.md5.as_deref()),
            opt(self.sha256.as_deref()),
            Cell::Bytes(self.size_bytes),
            text(status_str(self.status)),
            Cell::Millis(self.elapsed_ms),
            opt(self.error.as_deref()),
        ]
    }

    fn totals_cells(totals: &ReportTotals) -> Vec<Cell<'static>> {
        vec![
            Cell::Text(Cow::Owned(format!(
                "{} files ({} ok, {} failed)",
                totals.total_files, totals.ok, totals.failed
            ))),
            text(""),
            text(""),
            text(""),
            text("totals"),
            Cell::Bytes(totals.total_input_bytes),
            text(""),
            Cell::Millis(totals.elapsed_ms),
            text(""),
        ]
    }
}

impl ReportRow for DatReportRecord {
    const CSV_HEADER: &'static str = "path,verdict,game_name,game_id,platform,signature_group,dat_file_name,dat_file_id,dat_version,match_algo,detail,size_bytes,status,elapsed_ms,error";
    const TITLE: &'static str = "rom-converto dat report";
    const HEADING: &'static str = "Dat report";
    const COLUMNS: &'static [&'static str] = &[
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
    ];

    fn cells(&self) -> Vec<Cell<'_>> {
        vec![
            text(&self.path),
            text(&self.verdict),
            opt(self.game_name.as_deref()),
            opt(self.game_id.as_deref()),
            opt(self.platform.as_deref()),
            opt(self.signature_group.as_deref()),
            opt(self.dat_file_name.as_deref()),
            opt(self.dat_file_id.as_deref()),
            opt(self.dat_version.as_deref()),
            opt(self.match_algo.as_deref()),
            opt(self.detail.as_deref()),
            Cell::Bytes(self.size_bytes),
            text(status_str(self.status)),
            Cell::Millis(self.elapsed_ms),
            opt(self.error.as_deref()),
        ]
    }

    fn totals_cells(totals: &ReportTotals) -> Vec<Cell<'static>> {
        let mut cells = vec![Cell::Text(Cow::Owned(format!(
            "{} files ({} ok, {} skipped, {} failed)",
            totals.total_files, totals.ok, totals.skipped, totals.failed
        )))];
        cells.extend((0..10).map(|_| text("")));
        cells.extend([
            Cell::Bytes(totals.total_input_bytes),
            text(""),
            Cell::Millis(totals.elapsed_ms),
            text(""),
        ]);
        cells
    }
}

/// Render `records` to `path` through a temp file, so a failed or cancelled
/// write leaves any existing report in place.
fn write_rows<R: ReportRow>(
    path: &Path,
    records: &[R],
    totals: &ReportTotals,
    format: ReportFormat,
    cancel: &CancelToken,
) -> Result<()> {
    check_cancel(cancel)?;
    crate::util::atomic_write(path, true, |file| -> Result<()> {
        {
            let mut w = CancelWriter::new(BufWriter::new(&mut *file), cancel);
            match format {
                ReportFormat::Csv => write_csv(&mut w, records)?,
                ReportFormat::Json => write_json(&mut w, records, totals)?,
                ReportFormat::Html => write_html(&mut w, records, totals)?,
            }
            w.flush()?;
        }
        check_cancel(cancel)?;
        file.sync_all()?;
        check_cancel(cancel)?;
        Ok(())
    })
    .with_context(|| format!("writing report file {}", path.display()))
}

fn csv_field(s: &str) -> Cow<'_, str> {
    if s.contains([',', '"', '\n', '\r']) {
        Cow::Owned(format!("\"{}\"", s.replace('"', "\"\"")))
    } else {
        Cow::Borrowed(s)
    }
}

fn write_csv<W: Write, R: ReportRow>(w: &mut W, records: &[R]) -> Result<()> {
    writeln!(w, "{}", R::CSV_HEADER)?;
    for r in records {
        let cells = r.cells();
        let row: Vec<Cow<'_, str>> = cells.iter().map(Cell::csv).collect();
        writeln!(w, "{}", row.join(","))?;
    }
    Ok(())
}

#[derive(Serialize)]
struct ReportDoc<'a, R> {
    files: &'a [R],
    totals: &'a ReportTotals,
}

fn write_json<W: Write, R: ReportRow>(
    w: &mut W,
    records: &[R],
    totals: &ReportTotals,
) -> Result<()> {
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

fn write_html<W: Write, R: ReportRow>(
    w: &mut W,
    records: &[R],
    totals: &ReportTotals,
) -> Result<()> {
    writeln!(w, "<!DOCTYPE html>")?;
    writeln!(w, "<html lang=\"en\">")?;
    writeln!(w, "<head>")?;
    writeln!(w, "<meta charset=\"utf-8\">")?;
    writeln!(w, "<title>{}</title>", R::TITLE)?;
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
    writeln!(w, "<h1>{}</h1>", R::HEADING)?;
    writeln!(w, "<table>")?;
    writeln!(w, "<thead><tr>")?;
    for col in R::COLUMNS {
        write!(w, "<th>{col}</th>")?;
    }
    writeln!(w, "</tr></thead>")?;
    writeln!(w, "<tbody>")?;
    for r in records {
        write!(w, "<tr>")?;
        write_html_cells(w, &r.cells())?;
        writeln!(w, "</tr>")?;
    }
    writeln!(w, "</tbody>")?;
    writeln!(w, "<tfoot><tr>")?;
    write_html_cells(w, &R::totals_cells(totals))?;
    writeln!(w, "</tr></tfoot>")?;
    writeln!(w, "</table>")?;
    writeln!(w, "</body>")?;
    writeln!(w, "</html>")?;
    Ok(())
}

fn write_html_cells<W: Write>(w: &mut W, cells: &[Cell<'_>]) -> Result<()> {
    for cell in cells {
        match cell.html() {
            (body, true) => write!(w, "<td class=\"num\">{body}</td>")?,
            (body, false) => write!(w, "<td>{body}</td>")?,
        }
    }
    Ok(())
}

fn check_cancel(cancel: &CancelToken) -> std::io::Result<()> {
    if cancel.is_cancelled() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            Cancelled,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_record() -> ReportRecord {
        ReportRecord::new(ReportRecordInput {
            input_path: "in.iso".into(),
            output_path: "out.cso".into(),
            operation: ("compress").into(),
            status: FileStatus::Ok,
            input_bytes: 1024 * 1024,
            output_bytes: 256 * 1024,
            elapsed_ms: 1500,
            error: None,
        })
    }

    #[test]
    fn cancelled_report_preserves_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        std::fs::write(&path, b"existing").unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();

        assert!(
            write_report(
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

        write_report(
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
            ReportFormat::Csv => write_csv(&mut buf, records).unwrap(),
            ReportFormat::Json => write_json(&mut buf, records, totals).unwrap(),
            ReportFormat::Html => write_html(&mut buf, records, totals).unwrap(),
        }
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn csv_header_and_one_ok_row() {
        let out = render(&[ok_record()], &ReportTotals::default(), ReportFormat::Csv);
        let mut lines = out.lines();
        assert_eq!(lines.next().unwrap(), ReportRecord::CSV_HEADER);
        let row = lines.next().unwrap();
        assert_eq!(
            row, "in.iso,out.cso,compress,ok,1048576,262144,75.0,1500,",
            "{row}"
        );
    }

    #[test]
    fn csv_escapes_comma_in_path() {
        let rec = ReportRecord::new(ReportRecordInput {
            input_path: "a,b.iso".into(),
            output_path: String::new(),
            operation: ("compress").into(),
            status: FileStatus::Ok,
            input_bytes: 10,
            output_bytes: 5,
            elapsed_ms: 0,
            error: None,
        });
        let out = render(&[rec], &ReportTotals::default(), ReportFormat::Csv);
        assert!(out.contains("\"a,b.iso\""), "{out}");
    }

    #[test]
    fn csv_escapes_quote_in_path() {
        let rec = ReportRecord::new(ReportRecordInput {
            input_path: "a\"b.iso".into(),
            output_path: String::new(),
            operation: ("compress").into(),
            status: FileStatus::Ok,
            input_bytes: 10,
            output_bytes: 5,
            elapsed_ms: 0,
            error: None,
        });
        let out = render(&[rec], &ReportTotals::default(), ReportFormat::Csv);
        assert!(out.contains("\"a\"\"b.iso\""), "{out}");
    }

    #[test]
    fn csv_escapes_newline_in_path() {
        let rec = ReportRecord::new(ReportRecordInput {
            input_path: "a\nb.iso".into(),
            output_path: String::new(),
            operation: ("compress").into(),
            status: FileStatus::Ok,
            input_bytes: 10,
            output_bytes: 5,
            elapsed_ms: 0,
            error: None,
        });
        let out = render(&[rec], &ReportTotals::default(), ReportFormat::Csv);
        assert!(out.contains("\"a\nb.iso\""), "{out}");
        assert_eq!(out.lines().count(), 3, "embedded newline split the row");
    }

    #[test]
    fn json_stable_schema() {
        let skipped = ReportRecord::new(ReportRecordInput {
            input_path: "s.iso".into(),
            output_path: String::new(),
            operation: ("compress").into(),
            status: FileStatus::Skipped,
            input_bytes: 0,
            output_bytes: 0,
            elapsed_ms: 0,
            error: None,
        });
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
        let rec = ReportRecord::new(ReportRecordInput {
            input_path: "<game>.iso".into(),
            output_path: String::new(),
            operation: ("compress").into(),
            status: FileStatus::Ok,
            input_bytes: 10,
            output_bytes: 5,
            elapsed_ms: 0,
            error: None,
        });
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
        let rec = ReportRecord::new(ReportRecordInput {
            input_path: "in.iso".into(),
            output_path: String::new(),
            operation: ("compress").into(),
            status: FileStatus::Failed,
            input_bytes: 1024,
            output_bytes: 0,
            elapsed_ms: 10,
            error: Some("boom".into()),
        });
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
        let rec = ReportRecord::new(ReportRecordInput {
            input_path: "in.cso".into(),
            output_path: "out.iso".into(),
            operation: ("decompress").into(),
            status: FileStatus::Ok,
            input_bytes: 256 * 1024,
            output_bytes: 1024 * 1024,
            elapsed_ms: 0,
            error: None,
        });
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
        let original = ReportRecord::new(ReportRecordInput {
            input_path: "in.iso".into(),
            output_path: "out.cso".into(),
            operation: ("compress").into(),
            status: FileStatus::Skipped,
            input_bytes: 1024 * 1024,
            output_bytes: 256 * 1024,
            elapsed_ms: 1500,
            error: Some("note".into()),
        });
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
            ReportFormat::Csv => write_csv(&mut buf, records).unwrap(),
            ReportFormat::Json => write_json(&mut buf, records, &totals).unwrap(),
            ReportFormat::Html => write_html(&mut buf, records, &totals).unwrap(),
        }
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn hash_csv_header_and_empty_cells() {
        let out = render_hash(&[hash_ok_record()], ReportFormat::Csv);
        let mut lines = out.lines();
        assert_eq!(lines.next().unwrap(), HashReportRecord::CSV_HEADER);
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
            ReportFormat::Csv => write_csv(&mut buf, records).unwrap(),
            ReportFormat::Json => write_json(&mut buf, records, &totals).unwrap(),
            ReportFormat::Html => write_html(&mut buf, records, &totals).unwrap(),
        }
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn dat_csv_header_and_one_row() {
        let out = render_dat(&[dat_ok_record()], ReportFormat::Csv);
        let mut lines = out.lines();
        assert_eq!(lines.next().unwrap(), DatReportRecord::CSV_HEADER);
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

    /// Golden: the full HTML document for one row, so a change to the shared
    /// renderer that alters a byte of output fails here.
    #[test]
    fn html_golden_conversion_row() {
        assert_eq!(
            render(&[ok_record()], &ReportTotals::default(), ReportFormat::Html),
            r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>rom-converto run report</title>
<style>body{font-family:sans-serif;margin:1.5rem}table{border-collapse:collapse;width:100%}th,td{border:1px solid #ccc;padding:4px 8px;text-align:left;font-size:14px}thead th{background:#f0f0f0}tfoot td{font-weight:bold;background:#f7f7f7}td.num{text-align:right;font-variant-numeric:tabular-nums}</style>
</head>
<body>
<h1>Run report</h1>
<table>
<thead><tr>
<th>Input</th><th>Output</th><th>Operation</th><th>Status</th><th>Input size</th><th>Output size</th><th>Ratio</th><th>Elapsed</th><th>Error</th></tr></thead>
<tbody>
<tr><td>in.iso</td><td>out.cso</td><td>compress</td><td>ok</td><td class="num">1.0 MiB</td><td class="num">256.0 KiB</td><td class="num">75.0%</td><td class="num">1500 ms</td><td></td></tr>
</tbody>
<tfoot><tr>
<td>0 files (0 ok, 0 skipped, 0 failed)</td><td></td><td></td><td>totals</td><td class="num">0 B</td><td class="num">0 B</td><td></td><td class="num">0 ms</td><td></td></tr></tfoot>
</table>
</body>
</html>
"##
        );
    }

    /// Golden: the full HTML document for one row, so a change to the shared
    /// renderer that alters a byte of output fails here.
    #[test]
    fn html_golden_hash_row() {
        assert_eq!(
            render_hash(&[hash_ok_record()], ReportFormat::Html),
            r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>rom-converto hash report</title>
<style>body{font-family:sans-serif;margin:1.5rem}table{border-collapse:collapse;width:100%}th,td{border:1px solid #ccc;padding:4px 8px;text-align:left;font-size:14px}thead th{background:#f0f0f0}tfoot td{font-weight:bold;background:#f7f7f7}td.num{text-align:right;font-variant-numeric:tabular-nums}</style>
</head>
<body>
<h1>Hash report</h1>
<table>
<thead><tr>
<th>Path</th><th>CRC32</th><th>SHA1</th><th>MD5</th><th>SHA256</th><th>Size</th><th>Status</th><th>Elapsed</th><th>Error</th></tr></thead>
<tbody>
<tr><td>game.iso</td><td>352441c2</td><td>a9993e364706816aba3e25717850c26c9cd0d89d</td><td></td><td></td><td class="num">2.0 KiB</td><td>ok</td><td class="num">12 ms</td><td></td></tr>
</tbody>
<tfoot><tr>
<td>0 files (0 ok, 0 failed)</td><td></td><td></td><td></td><td>totals</td><td class="num">0 B</td><td></td><td class="num">0 ms</td><td></td></tr></tfoot>
</table>
</body>
</html>
"##
        );
    }

    /// Golden: the full HTML document for one row, so a change to the shared
    /// renderer that alters a byte of output fails here.
    #[test]
    fn html_golden_dat_row() {
        assert_eq!(
            render_dat(&[dat_ok_record()], ReportFormat::Html),
            r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>rom-converto dat report</title>
<style>body{font-family:sans-serif;margin:1.5rem}table{border-collapse:collapse;width:100%}th,td{border:1px solid #ccc;padding:4px 8px;text-align:left;font-size:14px}thead th{background:#f0f0f0}tfoot td{font-weight:bold;background:#f7f7f7}td.num{text-align:right;font-variant-numeric:tabular-nums}</style>
</head>
<body>
<h1>Dat report</h1>
<table>
<thead><tr>
<th>Path</th><th>Verdict</th><th>Game</th><th>Game id</th><th>Platform</th><th>Signature group</th><th>DAT file</th><th>DAT file id</th><th>Dat version</th><th>Match algo</th><th>Detail</th><th>Size</th><th>Status</th><th>Elapsed</th><th>Error</th></tr></thead>
<tbody>
<tr><td>game.chd</td><td>verified</td><td>Some Game (USA)</td><td>g-1</td><td>PlayStation</td><td>Redump</td><td>Sony - PlayStation - Games</td><td>d-1</td><td>2026-06-01</td><td>sha1</td><td></td><td class="num">667.6 MiB</td><td>ok</td><td class="num">850 ms</td><td></td></tr>
</tbody>
<tfoot><tr>
<td>0 files (0 ok, 0 skipped, 0 failed)</td><td></td><td></td><td></td><td></td><td></td><td></td><td></td><td></td><td></td><td></td><td class="num">0 B</td><td></td><td class="num">0 ms</td><td></td></tr></tfoot>
</table>
</body>
</html>
"##
        );
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
