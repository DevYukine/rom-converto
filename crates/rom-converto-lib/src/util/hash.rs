//! File hashing for the `hash` command and verify pipelines: CRC32, SHA-1,
//! MD5, and SHA-256, computed in a single streaming pass over each file.

use crate::util::{CancelToken, ProgressReporter};
use crc::{CRC_32_ISO_HDLC, Crc};
use sha2::Digest as _;
use std::io::Read;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashAlgo {
    Crc32,
    Sha1,
    Md5,
    Sha256,
}

impl HashAlgo {
    pub fn label(self) -> &'static str {
        match self {
            HashAlgo::Crc32 => "crc32",
            HashAlgo::Sha1 => "sha1",
            HashAlgo::Md5 => "md5",
            HashAlgo::Sha256 => "sha256",
        }
    }

    /// Ascending compute-cost order used for checksum-tier escalation:
    /// crc32 is cheapest to compute and compare, sha256 the priciest.
    pub fn cost_rank(self) -> u8 {
        match self {
            HashAlgo::Crc32 => 0,
            HashAlgo::Md5 => 1,
            HashAlgo::Sha1 => 2,
            HashAlgo::Sha256 => 3,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileDigests {
    pub crc32: Option<String>,
    pub sha1: Option<String>,
    pub md5: Option<String>,
    pub sha256: Option<String>,
    pub size_bytes: u64,
}

impl FileDigests {
    pub fn value(&self, algo: HashAlgo) -> Option<&str> {
        match algo {
            HashAlgo::Crc32 => self.crc32.as_deref(),
            HashAlgo::Sha1 => self.sha1.as_deref(),
            HashAlgo::Md5 => self.md5.as_deref(),
            HashAlgo::Sha256 => self.sha256.as_deref(),
        }
    }
}

/// A set of streaming hashers, one per requested algorithm, fed the
/// same byte chunks in a single pass. Each selected hasher is an
/// `Option` so unrequested algorithms allocate no state and cost
/// nothing in the update loop.
pub struct MultiHasher {
    crc: Option<crc::Digest<'static, u32>>,
    sha1: Option<sha1::Sha1>,
    md5: Option<md_5::Md5>,
    sha256: Option<sha2::Sha256>,
}

static CRC32_ISO_HDLC: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);

impl MultiHasher {
    pub fn new(algos: &[HashAlgo]) -> Self {
        let want = |a: HashAlgo| algos.contains(&a);
        Self {
            crc: want(HashAlgo::Crc32).then(|| CRC32_ISO_HDLC.digest()),
            sha1: want(HashAlgo::Sha1).then(sha1::Sha1::new),
            md5: want(HashAlgo::Md5).then(md_5::Md5::new),
            sha256: want(HashAlgo::Sha256).then(sha2::Sha256::new),
        }
    }

    pub fn update(&mut self, chunk: &[u8]) {
        if let Some(d) = self.crc.as_mut() {
            d.update(chunk);
        }
        if let Some(h) = self.sha1.as_mut() {
            h.update(chunk);
        }
        if let Some(h) = self.md5.as_mut() {
            h.update(chunk);
        }
        if let Some(h) = self.sha256.as_mut() {
            h.update(chunk);
        }
    }

    pub fn finalize(self, size_bytes: u64) -> FileDigests {
        FileDigests {
            crc32: self.crc.map(|d| format!("{:08x}", d.finalize())),
            sha1: self.sha1.map(|h| hex::encode(h.finalize())),
            md5: self.md5.map(|h| hex::encode(h.finalize())),
            sha256: self.sha256.map(|h| hex::encode(h.finalize())),
            size_bytes,
        }
    }
}

/// Parse a comma-separated `--algo` value into a deduplicated set of
/// algorithms, normalized to a stable column order (crc32, sha1, md5,
/// sha256) regardless of how the user ordered them.
pub fn parse_algos(spec: &str) -> Result<Vec<HashAlgo>, String> {
    let mut crc32 = false;
    let mut sha1 = false;
    let mut md5 = false;
    let mut sha256 = false;
    let mut seen_any = false;
    for token in spec.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        seen_any = true;
        match token.to_ascii_lowercase().as_str() {
            "crc32" => crc32 = true,
            "sha1" => sha1 = true,
            "md5" => md5 = true,
            "sha256" => sha256 = true,
            other => {
                return Err(format!(
                    "unknown algorithm: {other} (expected crc32, sha1, md5, or sha256)"
                ));
            }
        }
    }
    if !seen_any {
        return Err("no algorithms given (expected crc32, sha1, md5, or sha256)".to_string());
    }
    let mut algos = Vec::new();
    if crc32 {
        algos.push(HashAlgo::Crc32);
    }
    if sha1 {
        algos.push(HashAlgo::Sha1);
    }
    if md5 {
        algos.push(HashAlgo::Md5);
    }
    if sha256 {
        algos.push(HashAlgo::Sha256);
    }
    Ok(algos)
}

