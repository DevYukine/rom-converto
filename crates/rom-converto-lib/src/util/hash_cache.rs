//! Persistent content-hash and verify cache. Entries are keyed by the
//! canonicalized path and fingerprinted by (size, mtime); a size or mtime
//! change invalidates the entry automatically. Repeat `hash -R`, `dat verify`,
//! and `--on-conflict overwrite-invalid` runs over an unchanged collection read
//! from the cache instead of re-reading terabytes off disk.
//!
//! The store lives at `<config dir>/rom-converto/hash-cache.json.gz`, next to
//! the user config, as gzipped JSON behind a versioned envelope. A corrupt or
//! unknown-version file loads as empty. Saves are atomic (temp file plus
//! rename); a save failure logs a warning and never fails the run.

use crate::util::hash::{FileDigests, HashAlgo};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

const CACHE_VERSION: u32 = 1;
const CACHE_SUBPATH: &str = "rom-converto/hash-cache.json.gz";

/// Files whose mtime is within this window of now are not stored, to dodge
/// coarse filesystem timestamps on a file that may still be written.
const RECENT_MTIME_GUARD_SECS: i64 = 2;

type Fingerprint = (u64, i64, u32);
type MemberKey = (u64, i64, u32);

/// One track's cached digest set, mirroring the DAT layer's per-track record.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct CachedTrack {
    pub number: u32,
    pub kind: String,
    pub digests: FileDigests,
}

/// A cue set's cached digests: the whole concatenated image plus each track.
pub struct CueDigests {
    pub whole: FileDigests,
    pub tracks: Vec<CachedTrack>,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct CacheEntry {
    size: u64,
    mtime_secs: i64,
    mtime_nanos: u32,
    /// Digests of the raw file bytes (the `hash` command).
    #[serde(default)]
    raw: FileDigests,
    /// Digests of the decoded inner stream (`dat verify` of a container).
    #[serde(default)]
    decoded: FileDigests,
    /// Whole-image and per-track digests for a cue set keyed on this cue.
    #[serde(default)]
    whole: Option<FileDigests>,
    #[serde(default)]
    tracks: Option<Vec<CachedTrack>>,
    /// Member bin fingerprints so a changed bin invalidates the whole set.
    #[serde(default)]
    members: Option<Vec<MemberKey>>,
    /// Format label -> last verify verdict (only `true` is ever stored).
    #[serde(default)]
    verify_valid: HashMap<String, bool>,
}

#[derive(serde::Serialize)]
struct EnvelopeRef<'a> {
    version: u32,
    entries: &'a HashMap<String, CacheEntry>,
}

#[derive(serde::Deserialize)]
struct Envelope {
    version: u32,
    entries: HashMap<String, CacheEntry>,
}

struct State {
    entries: HashMap<String, CacheEntry>,
    /// On `--rebuild-cache`, the previously persisted entries this run does not
    /// touch. Never looked up (so the run recomputes), merged back under this
    /// run's fresh entries at save time so a rebuild scoped to a subdirectory,
    /// or one that stores nothing, does not discard the rest of the collection.
    preserved: HashMap<String, CacheEntry>,
    dirty: bool,
}

/// In-process shared cache. Access is serialized behind a `Mutex`; the batch
/// runners loop sequentially, and worker-pool parallelism is per-file chunk
/// work, so a mutex covers all in-process sharing.
pub struct HashCache {
    state: Mutex<State>,
    /// `None` means the cache is disabled (either `--no-cache` or no resolvable
    /// config dir); nothing is looked up, stored, or persisted.
    path: Option<PathBuf>,
}

impl HashCache {
    /// Open the cache. `no_cache` returns a disabled handle. `rebuild` starts
    /// empty but persists this run's freshly computed digests.
    pub fn load(no_cache: bool, rebuild: bool) -> Self {
        if no_cache {
            return Self::open_at(None, false);
        }
        let path = dirs::config_dir().map(|d| d.join(CACHE_SUBPATH));
        Self::open_at(path, rebuild)
    }

    fn open_at(path: Option<PathBuf>, rebuild: bool) -> Self {
        let existing = match &path {
            Some(p) => read_entries(p),
            None => HashMap::new(),
        };
        let (entries, preserved) = if rebuild {
            (HashMap::new(), existing)
        } else {
            (existing, HashMap::new())
        };
        HashCache {
            state: Mutex::new(State {
                entries,
                preserved,
                dirty: false,
            }),
            path,
        }
    }

    fn disabled(&self) -> bool {
        self.path.is_none()
    }

