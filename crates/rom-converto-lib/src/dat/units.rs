//! Walkable DAT inputs: a standalone file digested by its container decoder,
//! or a cue set whose member bins are hashed raw. Shared by verify, scan,
//! rename and fixdat so every workflow groups, caches and buckets the same way.

use crate::cue::CueParser;
use crate::dat::digest::{QuickDigest, TrackDigests, quick_crc_digest};
use crate::dat::verdict::DatVerdict;
use crate::dat::{DatError, DatResult, RomDigests, digest_inner_async, is_raw_reread_cheap};
use crate::util::fs::{collect_all_files, file_len};
use crate::util::hash::MultiHasher;
use crate::util::{
    CachedTrack, CancelToken, Cancelled, HashAlgo, HashCache, ProgressReporter, resolve_input,
    spawn_blocking_with_progress,
};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Archive members a DAT lookup extracts and digests in place of the archive.
pub const ARCHIVE_IMAGE_EXTS: &[&str] = &[
    "iso", "gcm", "wbfs", "rvz", "gcz", "wia", "nkit", "chd", "cso", "zso", "dax", "cue", "cia",
    "3ds", "cci", "cxi", "3dsx", "zcia", "zcci", "zcxi", "z3dsx", "nsp", "xci", "nca", "nsz",
    "xcz", "ncz", "wud", "wux", "xiso", "zar",
];

/// One walkable input. A cue set's bins are hashed raw in cue order; their
/// concatenation is the single-bin whole-image stream.
#[derive(Debug, Clone)]
pub enum DatUnit {
    File(PathBuf),
    CueSet { cue: PathBuf, bins: Vec<PathBuf> },
}

impl DatUnit {
    /// The path a report row names: the file, or the cue sheet of a set.
    pub fn display_path(&self) -> &Path {
        match self {
            DatUnit::File(p) => p,
            DatUnit::CueSet { cue, .. } => cue,
        }
    }

    /// Bytes to read for this unit, for aggregate progress.
    pub fn size_bytes(&self) -> u64 {
        match self {
            DatUnit::File(p) => file_len(p),
            DatUnit::CueSet { bins, .. } => bins.iter().map(|p| file_len(p)).sum(),
        }
    }

    fn file_name(&self) -> String {
        self.display_path()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string()
    }
}

/// Walk `dir`, group each .cue with its member .bin files, and drop the
/// grouped bins plus playlist sidecars from the flat list, keeping walk
/// order. Bins with no owning cue stay standalone; an unreadable cue is
/// skipped with a warning.
pub async fn collect_units(
    dir: &Path,
    max_depth: Option<usize>,
    cancel: &CancelToken,
) -> std::io::Result<Vec<DatUnit>> {
    let files = collect_all_files(dir, max_depth, cancel)?;
    let ext = |f: &Path| {
        f.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
    };
    let mut sets = HashMap::new();
    let mut covered = HashSet::new();
    for cue in files.iter().filter(|f| ext(f).as_deref() == Some("cue")) {
        let sheet = match CueParser::new(cue).parse().await {
            Ok(s) => s,
            Err(e) => {
                log::warn!("Skipping unreadable cue {}: {e}", cue.display());
                continue;
            }
        };
        let parent = cue.parent().unwrap_or_else(|| Path::new("."));
        let bins: Vec<PathBuf> = sheet
            .files
            .iter()
            .map(|f| parent.join(&f.filename))
            .collect();
        if bins.is_empty() {
            continue;
        }
        covered.extend(bins.iter().cloned());
        sets.insert(cue.clone(), bins);
    }
    let mut units = Vec::new();
    for f in files {
        match ext(&f).as_deref() {
            Some("cue") => {
                if let Some(bins) = sets.remove(&f) {
                    units.push(DatUnit::CueSet { cue: f, bins });
                }
            }
            Some("m3u") => {}
            _ if covered.contains(&f) => {}
            _ => units.push(DatUnit::File(f)),
        }
    }
    Ok(units)
}

