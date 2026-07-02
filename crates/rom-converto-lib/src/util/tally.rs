//! Per-run byte and file-count tracking, rendered into the final
//! `"{n} files: A -> B, saved C (p%) in T"`-style summary line.

use std::time::{Duration, Instant};

const KIB: f64 = 1024.0;

pub fn format_bytes(n: u64) -> String {
    let n = n as f64;
    if n < KIB {
        return format!("{} B", n as u64);
    }
    let units = ["KiB", "MiB", "GiB", "TiB"];
    let mut value = n / KIB;
    let mut unit = 0;
    while value >= KIB && unit < units.len() - 1 {
        value /= KIB;
        unit += 1;
    }
    format!("{value:.1} {}", units[unit])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    /// Converted successfully.
    Ok,
    /// Not converted because a valid output already existed.
    Skipped,
    /// Conversion returned an error.
    Failed,
}

/// Which summary shape [`Tally::summary_line`] renders: whether the run
/// produced output worth comparing, and how input and output sizes relate.
#[derive(Clone, Copy, Debug)]
pub enum TallyDirection {
    Compress,
    Decompress,
    Convert,
    CountOnly,
    DryRun,
}

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub status: FileStatus,
    pub elapsed: Duration,
}

#[derive(Debug)]
pub struct Tally {
    entries: Vec<FileEntry>,
    started: Instant,
}

impl Default for Tally {
    fn default() -> Self {
        Self::new()
    }
}

impl Tally {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            started: Instant::now(),
        }
    }

    pub fn record(&mut self, entry: FileEntry) {
        self.entries.push(entry);
    }

    pub fn record_ok(&mut self, input_bytes: u64, output_bytes: u64, elapsed: Duration) {
        self.record(FileEntry {
            input_bytes,
            output_bytes,
            status: FileStatus::Ok,
            elapsed,
        });
    }

    pub fn record_failed(&mut self) {
        self.record(FileEntry {
            input_bytes: 0,
            output_bytes: 0,
            status: FileStatus::Failed,
            elapsed: Duration::ZERO,
        });
    }

    pub fn record_skipped(&mut self) {
        self.record(FileEntry {
            input_bytes: 0,
            output_bytes: 0,
            status: FileStatus::Skipped,
            elapsed: Duration::ZERO,
        });
    }

    pub fn entries(&self) -> &[FileEntry] {
        &self.entries
    }

    pub fn count(&self) -> usize {
        self.entries.len()
    }

    pub fn ok_count(&self) -> usize {
        self.with_status(FileStatus::Ok)
    }

    pub fn skipped_count(&self) -> usize {
        self.with_status(FileStatus::Skipped)
    }

    pub fn failed_count(&self) -> usize {
        self.with_status(FileStatus::Failed)
    }

    fn with_status(&self, status: FileStatus) -> usize {
        self.entries.iter().filter(|e| e.status == status).count()
    }

    pub fn total_input_bytes(&self) -> u64 {
        self.ok_entries().map(|e| e.input_bytes).sum()
    }

    pub fn total_output_bytes(&self) -> u64 {
        self.ok_entries().map(|e| e.output_bytes).sum()
    }

    fn ok_entries(&self) -> impl Iterator<Item = &FileEntry> {
        self.entries.iter().filter(|e| e.status == FileStatus::Ok)
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn count_summary(count: usize, elapsed: Duration) -> String {
        let prefix = count_prefix(count, 0, 0);
        format!("{prefix} in {}", format_duration(elapsed))
    }

    pub fn summary_line(&self, direction: TallyDirection) -> String {
        let ok = self.ok_count();
        let failed = self.failed_count();
        let skipped = self.skipped_count();
        let input = self.total_input_bytes();
        let output = self.total_output_bytes();
        let el = format_duration(self.elapsed());
        let prefix = count_prefix(ok, failed, skipped);

        match direction {
            TallyDirection::CountOnly => {
                format!("{prefix} in {el}")
            }
            TallyDirection::DryRun => {
                format!("Dry run: {prefix} planned in {el}")
            }
            TallyDirection::Compress => {
                if input == 0 || output >= input {
                    format!(
                        "{prefix}: {} -> {}, no space saved in {el}",
                        format_bytes(input),
                        format_bytes(output)
                    )
                } else {
                    let saved = input - output;
                    let pct = (saved as f64 / input as f64 * 100.0).round() as u64;
                    format!(
                        "{prefix}: {} -> {}, saved {} ({pct}%) in {el}",
                        format_bytes(input),
                        format_bytes(output),
                        format_bytes(saved)
                    )
                }
            }
            TallyDirection::Decompress => {
                let grew = output.saturating_sub(input);
                if input == 0 {
                    format!(
                        "{prefix}: {} -> {} in {el}",
                        format_bytes(input),
                        format_bytes(output)
                    )
                } else {
                    let pct = (grew as f64 / input as f64 * 100.0).round() as u64;
                    format!(
                        "{prefix}: {} -> {}, expanded by {} ({pct}%) in {el}",
                        format_bytes(input),
                        format_bytes(output),
                        format_bytes(grew)
                    )
                }
            }
            TallyDirection::Convert => {
                format!(
                    "{prefix}: {} -> {} in {el}",
                    format_bytes(input),
                    format_bytes(output)
                )
            }
        }
    }
}

