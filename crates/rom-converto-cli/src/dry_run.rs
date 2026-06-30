use crate::util::WriteDecision;
use anyhow::Result;
use rom_converto_lib::util::{
    ConflictResolution, FileStatus, PlanDecision, PlanLine, ReportFormat, ReportRecord,
    ReportTotals, Tally, TallyDirection, write_report,
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
    log_plan_decision(
        operation,
        input,
        desired,
        decision,
        classify(desired, decision),
        media,
        missing_keys,
    );
}

/// Like `log_plan` but with the conflict outcome supplied by the caller, used
/// for `overwrite-invalid` where the keep-vs-rewrite choice comes from a
/// read-only verify the pure classifier cannot run.
pub fn log_plan_decision(
    operation: &str,
    input: &Path,
    desired: &Path,
    decision: &WriteDecision,
    outcome: PlanDecision,
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
        decision: outcome,
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
    ReportRecord::new(
        input.display().to_string(),
        output,
        format!("{operation} (dry run)"),
        status,
        input_bytes,
        0,
        0,
        None,
    )
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
        )?;
    }
    Ok(())
}

fn totals_from(tally: &Tally) -> ReportTotals {
    ReportTotals {
        total_files: tally.count(),
        ok: tally.ok_count(),
        skipped: tally.skipped_count(),
        failed: tally.failed_count(),
        total_input_bytes: tally.total_input_bytes(),
        total_output_bytes: tally.total_output_bytes(),
        elapsed_ms: tally.elapsed().as_millis().min(u64::MAX as u128) as u64,
    }
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
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
