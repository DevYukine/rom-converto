use crate::util::tally::{FileStatus, format_bytes};
use anyhow::{Context, Result};
use serde::{Serialize, Serializer};
use std::borrow::Cow;
use std::io::{BufWriter, Write};
use std::path::Path;

#[derive(Clone, Debug, Serialize)]
pub struct ReportRecord {
    pub input_path: String,
    pub output_path: String,
    pub operation: String,
    #[serde(serialize_with = "ser_status")]
    pub status: FileStatus,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub ratio_pct: Option<f64>,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ReportTotals {
    pub total_files: usize,
    pub ok: usize,
    pub skipped: usize,
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

/// Write a run report to `path`. The file is created and truncated directly,
/// bypassing the ROM on-conflict machinery: the report path is an output the
/// user named explicitly, not a converted ROM.
pub fn write_report(
    path: &Path,
    records: &[ReportRecord],
    totals: &ReportTotals,
    format: ReportFormat,
) -> Result<()> {
    let file = std::fs::File::create(path)
        .with_context(|| format!("creating report file {}", path.display()))?;
    let mut w = BufWriter::new(file);
    match format {
        ReportFormat::Csv => write_csv(&mut w, records, totals)?,
        ReportFormat::Json => write_json(&mut w, records, totals)?,
        ReportFormat::Html => write_html(&mut w, records, totals)?,
    }
    w.flush()?;
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
    let file = std::fs::File::create(path)
        .with_context(|| format!("creating report file {}", path.display()))?;
    let mut w = BufWriter::new(file);
    match format {
        ReportFormat::Csv => write_hash_csv(&mut w, records)?,
        ReportFormat::Json => write_hash_json(&mut w, records, totals)?,
        ReportFormat::Html => write_hash_html(&mut w, records, totals)?,
    }
    w.flush()?;
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
        let csv = render(&[rec.clone()], &ReportTotals::default(), ReportFormat::Csv);
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
}
