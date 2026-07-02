//! Dry-run planning: classifies each input into a [`PlanDecision`] and
//! renders the "Would ..." lines the CLI and GUI show in preview mode,
//! without touching the filesystem.

use super::conflict::ConflictResolution;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PlanDecision {
    New,
    Overwrite,
    Rename(PathBuf),
    Skip,
    KeepValid,
    RewriteInvalid,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PlanLine {
    pub operation: String,
    pub input: PathBuf,
    pub output: PathBuf,
    pub decision: PlanDecision,
    pub media: Option<String>,
    pub missing_keys: Option<String>,
}

impl PlanLine {
    pub fn display_text(&self) -> String {
        let label = match &self.decision {
            PlanDecision::Overwrite => "[overwrite]".to_string(),
            PlanDecision::Rename(p) => format!("[rename -> {}]", p.display()),
            PlanDecision::New => "[new]".to_string(),
            PlanDecision::Skip => "[skip]".to_string(),
            PlanDecision::KeepValid => "[keep (valid)]".to_string(),
            PlanDecision::RewriteInvalid => "[rewrite (invalid)]".to_string(),
        };
        let media = self
            .media
            .as_deref()
            .map(|m| format!(" ({m})"))
            .unwrap_or_default();
        let keys = self
            .missing_keys
            .as_deref()
            .map(|k| format!(" missing keys: {k}"))
            .unwrap_or_default();
        format!(
            "Would {} {} -> {}{media} {label}{keys}",
            self.operation,
            self.input.display(),
            self.output.display()
        )
    }
}

/// Classify the conflict outcome for a desired path against the resolver's
/// resolution. `desired` is the path passed to `resolve_conflict`; the resolved
/// path differs from it only when a rename redirected the write. `KeepValid`
/// and `RewriteInvalid` are not produced here because they require an async
/// integrity check; the caller sets those after verifying.
pub fn classify(desired: &Path, resolution: &ConflictResolution) -> PlanDecision {
    match resolution {
        ConflictResolution::Skip => PlanDecision::Skip,
        ConflictResolution::Write(p) if p != desired => PlanDecision::Rename(p.clone()),
        ConflictResolution::Write(_) if desired.exists() => PlanDecision::Overwrite,
        ConflictResolution::Write(_) => PlanDecision::New,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample(decision: PlanDecision, media: Option<&str>) -> PlanLine {
        PlanLine {
            operation: "compress".to_string(),
            input: PathBuf::from("game.iso"),
            output: PathBuf::from("game.cso"),
            decision,
            media: media.map(str::to_string),
            missing_keys: None,
        }
    }

    #[test]
    fn classify_new_when_absent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.chd");
        let res = ConflictResolution::Write(path.clone());
        assert_eq!(classify(&path, &res), PlanDecision::New);
    }

    #[test]
    fn classify_overwrite_when_exists() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.chd");
        std::fs::write(&path, b"x").unwrap();
        let res = ConflictResolution::Write(path.clone());
        assert_eq!(classify(&path, &res), PlanDecision::Overwrite);
    }

    #[test]
    fn classify_rename_when_path_differs() {
        let desired = PathBuf::from("game.chd");
        let renamed = PathBuf::from("game (1).chd");
        let res = ConflictResolution::Write(renamed.clone());
        assert_eq!(classify(&desired, &res), PlanDecision::Rename(renamed));
    }

    #[test]
    fn classify_skip() {
        let desired = PathBuf::from("game.chd");
        assert_eq!(
            classify(&desired, &ConflictResolution::Skip),
            PlanDecision::Skip
        );
    }

    #[test]
    fn display_text_new() {
        let line = sample(PlanDecision::New, Some("CSO"));
        assert_eq!(
            line.display_text(),
            "Would compress game.iso -> game.cso (CSO) [new]"
        );
    }

    #[test]
    fn display_text_no_media() {
        let line = sample(PlanDecision::New, None);
        assert_eq!(
            line.display_text(),
            "Would compress game.iso -> game.cso [new]"
        );
    }

    #[test]
    fn display_text_overwrite_invalid_keep_and_rewrite() {
        let keep = sample(PlanDecision::KeepValid, None);
        assert_eq!(
            keep.display_text(),
            "Would compress game.iso -> game.cso [keep (valid)]"
        );
        let rewrite = sample(PlanDecision::RewriteInvalid, None);
        assert_eq!(
            rewrite.display_text(),
            "Would compress game.iso -> game.cso [rewrite (invalid)]"
        );
    }

    #[test]
    fn display_text_rename() {
        let line = sample(PlanDecision::Rename(PathBuf::from("game (1).chd")), None);
        assert_eq!(
            line.display_text(),
            "Would compress game.iso -> game.cso [rename -> game (1).chd]"
        );
    }

    #[test]
    fn display_text_missing_keys() {
        let mut line = sample(PlanDecision::New, None);
        line.missing_keys = Some("prod.keys not found".to_string());
        assert_eq!(
            line.display_text(),
            "Would compress game.iso -> game.cso [new] missing keys: prod.keys not found"
        );
    }

    #[test]
    fn plan_line_serde_round_trip() {
        let line = sample(
            PlanDecision::Rename(PathBuf::from("game (1).cso")),
            Some("ZSO"),
        );
        let json = serde_json::to_string(&line).unwrap();
        let back: PlanLine = serde_json::from_str(&json).unwrap();
        assert_eq!(line.display_text(), back.display_text());
    }
}
