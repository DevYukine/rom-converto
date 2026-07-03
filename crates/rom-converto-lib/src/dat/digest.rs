//! Inner-stream digest dispatch: decode a container in flight and
//! hash its decoded payload without ever writing a temp file. Raw
//! files hash directly; RVZ/WBFS stream through their readers; CSO,
//! Z3DS and CHD reuse each format's pooled decoder taps.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use crate::dat::error::{DatError, DatResult};
use crate::util::hash::{FileDigests, HashAlgo, MultiHasher, hash_file_cancellable};
use crate::util::{CancelToken, ProgressReporter};

/// How a file's decoded inner stream is obtained, chosen by extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InnerStreamKind {
    /// Already a raw image: hash the file bytes as-is.
    Raw,
    /// RVZ disc image.
    Rvz,
    /// WBFS container.
    Wbfs,
    /// CSO / ZSO block-compressed image.
    CsoZso,
    /// Z3DS file (z3ds/zcia/zcci/zcxi/z3dsx).
    Z3ds,
    /// CHD: per-track digests plus the whole concatenated image.
    ChdTracks,
    /// A cue sheet; the set spans multiple files and is resolved by
    /// the caller, not here.
    CueSet,
    /// Compressed formats with no in-flight inner-hash support yet.
    UnsupportedCompressed,
}

/// Classify a path by its extension, case-insensitive. Unknown or
/// missing extensions fall through to [`InnerStreamKind::Raw`].
pub fn classify_input(path: &Path) -> InnerStreamKind {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        "rvz" => InnerStreamKind::Rvz,
        "wbfs" => InnerStreamKind::Wbfs,
        "cso" | "zso" => InnerStreamKind::CsoZso,
        "z3ds" | "zcia" | "zcci" | "zcxi" | "z3dsx" => InnerStreamKind::Z3ds,
        "chd" => InnerStreamKind::ChdTracks,
        "cue" => InnerStreamKind::CueSet,
        "nsz" | "xcz" => InnerStreamKind::UnsupportedCompressed,
        _ => InnerStreamKind::Raw,
    }
}

/// One track's decoded digest set.
#[derive(Debug, Clone)]
pub struct TrackDigests {
    pub track_number: u32,
    pub track_type: String,
    pub digests: FileDigests,
}

/// The result of digesting one input: either a single decoded stream
/// or a multi-track set with the whole concatenated-image digest.
#[derive(Debug, Clone)]
pub enum RomDigests {
    Single(FileDigests),
    Tracks {
        tracks: Vec<TrackDigests>,
        whole: FileDigests,
    },
}

/// Map a container error to a `DatError`, translating each format's
/// own cancellation variant to [`DatError::Cancelled`] so the CLI's
/// single cancel-detection arm suffices. Everything else is wrapped
/// as [`DatError::Container`] via `Display`.
fn map_chd(e: crate::chd::error::ChdError) -> DatError {
    match e {
        crate::chd::error::ChdError::Cancelled => DatError::Cancelled,
        other => DatError::Container(other.to_string()),
    }
}

fn map_cso(e: crate::cso::CsoError) -> DatError {
    match e {
        crate::cso::CsoError::Cancelled => DatError::Cancelled,
        other => DatError::Container(other.to_string()),
    }
}

fn map_z3ds(e: crate::nintendo::ctr::z3ds::error::Z3dsError) -> DatError {
    match e {
        crate::nintendo::ctr::z3ds::error::Z3dsError::Cancelled => DatError::Cancelled,
        other => DatError::Container(other.to_string()),
    }
}

/// Digest a sequential reader (RVZ or WBFS) with a 4 MiB read loop,
/// the same multi-hasher fold as `hash_file_cancellable`. `total` is
/// the decoded logical size used for progress sizing and recorded as
/// `size_bytes`.
fn digest_reader<R: Read>(
    mut reader: R,
    total: u64,
    algos: &[HashAlgo],
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
) -> DatResult<FileDigests> {
    use std::sync::atomic::Ordering;

    let mut hasher = MultiHasher::new(algos);
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(DatError::Cancelled);
        }
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        bytes_done.fetch_add(n as u64, Ordering::Relaxed);
    }
    Ok(hasher.finalize(total))
}

