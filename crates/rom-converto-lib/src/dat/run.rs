//! Playmatch matching driven by a local unit: tiered checksum escalation,
//! quick-digest shortcut, track reconciliation, and the report and rename
//! records built from a match result.

use crate::dat::digest::TrackDigests;
use crate::dat::model::{GameAndRelationMatchResult, GameFileMatchSearch};
use crate::dat::rename::RenameCandidate;
use crate::dat::units::{DatUnit, digest_unit, is_tierable, quick_digest, search_name};
use crate::dat::verdict::{DatVerdict, MatchStrength, match_strength, reconcile_tracks};
use crate::dat::{DatResult, PlaymatchClient, RomDigests};
use crate::util::report::DatReportRecord;
use crate::util::{CancelToken, ChecksumBounds, FileStatus, HashAlgo, HashCache, ProgressReporter};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// How a verify/identify run digests its units: the requested digests, the
/// tier bounds escalation may move between, the quick zip shortcut, and the
/// persistent hash cache.
#[derive(Clone, Copy)]
pub(crate) struct MatchPolicy<'a> {
    pub algos: &'a [HashAlgo],
    pub bounds: &'a ChecksumBounds,
    pub quick: bool,
    pub cache: Option<&'a HashCache>,
}

/// Digest `unit` and resolve its match, escalating past the cheap floor tier
/// to the full ceiling tier only when the floor alone does not verify. With
/// `quick`, an eligible zip's own CRC32 is tried first and used only when it
/// resolves an authoritative verified match. The hash cache makes any
/// escalation happen at most once per file.
pub(crate) async fn match_unit(
    kind: &'static str,
    client: &PlaymatchClient,
    unit: &DatUnit,
    policy: MatchPolicy<'_>,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> DatResult<DatMatchData> {
    let MatchPolicy {
        algos,
        bounds,
        quick,
        cache,
    } = policy;
    if quick && let Some(data) = quick_match(kind, client, unit, cache, cancel).await? {
        return Ok(data);
    }
    let (floor, escalation) = if is_tierable(unit) {
        bounds.split(algos)
    } else {
        (algos.to_vec(), Vec::new())
    };
    let name = search_name(unit, None);
    let digests = digest_unit(unit, &floor, cache, progress, cancel).await?;
    let data = resolve_unit(kind, client, unit, &name, &digests, cancel).await?;
    if escalation.is_empty() || data.verdict != DatVerdict::Hint.as_str() {
        return Ok(data);
    }
    let full: Vec<HashAlgo> = floor.into_iter().chain(escalation).collect();
    let digests = digest_unit(unit, &full, cache, progress, cancel).await?;
    resolve_unit(kind, client, unit, &name, &digests, cancel).await
}

/// The quick CRC-only shortcut on its own: `Some` only when the zip's
/// central-directory CRC32 resolves a verified match, so the caller can fall
/// back to full extraction and hashing otherwise.
pub(crate) async fn quick_match(
    kind: &'static str,
    client: &PlaymatchClient,
    unit: &DatUnit,
    cache: Option<&HashCache>,
    cancel: &CancelToken,
) -> DatResult<Option<DatMatchData>> {
    let Some(q) = quick_digest(unit, cache).await else {
        return Ok(None);
    };
    let name = search_name(unit, Some(&q));
    let digests = RomDigests::Single(q.digests);
    let data = resolve_unit(kind, client, unit, &name, &digests, cancel).await?;
    Ok((data.verdict == DatVerdict::Verified.as_str()).then_some(data))
}