/// Digest one unit through the persistent hash cache when one is given. A
/// file goes through its container decoder; a cue set hashes each member bin
/// raw in cue order into its own track digest and the whole-image digest.
/// Only single-stream and cue-set results are cached; a CHD's per-track
/// result has no whole-set fingerprint and stays uncached.
pub async fn digest_unit(
    unit: &DatUnit,
    algos: &[HashAlgo],
    cache: Option<&HashCache>,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> DatResult<RomDigests> {
    match unit {
        DatUnit::File(path) => {
            if let Some(d) = cache.and_then(|c| c.lookup_decoded(path, algos)) {
                return Ok(RomDigests::Single(d));
            }
            // An archive is digested through its image member; the cache
            // entry stays keyed by the archive so batch and single runs hit.
            let staged = {
                let path = path.clone();
                tokio::task::spawn_blocking(move || resolve_input(&path, ARCHIVE_IMAGE_EXTS))
                    .await?
                    .map_err(|e| DatError::InvalidInput(e.to_string()))?
            };
            let result = digest_inner_async(
                staged.path().to_path_buf(),
                algos.to_vec(),
                progress,
                cancel.clone(),
            )
            .await?;
            if let (Some(cache), RomDigests::Single(d)) = (cache, &result) {
                cache.store_decoded(path, d);
            }
            Ok(result)
        }
        DatUnit::CueSet { cue, bins } => {
            if let Some(hit) = cache.and_then(|c| c.lookup_cue_set(cue, bins, algos)) {
                let tracks = hit
                    .tracks
                    .into_iter()
                    .map(|t| TrackDigests {
                        track_number: t.number,
                        track_type: t.kind,
                        digests: t.digests,
                    })
                    .collect();
                return Ok(RomDigests::Tracks {
                    tracks,
                    whole: hit.whole,
                });
            }
            let result = spawn_blocking_with_progress(progress, {
                let bins = bins.clone();
                let algos = algos.to_vec();
                let cancel = cancel.clone();
                move |progress| digest_cue_set(&bins, &algos, progress, &cancel)
            })
            .await?;
            if let (Some(cache), RomDigests::Tracks { tracks, whole }) = (cache, &result) {
                let cached: Vec<CachedTrack> = tracks
                    .iter()
                    .map(|t| CachedTrack {
                        number: t.track_number,
                        kind: t.track_type.clone(),
                        digests: t.digests.clone(),
                    })
                    .collect();
                cache.store_cue_set(cue, bins, whole, &cached);
            }
            Ok(result)
        }
    }
}

// Each bin feeds its own track hasher and the whole-image hasher in one
// read, because the DAT may use either the single-bin or multi-bin
// convention.
fn digest_cue_set(
    bins: &[PathBuf],
    algos: &[HashAlgo],
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> DatResult<RomDigests> {
    let mut tracks = Vec::with_capacity(bins.len());
    let mut whole = MultiHasher::new(algos);
    let mut whole_size = 0u64;
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    for (i, bin) in bins.iter().enumerate() {
        let name = bin.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        progress.start(file_len(bin), &format!("Hashing {name}"));
        let mut file = std::fs::File::open(bin)?;
        let mut track = MultiHasher::new(algos);
        let mut size = 0u64;
        loop {
            if cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            track.update(&buf[..n]);
            whole.update(&buf[..n]);
            size += n as u64;
            progress.inc(n as u64);
        }
        progress.finish();
        whole_size += size;
        tracks.push(TrackDigests {
            track_number: (i + 1) as u32,
            track_type: String::new(),
            digests: track.finalize(size),
        });
    }
    Ok(RomDigests::Tracks {
        tracks,
        whole: whole.finalize(whole_size),
    })
}

/// `--quick` mode's zip-central-directory CRC shortcut for one unit. Only a
/// [`DatUnit::File`] qualifies (a cue set spans several bins), and a hash
/// cache hit already gives the real digest for free, so the CRC probe only
/// runs on a cache miss. The CRC-only digest is never stored in the cache.
pub async fn quick_digest(unit: &DatUnit, cache: Option<&HashCache>) -> Option<QuickDigest> {
    let DatUnit::File(path) = unit else {
        return None;
    };
    if cache.is_some_and(|c| c.lookup_decoded(path, &[HashAlgo::Crc32]).is_some()) {
        return None;
    }
    let path = path.clone();
    tokio::task::spawn_blocking(move || quick_crc_digest(&path))
        .await
        .ok()
        .flatten()
}

/// Search name for the primary lookup of a unit: the file name, or the
/// archive member's name for a quick digest so the search matches what the
/// full path would digest.
pub fn search_name(unit: &DatUnit, quick: Option<&QuickDigest>) -> String {
    quick
        .map(|q| q.member_name.clone())
        .unwrap_or_else(|| unit.file_name())
}

/// True when tiered checksum escalation is worth attempting: a second full
/// read is cheap for a raw file or a cue set's raw bins, but not for a
/// container whose first decode already dominates cost.
pub fn is_tierable(unit: &DatUnit) -> bool {
    match unit {
        DatUnit::File(path) => is_raw_reread_cheap(path),
        DatUnit::CueSet { .. } => true,
    }
}

/// How a per-unit digest error is reported instead of aborting the batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DigestBucket {
    Unsupported,
    Failed,
}

impl DigestBucket {
    pub fn verdict(self) -> DatVerdict {
        match self {
            DigestBucket::Unsupported => DatVerdict::Unsupported,
            DigestBucket::Failed => DatVerdict::Failed,
        }
    }
}

/// Bucket a digest error: an unsupported inner format and a plain per-file
/// failure become rows; cancellation and transport errors propagate. Callers
/// surface [`crate::util::NX_DAT_UNSUPPORTED_HINT`] once per run when any
/// unit lands in `Unsupported`.
pub fn bucket(e: DatError) -> DatResult<(DigestBucket, String)> {
    match e {
        DatError::Cancelled(_) => Err(Cancelled.into()),
        DatError::Transport(_) => Err(e),
        DatError::UnsupportedInnerHash { .. } => Ok((DigestBucket::Unsupported, e.to_string())),
        other => Ok((DigestBucket::Failed, other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cue_set_groups_bins() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("game.cue"),
            "FILE \"game (Track 1).bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\nFILE \"game (Track 2).bin\" BINARY\n  TRACK 02 AUDIO\n    INDEX 01 00:00:00\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("game (Track 1).bin"), b"1").unwrap();
        std::fs::write(dir.path().join("game (Track 2).bin"), b"22").unwrap();
        std::fs::write(dir.path().join("game.m3u"), b"game.cue\n").unwrap();
        std::fs::write(dir.path().join("loose.bin"), b"333").unwrap();

        let units = collect_units(dir.path(), None, &CancelToken::new())
            .await
            .unwrap();
        assert_eq!(units.len(), 2);
        let DatUnit::CueSet { cue, bins } = &units[0] else {
            panic!("first unit should be the cue set");
        };
        assert_eq!(cue, &dir.path().join("game.cue"));
        assert_eq!(bins.len(), 2);
        assert_eq!(units[0].size_bytes(), 3);
        assert!(matches!(&units[1], DatUnit::File(p) if p.ends_with("loose.bin")));
        assert!(is_tierable(&units[0]));
    }

    #[tokio::test]
    async fn quick_digest_zip_member() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("game.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("game.gba", opts).unwrap();
        zip.write_all(b"hello").unwrap();
        zip.finish().unwrap();

        let unit = DatUnit::File(zip_path);
        let q = quick_digest(&unit, None).await.unwrap();
        assert_eq!(q.member_name, "game.gba");
        assert_eq!(q.digests.size_bytes, 5);
        assert_eq!(search_name(&unit, Some(&q)), "game.gba");
        assert_eq!(search_name(&unit, None), "game.zip");
        assert!(
            quick_digest(
                &DatUnit::CueSet {
                    cue: PathBuf::from("a.cue"),
                    bins: vec![]
                },
                None
            )
            .await
            .is_none()
        );
    }

    #[test]
    fn bucket_unsupported() {
        let (kind, msg) = bucket(DatError::UnsupportedInnerHash { format: "NSZ" }).unwrap();
        assert_eq!(kind, DigestBucket::Unsupported);
        assert_eq!(kind.verdict(), DatVerdict::Unsupported);
        assert!(msg.contains("NSZ"));
        let (kind, _) = bucket(DatError::InvalidInput("bad".into())).unwrap();
        assert_eq!(kind, DigestBucket::Failed);
        assert!(bucket(DatError::Cancelled(Cancelled)).is_err());
        assert!(bucket(DatError::Transport("down".into())).is_err());
    }
}
