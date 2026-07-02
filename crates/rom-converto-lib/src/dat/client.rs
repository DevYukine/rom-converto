use crate::dat::error::{DatError, DatResult};
use crate::dat::model::*;
use crate::util::http::CLIENT;
use crate::util::{CancelToken, ProgressReporter};
use futures::stream::{self, StreamExt};
use serde::de::DeserializeOwned;
use std::time::Duration;

pub const DEFAULT_API_BASE: &str = "https://playmatch.retrorealm.dev/api/v2";
pub const BULK_MAX_ITEMS: usize = 100;
pub const BULK_MAX_BODY_BYTES: usize = 256 * 1024;
pub const MAX_IN_FLIGHT: usize = 2;

const PAGE_LIMIT: u64 = 50;
const MAX_PAGES: usize = 10_000;
const MAX_RETRIES: u32 = 5;

pub struct PlaymatchClient {
    base: String,
    http: &'static reqwest::Client,
}

#[derive(Debug, Clone, Default)]
pub struct DatFileFilter {
    pub platform_id: Option<String>,
    pub signature_group_id: Option<String>,
    pub name: Option<String>,
    pub subset: Option<String>,
    pub tag: Option<String>,
}

impl PlaymatchClient {
    pub fn new(api_base: Option<&str>) -> Self {
        let base = api_base
            .unwrap_or(DEFAULT_API_BASE)
            .trim_end_matches('/')
            .to_string();
        Self {
            base,
            http: &CLIENT,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    // Build a GET with a percent-encoded query string. The reqwest `query`
    // builder is feature-gated off in this crate, so the URL is assembled with
    // the re-exported url crate instead.
    fn get_with_query(&self, path: &str, pairs: &[(&str, String)]) -> reqwest::RequestBuilder {
        let raw = self.url(path);
        match reqwest::Url::parse_with_params(&raw, pairs.iter().map(|(k, v)| (*k, v.as_str()))) {
            Ok(url) => self.http.get(url),
            Err(_) => self.http.get(raw),
        }
    }

    // Race one request against cancellation, classify errors, and retry 429/5xx
    // with capped exponential backoff plus jitter. Transport errors (no HTTP
    // status) are never retried and abort immediately.
    async fn send_json<T: DeserializeOwned>(
        &self,
        req: reqwest::RequestBuilder,
        cancel: &CancelToken,
    ) -> DatResult<T> {
        let mut attempt: u32 = 0;
        loop {
            if cancel.is_cancelled() {
                return Err(DatError::Cancelled);
            }
            let Some(builder) = req.try_clone() else {
                return Err(DatError::BadResponse(
                    "request body is not cloneable for retry".to_string(),
                ));
            };

            let response = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(DatError::Cancelled),
                r = builder.send() => r,
            };

            let response = match response {
                Ok(r) => r,
                Err(e) => {
                    if e.status().is_none() {
                        return Err(DatError::Transport(e.to_string()));
                    }
                    return Err(DatError::HttpError(e));
                }
            };

            let status = response.status();
            if status.is_success() {
                let bytes = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(DatError::Cancelled),
                    b = response.bytes() => b,
                };
                let bytes = bytes.map_err(|e| DatError::Transport(e.to_string()))?;
                return serde_json::from_slice(&bytes)
                    .map_err(|e| DatError::BadResponse(e.to_string()));
            }

            let retryable = status.as_u16() == 429 || status.is_server_error();
            if retryable && attempt < MAX_RETRIES {
                let retry_after = status
                    .as_u16()
                    .eq(&429)
                    .then(|| retry_after_secs(response.headers()))
                    .flatten();
                let wait = retry_after.unwrap_or_else(|| backoff_delay(attempt));
                attempt += 1;
                let slept = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(DatError::Cancelled),
                    _ = tokio::time::sleep(wait) => true,
                };
                if slept {
                    continue;
                }
            }

            let bytes = response
                .bytes()
                .await
                .map_err(|e| DatError::Transport(e.to_string()))?;
            return Err(match serde_json::from_slice::<ApiErrorBody>(&bytes) {
                Ok(body) => DatError::Api {
                    code: body.code,
                    message: body.message,
                },
                Err(_) => {
                    DatError::BadResponse(format!("http {} with undecodable body", status.as_u16()))
                }
            });
        }
    }
}