/// Resolve a digested unit against the database. A single stream takes one
/// relations call; a track set tries the whole-image query first (single-bin
/// DATs) and falls back to per-track reconciliation (multi-bin DATs).
pub(crate) async fn resolve_unit(
    kind: &'static str,
    client: &PlaymatchClient,
    unit: &DatUnit,
    name: &str,
    digests: &RomDigests,
    cancel: &CancelToken,
) -> DatResult<DatMatchData> {
    let path = unit.display_path();
    match digests {
        RomDigests::Single(d) => {
            let search = GameFileMatchSearch::from_digests(name, d);
            let matched = client.identify_relations(&search, cancel).await?;
            Ok(match_data(kind, path, &matched, d.size_bytes))
        }
        RomDigests::Tracks { tracks, whole } => {
            let stem = path.file_stem().and_then(|n| n.to_str()).unwrap_or("file");
            let whole_search = GameFileMatchSearch::from_digests(&format!("{stem}.bin"), whole);
            let whole_match = client.identify_relations(&whole_search, cancel).await?;
            if match_strength(whole_match.game_match_type).is_verified() {
                return Ok(match_data(kind, path, &whole_match, whole.size_bytes));
            }
            let first_name = match unit {
                DatUnit::CueSet { bins, .. } => bins[0]
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file")
                    .to_string(),
                DatUnit::File(_) => format!("{stem} (Track 1).bin"),
            };
            let first_search = GameFileMatchSearch::from_digests(&first_name, &tracks[0].digests);
            let track_match = client.identify_relations(&first_search, cancel).await?;
            Ok(match_data_tracks(
                kind,
                path,
                &track_match,
                &whole_match,
                tracks,
                whole.size_bytes,
            ))
        }
    }
}

pub(crate) fn primary_digests(digests: &RomDigests) -> &crate::util::FileDigests {
    match digests {
        RomDigests::Single(d) => d,
        RomDigests::Tracks { whole, .. } => whole,
    }
}

/// One disc track reconciled against the matched game's file list.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct DatTrackCheck {
    pub track: u32,
    pub ok: bool,
    pub algo: Option<String>,
    pub matched_file: Option<String>,
}

/// One external database cross-reference (automatic or manual match with a
/// provider id).
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct ExternalId {
    pub provider: String,
    pub id: String,
}

/// Result of matching one local unit against the Playmatch DAT database.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct DatMatchData {
    /// `verify` or `identify`. Serialized as `match_kind` so it does not
    /// collide with the `kind` tag of [`crate::runner::models::RunRow`].
    #[serde(rename = "match_kind")]
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
    pub external_ids: Vec<ExternalId>,
    pub tracks: Vec<DatTrackCheck>,
    /// Decoded whole-image size, 0 when nothing was digested.
    pub size_bytes: u64,
    #[serde(rename = "match")]
    pub matched: Option<GameAndRelationMatchResult>,
    pub error: Option<String>,
}

impl DatMatchData {
    /// A verdict with no match data: an unsupported format or a digest
    /// failure.
    pub(crate) fn errored(
        kind: &'static str,
        path: &Path,
        verdict: DatVerdict,
        error: String,
    ) -> Self {
        Self {
            kind,
            path: path.to_path_buf(),
            verdict: verdict.as_str().to_string(),
            match_algo: None,
            game_name: None,
            platform: None,
            signature_group: None,
            dat_file: None,
            dat_file_id: None,
            dat_version: None,
            external_ids: Vec::new(),
            tracks: Vec::new(),
            size_bytes: 0,
            matched: None,
            error: Some(error),
        }
    }

    /// "ok/total tracks matched" for a track set, `None` for a single stream.
    pub fn track_detail(&self) -> Option<String> {
        (!self.tracks.is_empty()).then(|| {
            let ok = self.tracks.iter().filter(|t| t.ok).count();
            format!("{ok}/{} tracks matched", self.tracks.len())
        })
    }

    pub fn game_id(&self) -> Option<String> {
        self.matched.as_ref()?.game.as_ref().map(|g| g.id.clone())
    }
}

fn external_ids(matched: &GameAndRelationMatchResult) -> Vec<ExternalId> {
    matched
        .external_metadata
        .iter()
        .filter(|m| matches!(m.match_type.as_str(), "Automatic" | "Manual"))
        .filter_map(|m| {
            m.provider_id.clone().map(|id| ExternalId {
                provider: m.provider_name.clone(),
                id,
            })
        })
        .collect()
}

fn with_relations(
    kind: &'static str,
    path: &Path,
    matched: &GameAndRelationMatchResult,
    verdict: DatVerdict,
    match_algo: Option<HashAlgo>,
    size_bytes: u64,
) -> DatMatchData {
    DatMatchData {
        kind,
        path: path.to_path_buf(),
        verdict: verdict.as_str().to_string(),
        match_algo: match_algo.map(|a| a.label().to_string()),
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
        external_ids: external_ids(matched),
        tracks: Vec::new(),
        size_bytes,
        matched: Some(matched.clone()),
        error: None,
    }
}

