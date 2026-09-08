use crate::util::{WriteDecision, file_len, totals_from};
use anyhow::Result;
use rom_converto_lib::util::{
    CancelToken, ConflictResolution, FileStatus, PlanDecision, PlanLine, ReportFormat,
    ReportRecord, ReportRecordInput, Tally, TallyDirection, write_report,
};
use std::path::Path;

/// Classify the conflict outcome for a desired path against the resolver's
/// decision. `desired` is the path passed to resolve_output; the returned
/// path differs from it only when a rename redirected the write.
pub fn classify(desired: &Path, decision: &WriteDecision) -> PlanDecision {
    let resolution = match decision {
        WriteDecision::Write(p) => ConflictResolution::Write(p.clone()),
        WriteDecision::Skip => ConflictResolution::Skip,
    };
    rom_converto_lib::util::classify(desired, &resolution)
}

pub fn log_plan(
    operation: &str,
    input: &Path,
    desired: &Path,
    decision: &WriteDecision,
    media: Option<&str>,
    missing_keys: Option<&str>,
) {
    let target = match decision {
        WriteDecision::Write(p) => p.clone(),
        WriteDecision::Skip => desired.to_path_buf(),
    };
    let line = PlanLine {
        operation: operation.to_string(),
        input: input.to_path_buf(),
        output: target,
        decision: classify(desired, decision),
        media: media.map(str::to_string),
        missing_keys: missing_keys.map(str::to_string),
    };
    log::info!("{}", line.display_text());
}

/// Record one planned file in a tally as either ok (a would-be write) or
/// skipped, so the dry-run summary count matches what a real run would do.
pub fn record(tally: &mut Tally, input: &Path, decision: &WriteDecision) {
    match decision {
        WriteDecision::Skip => tally.record_skipped(),
        WriteDecision::Write(_) => tally.record_ok(file_len(input), 0, std::time::Duration::ZERO),
    }
}

/// Build a report record for a planned file. The output path is the resolved
/// target and the operation is suffixed so an exported plan is distinguishable
/// from a real run.
pub fn report_record(
    operation: &str,
    input: &Path,
    desired: &Path,
    decision: &WriteDecision,
) -> ReportRecord {
    let (output, status) = match decision {
        WriteDecision::Write(p) => (p.display().to_string(), FileStatus::Ok),
        WriteDecision::Skip => (desired.display().to_string(), FileStatus::Skipped),
    };
    let input_bytes = match decision {
        WriteDecision::Write(_) => file_len(input),
        WriteDecision::Skip => 0,
    };
    ReportRecord::new(ReportRecordInput {
        input_path: input.display().to_string(),
        output_path: output,
        operation: format!("{operation} (dry run)"),
        status,
        input_bytes,
        output_bytes: 0,
        elapsed_ms: 0,
        error: None,
    })
}

/// Emit the dry-run summary line and, when a report path is given, export the
/// plan. Writing the report is allowed under dry-run; only ROM output is
/// suppressed.
pub fn finish(tally: &Tally, records: &[ReportRecord], report: Option<&Path>) -> Result<()> {
    log::info!("{}", tally.summary_line(TallyDirection::DryRun));
    if let Some(path) = report {
        write_report(
            path,
            records,
            &totals_from(tally),
            ReportFormat::from_path(path),
            &CancelToken::new(),
        )?;
    }
    Ok(())
}

/// Emit the plan line, summary, and optional report for a single-file
/// dry-run, then return so the caller can short-circuit before the lib write.
pub fn single(
    operation: &str,
    input: &Path,
    desired: &Path,
    decision: &WriteDecision,
    media: Option<&str>,
    missing_keys: Option<&str>,
    report: Option<&Path>,
) -> Result<()> {
    log_plan(operation, input, desired, decision, media, missing_keys);
    let mut tally = Tally::new();
    record(&mut tally, input, decision);
    let records = [report_record(operation, input, desired, decision)];
    finish(&tally, &records, report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn classify_overwrite_when_exists() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.chd");
        std::fs::write(&path, b"x").unwrap();
        let decision = WriteDecision::Write(path.clone());
        assert!(matches!(
            classify(&path, &decision),
            PlanDecision::Overwrite
        ));
    }

    #[test]
    fn classify_new_when_absent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.chd");
        let decision = WriteDecision::Write(path.clone());
        assert!(matches!(classify(&path, &decision), PlanDecision::New));
    }

    #[test]
    fn classify_rename_when_path_differs() {
        let desired = PathBuf::from("game.chd");
        let renamed = PathBuf::from("game (1).chd");
        let decision = WriteDecision::Write(renamed);
        assert!(matches!(
            classify(&desired, &decision),
            PlanDecision::Rename(_)
        ));
    }

    #[test]
    fn classify_skip() {
        let desired = PathBuf::from("game.chd");
        let decision = WriteDecision::Skip;
        assert!(matches!(classify(&desired, &decision), PlanDecision::Skip));
    }
}
