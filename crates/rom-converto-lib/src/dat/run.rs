//! Playmatch matching driven by a local file: tiered checksum escalation,
//! and the report and rename records built from a match result.

use crate::dat::model::{GameAndRelationMatchResult, GameFileMatchSearch};
use crate::dat::rename::RenameCandidate;
use crate::dat::verdict::{DatVerdict, MatchStrength, match_strength};
use crate::dat::{PlaymatchClient, RomDigests};
use crate::util::fs::{file_len, has_ext};
use crate::util::report::DatReportRecord;
use crate::util::{CancelToken, ChecksumBounds, FileStatus, HashAlgo, ProgressReporter};
use anyhow::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};

pub(crate) async fn match_tiered(
    input: &Path,
    algos: &[HashAlgo],
    bounds: &ChecksumBounds,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
    api_base: Option<&str>,
) -> Result<GameAndRelationMatchResult> {
    let tierable = crate::dat::is_raw_reread_cheap(input) || has_ext(input, "cue");
    let (floor, escalation) = if tierable {
        bounds.split(algos)
    } else {
        (algos.to_vec(), Vec::new())
    };
    let mut matched = match_file(input, &floor, progress, cancel.clone(), api_base).await?;
    if !escalation.is_empty()
        && match_strength(matched.game_match_type) == MatchStrength::NameSizeHint
    {
        let full = floor.into_iter().chain(escalation).collect::<Vec<_>>();
        matched = match_file(input, &full, progress, cancel, api_base).await?;
    }
    Ok(matched)
}

pub(crate) async fn match_file(
    input: &Path,
    algos: &[HashAlgo],
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
    api_base: Option<&str>,
) -> Result<GameAndRelationMatchResult> {
    let digests = crate::dat::digest_inner_async(
        input.to_path_buf(),
        algos.to_vec(),
        progress,
        cancel.clone(),
    )
    .await?;
    let file_name = input
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let search = GameFileMatchSearch::from_digests(file_name, primary_digests(&digests));
    PlaymatchClient::new(api_base)
        .identify_relations(&search, &cancel)
        .await
        .map_err(anyhow::Error::from)
}

pub(crate) fn primary_digests(digests: &RomDigests) -> &crate::util::FileDigests {
    match digests {
        RomDigests::Single(d) => d,
        RomDigests::Tracks { whole, .. } => whole,
    }
}

/// Result of matching one local file against the Playmatch DAT database.
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

pub(crate) fn match_data(
    kind: &'static str,
    path: &Path,
    matched: &GameAndRelationMatchResult,
) -> DatMatchData {
    let (verdict, match_algo) = match match_strength(matched.game_match_type) {
        MatchStrength::Verified(algo) => (DatVerdict::Verified.as_str(), Some(algo.label())),
        MatchStrength::NameSizeHint => (DatVerdict::Hint.as_str(), None),
        MatchStrength::NoMatch => (DatVerdict::Unknown.as_str(), None),
    };
    DatMatchData {
        kind,
        path: path.to_path_buf(),
        verdict: verdict.to_string(),
        match_algo: match_algo.map(str::to_string),
        game_name: matched.game.as_ref().map(|g| g.name.clone()),
        platform: matched.platform.as_ref().map(|p| p.name.clone()),
        signature_group: matched.signature_group.as_ref().map(|g| g.name.clone()),
        dat_file: matched
            .dat_file
            .as_ref()
            .map(|d| d.name.clone())
            .or_else(|| matched.dat_file_import.as_ref().map(|i| i.name.clone())),
        dat_file_id: matched.dat_file.as_ref().map(|d| d.id.clone()).or_else(|| {
            matched
                .dat_file_import
                .as_ref()
                .map(|i| i.dat_file_id.clone())
        }),
        dat_version: matched
            .dat_file_import
            .as_ref()
            .map(|i| i.version.clone())
            .or_else(|| matched.dat_file.as_ref().map(|d| d.current_version.clone())),
        matched: Some(matched.clone()),
        error: None,
    }
}

pub(crate) fn report_record(
    path: &Path,
    matched: &GameAndRelationMatchResult,
    error: Option<String>,
) -> DatReportRecord {
    let data = match_data("dat", path, matched);
    DatReportRecord {
        path: path.display().to_string(),
        verdict: data.verdict,
        game_name: data.game_name,
        game_id: matched.game.as_ref().map(|g| g.id.clone()),
        platform: data.platform,
        signature_group: data.signature_group,
        dat_file_name: data.dat_file,
        dat_file_id: data.dat_file_id,
        dat_version: data.dat_version,
        match_algo: data.match_algo,
        detail: None,
        size_bytes: file_len(path),
        status: if error.is_some() {
            FileStatus::Failed
        } else {
            FileStatus::Ok
        },
        elapsed_ms: 0,
        error,
    }
}

pub(crate) fn error_report_record(path: &Path, error: String) -> DatReportRecord {
    DatReportRecord {
        path: path.display().to_string(),
        verdict: DatVerdict::Failed.as_str().to_string(),
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
        elapsed_ms: 0,
        error: Some(error),
    }
}

pub(crate) fn rename_candidate(
    path: PathBuf,
    matched: &GameAndRelationMatchResult,
) -> RenameCandidate {
    let verified = match_strength(matched.game_match_type).is_verified();
    RenameCandidate {
        path,
        game_id: matched.game.as_ref().map(|g| g.id.clone()),
        game_name: matched.game.as_ref().map(|g| g.name.clone()),
        file_name: (matched.game_files.len() == 1).then(|| matched.game_files[0].file_name.clone()),
        verified,
    }
}
