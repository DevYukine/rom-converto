//! Bulk library scan: digest every unit, resolve them through one bulk
//! identify pass (plus a redo pass for quick digests that did not verify),
//! and classify each into matched, misnamed, hint, unknown, unsupported or
//! failed.

use crate::dat::client::{BULK_MAX_ITEMS, MAX_IN_FLIGHT};
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
    /// Fully rehashed quick miss awaiting the batched redo identify.
    Redo,
}

/// Row for a unit whose identify result is final; records a verified
/// match's game id for the canonical-name pass.
fn finished_row(
    path: &Path,
    result: Option<&BulkIdentifyIdsResult>,
    matched_ids: &mut Vec<String>,
) -> DatScanRow {
    if let Some(id) = result
        .filter(|result| is_verified(Some(*result)))
        .and_then(|result| result.matched.as_ref()?.id.clone())
    {
        matched_ids.push(id);
    }
    scan_row_for(path, result)
}

/// Identify the rehashed quick misses collected so far and replace their
/// pending rows.
#[allow(clippy::too_many_arguments)]
async fn flush_redo(
    items: &mut Vec<BulkIdentifyItem>,
    owners: &mut Vec<usize>,
    rows: &mut [DatScanRow],
    matched_ids: &mut Vec<String>,
    units: &[DatUnit],
    client: &PlaymatchClient,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> DatResult<()> {
    if items.is_empty() {
        return Ok(());
    }
    progress.start(0, &format!("Matching {} rehashed files", items.len()));
    let bulk = client
        .identify_bulk_ids(std::mem::take(items), cancel)
        .await?;
    // Every owner gets a final row: a result the server dropped is "failed".
    let mut results = vec![None; owners.len()];
    for result in &bulk {
        if result.index < results.len() {
            results[result.index] = Some(result);
        }
    }
    for (&row_index, result) in owners.iter().zip(results) {
        rows[row_index] = finished_row(units[row_index].display_path(), result, matched_ids);
    }
    owners.clear();
    Ok(())
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

/// Digest and classify each input page before retaining the next one. The
/// final report remains in walk order; digest/request/result state is bounded
/// to one full identify pipeline window.
pub async fn scan_units(
    units: &[DatUnit],
    algos: &[HashAlgo],
    quick: bool,
    cache: Option<&HashCache>,
    client: &PlaymatchClient,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> DatResult<DatScanData> {
    progress.start(units.len() as u64, "Hashing files");
    let file_progress = progress.child("file");
    let mut data = DatScanData::default();
    let mut rows = Vec::with_capacity(units.len());
    let mut matched_ids = Vec::new();
    let mut redo_items = Vec::new();
    let mut redo_owners = Vec::new();
    let mut hashed = 0u64;

    for page in units.chunks(BULK_MAX_ITEMS * MAX_IN_FLIGHT) {
        // The matching and rehash phases below replace the unit-count bar,
        // so every page after the first restores it with the units the
        // earlier pages already hashed.
        if hashed > 0 {
            progress.start(units.len() as u64, "Hashing files");
            progress.inc(hashed);
        }
        let mut digested = Vec::with_capacity(page.len());
        for unit in page {
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
            match result {
                Ok(digests) => {
                    digested.push(Digested::Ok {
                        digests,
                        name,
                        quick,
                    });
                }
                Err(e) => {
                    let row = bucket_row(unit.display_path(), e)?;
                    digested.push(Digested::Row(row));
                }
            }
            progress.batch_advance(unit.size_bytes());
            progress.inc(1);
            hashed += 1;
        }

        let mut owners = Vec::new();
        let mut items = Vec::new();
        for (i, d) in digested.iter().enumerate() {
            if let Digested::Ok { digests, name, .. } = d {
                owners.push(i);
                items.push(bulk_item(name, digests));
            }
        }
        let bulk = if items.is_empty() {
            Vec::new()
        } else {
            progress.start(0, &format!("Matching {} files", items.len()));
            client.identify_bulk_ids(items, cancel).await?
        };
        let mut results = vec![None; page.len()];
        for (i, result) in by_index(&bulk, &owners) {
            results[i] = Some(result);
        }

        // Rehash quick probes that did not produce a verified hash match.
        // Their identify requests are batched across pages like develop's
        // single redo pass, so misses spread over many pages still cost
        // ceil(misses / BULK_MAX_ITEMS) requests instead of one wave per page.
        let redo: Vec<usize> = (0..digested.len())
            .filter(|i| {
                matches!(digested[*i], Digested::Ok { quick: true, .. })
                    && !is_verified(results[*i])
            })
            .collect();
        if !redo.is_empty() {
            progress.start(redo.len() as u64, "Rehashing quick misses");
        }
        for i in redo {
            if cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            results[i] = None;
            let name = search_name(&page[i], None);
            match digest_unit(&page[i], algos, cache, file_progress.as_ref(), cancel).await {
                Ok(full) => {
                    redo_items.push(bulk_item(&name, &full));
                    redo_owners.push(rows.len() + i);
                    digested[i] = Digested::Redo;
                }
                Err(e) => {
                    let row = bucket_row(page[i].display_path(), e)?;
                    digested[i] = Digested::Row(row);
                }
            }
            progress.inc(1);
        }

        for (i, d) in digested.into_iter().enumerate() {
            let row = match d {
                Digested::Row(row) => row,
                // Filled in once the batched redo identify returns.
                Digested::Redo => DatScanRow::new(page[i].display_path(), "pending", None),
                Digested::Ok { .. } => {
                    finished_row(page[i].display_path(), results[i], &mut matched_ids)
                }
            };
            rows.push(row);
        }
        if redo_items.len() >= BULK_MAX_ITEMS * MAX_IN_FLIGHT {
            flush_redo(
                &mut redo_items,
                &mut redo_owners,
                &mut rows,
                &mut matched_ids,
                units,
                client,
                progress,
                cancel,
            )
            .await?;
        }
    }
    flush_redo(
        &mut redo_items,
        &mut redo_owners,
        &mut rows,
        &mut matched_ids,
        units,
        client,
        progress,
        cancel,
    )
    .await?;

    matched_ids.sort();
    matched_ids.dedup();
    let games = if matched_ids.is_empty() {
        Vec::new()
    } else {
        client.games_bulk(matched_ids, cancel).await?
    };
    let names: HashMap<&str, &str> = games
        .iter()
        .filter_map(|game| Some((game.id.as_str(), game.data.as_ref()?.name.as_str())))
        .collect();
    for mut row in rows {
        apply_canonical_name(&mut row, &names);
        progress.row(&RunRow::DatScan(row.clone()));
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
/// hint, and a hash-verified match is matched. A verified match's name is
/// resolved separately via [`apply_canonical_name`] once `games_bulk` has run
/// for the whole scan, so it may still be reclassified misnamed there.
pub fn scan_row_for(path: &Path, result: Option<&BulkIdentifyIdsResult>) -> DatScanRow {
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
        // Scan's "matched" has no DatVerdict counterpart: it is verify's
        // Verified spelled for the scan summary. Left as "matched" here;
        // apply_canonical_name reclassifies it misnamed once the canonical
        // name is known.
        MatchStrength::Verified(algo) => DatScanRow {
            path: path.to_path_buf(),
            status: "matched",
            game_name: None,
            game_id: matched.id.clone(),
            match_algo: Some(algo.label().to_string()),
            canonical_stem: None,
            error: None,
        },
    }
}

/// Applies the canonical game name resolved by a bulk `games_bulk` lookup to
/// a verified match, reclassifying it misnamed when the local file stem
/// differs from the canonical name. Rows with no matched game id are
/// unchanged.
fn apply_canonical_name(row: &mut DatScanRow, names: &HashMap<&str, &str>) {
    let Some(name) = row.game_id.as_deref().and_then(|id| names.get(id)) else {
        return;
    };
    let local_stem = row
        .path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    row.status = if (*name).eq_ignore_ascii_case(local_stem) {
        "matched"
    } else {
        DatVerdict::Misnamed.as_str()
    };
    row.game_name = Some((*name).to_string());
    row.canonical_stem = Some((*name).to_string());
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
        let names: HashMap<&str, &str> = [("g1", "Canonical Name")].into_iter().collect();
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
            let mut row = scan_row_for(Path::new(path), res.as_ref());
            apply_canonical_name(&mut row, &names);
            data.push(row);
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
    #[tokio::test]
    async fn scan_matches_in_pages_and_preserves_walk_order() {
        use std::io::Write as _;
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let units: Vec<_> = (0..BULK_MAX_ITEMS * MAX_IN_FLIGHT + 1)
            .map(|i| {
                let path = dir.path().join(format!("game-{i:03}.zip"));
                let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
                let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored);
                zip.start_file(format!("game-{i:03}.iso"), options).unwrap();
                zip.write_all(&[i as u8; 4]).unwrap();
                zip.finish().unwrap();
                DatUnit::File(path)
            })
            .collect();
        let (page_tx, page_rx) = std::sync::mpsc::channel::<usize>();
        let server = crate::dat::client::mock_api::spawn(move |request_line, body| {
            if !request_line.starts_with("POST /identify/bulk/") {
                return serde_json::json!({"match": null}).to_string();
            }
            let request: serde_json::Value = serde_json::from_slice(body).unwrap();
            let count = request["items"].as_array().unwrap().len();
            let _ = page_tx.send(count);
            let results: Vec<_> = (0..count)
                .map(|index| {
                    serde_json::json!({
                        "index": index,
                        "status": "ok",
                        "match": null
                    })
                })
                .collect();
            serde_json::json!({
                "summary": {
                    "total": count,
                    "succeeded": count,
                    "failed": 0,
                    "matched": 0,
                    "unmatched": count
                },
                "results": results
            })
            .to_string()
        });

        let client = PlaymatchClient::new(Some(server.url()));
        let scan = tokio::time::timeout(
            Duration::from_secs(60),
            scan_units(
                &units,
                &[HashAlgo::Crc32],
                true,
                None,
                &client,
                &crate::util::NoProgress,
                &CancelToken::new(),
            ),
        )
        .await;

        drop(server);
        let mut page_sizes: Vec<usize> = page_rx.into_iter().collect();
        let data = scan.expect("paged DAT scan timed out").unwrap();

        page_sizes.sort_unstable();
        assert_eq!(
            page_sizes,
            vec![
                1,
                1,
                BULK_MAX_ITEMS,
                BULK_MAX_ITEMS,
                BULK_MAX_ITEMS,
                BULK_MAX_ITEMS
            ]
        );
        assert_eq!(data.rows.len(), units.len());
        assert_eq!(data.unknown, units.len());
        assert!(
            data.rows
                .iter()
                .zip(&units)
                .all(|(row, unit)| row.path == unit.display_path())
        );
    }
}
