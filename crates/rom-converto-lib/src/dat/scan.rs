//! Bulk library scan: digest every unit, resolve them through one bulk
//! identify pass (plus a redo pass for quick digests that did not verify),
//! and classify each into matched, misnamed, hint, unknown, unsupported or
//! failed.

use crate::dat::model::{
    BulkIdentifyIdsResult, BulkIdentifyItem, BulkItemStatus, GameFileMatchSearch,
};
use crate::dat::run::primary_digests;
use crate::dat::units::{DatUnit, bucket, digest_unit, quick_digest, search_name};
use crate::dat::verdict::{DatVerdict, MatchStrength, match_strength};
use crate::dat::{DatError, DatResult, PlaymatchClient, RomDigests};
use crate::runner::models::RunRow;
use crate::util::{
    CancelToken, Cancelled, HashAlgo, HashCache, NX_DAT_UNSUPPORTED_HINT, ProgressReporter,
};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One scanned unit's classification.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct DatScanRow {
    pub path: PathBuf,
    pub status: &'static str,
    pub game_name: Option<String>,
    pub game_id: Option<String>,
    pub match_algo: Option<String>,
    pub canonical_stem: Option<String>,
    pub error: Option<String>,
}

impl DatScanRow {
    fn new(path: &Path, status: &'static str, error: Option<String>) -> Self {
        Self {
            path: path.to_path_buf(),
            status,
            game_name: None,
            game_id: None,
            match_algo: None,
            canonical_stem: None,
            error,
        }
    }
}

/// Result of a `dat.scan` run: per-status counts and one row per unit in
/// walk order.
#[derive(Debug, Default, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct DatScanData {
    pub matched: usize,
    pub misnamed: usize,
    pub hint: usize,
    pub unknown: usize,
    pub unsupported: usize,
    pub failed: usize,
    pub rows: Vec<DatScanRow>,
}

impl DatScanData {
    /// Append a row, bumping the count for its status.
    pub fn push(&mut self, row: DatScanRow) {
        match row.status {
            "matched" => self.matched += 1,
            "misnamed" => self.misnamed += 1,
            "hint" => self.hint += 1,
            "unknown" => self.unknown += 1,
            "unsupported" => self.unsupported += 1,
            _ => self.failed += 1,
        }
        self.rows.push(row);
    }
}

enum Digested {
    Ok {
        digests: RomDigests,
        name: String,
        quick: bool,
    },
    Row(DatScanRow),
}

fn bucket_row(path: &Path, e: DatError) -> DatResult<DatScanRow> {
    let (kind, msg) = bucket(e)?;
    Ok(DatScanRow::new(
        path,
        kind.verdict().as_str(),
        (kind.verdict() == DatVerdict::Failed).then_some(msg),
    ))
}

fn bulk_item(name: &str, digests: &RomDigests) -> BulkIdentifyItem {
    BulkIdentifyItem {
        search: GameFileMatchSearch::from_digests(name, primary_digests(digests)),
        key: None,
    }
}

fn is_verified(result: Option<&BulkIdentifyIdsResult>) -> bool {
    result.is_some_and(|r| {
        r.status == BulkItemStatus::Ok
            && r.matched
                .as_ref()
                .is_some_and(|m| match_strength(m.game_match_type).is_verified())
    })
}

