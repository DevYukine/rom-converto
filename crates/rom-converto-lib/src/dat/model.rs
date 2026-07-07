use crate::util::FileDigests;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GameMatchType {
    #[serde(rename = "SHA256")]
    Sha256,
    #[serde(rename = "SHA1")]
    Sha1,
    #[serde(rename = "MD5")]
    Md5,
    #[serde(rename = "CRC")]
    Crc,
    FileNameAndSize,
    NoMatch,
}

impl GameMatchType {
    pub fn is_hash_verified(self) -> bool {
        matches!(
            self,
            GameMatchType::Sha256 | GameMatchType::Sha1 | GameMatchType::Md5 | GameMatchType::Crc
        )
    }
}

// ---- identify request (query params for GET, body items for bulk) ----
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameFileMatchSearch {
    pub file_name: String,
    pub file_size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crc: Option<String>,
}

impl GameFileMatchSearch {
    pub fn from_digests(file_name: &str, d: &FileDigests) -> Self {
        Self {
            file_name: file_name.to_string(),
            file_size: d.size_bytes,
            md5: d.md5.clone(),
            sha1: d.sha1.clone(),
            sha256: d.sha256.clone(),
            crc: d.crc32.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BulkIdentifyItem {
    #[serde(flatten)]
    pub search: GameFileMatchSearch,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BulkIdentifyRequest {
    pub items: Vec<BulkIdentifyItem>,
}

// ---- identify responses ----
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameMetadataMatchResult {
    pub game_match_type: GameMatchType,
    pub id: Option<String>,
    #[serde(default)]
    pub external_metadata: Vec<ExternalMetadata>,
}

// providerName is the spec MetadataProvider enum and matchType is
// MetadataMatchType (Automatic | Failed | Manual | None). Both are kept as
// String so new providers never break deserialization. Consumers that print
// external ids filter to match_type "Automatic" or "Manual" AND
// provider_id.is_some().
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalMetadata {
    pub provider_name: String,
    pub match_type: String,
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BulkItemStatus {
    Ok,
    Invalid,
    Error,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BulkItemError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub field: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkIdentifyIdsResult {
    pub index: usize,
    #[serde(default)]
    pub key: Option<String>,
    pub status: BulkItemStatus,
    #[serde(rename = "match", default)]
    pub matched: Option<GameMetadataMatchResult>,
    #[serde(default)]
    pub error: Option<BulkItemError>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkIdentifySummary {
    pub total: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub matched: u64,
    pub unmatched: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BulkIdentifyIdsResponse {
    pub summary: BulkIdentifySummary,
    pub results: Vec<BulkIdentifyIdsResult>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkIdentifyRelationsResult {
    pub index: usize,
    #[serde(default)]
    pub key: Option<String>,
    pub status: BulkItemStatus,
    #[serde(rename = "match", default)]
    pub matched: Option<GameAndRelationMatchResult>,
    #[serde(default)]
    pub error: Option<BulkItemError>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BulkIdentifyRelationsResponse {
    pub summary: BulkIdentifySummary,
    pub results: Vec<BulkIdentifyRelationsResult>,
}

// ---- relations result (spec: GameAndRelationMatchResultV2) ----
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameAndRelationMatchResult {
    pub game_match_type: GameMatchType,
    #[serde(default)]
    pub game: Option<PlaymatchGame>,
    #[serde(default)]
    pub platform: Option<PlaymatchPlatform>,
    #[serde(default)]
    pub company: Option<PlaymatchCompany>,
    #[serde(default)]
    pub signature_group: Option<PlaymatchSignatureGroup>,
    #[serde(default)]
    pub dat_file: Option<PlaymatchDatFile>,
    #[serde(default)]
    pub dat_file_import: Option<PlaymatchDatFileImport>,
    #[serde(default)]
    pub game_files: Vec<PlaymatchGameFile>,
    #[serde(default)]
    pub external_metadata: Vec<ExternalMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaymatchCompany {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaymatchGame {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub clone_of: Option<String>,
    pub current_in_latest_dat: bool,
    #[serde(default)]
    pub last_seen_dat_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaymatchGameFile {
    pub id: String,
    pub game_id: String,
    pub file_name: String,
    #[serde(default)]
    pub file_size_in_bytes: Option<u64>,
    #[serde(default)]
    pub crc: Option<String>,
    #[serde(default)]
    pub md5: Option<String>,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,
    pub current_in_latest_dat: bool,
    #[serde(default)]
    pub last_seen_dat_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaymatchPlatform {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaymatchSignatureGroup {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaymatchDatFile {
    pub id: String,
    pub name: String,
    pub platform_id: String,
    pub signature_group_id: String,
    pub current_version: String,
    #[serde(default)]
    pub subset: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaymatchDatFileImport {
    pub id: String,
    pub dat_file_id: String,
    pub name: String,
    pub version: String,
    pub imported_at: String,
}

// ---- games bulk (POST /games/bulk, BulkIdsRequest) ----
#[derive(Debug, Clone, Serialize)]
pub struct BulkIdsRequest {
    pub ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameMetadataResponse {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub clone_of: Option<String>,
}

// BulkByIdStatusV2 = ok | notFound
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BulkByIdStatus {
    Ok,
    NotFound,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkGameByIdResult {
    pub id: String,
    pub status: BulkByIdStatus,
    #[serde(default)]
    pub data: Option<GameMetadataResponse>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BulkGamesByIdResponse {
    pub results: Vec<BulkGameByIdResult>,
}

// ---- dat-files enumeration ----
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatFileSummary {
    pub id: String,
    pub name: String,
    pub signature_group: NamedRef,
    pub platform: NamedRef,
    #[serde(default)]
    pub company: Option<NamedRef>,
    pub current_version: String,
    #[serde(default)]
    pub latest_dat_file_import: Option<LatestDatFileImport>,
    #[serde(default)]
    pub subset: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatestDatFileImport {
    pub id: String,
    pub version: String,
    pub imported_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatFileGame {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub clone_of: Option<String>,
    pub current_in_latest_dat: bool,
    #[serde(default)]
    pub files: Option<Vec<PlaymatchGameFile>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformMetadataResponse {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub company_name: Option<String>,
}

// ---- pagination envelope (spec: PageOf<T> + PageMeta) ----
#[derive(Debug, Clone, Deserialize)]
pub struct Page<T> {
    pub data: Vec<T>,
    pub pagination: PageMeta,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageMeta {
    pub limit: u64,
    pub has_next_page: bool,
    pub has_previous_page: bool,
    #[serde(default)]
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub total_items: Option<u64>,
}

// ---- error body (spec: V2ErrorBody) ----
#[derive(Debug, Clone, Deserialize)]
pub struct ApiErrorBody {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub limit: Option<u64>,
    #[serde(default)]
    pub received: Option<u64>,
    #[serde(default)]
    pub restart: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Single identify 200 with extra server-only fields the structs ignore.
    const IDENTIFY_SINGLE: &str = r#"{
        "gameMatchType": "SHA256",
        "id": "3ba8ee53-678f-4dcb-b88c-379424c30a88",
        "createdAt": "2026-01-02T00:00:00Z",
        "externalMetadata": [
            { "providerName": "IGDB", "providerId": "1020", "matchType": "Automatic", "comment": null },
            { "providerName": "RetroAchievements", "providerId": null, "matchType": "None" }
        ]
    }"#;

    const BULK_IDS: &str = r#"{
        "summary": { "total": 2, "succeeded": 2, "failed": 0, "matched": 1, "unmatched": 1 },
        "results": [
            { "index": 0, "key": "a", "status": "ok",
              "match": { "gameMatchType": "SHA256", "id": "id-1", "externalMetadata": [] } },
            { "index": 1, "key": "b", "status": "ok",
              "match": { "gameMatchType": "NoMatch", "id": null, "externalMetadata": [] } }
        ]
    }"#;

    const RELATIONS: &str = r#"{
        "gameMatchType": "SHA1",
        "game": { "id": "g-1", "name": "Some Game (USA)", "currentInLatestDat": true },
      "platform": { "id": "p-1", "name": "Some Platform" },
      "company": { "id": "c-1", "name": "Some Company" },
      "signatureGroup": { "id": "s-1", "name": "Some Group" },
      "datFile": {
        "id": "d-1",
        "name": "Some DAT",
        "platformId": "p-1",
        "signatureGroupId": "s-1",
        "currentVersion": "2026-06-01",
        "subset": null,
        "tags": []
      },
      "datFileImport": { "id": "i-1", "datFileId": "d-1", "name": "imp", "version": "2026-06-01", "importedAt": "2026-06-01T00:00:00Z" },
        "gameFiles": [
            { "id": "f-1", "gameId": "g-1", "fileName": "Some Game (USA) (Track 01).bin",
              "fileSizeInBytes": 37633632, "sha1": "aaaa", "crc": "1234abcd", "currentInLatestDat": true },
            { "id": "f-2", "gameId": "g-1", "fileName": "Some Game (USA) (Track 02).bin",
              "fileSizeInBytes": 1234, "sha1": "bbbb", "currentInLatestDat": true }
        ],
        "externalMetadata": []
    }"#;

    #[test]
    fn parse_single_identify_200() {
        let r: GameMetadataMatchResult = serde_json::from_str(IDENTIFY_SINGLE).unwrap();
        assert_eq!(r.game_match_type, GameMatchType::Sha256);
        assert!(r.game_match_type.is_hash_verified());
        assert_eq!(
            r.id.as_deref(),
            Some("3ba8ee53-678f-4dcb-b88c-379424c30a88")
        );
        assert_eq!(r.external_metadata.len(), 2);
        assert_eq!(r.external_metadata[0].provider_name, "IGDB");
        assert_eq!(r.external_metadata[0].provider_id.as_deref(), Some("1020"));
        assert!(r.external_metadata[1].provider_id.is_none());
    }

    #[test]
    fn parse_bulk_ids_ok_and_nomatch() {
        let r: BulkIdentifyIdsResponse = serde_json::from_str(BULK_IDS).unwrap();
        assert_eq!(r.summary.total, 2);
        assert_eq!(r.results.len(), 2);
        assert_eq!(r.results[0].index, 0);
        assert_eq!(r.results[0].status, BulkItemStatus::Ok);
        assert_eq!(
            r.results[0].matched.as_ref().unwrap().game_match_type,
            GameMatchType::Sha256
        );
        let second = &r.results[1];
        assert_eq!(second.key.as_deref(), Some("b"));
        assert_eq!(
            second.matched.as_ref().unwrap().game_match_type,
            GameMatchType::NoMatch
        );
        assert!(
            !second
                .matched
                .as_ref()
                .unwrap()
                .game_match_type
                .is_hash_verified()
        );
    }

    #[test]
    fn parse_relations_with_game_files() {
        let r: GameAndRelationMatchResult = serde_json::from_str(RELATIONS).unwrap();
        assert_eq!(r.game_match_type, GameMatchType::Sha1);
        assert_eq!(r.game.as_ref().unwrap().name, "Some Game (USA)");
        assert_eq!(r.company.as_ref().unwrap().name, "Some Company");
        assert_eq!(r.signature_group.as_ref().unwrap().name, "Some Group");
        assert_eq!(r.dat_file.as_ref().unwrap().name, "Some DAT");
        assert_eq!(r.dat_file.as_ref().unwrap().id, "d-1");
        assert_eq!(r.dat_file_import.as_ref().unwrap().version, "2026-06-01");
        assert_eq!(r.game_files.len(), 2);
        assert_eq!(r.game_files[0].file_size_in_bytes, Some(37633632));
        assert_eq!(r.game_files[0].sha1.as_deref(), Some("aaaa"));
        assert!(r.game_files[1].sha256.is_none());
    }

    #[test]
    fn game_match_type_roundtrips_wire_names() {
        assert_eq!(
            serde_json::to_string(&GameMatchType::Sha256).unwrap(),
            "\"SHA256\""
        );
        assert_eq!(
            serde_json::to_string(&GameMatchType::FileNameAndSize).unwrap(),
            "\"FileNameAndSize\""
        );
        let crc: GameMatchType = serde_json::from_str("\"CRC\"").unwrap();
        assert_eq!(crc, GameMatchType::Crc);
    }

    #[test]
    fn bulk_item_flattens_search_and_key() {
        let item = BulkIdentifyItem {
            search: GameFileMatchSearch {
                file_name: "x.bin".to_string(),
                file_size: 42,
                md5: None,
                sha1: Some("dd".to_string()),
                sha256: None,
                crc: None,
            },
            key: Some("k0".to_string()),
        };
        let v: serde_json::Value = serde_json::to_value(&item).unwrap();
        assert_eq!(v["fileName"], "x.bin");
        assert_eq!(v["fileSize"], 42);
        assert_eq!(v["sha1"], "dd");
        assert_eq!(v["key"], "k0");
        assert!(v.get("md5").is_none());
    }

    #[test]
    fn from_digests_maps_crc_to_crc_field() {
        let d = FileDigests {
            crc32: Some("1234abcd".to_string()),
            sha1: Some("aa".to_string()),
            md5: None,
            sha256: None,
            size_bytes: 100,
        };
        let s = GameFileMatchSearch::from_digests("g.iso", &d);
        assert_eq!(s.file_name, "g.iso");
        assert_eq!(s.file_size, 100);
        assert_eq!(s.crc.as_deref(), Some("1234abcd"));
        assert_eq!(s.sha1.as_deref(), Some("aa"));
        assert!(s.md5.is_none());
    }

    #[test]
    fn parse_page_meta_and_error_body() {
        let page: Page<PlatformMetadataResponse> = serde_json::from_str(
            r#"{ "data": [ { "id": "p1", "name": "Plat" } ],
                "pagination": { "limit": 50, "hasNextPage": true, "hasPreviousPage": false, "nextCursor": "abc" } }"#,
        )
        .unwrap();
        assert_eq!(page.data.len(), 1);
        assert!(page.pagination.has_next_page);
        assert_eq!(page.pagination.next_cursor.as_deref(), Some("abc"));

        let missing_message: Result<ApiErrorBody, _> =
            serde_json::from_str(r#"{ "code": "bulk_too_large", "limit": 100, "received": 250 }"#);
        assert!(
            missing_message.is_err(),
            "message is required by the struct"
        );

        let err: ApiErrorBody = serde_json::from_str(
            r#"{ "code": "cursor_filter_mismatch", "message": "restart", "restart": true }"#,
        )
        .unwrap();
        assert_eq!(err.code, "cursor_filter_mismatch");
        assert_eq!(err.restart, Some(true));
    }
}