/// Parse a single checksum-tier bound (one of `crc32`, `md5`, `sha1`,
/// `sha256`) used by `--input-checksum-min`/`--input-checksum-max`.
pub fn parse_checksum_bound(spec: &str) -> Result<HashAlgo, String> {
    match spec.trim().to_ascii_lowercase().as_str() {
        "crc32" => Ok(HashAlgo::Crc32),
        "sha1" => Ok(HashAlgo::Sha1),
        "md5" => Ok(HashAlgo::Md5),
        "sha256" => Ok(HashAlgo::Sha256),
        other => Err(format!(
            "unknown checksum tier: {other} (expected crc32, md5, sha1, or sha256)"
        )),
    }
}

/// Floor/ceiling for tiered checksum escalation: `min` is always computed
/// up front for identification, `max` bounds how far escalation may go when
/// the floor tier alone does not resolve a confident match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChecksumBounds {
    pub min: HashAlgo,
    pub max: HashAlgo,
}

impl ChecksumBounds {
    pub fn new(min: HashAlgo, max: HashAlgo) -> Result<Self, String> {
        if min.cost_rank() > max.cost_rank() {
            return Err(format!(
                "--input-checksum-min ({}) must be no stronger than --input-checksum-max ({})",
                min.label(),
                max.label()
            ));
        }
        Ok(Self { min, max })
    }

    /// Split `requested` into a cheap floor tier always computed first and
    /// an escalation tier computed only when the floor tier does not
    /// resolve a confident match. Both preserve `requested`'s original
    /// order. If nothing in `requested` is at or below `min`'s rank
    /// (e.g. the caller asked for `sha256` only with the default
    /// `crc32` floor), tiering has no benefit, so the whole set is
    /// returned as the floor and escalation is left empty.
    pub fn split(&self, requested: &[HashAlgo]) -> (Vec<HashAlgo>, Vec<HashAlgo>) {
        let floor: Vec<HashAlgo> = requested
            .iter()
            .copied()
            .filter(|a| a.cost_rank() <= self.min.cost_rank())
            .collect();
        if floor.is_empty() {
            return (requested.to_vec(), Vec::new());
        }
        let escalation: Vec<HashAlgo> = requested
            .iter()
            .copied()
            .filter(|a| a.cost_rank() <= self.max.cost_rank() && !floor.contains(a))
            .collect();
        (floor, escalation)
    }

    /// Reject a `--algo` selection that asks for a digest stronger than
    /// `max`: `split` would otherwise drop it from both the floor and the
    /// escalation tier and it would never be computed.
    pub fn validate_requested(&self, requested: &[HashAlgo]) -> Result<(), String> {
        if let Some(bad) = requested
            .iter()
            .find(|a| a.cost_rank() > self.max.cost_rank())
        {
            return Err(format!(
                "--algo {} is stronger than --input-checksum-max ({}); raise the max or drop it from --algo",
                bad.label(),
                self.max.label()
            ));
        }
        Ok(())
    }
}