// Retry-After header in whole seconds, when present and parseable.
fn retry_after_secs(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

// 500ms * 2^attempt capped at 8s, plus 0..250ms jitter drawn from the wall
// clock (no rand dependency).
fn backoff_delay(attempt: u32) -> Duration {
    let base_ms = 500u64.saturating_mul(1u64 << attempt.min(4)).min(8_000);
    let jitter = jitter_ms();
    Duration::from_millis(base_ms + jitter)
}

fn jitter_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| (d.subsec_nanos() as u64) % 250)
        .unwrap_or(0)
}

// Greedy pack respecting both the item-count and serialized-byte caps. The byte
// cost is measured on real serialization of each item plus one byte for the
// array comma/bracket framing, so the flattened `fileSize` number is counted as
// it goes over the wire.
pub fn chunk_bulk_items(items: Vec<BulkIdentifyItem>) -> DatResult<Vec<Vec<BulkIdentifyItem>>> {
    let mut chunks: Vec<Vec<BulkIdentifyItem>> = Vec::new();
    let mut current: Vec<BulkIdentifyItem> = Vec::new();
    let mut current_bytes = frame_overhead();

    for item in items {
        let item_bytes = item_serialized_len(&item)?;
        let would_be = current_bytes + item_bytes + 1;
        let over_bytes = !current.is_empty() && would_be > BULK_MAX_BODY_BYTES;
        let over_count = current.len() >= BULK_MAX_ITEMS;
        if over_bytes || over_count {
            chunks.push(std::mem::take(&mut current));
            current_bytes = frame_overhead();
        }
        if current.is_empty() {
            current_bytes = frame_overhead() + item_bytes;
        } else {
            current_bytes += item_bytes + 1;
        }
        current.push(item);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    Ok(chunks)
}

// Bytes of `{"items":[]}` around the item list.
fn frame_overhead() -> usize {
    r#"{"items":[]}"#.len()
}

fn item_serialized_len(item: &BulkIdentifyItem) -> DatResult<usize> {
    serde_json::to_vec(item)
        .map(|v| v.len())
        .map_err(|e| DatError::BadResponse(e.to_string()))
}

impl PlaymatchClient {
    pub async fn identify_ids(
        &self,
        q: &GameFileMatchSearch,
        cancel: &CancelToken,
    ) -> DatResult<GameMetadataMatchResult> {
        let req = self.get_with_query("/identify/ids", &identify_query(q));
        self.send_json(req, cancel).await
    }

    pub async fn identify_relations(
        &self,
        q: &GameFileMatchSearch,
        cancel: &CancelToken,
    ) -> DatResult<GameAndRelationMatchResult> {
        let req = self.get_with_query("/identify/relations", &identify_query(q));
        self.send_json(req, cancel).await
    }

    pub async fn identify_bulk_ids(
        &self,
        items: Vec<BulkIdentifyItem>,
        cancel: &CancelToken,
    ) -> DatResult<Vec<BulkIdentifyIdsResult>> {
        self.run_bulk(items, cancel, |chunk, cancel| async move {
            let req = self
                .http
                .post(self.url("/identify/bulk/ids"))
                .json(&BulkIdentifyRequest { items: chunk });
            let resp: BulkIdentifyIdsResponse = self.send_json(req, &cancel).await?;
            Ok(resp.results)
        })
        .await
    }

    pub async fn identify_bulk_relations(
        &self,
        items: Vec<BulkIdentifyItem>,
        cancel: &CancelToken,
    ) -> DatResult<Vec<BulkIdentifyRelationsResult>> {
        self.run_bulk(items, cancel, |chunk, cancel| async move {
            let req = self
                .http
                .post(self.url("/identify/bulk/relations"))
                .json(&BulkIdentifyRequest { items: chunk });
            let resp: BulkIdentifyRelationsResponse = self.send_json(req, &cancel).await?;
            Ok(resp.results)
        })
        .await
    }
}

fn identify_query(q: &GameFileMatchSearch) -> Vec<(&'static str, String)> {
    let mut pairs = vec![
        ("fileName", q.file_name.clone()),
        ("fileSize", q.file_size.to_string()),
    ];
    if let Some(v) = &q.md5 {
        pairs.push(("md5", v.clone()));
    }
    if let Some(v) = &q.sha1 {
        pairs.push(("sha1", v.clone()));
    }
    if let Some(v) = &q.sha256 {
        pairs.push(("sha256", v.clone()));
    }
    if let Some(v) = &q.crc {
        pairs.push(("crc", v.clone()));
    }
    pairs
}

// Per-chunk result index shaping. Bulk results carry an `index` local to the
// submitted chunk; re-stitching offsets it by the chunk's start.
trait BulkResult {
    fn index(&self) -> usize;
    fn set_index(&mut self, index: usize);
}

impl BulkResult for BulkIdentifyIdsResult {
    fn index(&self) -> usize {
        self.index
    }
    fn set_index(&mut self, index: usize) {
        self.index = index;
    }
}

impl BulkResult for BulkIdentifyRelationsResult {
    fn index(&self) -> usize {
        self.index
    }
    fn set_index(&mut self, index: usize) {
        self.index = index;
    }
}

impl PlaymatchClient {
    // Chunk, drive <=MAX_IN_FLIGHT chunks concurrently, assert each chunk's
    // result count, re-stitch into request order by chunk offset.
    async fn run_bulk<R, F, Fut>(
        &self,
        items: Vec<BulkIdentifyItem>,
        cancel: &CancelToken,
        run_chunk: F,
    ) -> DatResult<Vec<R>>
    where
        R: BulkResult,
        F: Fn(Vec<BulkIdentifyItem>, CancelToken) -> Fut,
        Fut: std::future::Future<Output = DatResult<Vec<R>>>,
    {
        let chunks = chunk_bulk_items(items)?;
        let mut offsets = Vec::with_capacity(chunks.len());
        let mut acc = 0usize;
        for chunk in &chunks {
            offsets.push(acc);
            acc += chunk.len();
        }
        let total = acc;

        let run_chunk = &run_chunk;
        let mut stream = stream::iter(chunks.into_iter().enumerate())
            .map(|(ci, chunk)| {
                let offset = offsets[ci];
                let cancel = cancel.clone();
                async move {
                    if cancel.is_cancelled() {
                        return Err(DatError::Cancelled);
                    }
                    let count = chunk.len();
                    let mut results = run_chunk(chunk, cancel).await?;
                    if results.len() != count {
                        return Err(DatError::BadResponse(format!(
                            "bulk chunk returned {} results for {} items",
                            results.len(),
                            count
                        )));
                    }
                    for r in results.iter_mut() {
                        r.set_index(r.index() + offset);
                    }
                    Ok(results)
                }
            })
            .buffer_unordered(MAX_IN_FLIGHT);

        let mut out: Vec<Option<R>> = (0..total).map(|_| None).collect();
        while let Some(chunk_result) = stream.next().await {
            for r in chunk_result? {
                let idx = r.index();
                if idx >= total {
                    return Err(DatError::BadResponse(format!(
                        "bulk result index {idx} out of range"
                    )));
                }
                out[idx] = Some(r);
            }
        }

        out.into_iter()
            .enumerate()
            .map(|(i, slot)| {
                slot.ok_or_else(|| {
                    DatError::BadResponse(format!("missing bulk result at index {i}"))
                })
            })
            .collect()
    }

    pub async fn games_bulk(
        &self,
        ids: Vec<String>,
        cancel: &CancelToken,
    ) -> DatResult<Vec<BulkGameByIdResult>> {
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(BULK_MAX_ITEMS) {
            if cancel.is_cancelled() {
                return Err(DatError::Cancelled);
            }
            let req = self
                .http
                .post(self.url("/games/bulk"))
                .json(&BulkIdsRequest {
                    ids: chunk.to_vec(),
                });
            let resp: BulkGamesByIdResponse = self.send_json(req, cancel).await?;
            out.extend(resp.results);
        }
        Ok(out)
    }

    // Follow nextCursor to the end. Continuation keys only on hasNextPage +
    // nextCursor, never on data emptiness. Hitting MAX_PAGES with more pages
    // remaining is a hard Truncated error, never a silent partial result.
    async fn paginate<T: DeserializeOwned>(
        &self,
        path: &str,
        base_query: &[(&str, String)],
        progress: Option<(&dyn ProgressReporter, &str)>,
        cancel: &CancelToken,
    ) -> DatResult<Vec<T>> {
        let mut out: Vec<T> = Vec::new();
        let mut cursor: Option<String> = None;
        if let Some((reporter, label)) = progress {
            reporter.set_phase(label);
        }

        for page_index in 0..MAX_PAGES {
            if cancel.is_cancelled() {
                return Err(DatError::Cancelled);
            }
            let mut query: Vec<(&str, String)> = base_query.to_vec();
            query.push(("limit", PAGE_LIMIT.to_string()));
            if let Some(c) = &cursor {
                query.push(("cursor", c.clone()));
            }

            let req = self.get_with_query(path, &query);
            let page: Page<T> = self.send_json(req, cancel).await?;
            out.extend(page.data);

            if let Some((reporter, _)) = progress {
                reporter.inc((page_index + 1) as u64);
            }

            if !page.pagination.has_next_page {
                return Ok(out);
            }
            cursor = Some(page.pagination.next_cursor.ok_or_else(|| {
                DatError::BadResponse("hasNextPage true but nextCursor absent".to_string())
            })?);
        }

        Err(DatError::Truncated(MAX_PAGES))
    }

    pub async fn list_dat_files(
        &self,
        filter: &DatFileFilter,
        cancel: &CancelToken,
    ) -> DatResult<Vec<DatFileSummary>> {
        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(v) = &filter.platform_id {
            query.push(("platformId", v.clone()));
        }
        if let Some(v) = &filter.signature_group_id {
            query.push(("signatureGroupId", v.clone()));
        }
        if let Some(v) = &filter.name {
            query.push(("name", v.clone()));
        }
        if let Some(v) = &filter.subset {
            query.push(("subset", v.clone()));
        }
        if let Some(v) = &filter.tag {
            query.push(("tag", v.clone()));
        }
        self.paginate("/dat-files", &query, None, cancel).await
    }

    pub async fn dat_file_games(
        &self,
        dat_file_id: &str,
        include_files: bool,
        progress: &dyn ProgressReporter,
        cancel: &CancelToken,
    ) -> DatResult<Vec<DatFileGame>> {
        let path = format!("/dat-files/{dat_file_id}/games");
        let query = vec![
            ("includeFiles", include_files.to_string()),
            ("currentOnly", "true".to_string()),
        ];
        self.paginate(&path, &query, Some((progress, "Fetching DAT")), cancel)
            .await
    }

    pub async fn platforms_search(
        &self,
        query: &str,
        cancel: &CancelToken,
    ) -> DatResult<Vec<PlatformMetadataResponse>> {
        let q = vec![("query", query.to_string())];
        self.paginate("/platforms/search", &q, None, cancel).await
    }

    pub async fn platforms(
        &self,
        cancel: &CancelToken,
    ) -> DatResult<Vec<PlatformMetadataResponse>> {
        self.paginate("/platforms", &[], None, cancel).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(n: usize) -> BulkIdentifyItem {
        BulkIdentifyItem {
            search: GameFileMatchSearch {
                file_name: format!("file-{n}.bin"),
                file_size: n as u64,
                md5: None,
                sha1: Some(format!("{n:040x}")),
                sha256: None,
                crc: None,
            },
            key: Some(format!("k{n}")),
        }
    }

    #[test]
    fn chunk_respects_item_cap() {
        let items: Vec<_> = (0..250).map(item).collect();
        let chunks = chunk_bulk_items(items).unwrap();
        assert!(chunks.iter().all(|c| c.len() <= BULK_MAX_ITEMS));
        let total: usize = chunks.iter().map(|c| c.len()).sum();
        assert_eq!(total, 250);
        // 250 items at 100 per chunk => 3 chunks.
        assert_eq!(chunks.len(), 3);
    }

    #[test]
    fn chunk_preserves_order() {
        let items: Vec<_> = (0..205).map(item).collect();
        let chunks = chunk_bulk_items(items).unwrap();
        let flat: Vec<String> = chunks
            .into_iter()
            .flatten()
            .map(|i| i.search.file_name)
            .collect();
        let expected: Vec<String> = (0..205).map(|n| format!("file-{n}.bin")).collect();
        assert_eq!(flat, expected);
    }

    #[test]
    fn chunk_respects_byte_cap() {
        // Large sha256 payloads push the byte cap before the item cap.
        let big: Vec<_> = (0..100)
            .map(|n| BulkIdentifyItem {
                search: GameFileMatchSearch {
                    file_name: "x".repeat(3000),
                    file_size: n,
                    md5: Some("a".repeat(32)),
                    sha1: Some("b".repeat(40)),
                    sha256: Some("c".repeat(64)),
                    crc: Some("dddddddd".to_string()),
                },
                key: Some(format!("k{n}")),
            })
            .collect();
        let chunks = chunk_bulk_items(big).unwrap();
        for chunk in &chunks {
            let body = serde_json::to_vec(&BulkIdentifyRequest {
                items: chunk.clone(),
            })
            .unwrap();
            assert!(
                body.len() <= BULK_MAX_BODY_BYTES,
                "chunk body {} exceeds cap {}",
                body.len(),
                BULK_MAX_BODY_BYTES
            );
            assert!(!chunk.is_empty());
        }
        assert!(chunks.len() > 1, "byte cap should force multiple chunks");
    }

    #[test]
    fn chunk_single_oversize_item_stands_alone() {
        // One item larger than the cap still forms a chunk rather than looping.
        let one = vec![BulkIdentifyItem {
            search: GameFileMatchSearch {
                file_name: "y".repeat(BULK_MAX_BODY_BYTES + 100),
                file_size: 1,
                md5: None,
                sha1: None,
                sha256: None,
                crc: None,
            },
            key: None,
        }];
        let chunks = chunk_bulk_items(one).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 1);
    }

    #[test]
    fn empty_input_yields_no_chunks() {
        assert!(chunk_bulk_items(Vec::new()).unwrap().is_empty());
    }

    #[test]
    fn backoff_is_capped() {
        for attempt in 0..10 {
            let d = backoff_delay(attempt);
            assert!(d <= Duration::from_millis(8_000 + 249));
        }
    }

    #[test]
    fn new_trims_trailing_slash() {
        let c = PlaymatchClient::new(Some("https://example.test/api/v2/"));
        assert_eq!(c.url("/platforms"), "https://example.test/api/v2/platforms");
    }

    #[tokio::test]
    async fn live_identify_smoke() {
        if std::env::var("ROM_CONVERTO_PLAYMATCH_LIVE").is_err() {
            return;
        }
        let client = PlaymatchClient::new(None);
        let cancel = CancelToken::new();
        let query = GameFileMatchSearch {
            file_name: "smoke.bin".to_string(),
            file_size: 1,
            md5: None,
            sha1: Some("da39a3ee5e6b4b0d3255bfef95601890afd80709".to_string()),
            sha256: None,
            crc: None,
        };
        let result = client.identify_ids(&query, &cancel).await.unwrap();
        // Any of the wire variants is acceptable; the point is a decodable 200.
        let _ = result.game_match_type;
    }
}