/// Digest `units` and resolve them through the bulk identify endpoints on
/// `client`. Rows stream through `progress.row` as they settle: a `pending`
/// row once a unit is digested, then its final classification.
pub async fn scan_units(
    units: &[DatUnit],
    algos: &[HashAlgo],
    quick: bool,
    cache: Option<&HashCache>,
    client: &PlaymatchClient,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> DatResult<DatScanData> {
    // The outer bar counts files; the hasher's per-file byte progress goes to
    // a child channel so it never resets the outer counter.
    progress.start(units.len() as u64, "Hashing files");
    let file_progress = progress.child("file");
    let mut digested = Vec::with_capacity(units.len());
    for unit in units {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let q = if quick {
            quick_digest(unit, cache).await
        } else {
            None
        };
        let name = search_name(unit, q.as_ref());
        let (result, quick) = match q {
            Some(q) => (Ok(RomDigests::Single(q.digests)), true),
            None => (
                digest_unit(unit, algos, cache, file_progress.as_ref(), cancel).await,
                false,
            ),
        };
        let row = match result {
            Ok(digests) => {
                digested.push(Digested::Ok {
                    digests,
                    name,
                    quick,
                });
                DatScanRow::new(unit.display_path(), "pending", None)
            }
            Err(e) => {
                let row = bucket_row(unit.display_path(), e)?;
                digested.push(Digested::Row(row.clone()));
                row
            }
        };
        progress.row(&RunRow::DatScan(row));
        progress.batch_advance(unit.size_bytes());
        progress.inc(1);
    }

    let mut owners = Vec::new();
    let mut items = Vec::new();
    for (i, d) in digested.iter().enumerate() {
        if let Digested::Ok { digests, name, .. } = d {
            owners.push(i);
            items.push(bulk_item(name, digests));
        }
    }
    // A zero total marks the one-shot network phases as indeterminate: they
    // report no per-item progress to count.
    if !items.is_empty() {
        progress.start(0, &format!("Matching {} files", items.len()));
    }
    let bulk = client.identify_bulk_ids(items, cancel).await?;
    let mut results: HashMap<usize, &BulkIdentifyIdsResult> = HashMap::with_capacity(bulk.len());
    results.extend(by_index(&bulk, &owners));

    // A quick digest that did not hash-verify is redone with a full decode
    // and queried again in one small second bulk pass, so the match stays
    // authoritative without one round trip per straggler.
    let mut redo_owners = Vec::new();
    let mut redo_items = Vec::new();
    let redo: Vec<usize> = (0..digested.len())
        .filter(|i| {
            matches!(digested[*i], Digested::Ok { quick: true, .. })
                && !is_verified(results.get(i).copied())
        })
        .collect();
    if !redo.is_empty() {
        progress.start(redo.len() as u64, "Rehashing quick misses");
    }
    for i in redo {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        results.remove(&i);
        // The full digest reads the archive itself, so it searches under the
        // archive's name rather than the quick probe's member name.
        let name = search_name(&units[i], None);
        match digest_unit(&units[i], algos, cache, file_progress.as_ref(), cancel).await {
            Ok(full) => {
                redo_items.push(bulk_item(&name, &full));
                redo_owners.push(i);
                digested[i] = Digested::Ok {
                    digests: full,
                    name,
                    quick: false,
                };
            }
            Err(e) => digested[i] = Digested::Row(bucket_row(units[i].display_path(), e)?),
        }
        progress.inc(1);
    }
    let redo_bulk = if redo_items.is_empty() {
        Vec::new()
    } else {
        progress.start(0, &format!("Matching {} rehashed files", redo_items.len()));
        client.identify_bulk_ids(redo_items, cancel).await?
    };
    results.extend(by_index(&redo_bulk, &redo_owners));

    let mut matched_ids: Vec<String> = results
        .values()
        .filter(|r| is_verified(Some(r)))
        .filter_map(|r| r.matched.as_ref()?.id.clone())
        .collect();
    matched_ids.sort();
    matched_ids.dedup();
    let games = if matched_ids.is_empty() {
        Vec::new()
    } else {
        client.games_bulk(matched_ids, cancel).await?
    };
    let name_for_id = |id: &str| -> Option<String> {
        games
            .iter()
            .find(|g| g.id == id)
            .and_then(|g| g.data.as_ref())
            .map(|d| d.name.clone())
    };

    let mut data = DatScanData::default();
    for (i, d) in digested.into_iter().enumerate() {
        let row = match d {
            Digested::Row(row) => row,
            Digested::Ok { .. } => {
                let row = scan_row_for(
                    units[i].display_path(),
                    results.get(&i).copied(),
                    &name_for_id,
                );
                progress.row(&RunRow::DatScan(row.clone()));
                row
            }
        };
        data.push(row);
    }
    if data.unsupported > 0 {
        progress.warn(NX_DAT_UNSUPPORTED_HINT);
    }
    Ok(data)
}

/// Bulk results keyed by the unit index that owns each item position.
fn by_index<'a>(
    bulk: &'a [BulkIdentifyIdsResult],
    owners: &[usize],
) -> impl Iterator<Item = (usize, &'a BulkIdentifyIdsResult)> {
    bulk.iter()
        .filter_map(|r| owners.get(r.index).map(|&unit| (unit, r)))
}