fn count_prefix(ok: usize, failed: usize, skipped: usize) -> String {
    if failed == 0 && skipped == 0 {
        return format!("{ok} files");
    }
    let mut parts = vec![format!("{ok} ok")];
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if skipped > 0 {
        parts.push(format!("{skipped} skipped"));
    }
    parts.join(", ")
}

fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}.{}s", secs, d.subsec_millis() / 100)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_entry(input: u64, output: u64) -> FileEntry {
        FileEntry {
            input_bytes: input,
            output_bytes: output,
            status: FileStatus::Ok,
            elapsed: Duration::ZERO,
        }
    }

    #[test]
    fn format_bytes_boundaries() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(1_048_576), "1.0 MiB");
        assert_eq!(format_bytes(1_073_741_824), "1.0 GiB");
        assert_eq!(format_bytes(1_099_511_627_776), "1.0 TiB");
        assert_eq!(format_bytes(3 * 1_099_511_627_776), "3.0 TiB");
    }

    #[test]
    fn tally_compress_saved_and_percent() {
        let mut t = Tally::new();
        t.record(ok_entry(1024 * 1024, 256 * 1024));
        t.record(ok_entry(1024 * 1024, 256 * 1024));
        let line = t.summary_line(TallyDirection::Compress);
        assert!(line.contains("saved"), "{line}");
        assert!(line.contains("->"), "{line}");
        assert!(line.contains("(75%)"), "{line}");
        assert!(!line.contains('-') || line.contains("->"));
        assert!(!line.replace("->", "").contains('-'), "{line}");
    }

    #[test]
    fn tally_compress_incompressible() {
        let mut t = Tally::new();
        t.record(ok_entry(1000, 1000));
        let line = t.summary_line(TallyDirection::Compress);
        assert!(line.contains("no space saved"), "{line}");
        assert!(!line.replace("->", "").contains('-'), "{line}");
    }

    #[test]
    fn tally_decompress_direction() {
        let mut t = Tally::new();
        t.record(ok_entry(256 * 1024, 1024 * 1024));
        let line = t.summary_line(TallyDirection::Decompress);
        assert!(line.contains("expanded by"), "{line}");
        assert!(!line.contains("saved"), "{line}");
        assert!(!line.replace("->", "").contains('-'), "{line}");
    }

    #[test]
    fn tally_mixed_ok_skipped_failed() {
        let mut t = Tally::new();
        t.record(ok_entry(2048, 1024));
        t.record_skipped();
        t.record_failed();
        assert_eq!(t.ok_count(), 1);
        assert_eq!(t.skipped_count(), 1);
        assert_eq!(t.failed_count(), 1);
        assert_eq!(t.count(), 3);
        assert_eq!(t.total_input_bytes(), 2048);
        assert_eq!(t.total_output_bytes(), 1024);
        let line = t.summary_line(TallyDirection::Compress);
        assert!(line.contains("1 ok"), "{line}");
        assert!(line.contains("1 failed"), "{line}");
        assert!(line.contains("1 skipped"), "{line}");
    }

    #[test]
    fn summary_lines_have_no_unicode_dashes() {
        let mut t = Tally::new();
        t.record(ok_entry(1024 * 1024, 256 * 1024));
        for dir in [
            TallyDirection::Compress,
            TallyDirection::Decompress,
            TallyDirection::Convert,
            TallyDirection::CountOnly,
            TallyDirection::DryRun,
        ] {
            let line = t.summary_line(dir);
            assert!(!line.contains('\u{2014}'), "em dash in {line}");
            assert!(!line.contains('\u{2013}'), "en dash in {line}");
        }
    }
}
