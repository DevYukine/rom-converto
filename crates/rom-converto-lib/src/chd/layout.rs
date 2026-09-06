//! Pure CHD layout helpers: the track-metadata text, the padded
//! frame maps the CD paths slice hunks with, and the header hash
//! chaining. No I/O, so the convert / extract / digest paths all
//! share one description of the on-disk layout.

use crate::chd::models::{CHD_METADATA_TAG_CD, ChdMetadataHeader, SHA1_BYTES};
use crate::chd::reader::cue_generator::{ChdTrackInfo, chd_type_datasize};
use crate::chd::writer::metadata::MetadataHash;
use sha1::{Digest, Sha1};

/// chdman pads every track, including a lone final one, to a 4-frame
/// boundary; the zero padding frames count into the logical size and
/// the raw SHA-1, while CHT2 `FRAMES:` records the real count.
/// Measured against chdman 0.288: a 10-sector iso produces a CHD with
/// logical size 12 * 2448 and a data SHA-1 over all 12 frames.
pub(crate) const CD_TRACK_PADDING: u32 = 4;

pub(crate) fn padded_track_frames(data_sectors: u32) -> u32 {
    data_sectors.div_ceil(CD_TRACK_PADDING) * CD_TRACK_PADDING
}

/// Older text track-metadata tags, checked as fallbacks after `CHT2`.
/// `CHTR` predates `CHT2`'s pregap/postgap fields; `CHGD`/`CHGT` are
/// GD-ROM equivalents. `CHCD` (old CD-ROM TOC) is binary, not text,
/// and is intentionally never matched here.
pub(crate) const CHD_METADATA_TAG_CD_TRACK: [u8; 4] = *b"CHTR";
pub(crate) const CHD_METADATA_TAG_GD_TRACK: [u8; 4] = *b"CHGD";
pub(crate) const CHD_METADATA_TAG_GD_TRACK_LEGACY: [u8; 4] = *b"CHGT";

/// Concatenated text of every metadata entry for the first present
/// track-tag family, in `CHT2`, `CHTR`, `CHGD`, `CHGT` priority order.
/// chdman writes one entry per track, while older rom-converto builds
/// packed every track into a single entry; joining with a space parses
/// both layouts identically. Families are never mixed within one file.
pub(crate) fn cd_track_metadata_text(metadata: &[ChdMetadataHeader]) -> Option<String> {
    const TAG_PRIORITY: [[u8; 4]; 4] = [
        CHD_METADATA_TAG_CD,
        CHD_METADATA_TAG_CD_TRACK,
        CHD_METADATA_TAG_GD_TRACK,
        CHD_METADATA_TAG_GD_TRACK_LEGACY,
    ];
    let tag = TAG_PRIORITY
        .iter()
        .find(|tag| metadata.iter().any(|m| m.tag == **tag))?;
    let parts: Vec<String> = metadata
        .iter()
        .filter(|m| m.tag == *tag)
        .map(|m| {
            String::from_utf8_lossy(&m.data)
                .trim_end_matches('\0')
                .trim()
                .to_string()
        })
        .collect();
    Some(parts.join(" "))
}

/// Whether a CHD's physical stream uses chdman's per-track 4-frame
/// padding. Pre-padding rom-converto builds wrote the frames
/// back-to-back; those files are recognized by a physical frame count
/// (`logical_bytes / FRAME_SIZE`) that matches the raw `FRAMES:` sum
/// and not the padded one. Anything else, chdman output included, is
/// treated as padded.
pub(crate) fn chd_layout_is_padded(tracks: &[ChdTrackInfo], physical_frames: u64) -> bool {
    let unpadded: u64 = tracks.iter().map(|t| t.frames as u64).sum();
    let padded: u64 = tracks
        .iter()
        .map(|t| padded_track_frames(t.frames) as u64)
        .sum();
    physical_frames != unpadded || unpadded == padded
}