pub(crate) fn match_data(
    kind: &'static str,
    path: &Path,
    matched: &GameAndRelationMatchResult,
    size_bytes: u64,
) -> DatMatchData {
    let (verdict, match_algo) = match match_strength(matched.game_match_type) {
        MatchStrength::Verified(algo) => (DatVerdict::Verified, Some(algo)),
        MatchStrength::NameSizeHint => (DatVerdict::Hint, None),
        MatchStrength::NoMatch => (DatVerdict::Unknown, None),
    };
    with_relations(kind, path, matched, verdict, match_algo, size_bytes)
}

/// A track set is Verified only when every local track reconciles by a real
/// hash; a hash-verified track 1 with an unreconciled other track is not
/// whole-set verification. Display fields come from the per-track query when
/// it reconciled or resolved a game, else from the whole-image query.
fn match_data_tracks(
    kind: &'static str,
    path: &Path,
    track_match: &GameAndRelationMatchResult,
    whole_match: &GameAndRelationMatchResult,
    tracks: &[TrackDigests],
    size_bytes: u64,
) -> DatMatchData {
    let recon = reconcile_tracks(tracks, &track_match.game_files);
    let track_resolved = recon.all_ok || match_strength(track_match.game_match_type).is_verified();
    let display = if track_resolved {
        track_match
    } else {
        whole_match
    };
    let verdict = if recon.all_ok {
        DatVerdict::Verified
    } else if match_strength(track_match.game_match_type) == MatchStrength::NameSizeHint
        || match_strength(whole_match.game_match_type) == MatchStrength::NameSizeHint
    {
        DatVerdict::Hint
    } else {
        DatVerdict::Unknown
    };
    let mut data = with_relations(kind, path, display, verdict, None, size_bytes);
    data.tracks = recon
        .tracks
        .iter()
        .map(|t| DatTrackCheck {
            track: t.track_number,
            ok: t.ok,
            algo: t.algo.map(|a| a.label().to_string()),
            matched_file: t.matched_file.clone(),
        })
        .collect();
    data
}

pub(crate) fn report_record(data: &DatMatchData, elapsed_ms: u64) -> DatReportRecord {
    DatReportRecord {
        path: data.path.display().to_string(),
        verdict: data.verdict.clone(),
        game_name: data.game_name.clone(),
        game_id: data.game_id(),
        platform: data.platform.clone(),
        signature_group: data.signature_group.clone(),
        dat_file_name: data.dat_file.clone(),
        dat_file_id: data.dat_file_id.clone(),
        dat_version: data.dat_version.clone(),
        match_algo: data.match_algo.clone(),
        detail: data.track_detail(),
        size_bytes: data.size_bytes,
        status: if data.verdict == DatVerdict::Failed.as_str() {
            FileStatus::Failed
        } else {
            FileStatus::Ok
        },
        elapsed_ms,
        error: data.error.clone(),
    }
}

/// A report row with no match data: a verdict and the error that produced it.
pub(crate) fn error_report_record(
    path: &Path,
    verdict: DatVerdict,
    error: Option<String>,
    elapsed_ms: u64,
) -> DatReportRecord {
    DatReportRecord {
        path: path.display().to_string(),
        verdict: verdict.as_str().to_string(),
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
        status: if verdict == DatVerdict::Failed {
            FileStatus::Failed
        } else {
            FileStatus::Ok
        },
        elapsed_ms,
        error,
    }
}

/// Build a rename candidate from a file's relations match. `verified` is set
/// only for a hash-rung match (hints never rename); the file-level canonical
/// name is taken from the sole matching gameFiles entry when present.
pub(crate) fn rename_candidate(
    path: PathBuf,
    matched: Option<&GameAndRelationMatchResult>,
) -> RenameCandidate {
    let Some(matched) = matched else {
        return RenameCandidate {
            path,
            game_id: None,
            game_name: None,
            file_name: None,
            verified: false,
        };
    };
    RenameCandidate {
        path,
        game_id: matched.game.as_ref().map(|g| g.id.clone()),
        game_name: matched.game.as_ref().map(|g| g.name.clone()),
        file_name: (matched.game_files.len() == 1).then(|| matched.game_files[0].file_name.clone()),
        verified: match_strength(matched.game_match_type).is_verified(),
    }
}