/// Classify one unit's bulk-ids result: a missing or non-ok result is failed
/// (never silently dropped), NoMatch is unknown, a FileNameAndSize match is
/// hint, and a hash-verified match is matched unless the local stem differs
/// from the canonical game name (misnamed).
pub fn scan_row_for(
    path: &Path,
    result: Option<&BulkIdentifyIdsResult>,
    name_for_id: &impl Fn(&str) -> Option<String>,
) -> DatScanRow {
    let Some(result) = result else {
        return DatScanRow::new(
            path,
            DatVerdict::Failed.as_str(),
            Some("no result returned for this file".to_string()),
        );
    };
    if result.status != BulkItemStatus::Ok {
        let msg = result
            .error
            .as_ref()
            .map(|e| e.message.clone())
            .unwrap_or_else(|| "bulk identify item failed".to_string());
        return DatScanRow::new(path, DatVerdict::Failed.as_str(), Some(msg));
    }
    let Some(matched) = &result.matched else {
        return DatScanRow::new(path, DatVerdict::Unknown.as_str(), None);
    };
    match match_strength(matched.game_match_type) {
        MatchStrength::NoMatch => DatScanRow::new(path, DatVerdict::Unknown.as_str(), None),
        MatchStrength::NameSizeHint => DatScanRow::new(path, DatVerdict::Hint.as_str(), None),
        MatchStrength::Verified(algo) => {
            let game_name = matched.id.as_deref().and_then(name_for_id);
            let local_stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            // Scan's "matched" has no DatVerdict counterpart: it is verify's
            // Verified spelled for the scan summary.
            let status = match &game_name {
                Some(name) if !name.eq_ignore_ascii_case(local_stem) => {
                    DatVerdict::Misnamed.as_str()
                }
                _ => "matched",
            };
            DatScanRow {
                path: path.to_path_buf(),
                status,
                game_name: game_name.clone(),
                game_id: matched.id.clone(),
                match_algo: Some(algo.label().to_string()),
                canonical_stem: game_name,
                error: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dat::model::{BulkItemError, GameMatchType, GameMetadataMatchResult};

    fn result(
        status: BulkItemStatus,
        matched: Option<GameMetadataMatchResult>,
    ) -> BulkIdentifyIdsResult {
        BulkIdentifyIdsResult {
            index: 0,
            key: None,
            status,
            matched,
            error: (status != BulkItemStatus::Ok).then(|| BulkItemError {
                code: "E".into(),
                message: "boom".into(),
                field: None,
            }),
        }
    }

    fn matched(t: GameMatchType, id: &str) -> GameMetadataMatchResult {
        GameMetadataMatchResult {
            game_match_type: t,
            id: Some(id.into()),
            external_metadata: Vec::new(),
        }
    }

    #[test]
    fn scan_tally_counts_misnamed() {
        let names = |id: &str| (id == "g1").then(|| "Canonical Name".to_string());
        let mut data = DatScanData::default();
        for (path, res) in [
            (
                "roms/Canonical Name.gba",
                Some(result(
                    BulkItemStatus::Ok,
                    Some(matched(GameMatchType::Crc, "g1")),
                )),
            ),
            (
                "roms/renamed.gba",
                Some(result(
                    BulkItemStatus::Ok,
                    Some(matched(GameMatchType::Sha1, "g1")),
                )),
            ),
            (
                "roms/hint.gba",
                Some(result(
                    BulkItemStatus::Ok,
                    Some(matched(GameMatchType::FileNameAndSize, "g2")),
                )),
            ),
            (
                "roms/unknown.gba",
                Some(result(
                    BulkItemStatus::Ok,
                    Some(matched(GameMatchType::NoMatch, "g3")),
                )),
            ),
            ("roms/none.gba", Some(result(BulkItemStatus::Ok, None))),
            ("roms/error.gba", Some(result(BulkItemStatus::Error, None))),
            ("roms/dropped.gba", None),
        ] {
            data.push(scan_row_for(Path::new(path), res.as_ref(), &names));
        }
        data.push(DatScanRow::new(
            Path::new("roms/x.nsz"),
            "unsupported",
            None,
        ));

        assert_eq!(data.matched, 1);
        assert_eq!(data.misnamed, 1);
        assert_eq!(data.hint, 1);
        assert_eq!(data.unknown, 2);
        assert_eq!(data.failed, 2);
        assert_eq!(data.unsupported, 1);
        let misnamed = &data.rows[1];
        assert_eq!(misnamed.status, "misnamed");
        assert_eq!(misnamed.canonical_stem.as_deref(), Some("Canonical Name"));
        assert_eq!(misnamed.match_algo.as_deref(), Some("sha1"));
        assert_eq!(data.rows[0].status, "matched");
        assert_eq!(data.rows[5].error.as_deref(), Some("boom"));
        assert_eq!(
            data.rows[6].error.as_deref(),
            Some("no result returned for this file")
        );
    }
}