/// Per-frame span map for the physical CHD CD stream: `frame_sizes[i]`
/// is the payload width of frame `i` and `frame_track[i]` is the index
/// (into `tracks`) of the track that owns frame `i`. chdman pads every
/// track to a 4-frame boundary, so each track's `FRAMES:` payload
/// frames are followed by padding frames of width 0 that contribute
/// nothing to the output; `padded` is false only for legacy unpadded
/// layouts (see [`chd_layout_is_padded`]). Both vecs are laid out
/// exactly as `extract_hunks` shapes the stream, so hashing frame by
/// frame through them reproduces the bin `chdman extractcd` writes.
/// Pure so it is unit-testable against a synthetic CHT2 metadata
/// string.
pub(crate) fn chd_frame_spans(tracks: &[ChdTrackInfo], padded: bool) -> (Vec<usize>, Vec<usize>) {
    let mut frame_sizes = Vec::new();
    let mut frame_track = Vec::new();
    for (i, t) in tracks.iter().enumerate() {
        let physical = if padded {
            padded_track_frames(t.frames)
        } else {
            t.frames
        } as usize;
        let datasize = chd_type_datasize(&t.track_type);
        frame_sizes.extend(std::iter::repeat_n(datasize, t.frames as usize));
        frame_sizes.extend(std::iter::repeat_n(0, physical - t.frames as usize));
        frame_track.extend(std::iter::repeat_n(i, physical));
    }
    (frame_sizes, frame_track)
}

/// Decoded payload byte count of one track: `frames * datasize`. This
/// is the value stored as each track's `FileDigests.size_bytes`.
pub(crate) fn chd_track_decoded_size(track: &ChdTrackInfo) -> u64 {
    track.frames as u64 * chd_type_datasize(&track.track_type) as u64
}

/// Byte-swap the 16-bit samples of one audio sector in place. chdman
/// stores CD audio big-endian and swaps back on extract; the writer
/// and reader apply this to audio-track frames only.
pub(crate) fn swap_audio_sector(sector: &mut [u8]) {
    for pair in sector.as_chunks_mut::<2>().0 {
        pair.swap(0, 1);
    }
}

/// Per-frame audio flags for the physical CD stream, laid out like
/// [`chd_frame_spans`] (per-track padding frames included when
/// `padded`): `true` where the frame's track is AUDIO.
pub(crate) fn chd_frame_audio(tracks: &[ChdTrackInfo], padded: bool) -> Vec<bool> {
    tracks
        .iter()
        .flat_map(|t| {
            let physical = if padded {
                padded_track_frames(t.frames)
            } else {
                t.frames
            } as usize;
            std::iter::repeat_n(t.track_type == "AUDIO", physical)
        })
        .collect()
}