/// Decode a legacy disc container (GCZ, WIA, NKit ISO, or NKit GCZ)
/// and digest its logical disc content, matching how `dol/rvl info`
/// and `compress_disc` decode the same inputs. Container decode errors
/// surface through `digest_reader`'s `?` as I/O errors.
fn digest_legacy(
    fmt: crate::nintendo::legacy_input::LegacyFormat,
    path: &Path,
    algos: &[HashAlgo],
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
) -> DatResult<RomDigests> {
    use crate::nintendo::legacy_input::LegacyFormat;
    let single = match fmt {
        LegacyFormat::Gcz => {
            let mut r = crate::nintendo::gcz::GczReader::open(path)
                .map_err(|e| DatError::Container(e.to_string()))?;
            let total = r.data_size();
            digest_reader(&mut r, total, algos, bytes_done, cancel)?
        }
        LegacyFormat::Wia => {
            let mut r = crate::nintendo::wia::WiaReader::open(path)
                .map_err(|e| DatError::Container(e.to_string()))?;
            let total = r.iso_size();
            digest_reader(&mut r, total, algos, bytes_done, cancel)?
        }
        LegacyFormat::NkitIso => {
            let mut r = crate::nintendo::nkit::NkitReader::open(path)
                .map_err(|e| DatError::Container(e.to_string()))?;
            let total = r.image_size();
            digest_reader(&mut r, total, algos, bytes_done, cancel)?
        }
        LegacyFormat::NkitGcz => {
            let gcz = crate::nintendo::gcz::GczReader::open(path)
                .map_err(|e| DatError::Container(e.to_string()))?;
            let mut r = crate::nintendo::nkit::NkitReader::from_source(gcz)
                .map_err(|e| DatError::Container(e.to_string()))?;
            let total = r.image_size();
            digest_reader(&mut r, total, algos, bytes_done, cancel)?
        }
    };
    Ok(RomDigests::Single(single))
}

/// Synchronous dispatch core. Runs inside the caller's blocking
/// context and ticks `bytes_done` as it hashes. `digest_inner` and
/// `digest_inner_async` both funnel through here.
fn digest_dispatch(
    path: &Path,
    algos: &[HashAlgo],
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
) -> DatResult<RomDigests> {
    match classify_input(path) {
        InnerStreamKind::Raw => {
            // Legacy containers are magic-sniffed only for raw-looking
            // extensions, so known non-raw extensions keep their
            // extension-based errors and never pay a file open.
            if let Some(fmt) = crate::nintendo::legacy_input::detect_legacy_format(path)
                .map_err(DatError::IoError)?
            {
                return digest_legacy(fmt, path, algos, bytes_done, cancel);
            }
            let d = digest_raw(path, algos, bytes_done, cancel)?;
            Ok(RomDigests::Single(d))
        }
        InnerStreamKind::Rvz => {
            let mut reader = crate::nintendo::rvz::decompress::RvzDiscReader::open(path)
                .map_err(|e| DatError::Container(e.to_string()))?;
            let total = reader.iso_size();
            let d = digest_reader(&mut reader, total, algos, bytes_done, cancel)?;
            Ok(RomDigests::Single(d))
        }
        InnerStreamKind::Wbfs => {
            let mut reader = crate::nintendo::wbfs::WbfsReader::open(path)
                .map_err(|e| DatError::Container(e.to_string()))?;
            let total = reader.disc_size();
            let d = digest_reader(&mut reader, total, algos, bytes_done, cancel)?;
            Ok(RomDigests::Single(d))
        }
        InnerStreamKind::CsoZso => {
            let d =
                crate::cso::digest_cso_inner(path, algos, bytes_done, cancel).map_err(map_cso)?;
            Ok(RomDigests::Single(d))
        }
        InnerStreamKind::Z3ds => {
            let d = crate::nintendo::ctr::z3ds::digest_z3ds_inner(path, algos, bytes_done, cancel)
                .map_err(map_z3ds)?;
            Ok(RomDigests::Single(d))
        }
        InnerStreamKind::ChdTracks => {
            let (tracks, whole) =
                crate::chd::digest_chd_tracks(path, algos, bytes_done, cancel).map_err(map_chd)?;
            if tracks.is_empty() {
                // DVD-type CHD: one flat stream.
                Ok(RomDigests::Single(whole))
            } else {
                let tracks = tracks
                    .into_iter()
                    .map(|t| TrackDigests {
                        track_number: t.track_number as u32,
                        track_type: t.track_type,
                        digests: t.digests,
                    })
                    .collect();
                Ok(RomDigests::Tracks { tracks, whole })
            }
        }
        InnerStreamKind::CueSet => Err(DatError::InvalidInput(
            "CUE sheets are not hashed directly; pass the .bin tracks or the directory instead"
                .to_string(),
        )),
        InnerStreamKind::UnsupportedCompressed => {
            let format = match path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .as_deref()
            {
                Some("xcz") => "xcz",
                _ => "nsz",
            };
            Err(DatError::UnsupportedInnerHash { format })
        }
    }
}