    /// Raw-byte digests for `path`, only when the fingerprint still matches and
    /// every requested algorithm is present.
    pub fn lookup_raw(&self, path: &Path, algos: &[HashAlgo]) -> Option<FileDigests> {
        self.lookup(path, algos, |e| &e.raw)
    }

    /// Decoded inner-stream digests for `path`, subject to the same checks.
    pub fn lookup_decoded(&self, path: &Path, algos: &[HashAlgo]) -> Option<FileDigests> {
        self.lookup(path, algos, |e| &e.decoded)
    }

    fn lookup(
        &self,
        path: &Path,
        algos: &[HashAlgo],
        pick: impl Fn(&CacheEntry) -> &FileDigests,
    ) -> Option<FileDigests> {
        if self.disabled() {
            return None;
        }
        let key = canonical_key(path)?;
        let fp = fingerprint(path)?;
        let state = self.state.lock().unwrap();
        let entry = state.entries.get(&key)?;
        if !entry_matches(entry, fp) {
            return None;
        }
        let digests = pick(entry);
        if has_all(digests, algos) {
            Some(digests.clone())
        } else {
            None
        }
    }

    /// Merge raw-byte digests for `path` into its entry, replacing the entry
    /// when the file's fingerprint has changed.
    pub fn store_raw(&self, path: &Path, digests: &FileDigests) {
        self.store(path, |e| &mut e.raw, digests);
    }

    /// Merge decoded inner-stream digests for `path` into its entry.
    pub fn store_decoded(&self, path: &Path, digests: &FileDigests) {
        self.store(path, |e| &mut e.decoded, digests);
    }

    fn store(
        &self,
        path: &Path,
        pick: impl Fn(&mut CacheEntry) -> &mut FileDigests,
        digests: &FileDigests,
    ) {
        if self.disabled() {
            return;
        }
        let Some(key) = canonical_key(path) else {
            return;
        };
        let Some(fp) = fingerprint(path) else {
            return;
        };
        if too_recent(fp) {
            return;
        }
        let mut state = self.state.lock().unwrap();
        let entry = entry_for(&mut state.entries, key, fp);
        merge_digests(pick(entry), digests);
        state.dirty = true;
    }

    /// Whole-image and per-track digests for a cue set, valid only when the cue
    /// and every member bin still match their stored fingerprints and every
    /// requested algorithm is present.
    pub fn lookup_cue_set(
        &self,
        cue: &Path,
        members: &[PathBuf],
        algos: &[HashAlgo],
    ) -> Option<CueDigests> {
        if self.disabled() {
            return None;
        }
        let key = canonical_key(cue)?;
        let fp = fingerprint(cue)?;
        let state = self.state.lock().unwrap();
        let entry = state.entries.get(&key)?;
        if !entry_matches(entry, fp) {
            return None;
        }
        let stored = entry.members.as_ref()?;
        if stored.len() != members.len() {
            return None;
        }
        for (member, want) in members.iter().zip(stored) {
            if fingerprint(member)? != *want {
                return None;
            }
        }
        let whole = entry.whole.as_ref()?;
        let tracks = entry.tracks.as_ref()?;
        if !has_all(whole, algos) || !tracks.iter().all(|t| has_all(&t.digests, algos)) {
            return None;
        }
        Some(CueDigests {
            whole: whole.clone(),
            tracks: tracks.clone(),
        })
    }

    /// Store a cue set's digests plus each member bin's fingerprint. Skipped if
    /// the cue or any member was modified within the recent-mtime guard.
    pub fn store_cue_set(
        &self,
        cue: &Path,
        members: &[PathBuf],
        whole: &FileDigests,
        tracks: &[CachedTrack],
    ) {
        if self.disabled() {
            return;
        }
        let Some(key) = canonical_key(cue) else {
            return;
        };
        let Some(fp) = fingerprint(cue) else {
            return;
        };
        if too_recent(fp) {
            return;
        }
        let mut member_keys = Vec::with_capacity(members.len());
        for member in members {
            let Some(mfp) = fingerprint(member) else {
                return;
            };
            if too_recent(mfp) {
                return;
            }
            member_keys.push(mfp);
        }
        let mut state = self.state.lock().unwrap();
        let entry = entry_for(&mut state.entries, key, fp);
        entry.whole = Some(whole.clone());
        entry.tracks = Some(tracks.to_vec());
        entry.members = Some(member_keys);
        state.dirty = true;
    }