pub(crate) fn compute_overall_sha1(
    raw_sha1: [u8; SHA1_BYTES],
    metadata_hashes: &[MetadataHash],
) -> [u8; SHA1_BYTES] {
    let mut overall = Sha1::new();
    overall.update(raw_sha1);

    if !metadata_hashes.is_empty() {
        let mut hashes = metadata_hashes.to_vec();
        hashes.sort_by(|a, b| a.tag.cmp(&b.tag).then(a.sha1.cmp(&b.sha1)));
        for hash in hashes {
            overall.update(hash.tag);
            overall.update(hash.sha1);
        }
    }

    overall.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chd::reader::cue_generator::parse_chd_track_metadata;

    #[test]
    fn padded_track_frames_rounds_to_four() {
        assert_eq!(padded_track_frames(10), 12);
        assert_eq!(padded_track_frames(12), 12);
        assert_eq!(padded_track_frames(1), 4);
    }

    #[test]
    fn frame_spans_single_data_track() {
        let tracks = parse_chd_track_metadata("TRACK:1 TYPE:MODE1 FRAMES:20 PREGAP:0").unwrap();
        let (sizes, track) = chd_frame_spans(&tracks, true);
        assert_eq!(sizes.len(), 20);
        assert_eq!(track.len(), 20);
        assert!(sizes.iter().all(|&s| s == 2048));
        assert!(track.iter().all(|&t| t == 0));
        assert_eq!(chd_track_decoded_size(&tracks[0]), 20 * 2048);
    }

    /// A two-track disc with a nonzero pregap on the audio track: the
    /// `FRAMES:` counts are used as-is (pregap frames stored in the CHD
    /// are inside `FRAMES`), and per-frame routing keys on the frame
    /// index so the differing datasizes (2352 data, 2352 audio) and the
    /// track boundary line up.
    #[test]
    fn frame_spans_multi_track_with_pregap() {
        let meta =
            "TRACK:1 TYPE:MODE1_RAW FRAMES:300 PREGAP:0 TRACK:2 TYPE:AUDIO FRAMES:500 PREGAP:150";
        let tracks = parse_chd_track_metadata(meta).unwrap();
        let (sizes, track) = chd_frame_spans(&tracks, true);

        assert_eq!(sizes.len(), 800);
        assert_eq!(track.len(), 800);
        assert!(sizes[..300].iter().all(|&s| s == 2352));
        assert!(track[..300].iter().all(|&t| t == 0));
        assert!(sizes[300..].iter().all(|&s| s == 2352));
        assert!(track[300..].iter().all(|&t| t == 1));

        assert_eq!(chd_track_decoded_size(&tracks[0]), 300 * 2352);
        assert_eq!(chd_track_decoded_size(&tracks[1]), 500 * 2352);
        let whole: u64 = sizes.iter().map(|&s| s as u64).sum();
        assert_eq!(whole, 800 * 2352);
    }

    /// Track frame counts that are not 4-frame multiples: each track's
    /// payload frames are followed by width-0 padding frames (10 -> 12,
    /// 5 -> 8, 7 -> 8 physical frames), matching chdman's layout.
    #[test]
    fn frame_spans_mixed_datasizes() {
        let meta = "TRACK:1 TYPE:MODE1 FRAMES:10 TRACK:2 TYPE:MODE2_FORM1 FRAMES:5 TRACK:3 TYPE:AUDIO FRAMES:7";
        let tracks = parse_chd_track_metadata(meta).unwrap();
        let (sizes, track) = chd_frame_spans(&tracks, true);
        assert_eq!(sizes.len(), 28);
        assert_eq!(&sizes[0..10], &[2048; 10]);
        assert_eq!(&sizes[10..12], &[0; 2]);
        assert_eq!(&sizes[12..17], &[2048; 5]);
        assert_eq!(&sizes[17..20], &[0; 3]);
        assert_eq!(&sizes[20..27], &[2352; 7]);
        assert_eq!(sizes[27], 0);
        assert_eq!(&track[9..13], &[0, 0, 0, 1]);
        assert_eq!(&track[19..21], &[1, 2]);

        let audio = chd_frame_audio(&tracks, true);
        assert_eq!(audio.len(), 28);
        assert!(!audio[19]);
        assert!(audio[20..28].iter().all(|&a| a));
    }

    /// Pre-padding rom-converto builds wrote multi-track streams
    /// unpadded; when the physical frame count matches the raw
    /// `FRAMES:` sum the reader must not inject padding.
    #[test]
    fn frame_spans_legacy_unpadded_layout() {
        let meta = "TRACK:1 TYPE:MODE1_RAW FRAMES:10 TRACK:2 TYPE:AUDIO FRAMES:7";
        let tracks = parse_chd_track_metadata(meta).unwrap();
        assert!(!chd_layout_is_padded(&tracks, 17));
        assert!(chd_layout_is_padded(&tracks, 20));

        let (sizes, track) = chd_frame_spans(&tracks, false);
        assert_eq!(sizes.len(), 17);
        assert!(sizes.iter().all(|&s| s == 2352));
        assert_eq!(&track[9..11], &[0, 1]);
        let audio = chd_frame_audio(&tracks, false);
        assert_eq!(audio.len(), 17);
        assert!(!audio[9]);
        assert!(audio[10]);
    }

    fn metadata_header(tag: [u8; 4], text: &str) -> ChdMetadataHeader {
        let mut data = text.as_bytes().to_vec();
        data.push(0);
        ChdMetadataHeader {
            tag,
            flags: crate::chd::models::CHD_METADATA_FLAG_HASHED,
            reserved: [0; crate::chd::models::CHD_METADATA_RESERVED_BYTES],
            data,
        }
    }

    #[test]
    fn cd_track_metadata_text_prefers_cht2_over_chtr() {
        let metadata = vec![
            metadata_header(
                CHD_METADATA_TAG_CD_TRACK,
                "TRACK:1 TYPE:MODE1_RAW FRAMES:300",
            ),
            metadata_header(CHD_METADATA_TAG_CD, "TRACK:1 TYPE:AUDIO FRAMES:150"),
        ];
        let text = cd_track_metadata_text(&metadata).expect("CHT2 present");
        assert_eq!(text, "TRACK:1 TYPE:AUDIO FRAMES:150");
    }

    #[test]
    fn cd_track_metadata_text_none_for_chcd_only() {
        let metadata = vec![metadata_header(*b"CHCD", "binary toc, not text")];
        assert!(cd_track_metadata_text(&metadata).is_none());
    }
}