/// Compute every requested digest for `path` in a single streaming pass.
/// The file is read in fixed-size chunks and every selected hasher is fed
/// each chunk, so memory stays constant no matter how large the file is.
pub fn hash_file_cancellable(
    path: &Path,
    algos: &[HashAlgo],
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> std::io::Result<FileDigests> {
    let mut file = std::fs::File::open(path)?;
    let size_bytes = file.metadata()?.len();
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    progress.start(size_bytes, &format!("Hashing {name}"));

    let mut hasher = MultiHasher::new(algos);

    let mut buf = vec![0u8; 4 * 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        if cancel.is_cancelled() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "operation cancelled",
            ));
        }
        hasher.update(&buf[..n]);
        progress.inc(n as u64);
    }
    progress.finish();

    Ok(hasher.finalize(size_bytes))
}

pub fn hash_file(
    path: &Path,
    algos: &[HashAlgo],
    progress: &dyn ProgressReporter,
) -> std::io::Result<FileDigests> {
    hash_file_cancellable(path, algos, progress, &CancelToken::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::NoProgress;

    const ALL: &[HashAlgo] = &[
        HashAlgo::Crc32,
        HashAlgo::Sha1,
        HashAlgo::Md5,
        HashAlgo::Sha256,
    ];

    fn hash_bytes(data: &[u8], algos: &[HashAlgo]) -> FileDigests {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.bin");
        std::fs::write(&path, data).unwrap();
        hash_file(&path, algos, &NoProgress).unwrap()
    }

    #[test]
    fn empty_input_known_answers() {
        let d = hash_bytes(b"", ALL);
        assert_eq!(d.crc32.as_deref(), Some("00000000"));
        assert_eq!(
            d.sha1.as_deref(),
            Some("da39a3ee5e6b4b0d3255bfef95601890afd80709")
        );
        assert_eq!(d.md5.as_deref(), Some("d41d8cd98f00b204e9800998ecf8427e"));
        assert_eq!(
            d.sha256.as_deref(),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        assert_eq!(d.size_bytes, 0);
    }

    #[test]
    fn abc_known_answers() {
        let d = hash_bytes(b"abc", ALL);
        assert_eq!(d.crc32.as_deref(), Some("352441c2"));
        assert_eq!(
            d.sha1.as_deref(),
            Some("a9993e364706816aba3e25717850c26c9cd0d89d")
        );
        assert_eq!(d.md5.as_deref(), Some("900150983cd24fb0d6963f7d28e17f72"));
        assert_eq!(
            d.sha256.as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        assert_eq!(d.size_bytes, 3);
    }

    #[test]
    fn single_pass_multi_hash_matches_individual() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let all = hash_bytes(&data, ALL);
        let crc = hash_bytes(&data, &[HashAlgo::Crc32]);
        let sha1 = hash_bytes(&data, &[HashAlgo::Sha1]);
        let md5 = hash_bytes(&data, &[HashAlgo::Md5]);
        let sha256 = hash_bytes(&data, &[HashAlgo::Sha256]);
        assert_eq!(all.crc32, crc.crc32);
        assert_eq!(all.sha1, sha1.sha1);
        assert_eq!(all.md5, md5.md5);
        assert_eq!(all.sha256, sha256.sha256);
    }

    #[test]
    fn requested_algos_only() {
        let d = hash_bytes(b"abc", &[HashAlgo::Sha1]);
        assert!(d.sha1.is_some());
        assert!(d.crc32.is_none());
        assert!(d.md5.is_none());
        assert!(d.sha256.is_none());
    }

    #[test]
    fn hex_widths() {
        let d = hash_bytes(b"abc", ALL);
        assert_eq!(d.crc32.as_ref().unwrap().len(), 8);
        assert_eq!(d.sha1.as_ref().unwrap().len(), 40);
        assert_eq!(d.md5.as_ref().unwrap().len(), 32);
        assert_eq!(d.sha256.as_ref().unwrap().len(), 64);
        for value in [d.crc32, d.sha1, d.md5, d.sha256].into_iter().flatten() {
            assert!(
                value
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            );
        }
    }

    #[test]
    fn cancelled_token_stops_hashing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.bin");
        std::fs::write(&path, vec![0u8; 12 * 1024 * 1024]).unwrap();

        let token = CancelToken::new();
        token.cancel();

        let err =
            hash_file_cancellable(&path, &[HashAlgo::Sha256], &NoProgress, &token).unwrap_err();
        assert_eq!(err.to_string(), "operation cancelled");
    }

    #[test]
    fn uncancelled_token_completes() {
        let data = vec![0u8; 12 * 1024 * 1024];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.bin");
        std::fs::write(&path, &data).unwrap();

        let token = CancelToken::new();
        let digests =
            hash_file_cancellable(&path, &[HashAlgo::Sha256], &NoProgress, &token).unwrap();
        assert_eq!(digests.size_bytes, data.len() as u64);
        assert_eq!(
            digests.sha256,
            hash_bytes(&data, &[HashAlgo::Sha256]).sha256
        );
    }

    #[test]
    fn parse_algos_normalizes_order() {
        assert_eq!(
            parse_algos("sha1,crc32").unwrap(),
            vec![HashAlgo::Crc32, HashAlgo::Sha1]
        );
    }

    #[test]
    fn parse_algos_is_case_insensitive() {
        assert_eq!(parse_algos("SHA256").unwrap(), vec![HashAlgo::Sha256]);
    }

    #[test]
    fn parse_algos_dedupes() {
        assert_eq!(parse_algos("crc32,crc32").unwrap(), vec![HashAlgo::Crc32]);
    }

    #[test]
    fn parse_algos_rejects_unknown() {
        assert!(parse_algos("crc32,sha3").is_err());
    }

    #[test]
    fn parse_algos_rejects_empty() {
        assert!(parse_algos("").is_err());
        assert!(parse_algos(" , ").is_err());
    }

    #[test]
    fn cost_rank_orders_crc32_cheapest_and_sha256_priciest() {
        assert!(HashAlgo::Crc32.cost_rank() < HashAlgo::Md5.cost_rank());
        assert!(HashAlgo::Md5.cost_rank() < HashAlgo::Sha1.cost_rank());
        assert!(HashAlgo::Sha1.cost_rank() < HashAlgo::Sha256.cost_rank());
    }

    #[test]
    fn parse_checksum_bound_accepts_known_algos() {
        assert_eq!(parse_checksum_bound("crc32"), Ok(HashAlgo::Crc32));
        assert_eq!(parse_checksum_bound("SHA256"), Ok(HashAlgo::Sha256));
        assert_eq!(parse_checksum_bound(" sha1 "), Ok(HashAlgo::Sha1));
        assert_eq!(parse_checksum_bound("md5"), Ok(HashAlgo::Md5));
    }

    #[test]
    fn parse_checksum_bound_rejects_unknown() {
        assert!(parse_checksum_bound("sha3").is_err());
        assert!(parse_checksum_bound("").is_err());
    }

    #[test]
    fn checksum_bounds_rejects_inverted_range() {
        assert!(ChecksumBounds::new(HashAlgo::Sha256, HashAlgo::Crc32).is_err());
        assert!(ChecksumBounds::new(HashAlgo::Crc32, HashAlgo::Crc32).is_ok());
    }

    #[test]
    fn checksum_bounds_split_default_defers_sha1_to_escalation() {
        let bounds = ChecksumBounds::new(HashAlgo::Crc32, HashAlgo::Sha256).unwrap();
        let (floor, escalation) = bounds.split(&[HashAlgo::Crc32, HashAlgo::Sha1]);
        assert_eq!(floor, vec![HashAlgo::Crc32]);
        assert_eq!(escalation, vec![HashAlgo::Sha1]);
    }

    #[test]
    fn checksum_bounds_split_max_caps_escalation() {
        let bounds = ChecksumBounds::new(HashAlgo::Crc32, HashAlgo::Md5).unwrap();
        let (floor, escalation) = bounds.split(ALL);
        assert_eq!(floor, vec![HashAlgo::Crc32]);
        assert_eq!(escalation, vec![HashAlgo::Md5]);
    }

    #[test]
    fn checksum_bounds_split_min_raises_floor() {
        let bounds = ChecksumBounds::new(HashAlgo::Sha1, HashAlgo::Sha256).unwrap();
        let (floor, escalation) = bounds.split(ALL);
        assert_eq!(floor, vec![HashAlgo::Crc32, HashAlgo::Sha1, HashAlgo::Md5]);
        assert_eq!(escalation, vec![HashAlgo::Sha256]);
    }

    #[test]
    fn checksum_bounds_validate_requested_rejects_algo_above_max() {
        let bounds = ChecksumBounds::new(HashAlgo::Crc32, HashAlgo::Md5).unwrap();
        assert!(
            bounds
                .validate_requested(&[HashAlgo::Crc32, HashAlgo::Sha1])
                .is_err()
        );
        assert!(
            bounds
                .validate_requested(&[HashAlgo::Crc32, HashAlgo::Md5])
                .is_ok()
        );
    }

    #[test]
    fn checksum_bounds_split_falls_back_when_floor_empty() {
        // Requested set has nothing at or below the default crc32 floor:
        // no benefit to tiering, whole set computed in one pass.
        let bounds = ChecksumBounds::new(HashAlgo::Crc32, HashAlgo::Sha256).unwrap();
        let (floor, escalation) = bounds.split(&[HashAlgo::Sha256]);
        assert_eq!(floor, vec![HashAlgo::Sha256]);
        assert!(escalation.is_empty());
    }

    fn multi_hash(data: &[u8], algos: &[HashAlgo]) -> FileDigests {
        let mut h = MultiHasher::new(algos);
        h.update(data);
        h.finalize(data.len() as u64)
    }

    #[test]
    fn multi_hasher_empty_known_answers() {
        let d = multi_hash(b"", ALL);
        assert_eq!(d.crc32.as_deref(), Some("00000000"));
        assert_eq!(
            d.sha1.as_deref(),
            Some("da39a3ee5e6b4b0d3255bfef95601890afd80709")
        );
        assert_eq!(d.md5.as_deref(), Some("d41d8cd98f00b204e9800998ecf8427e"));
        assert_eq!(
            d.sha256.as_deref(),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        assert_eq!(d.size_bytes, 0);
    }

    #[test]
    fn multi_hasher_abc_known_answers() {
        let d = multi_hash(b"abc", ALL);
        assert_eq!(d.crc32.as_deref(), Some("352441c2"));
        assert_eq!(
            d.sha1.as_deref(),
            Some("a9993e364706816aba3e25717850c26c9cd0d89d")
        );
        assert_eq!(d.md5.as_deref(), Some("900150983cd24fb0d6963f7d28e17f72"));
        assert_eq!(
            d.sha256.as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        assert_eq!(d.size_bytes, 3);
    }

    #[test]
    fn multi_hasher_chunked_matches_single_update() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let one = multi_hash(&data, ALL);
        let mut split = MultiHasher::new(ALL);
        for chunk in data.chunks(97) {
            split.update(chunk);
        }
        let split = split.finalize(data.len() as u64);
        assert_eq!(one, split);
    }

    #[test]
    fn multi_hasher_matches_hash_file() {
        let data: Vec<u8> = (0..50_000u32).map(|i| (i % 253) as u8).collect();
        let via_file = hash_bytes(&data, ALL);
        let via_multi = multi_hash(&data, ALL);
        assert_eq!(via_file, via_multi);
    }

    #[test]
    fn multi_hasher_respects_requested_algos() {
        let d = multi_hash(b"abc", &[HashAlgo::Sha256]);
        assert!(d.sha256.is_some());
        assert!(d.crc32.is_none());
        assert!(d.sha1.is_none());
        assert!(d.md5.is_none());
    }
}