    /// True when `path` verified valid for `label` on a previous run and its
    /// fingerprint is unchanged.
    pub fn lookup_verify(&self, path: &Path, label: &str) -> bool {
        if self.disabled() {
            return false;
        }
        let Some(key) = canonical_key(path) else {
            return false;
        };
        let Some(fp) = fingerprint(path) else {
            return false;
        };
        let state = self.state.lock().unwrap();
        let Some(entry) = state.entries.get(&key) else {
            return false;
        };
        entry_matches(entry, fp) && entry.verify_valid.get(label).copied().unwrap_or(false)
    }

    /// Record a `label` verify verdict for `path`. Only `valid == true` is
    /// stored: an invalid output gets rewritten, which changes its mtime.
    pub fn store_verify(&self, path: &Path, label: &str, valid: bool) {
        if self.disabled() || !valid {
            return;
        }
        let Some(key) = canonical_key(path) else {
            return;
        };
        let Some(fp) = fingerprint(path) else {
            return;
        };
        if too_recent(fp) {
            return;
        }
        let mut state = self.state.lock().unwrap();
        let entry = entry_for(&mut state.entries, key, fp);
        entry.verify_valid.insert(label.to_string(), true);
        state.dirty = true;
    }

    /// Persist the cache if anything changed. Errors log a warning and are
    /// swallowed so a cache write can never fail the run.
    pub fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let mut state = self.state.lock().unwrap();
        if !state.dirty {
            return;
        }
        let result = if state.preserved.is_empty() {
            write_atomic(path, &state.entries)
        } else {
            let mut merged = state.preserved.clone();
            for (key, entry) in &state.entries {
                merged.insert(key.clone(), entry.clone());
            }
            write_atomic(path, &merged)
        };
        match result {
            Ok(()) => state.dirty = false,
            Err(e) => log::warn!("Could not write hash cache to {}: {e}", path.display()),
        }
    }
}

fn entry_matches(entry: &CacheEntry, fp: Fingerprint) -> bool {
    entry.size == fp.0 && entry.mtime_secs == fp.1 && entry.mtime_nanos == fp.2
}

/// Get the entry for `key`, resetting it to a fresh entry when the stored
/// fingerprint no longer matches the file on disk (size or mtime changed).
fn entry_for(
    entries: &mut HashMap<String, CacheEntry>,
    key: String,
    fp: Fingerprint,
) -> &mut CacheEntry {
    let entry = entries.entry(key).or_default();
    if entry.size != fp.0 || entry.mtime_secs != fp.1 || entry.mtime_nanos != fp.2 {
        *entry = CacheEntry {
            size: fp.0,
            mtime_secs: fp.1,
            mtime_nanos: fp.2,
            ..Default::default()
        };
    }
    entry
}

fn has_all(digests: &FileDigests, algos: &[HashAlgo]) -> bool {
    algos.iter().all(|a| digests.value(*a).is_some())
}

/// Union `src` into `dst`, preferring `src` where present, and adopt its size.
fn merge_digests(dst: &mut FileDigests, src: &FileDigests) {
    if src.crc32.is_some() {
        dst.crc32 = src.crc32.clone();
    }
    if src.sha1.is_some() {
        dst.sha1 = src.sha1.clone();
    }
    if src.md5.is_some() {
        dst.md5 = src.md5.clone();
    }
    if src.sha256.is_some() {
        dst.sha256 = src.sha256.clone();
    }
    dst.size_bytes = src.size_bytes;
}

fn fingerprint(path: &Path) -> Option<Fingerprint> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let (secs, nanos) = match modified.duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
        Err(e) => (
            -(e.duration().as_secs() as i64),
            e.duration().subsec_nanos(),
        ),
    };
    Some((meta.len(), secs, nanos))
}

/// A negative or small difference (recently modified, or an mtime in the
/// future) means the timestamp cannot yet be trusted as a stable key.
fn too_recent(fp: Fingerprint) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    now - fp.1 < RECENT_MTIME_GUARD_SECS
}

/// Non-UTF8 paths are not cached rather than lossily coerced to a `String`,
/// which could collapse two distinct paths onto one key and serve a wrong
/// digest.
fn canonical_key(path: &Path) -> Option<String> {
    std::fs::canonicalize(path)
        .ok()?
        .to_str()
        .map(str::to_owned)
}

fn read_entries(path: &Path) -> HashMap<String, CacheEntry> {
    read_envelope(path).unwrap_or_default()
}

