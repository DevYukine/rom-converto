//! Streaming digests over a CHD's decoded content, per CHT2 track for
//! CD-mode images and over the flat sector stream for DVD-mode ones.

use crate::cd::FRAME_SIZE;
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::reader::ChdFlavor;
use crate::chd::reader::cue_generator::parse_chd_track_metadata;
use crate::util::CancelToken;
use crate::util::hash::{FileDigests, HashAlgo, MultiHasher};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use super::*;

/// One track's decoded digest set plus its CHT2 identity. `dat`
/// maps this into its own `TrackDigests` at the digest boundary so
/// this module never depends on the `dat` types.
#[derive(Debug, Clone)]
pub struct ChdTrackDigest {
    pub track_number: u8,
    pub track_type: String,
    pub digests: FileDigests,
}

/// Digest a CHD's decoded content in a single streaming pass, no temp
/// files. CD-mode CHDs return one [`ChdTrackDigest`] per CHT2 track
/// plus the whole concatenated-bin digest. DVD-mode CHDs (no CHT2
/// metadata) return an empty track list and the flat decoded ISO
/// digest as `whole`; the caller treats that as a single stream.
///
/// The per-track shaping matches [`extract_from_chd`] exactly (CHT2
/// `FRAMES:` counts, per-frame datasize slicing), so each track's
/// digest equals the corresponding slice of the extracted bin and
/// `whole` equals the extracted bin's digest.
///
/// Synchronous: intended to run inside the caller's `spawn_blocking`.
/// Progress is relayed through the shared `bytes_done` counter, same
/// convention as [`extract_from_chd`]'s blocking body.
pub fn digest_chd_tracks(
    path: &std::path::Path,
    algos: &[HashAlgo],
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
) -> ChdResult<(Vec<ChdTrackDigest>, FileDigests)> {
    use crate::chd::reader::open_chd_sync;
    use crate::chd::reader::worker::{
        ChdExtractWork, ChdExtractedOut, TrackDigestArgs, digest_hunks_dvd, digest_hunks_per_track,
        make_chd_dvd_extract_workers, make_chd_extract_workers,
    };
    use crate::util::worker_pool::{Pool, parallelism};

    let handle = open_chd_sync(path)?;
    let hunk_bytes = handle.header.hunk_bytes as usize;
    let n_threads = parallelism();

    if handle.flavor() == ChdFlavor::Dvd {
        // Flat decoded stream capped at logical_bytes, same coverage
        // as extract_hunks_dvd.
        let logical_bytes = handle.header.logical_bytes;
        let pool: Pool<ChdExtractWork, ChdExtractedOut, ChdError> =
            Pool::spawn(make_chd_dvd_extract_workers(
                n_threads,
                &handle.file,
                hunk_bytes,
                handle.header.compressors(),
            )?);
        let mut whole = MultiHasher::new(algos);
        let result = digest_hunks_dvd(
            &pool,
            &handle.map,
            hunk_bytes,
            logical_bytes,
            &mut whole,
            bytes_done,
            cancel,
        );
        pool.shutdown();
        result?;
        return Ok((Vec::new(), whole.finalize(logical_bytes)));
    }

    let meta_str = cd_track_metadata_text(&handle.metadata)
        .ok_or_else(|| ChdError::InvalidTrackMetadata("no CHT2 metadata found".to_string()))?;
    let tracks = parse_chd_track_metadata(&meta_str)?;

    let padded = chd_layout_is_padded(&tracks, handle.header.logical_bytes / FRAME_SIZE as u64);
    let (frame_sizes, frame_track) = chd_frame_spans(&tracks, padded);
    let frame_audio = chd_frame_audio(&tracks, padded);
    let mut hashers: Vec<MultiHasher> =
        (0..tracks.len()).map(|_| MultiHasher::new(algos)).collect();
    let mut whole = MultiHasher::new(algos);

    let pool: Pool<ChdExtractWork, ChdExtractedOut, ChdError> =
        Pool::spawn(make_chd_extract_workers(
            n_threads,
            &handle.file,
            hunk_bytes,
            handle.header.compressors(),
        )?);
    let result = digest_hunks_per_track(
        &pool,
        TrackDigestArgs {
            map: &handle.map,
            hunk_bytes,
            frame_sizes: &frame_sizes,
            frame_track: &frame_track,
            frame_audio: &frame_audio,
            hashers: &mut hashers,
            whole: &mut whole,
            bytes_done,
            cancel,
        },
    );
    pool.shutdown();
    result?;

    let whole_size: u64 = frame_sizes.iter().map(|&s| s as u64).sum();
    let track_digests = tracks
        .iter()
        .zip(hashers)
        .map(|(t, h)| ChdTrackDigest {
            track_number: t.track_number,
            track_type: t.track_type.clone(),
            digests: h.finalize(chd_track_decoded_size(t)),
        })
        .collect();
    Ok((track_digests, whole.finalize(whole_size)))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::util::NoProgress;
    use test_fixtures::mixed_iso;

    /// digest_chd_tracks over a CD-mode CHD must match the extracted
    /// bin: `whole` equals the bin's hash and the single track's digest
    /// equals the same, with track datasize accounting for padding
    /// (the extracted bin drops padding frames, and the per-track
    /// FRAMES count is used as-is).
    #[tokio::test]
    async fn digest_chd_tracks_cd_matches_extracted_bin() {
        use crate::util::hash::{HashAlgo, hash_file};

        let dir = tempfile::tempdir().unwrap();
        let iso = mixed_iso(13);
        let iso_path = dir.path().join("game.iso");
        std::fs::write(&iso_path, &iso).unwrap();
        let chd_path = dir.path().join("game.chd");
        convert_iso_to_cd_chd(
            &NoProgress,
            iso_path,
            chd_path.clone(),
            ChdOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();

        let out_cue = dir.path().join("restored.cue");
        extract_from_chd(
            &NoProgress,
            chd_path.clone(),
            out_cue.clone(),
            None,
            CancelToken::new(),
        )
        .await
        .unwrap();
        let bin_path = out_cue.with_extension("bin");

        let algos = [HashAlgo::Crc32, HashAlgo::Sha1, HashAlgo::Sha256];
        let bytes_done = Arc::new(AtomicU64::new(0));
        let (tracks, whole) = tokio::task::spawn_blocking({
            let chd_path = chd_path.clone();
            let bytes_done = bytes_done.clone();
            move || digest_chd_tracks(&chd_path, &algos, &bytes_done, &CancelToken::new())
        })
        .await
        .unwrap()
        .unwrap();

        let bin_hash = hash_file(&bin_path, &algos, &NoProgress, &CancelToken::new()).unwrap();
        assert_eq!(whole, bin_hash, "whole digest must equal extracted bin");
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].track_number, 1);
        assert_eq!(tracks[0].digests, bin_hash, "single track equals whole bin");
    }

    /// digest_chd_tracks over a DVD-mode CHD returns an empty track
    /// list and the flat ISO digest, matching a hash of the extracted
    /// iso.
    #[tokio::test]
    async fn digest_chd_tracks_dvd_matches_extracted_iso() {
        use crate::util::hash::{HashAlgo, hash_file};

        let dir = tempfile::tempdir().unwrap();
        let iso = mixed_iso(20);
        let iso_path = dir.path().join("game.iso");
        std::fs::write(&iso_path, &iso).unwrap();
        let chd_path = dir.path().join("game.chd");
        convert_iso_to_chd(
            &NoProgress,
            iso_path,
            chd_path.clone(),
            ChdOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();

        let out_iso = dir.path().join("restored.iso");
        extract_from_chd(
            &NoProgress,
            chd_path.clone(),
            out_iso.clone(),
            None,
            CancelToken::new(),
        )
        .await
        .unwrap();

        let algos = [HashAlgo::Sha1, HashAlgo::Md5];
        let bytes_done = Arc::new(AtomicU64::new(0));
        let (tracks, whole) = tokio::task::spawn_blocking({
            let chd_path = chd_path.clone();
            let bytes_done = bytes_done.clone();
            move || digest_chd_tracks(&chd_path, &algos, &bytes_done, &CancelToken::new())
        })
        .await
        .unwrap()
        .unwrap();

        assert!(tracks.is_empty(), "DVD CHD yields no per-track digests");
        let iso_hash = hash_file(&out_iso, &algos, &NoProgress, &CancelToken::new()).unwrap();
        assert_eq!(whole, iso_hash, "whole digest must equal extracted iso");
    }
}