/// Hash a raw file directly. Ticks `bytes_done` rather than a
/// `ProgressReporter` so the same blocking body serves both the sync
/// and async entry points.
fn digest_raw(
    path: &Path,
    algos: &[HashAlgo],
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
) -> DatResult<FileDigests> {
    struct AtomicProgress<'a>(&'a Arc<AtomicU64>);
    impl ProgressReporter for AtomicProgress<'_> {
        fn start(&self, _: u64, _: &str) {}
        fn inc(&self, delta: u64) {
            self.0
                .fetch_add(delta, std::sync::atomic::Ordering::Relaxed);
        }
        fn finish(&self) {}
    }
    // Safe: AtomicProgress only touches the Arc's atomic, which is Sync.
    let reporter = AtomicProgress(bytes_done);
    hash_file_cancellable(path, algos, &reporter, cancel).map_err(|e| {
        if e.kind() == std::io::ErrorKind::Interrupted {
            DatError::Cancelled
        } else {
            DatError::IoError(e)
        }
    })
}

/// Synchronous inner-stream digest. Decodes the container in flight
/// and returns per-track or single-stream digests, no temp files.
/// Progress is reported directly to `progress`.
pub fn digest_inner(
    path: &Path,
    algos: &[HashAlgo],
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> DatResult<RomDigests> {
    let bytes_done = Arc::new(AtomicU64::new(0));
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    progress.start(file_len(path), &format!("Hashing {name}"));
    let result = digest_dispatch(path, algos, &bytes_done, cancel);
    let done = bytes_done.load(std::sync::atomic::Ordering::Relaxed);
    if done > 0 {
        progress.inc(done);
    }
    progress.finish();
    result
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Async inner-stream digest: runs [`digest_dispatch`] inside
/// `spawn_blocking` and drains its shared byte counter into
/// `progress` every tick, matching the codec decompress entry points.
pub async fn digest_inner_async(
    path: PathBuf,
    algos: Vec<HashAlgo>,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> DatResult<RomDigests> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();
    progress.start(file_len(&path), &format!("Hashing {name}"));

    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();
    let cancel_bg = cancel.clone();
    let path_owned = path.clone();

    let mut handle = tokio::task::spawn_blocking(move || -> DatResult<RomDigests> {
        digest_dispatch(&path_owned, &algos, &bytes_done_bg, &cancel_bg)
    });

    // Drain the shared counter into `progress` every 100 ms while the
    // blocking pass runs. `DatError` carries no `From<JoinError>`, so
    // the join is handled here rather than via `await_with_progress_cancel`.
    use std::sync::atomic::Ordering;
    let joined = loop {
        match tokio::time::timeout(std::time::Duration::from_millis(100), &mut handle).await {
            Ok(joined) => break joined,
            Err(_) => {
                let delta = bytes_done.swap(0, Ordering::Relaxed);
                if delta > 0 {
                    progress.inc(delta);
                }
            }
        }
    };
    let remaining = bytes_done.swap(0, Ordering::Relaxed);
    if remaining > 0 {
        progress.inc(remaining);
    }
    progress.finish();

    let result = match joined {
        Ok(inner) => inner,
        Err(join_err) => return Err(DatError::Container(join_err.to_string())),
    };
    // Cover the race where the pass finished a unit just as the token
    // fired: surface it as Cancelled.
    if result.is_ok() && cancel.is_cancelled() {
        return Err(DatError::Cancelled);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn kind(name: &str) -> InnerStreamKind {
        classify_input(Path::new(name))
    }

    #[test]
    fn classify_extension_table() {
        assert_eq!(kind("game.iso"), InnerStreamKind::Raw);
        assert_eq!(kind("game.bin"), InnerStreamKind::Raw);
        assert_eq!(kind("game.gcm"), InnerStreamKind::Raw);
        assert_eq!(kind("game.nds"), InnerStreamKind::Raw);
        assert_eq!(kind("game.3ds"), InnerStreamKind::Raw);
        assert_eq!(kind("game.cia"), InnerStreamKind::Raw);
        assert_eq!(kind("noext"), InnerStreamKind::Raw);

        assert_eq!(kind("game.rvz"), InnerStreamKind::Rvz);
        assert_eq!(kind("game.wia"), InnerStreamKind::Raw);
        assert_eq!(kind("game.wbfs"), InnerStreamKind::Wbfs);
        assert_eq!(kind("game.cso"), InnerStreamKind::CsoZso);
        assert_eq!(kind("game.zso"), InnerStreamKind::CsoZso);

        assert_eq!(kind("game.z3ds"), InnerStreamKind::Z3ds);
        assert_eq!(kind("game.zcia"), InnerStreamKind::Z3ds);
        assert_eq!(kind("game.zcci"), InnerStreamKind::Z3ds);
        assert_eq!(kind("game.zcxi"), InnerStreamKind::Z3ds);
        assert_eq!(kind("game.z3dsx"), InnerStreamKind::Z3ds);

        assert_eq!(kind("game.chd"), InnerStreamKind::ChdTracks);
        assert_eq!(kind("game.cue"), InnerStreamKind::CueSet);
        assert_eq!(kind("game.nsz"), InnerStreamKind::UnsupportedCompressed);
        assert_eq!(kind("game.xcz"), InnerStreamKind::UnsupportedCompressed);
    }

    #[test]
    fn classify_is_case_insensitive() {
        assert_eq!(kind("GAME.CHD"), InnerStreamKind::ChdTracks);
        assert_eq!(kind("Game.RvZ"), InnerStreamKind::Rvz);
        assert_eq!(kind("thing.CSO"), InnerStreamKind::CsoZso);
        assert_eq!(kind("thing.NSZ"), InnerStreamKind::UnsupportedCompressed);
    }

    #[test]
    fn unsupported_compressed_reports_format() {
        let dir = tempfile::tempdir().unwrap();
        let progress = crate::util::NoProgress;
        let cancel = CancelToken::new();

        let xcz = dir.path().join("game.xcz");
        std::fs::write(&xcz, b"not a legacy container").unwrap();
        let err = digest_inner(&xcz, &[HashAlgo::Sha1], &progress, &cancel).unwrap_err();
        assert!(matches!(
            err,
            DatError::UnsupportedInnerHash { format: "xcz" }
        ));

        let nsz = dir.path().join("game.nsz");
        std::fs::write(&nsz, b"not a legacy container").unwrap();
        let err = digest_inner(&nsz, &[HashAlgo::Sha1], &progress, &cancel).unwrap_err();
        assert!(matches!(
            err,
            DatError::UnsupportedInnerHash { format: "nsz" }
        ));
    }

    #[test]
    fn cue_set_is_caller_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let cue = dir.path().join("game.cue");
        std::fs::write(&cue, b"FILE \"track.bin\" BINARY").unwrap();
        let progress = crate::util::NoProgress;
        let cancel = CancelToken::new();
        let err = digest_inner(&cue, &[HashAlgo::Sha1], &progress, &cancel).unwrap_err();
        assert!(matches!(err, DatError::InvalidInput(_)));
    }

    #[test]
    fn missing_cue_reports_cue_error_without_io() {
        let dir = tempfile::tempdir().unwrap();
        let cue = dir.path().join("does-not-exist.cue");
        let progress = crate::util::NoProgress;
        let cancel = CancelToken::new();
        let err = digest_inner(&cue, &[HashAlgo::Sha1], &progress, &cancel).unwrap_err();
        assert!(matches!(err, DatError::InvalidInput(_)), "{err:?}");
    }

    #[test]
    fn missing_nsz_reports_unsupported_format() {
        let dir = tempfile::tempdir().unwrap();
        let nsz = dir.path().join("does-not-exist.nsz");
        let progress = crate::util::NoProgress;
        let cancel = CancelToken::new();
        let err = digest_inner(&nsz, &[HashAlgo::Sha1], &progress, &cancel).unwrap_err();
        assert!(
            matches!(err, DatError::UnsupportedInnerHash { format: "nsz" }),
            "{err:?}"
        );
    }

    #[test]
    fn raw_digest_matches_hash_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.iso");
        let data: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();

        let algos = [HashAlgo::Crc32, HashAlgo::Sha1, HashAlgo::Sha256];
        let progress = crate::util::NoProgress;
        let cancel = CancelToken::new();
        let d = digest_inner(&path, &algos, &progress, &cancel).unwrap();
        let RomDigests::Single(single) = d else {
            panic!("raw input must be Single");
        };
        let direct = crate::util::hash_file(&path, &algos, &progress).unwrap();
        assert_eq!(single, direct);
    }

    fn legacy_algos() -> [HashAlgo; 3] {
        [HashAlgo::Crc32, HashAlgo::Sha1, HashAlgo::Sha256]
    }

    fn decoded_digest(path: &Path) -> FileDigests {
        let algos = legacy_algos();
        let progress = crate::util::NoProgress;
        let cancel = CancelToken::new();
        let RomDigests::Single(got) = digest_inner(path, &algos, &progress, &cancel).unwrap()
        else {
            panic!("legacy container must be Single");
        };
        got
    }

    fn plain_digest(path: &Path) -> FileDigests {
        crate::util::hash_file(path, &legacy_algos(), &crate::util::NoProgress).unwrap()
    }

    #[test]
    fn gcz_digest_matches_iso() {
        use crate::nintendo::dol::test_fixtures::make_fake_gamecube_iso;
        use crate::nintendo::gcz::test_fixtures::make_gcz;

        let dir = tempfile::tempdir().unwrap();
        let iso_bytes = make_fake_gamecube_iso(5 * 1024 * 1024 + 123);
        let iso = dir.path().join("game.iso");
        std::fs::write(&iso, &iso_bytes).unwrap();
        let gcz = dir.path().join("game.gcz");
        std::fs::write(&gcz, make_gcz(&iso_bytes, 0x8000, 0)).unwrap();

        assert_eq!(decoded_digest(&gcz), plain_digest(&iso));
    }

    #[test]
    fn wia_digest_matches_iso() {
        use crate::nintendo::rvl::test_fixtures::make_fake_wii_iso_with_partition;
        use crate::nintendo::wia::test_fixtures::make_wia;

        let dir = tempfile::tempdir().unwrap();
        let iso_bytes = make_fake_wii_iso_with_partition(2);
        let iso = dir.path().join("game.iso");
        std::fs::write(&iso, &iso_bytes).unwrap();
        let wia = dir.path().join("game.wia");
        std::fs::write(&wia, make_wia(&iso_bytes, 3, 0x20_0000)).unwrap();

        assert_eq!(decoded_digest(&wia), plain_digest(&iso));
    }

    #[test]
    fn nkit_iso_digest_matches_iso() {
        use crate::nintendo::nkit::test_fixtures::{make_fake_gc_fs_iso, make_nkit_gc};

        let dir = tempfile::tempdir().unwrap();
        let iso_bytes = make_fake_gc_fs_iso();
        let iso = dir.path().join("game.iso");
        std::fs::write(&iso, &iso_bytes).unwrap();
        let nkit = dir.path().join("game.nkit.iso");
        std::fs::write(&nkit, make_nkit_gc(&iso_bytes)).unwrap();

        let got = decoded_digest(&nkit);
        // The on-disk NKit container bytes differ from the source ISO,
        // so routing `.nkit.iso` as Raw would silently hash the wrong
        // content. The decoded digest must match the ISO, not the file.
        assert_ne!(std::fs::read(&nkit).unwrap(), std::fs::read(&iso).unwrap());
        assert_ne!(got, plain_digest(&nkit));
        assert_eq!(got, plain_digest(&iso));
    }

    #[test]
    fn nkit_gcz_digest_matches_iso() {
        use crate::nintendo::nkit::test_fixtures::{
            crc_of, make_fake_gc_fs_iso, make_nkit_gc, make_nkit_gcz,
        };

        let dir = tempfile::tempdir().unwrap();
        let iso_bytes = make_fake_gc_fs_iso();
        let iso = dir.path().join("game.iso");
        std::fs::write(&iso, &iso_bytes).unwrap();
        let nkit_bytes = make_nkit_gc(&iso_bytes);
        let nkit_gcz = dir.path().join("game.nkit.gcz");
        std::fs::write(&nkit_gcz, make_nkit_gcz(&nkit_bytes, crc_of(&iso_bytes))).unwrap();

        assert_eq!(decoded_digest(&nkit_gcz), plain_digest(&iso));
    }

    #[test]
    fn nkit_wii_iso_digest_matches_iso() {
        use crate::nintendo::nkit::test_fixtures::{make_fake_wii_fs_iso, make_nkit_wii};

        let dir = tempfile::tempdir().unwrap();
        let iso_bytes = make_fake_wii_fs_iso();
        let iso = dir.path().join("game.iso");
        std::fs::write(&iso, &iso_bytes).unwrap();
        let nkit = dir.path().join("game.nkit.iso");
        std::fs::write(&nkit, make_nkit_wii(&iso_bytes)).unwrap();

        assert_eq!(decoded_digest(&nkit), plain_digest(&iso));
    }

    #[test]
    fn gcz_digest_rejects_corrupted_container() {
        use crate::nintendo::dol::test_fixtures::make_fake_gamecube_iso;
        use crate::nintendo::gcz::test_fixtures::make_gcz;

        let dir = tempfile::tempdir().unwrap();
        let iso_bytes = make_fake_gamecube_iso(1024 * 1024);
        let gcz = dir.path().join("game.gcz");
        let mut bytes = make_gcz(&iso_bytes, 0x8000, 0);
        let n = bytes.len();
        bytes[n - 5] ^= 0xFF;
        std::fs::write(&gcz, &bytes).unwrap();

        let algos = legacy_algos();
        let progress = crate::util::NoProgress;
        let cancel = CancelToken::new();
        let err = digest_inner(&gcz, &algos, &progress, &cancel).unwrap_err();
        assert!(
            matches!(err, DatError::Container(_) | DatError::IoError(_)),
            "{err}"
        );
    }
}