fn read_envelope(path: &Path) -> Option<HashMap<String, CacheEntry>> {
    let file = std::fs::File::open(path).ok()?;
    let decoder = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
    let envelope: Envelope = serde_json::from_reader(decoder).ok()?;
    if envelope.version != CACHE_VERSION {
        return None;
    }
    Some(envelope.entries)
}

fn write_atomic(path: &Path, entries: &HashMap<String, CacheEntry>) -> std::io::Result<()> {
    use std::io::Write as _;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    let mut encoder = flate2::write::GzEncoder::new(&mut tmp, flate2::Compression::default());
    serde_json::to_writer(
        &mut encoder,
        &EnvelopeRef {
            version: CACHE_VERSION,
            entries,
        },
    )?;
    encoder.finish()?;
    tmp.flush()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::NoProgress;
    use crate::util::hash::hash_file;

    const ALL: &[HashAlgo] = &[
        HashAlgo::Crc32,
        HashAlgo::Sha1,
        HashAlgo::Md5,
        HashAlgo::Sha256,
    ];

    /// A file with an mtime old enough to clear the recent-mtime guard.
    fn write_old(path: &Path, data: &[u8]) {
        std::fs::write(path, data).unwrap();
        set_mtime(path, std::time::Duration::from_secs(60));
    }

    /// Backdate `path`'s mtime by `ago` from now.
    fn set_mtime(path: &Path, ago: std::time::Duration) {
        let when = std::time::SystemTime::now() - ago;
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_modified(when).unwrap();
    }

    fn cache_at(dir: &Path, rebuild: bool) -> HashCache {
        HashCache::open_at(Some(dir.join("hash-cache.json.gz")), rebuild)
    }

    fn digests_of(path: &Path) -> FileDigests {
        hash_file(path, ALL, &NoProgress).unwrap()
    }

    #[test]
    fn hit_on_unchanged_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("game.iso");
        write_old(&file, b"stable contents");
        let cache = cache_at(dir.path(), false);
        let d = digests_of(&file);
        cache.store_raw(&file, &d);
        assert_eq!(cache.lookup_raw(&file, ALL), Some(d));
    }

    #[test]
    fn miss_on_size_change() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("game.iso");
        write_old(&file, b"first");
        let cache = cache_at(dir.path(), false);
        cache.store_raw(&file, &digests_of(&file));
        write_old(&file, b"first and more");
        assert_eq!(cache.lookup_raw(&file, ALL), None);
    }

    #[test]
    fn miss_on_mtime_change() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("game.iso");
        write_old(&file, b"same length!!");
        let cache = cache_at(dir.path(), false);
        cache.store_raw(&file, &digests_of(&file));
        // Same byte length, newer mtime (still old enough to store).
        set_mtime(&file, std::time::Duration::from_secs(30));
        assert_eq!(cache.lookup_raw(&file, ALL), None);
    }

    #[test]
    fn miss_when_requested_algo_absent() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("game.iso");
        write_old(&file, b"partial algos");
        let cache = cache_at(dir.path(), false);
        let only_crc = hash_file(&file, &[HashAlgo::Crc32], &NoProgress).unwrap();
        cache.store_raw(&file, &only_crc);
        assert!(cache.lookup_raw(&file, &[HashAlgo::Crc32]).is_some());
        assert!(cache.lookup_raw(&file, &[HashAlgo::Sha256]).is_none());
    }

    #[test]
    fn merges_partial_algo_sets() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("game.iso");
        write_old(&file, b"merge me");
        let cache = cache_at(dir.path(), false);
        cache.store_raw(
            &file,
            &hash_file(&file, &[HashAlgo::Crc32], &NoProgress).unwrap(),
        );
        cache.store_raw(
            &file,
            &hash_file(&file, &[HashAlgo::Sha256], &NoProgress).unwrap(),
        );
        let hit = cache
            .lookup_raw(&file, &[HashAlgo::Crc32, HashAlgo::Sha256])
            .unwrap();
        assert!(hit.crc32.is_some());
        assert!(hit.sha256.is_some());
    }

    #[test]
    fn save_load_roundtrip_through_gzip() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("game.iso");
        write_old(&file, b"persist me");
        let d = digests_of(&file);
        {
            let cache = cache_at(dir.path(), false);
            cache.store_raw(&file, &d);
            cache.save();
        }
        let reopened = cache_at(dir.path(), false);
        assert_eq!(reopened.lookup_raw(&file, ALL), Some(d));
    }

    #[test]
    fn corrupt_file_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hash-cache.json.gz");
        std::fs::write(&path, b"not gzip, not json").unwrap();
        assert!(read_entries(&path).is_empty());
    }

    #[test]
    fn recent_mtime_guard_skips_store() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("fresh.iso");
        std::fs::write(&file, b"just written").unwrap();
        let cache = cache_at(dir.path(), false);
        cache.store_raw(&file, &digests_of(&file));
        assert_eq!(cache.lookup_raw(&file, ALL), None);
    }

    #[test]
    fn verify_verdict_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("out.chd");
        write_old(&file, b"valid output");
        let cache = cache_at(dir.path(), false);
        assert!(!cache.lookup_verify(&file, "chd"));
        cache.store_verify(&file, "chd", true);
        assert!(cache.lookup_verify(&file, "chd"));
        assert!(!cache.lookup_verify(&file, "cso"));
    }

    #[test]
    fn invalid_verify_is_not_stored() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("out.chd");
        write_old(&file, b"bad output");
        let cache = cache_at(dir.path(), false);
        cache.store_verify(&file, "chd", false);
        assert!(!cache.lookup_verify(&file, "chd"));
    }

    #[test]
    fn cue_set_roundtrip_and_member_invalidation() {
        let dir = tempfile::tempdir().unwrap();
        let cue = dir.path().join("game.cue");
        let bin = dir.path().join("game.bin");
        write_old(&cue, b"FILE \"game.bin\" BINARY");
        write_old(&bin, b"track bytes");
        let members = vec![bin.clone()];
        let whole = digests_of(&bin);
        let tracks = vec![CachedTrack {
            number: 1,
            kind: String::new(),
            digests: whole.clone(),
        }];
        let cache = cache_at(dir.path(), false);
        cache.store_cue_set(&cue, &members, &whole, &tracks);
        let hit = cache.lookup_cue_set(&cue, &members, ALL).unwrap();
        assert_eq!(hit.whole, whole);
        assert_eq!(hit.tracks.len(), 1);

        // A changed member bin invalidates the whole set.
        write_old(&bin, b"track bytes changed");
        assert!(cache.lookup_cue_set(&cue, &members, ALL).is_none());
    }

    #[test]
    fn disabled_handle_never_hits() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("game.iso");
        write_old(&file, b"contents");
        let cache = HashCache::open_at(None, false);
        cache.store_raw(&file, &digests_of(&file));
        assert_eq!(cache.lookup_raw(&file, ALL), None);
        cache.save(); // no path: no-op, no panic
    }

    #[test]
    fn rebuild_starts_empty_but_saves() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("game.iso");
        write_old(&file, b"rebuild contents");
        let d = digests_of(&file);
        {
            let cache = cache_at(dir.path(), false);
            cache.store_raw(&file, &d);
            cache.save();
        }
        // Rebuild ignores the prior contents on open but persists this run.
        let cache = cache_at(dir.path(), true);
        assert_eq!(cache.lookup_raw(&file, ALL), None);
        cache.store_raw(&file, &d);
        assert_eq!(cache.lookup_raw(&file, ALL), Some(d.clone()));
        cache.save();
        let reopened = cache_at(dir.path(), false);
        assert_eq!(reopened.lookup_raw(&file, ALL), Some(d));
    }

    #[test]
    fn rebuild_scoped_run_keeps_untouched_entries() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.iso");
        let b = dir.path().join("b.iso");
        write_old(&a, b"first file");
        write_old(&b, b"second file");
        let da = digests_of(&a);
        let db = digests_of(&b);
        {
            let cache = cache_at(dir.path(), false);
            cache.store_raw(&a, &da);
            cache.store_raw(&b, &db);
            cache.save();
        }
        // A rebuild that only touches `a` must not drop `b`'s entry.
        {
            let cache = cache_at(dir.path(), true);
            cache.store_raw(&a, &da);
            cache.save();
        }
        let reopened = cache_at(dir.path(), false);
        assert_eq!(reopened.lookup_raw(&a, ALL), Some(da));
        assert_eq!(reopened.lookup_raw(&b, ALL), Some(db));
    }

    #[test]
    fn rebuild_that_stores_nothing_leaves_file_intact() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("game.iso");
        write_old(&file, b"keep me");
        let d = digests_of(&file);
        {
            let cache = cache_at(dir.path(), false);
            cache.store_raw(&file, &d);
            cache.save();
        }
        // A rebuild run that errors before hashing anything must not wipe the
        // on-disk cache.
        {
            let cache = cache_at(dir.path(), true);
            cache.save();
        }
        let reopened = cache_at(dir.path(), false);
        assert_eq!(reopened.lookup_raw(&file, ALL), Some(d));
    }
}
