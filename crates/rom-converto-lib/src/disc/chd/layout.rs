//! Pure CHD layout helpers: the track-metadata text, the per-track
//! span table the CD paths slice hunks with, and the header hash
//! chaining. No I/O, so the convert / extract / digest paths all
//! share one description of the on-disk layout.

use crate::disc::cd::FRAME_SIZE;
use crate::disc::chd::error::{ChdError, ChdResult};
use crate::disc::chd::models::{CHD_METADATA_TAG_CD, ChdMetadataHeader, SHA1_BYTES};
use crate::disc::chd::reader::cue_generator::{ChdTrackInfo, chd_type_datasize};
use crate::disc::chd::writer::metadata::MetadataHash;
use sha1::{Digest, Sha1};

/// The reference implementation pads every track, including a lone
/// final one, to a 4-frame boundary. Zero padding frames count toward
/// the logical size and raw SHA-1, while CHT2 `FRAMES:` records the
/// real count. Measured against its version 0.288, a 10-sector iso
/// produces a CHD with logical size 12 * 2448 and a data SHA-1 over all
/// 12 frames.
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

/// Text of every metadata entry for the first present track-tag family,
/// in priority order: `CHT2`, `CHTR`, `CHGD`, `CHGT`, returned with the
/// tag that was chosen so callers can derive tag-specific behavior (the
/// legacy `CHGT` audio byte order). The reference implementation writes
/// one entry per track. Older rom-converto builds packed every track
/// into one entry, so joining with a space parses both layouts
/// identically. Entries of lower-priority families are ignored.
pub(crate) fn cd_track_metadata(metadata: &[ChdMetadataHeader]) -> Option<([u8; 4], String)> {
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
    Some((*tag, parts.join(" ")))
}

/// Text of the chosen track-tag family, without its tag.
pub(crate) fn cd_track_metadata_text(metadata: &[ChdMetadataHeader]) -> Option<String> {
    cd_track_metadata(metadata).map(|(_, text)| text)
}

/// Whether a CHD's physical stream uses the reference implementation's
/// per-track 4-frame padding. Pre-padding rom-converto builds wrote
/// frames back to back. Identify those files by a physical frame count
/// (`logical_bytes / FRAME_SIZE`) matching the raw `FRAMES:` sum rather
/// than the padded count. Other layouts, including reference output, are
/// treated as padded.
pub(crate) fn chd_layout_is_padded(tracks: &[ChdTrackInfo], physical_frames: u64) -> bool {
    let unpadded: u64 = tracks.iter().map(|t| t.frames as u64).sum();
    let padded: u64 = tracks
        .iter()
        .map(|t| padded_track_frames(t.frames) as u64)
        .sum();
    physical_frames != unpadded || unpadded == padded
}

/// Reject track metadata before anything reads or allocates: the last
/// data-carrying frame (padded start of the last track plus its data
/// frames) must exist in the physical stream (`header.logical_bytes`
/// divided by the frame size). This guards reads, not allocation: the
/// span table of [`chd_track_spans`] scales with the track count, so a
/// forged frame count is never materialized.
pub(crate) fn chd_validate_frame_total(
    tracks: &[ChdTrackInfo],
    padded: bool,
    physical_frames: u64,
) -> ChdResult<()> {
    let spanned = |t: &ChdTrackInfo| {
        if padded {
            padded_track_frames(t.frames) as u64
        } else {
            t.frames as u64
        }
    };
    let Some((last, earlier)) = tracks.split_last() else {
        return Ok(());
    };
    let last_data_end: u64 =
        earlier.iter().map(spanned).sum::<u64>() + (last.frames - last.pad_frames) as u64;
    if last_data_end > physical_frames {
        return Err(ChdError::InvalidTrackMetadata(format!(
            "track metadata claims {last_data_end} frames, the CHD holds {physical_frames}"
        )));
    }
    Ok(())
}

/// Reject a CD-mode CHD whose hunk size is not a whole number of
/// frames: the frame loops assume hunks tile the stream at frame
/// granularity, and a misaligned hunk would slice frames across hunk
/// boundaries.
pub(crate) fn chd_validate_cd_hunks(hunk_bytes: u32) -> ChdResult<()> {
    if hunk_bytes.is_multiple_of(FRAME_SIZE as u32) {
        Ok(())
    } else {
        Err(ChdError::InvalidTrackMetadata(format!(
            "CD hunk size {hunk_bytes} is not a multiple of the {}-byte frame",
            FRAME_SIZE
        )))
    }
}

/// Per-track span of the physical CD stream. Frames resolve to their
/// track through [`chd_span_frame`] instead of per-frame vectors, so
/// memory scales with the track count even when a CD-mode CHD of a DVD
/// ISO declares millions of frames.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ChdTrackSpan {
    /// First physical frame of the track.
    pub start_frame: u64,
    /// Data frames the track carries (`FRAMES:` minus `PAD:`).
    pub data_frames: u64,
    /// One past the track's last physical frame, boundary padding
    /// included.
    pub end_frame: u64,
    /// Payload bytes of one data frame.
    pub datasize: usize,
    /// Track's index in the metadata order.
    pub track: usize,
    /// Whether decoded audio frames of this track are byte-swapped on
    /// the way out: the chosen metadata family swaps and the track is
    /// AUDIO.
    pub swap_audio: bool,
}

/// Span table for the physical CD stream, one entry per track. Runs the
/// [`chd_validate_frame_total`] bound first. `padded` is false only for
/// legacy unpadded layouts (see [`chd_layout_is_padded`]).
pub(crate) fn chd_track_spans(
    tracks: &[ChdTrackInfo],
    padded: bool,
    tag: [u8; 4],
    physical_frames: u64,
) -> ChdResult<Vec<ChdTrackSpan>> {
    chd_validate_frame_total(tracks, padded, physical_frames)?;
    let swap_audio = chd_audio_needs_swap(tag);
    let mut start_frame = 0u64;
    Ok(tracks
        .iter()
        .enumerate()
        .map(|(track, t)| {
            let end_frame = start_frame
                + if padded {
                    padded_track_frames(t.frames) as u64
                } else {
                    t.frames as u64
                };
            let span = ChdTrackSpan {
                start_frame,
                data_frames: (t.frames - t.pad_frames) as u64,
                end_frame,
                datasize: chd_type_datasize(&t.track_type),
                track,
                swap_audio: swap_audio && t.track_type == "AUDIO",
            };
            start_frame = end_frame;
            span
        })
        .collect())
}

/// Resolve one physical frame to `(payload width, track index,
/// swap-audio flag)`. A track's `PAD:` gap frames and boundary padding
/// frames carry width 0 and never swap.
pub(crate) fn chd_span_frame(spans: &[ChdTrackSpan], frame: u64) -> Option<(usize, usize, bool)> {
    let span = spans.get(
        spans
            .partition_point(|s| s.start_frame <= frame)
            .checked_sub(1)?,
    )?;
    if frame - span.start_frame < span.data_frames {
        Some((span.datasize, span.track, span.swap_audio))
    } else {
        Some((0, span.track, false))
    }
}

/// Decoded payload byte count of one track: `(frames - pad_frames) *
/// datasize`, since `PAD:` zero frames are counted inside `FRAMES:` but
/// carry no data. This is the value stored as each track's
/// `FileDigests.size_bytes`.
pub(crate) fn chd_track_decoded_size(track: &ChdTrackInfo) -> u64 {
    (track.frames - track.pad_frames) as u64 * chd_type_datasize(&track.track_type) as u64
}

/// Byte-swap the 16-bit samples of one audio sector in place. The
/// reference implementation stores CD audio big-endian and swaps back
/// on extract. The writer and reader apply this only to audio-track
/// frames; legacy `CHGT` GD-ROM metadata stores audio little-endian
/// and those files skip the swap (see [`chd_audio_needs_swap`]).
pub(crate) fn swap_audio_sector(sector: &mut [u8]) {
    for pair in sector.as_chunks_mut::<2>().0 {
        pair.swap(0, 1);
    }
}

/// Whether CD audio frames must be byte-swapped on the way out. The
/// reference implementation stores CD audio big-endian and swaps back
/// on extract, except when the track metadata family is the legacy
/// `CHGT` GD-ROM tag: those files store audio little-endian, so
/// extract and hash pass the stored bytes through unchanged. `tag` is
/// the family [`cd_track_metadata`] chose, not a mere membership test,
/// so a file carrying both `CHGD` and `CHGT` entries follows `CHGD`.
pub(crate) fn chd_audio_needs_swap(tag: [u8; 4]) -> bool {
    tag != CHD_METADATA_TAG_GD_TRACK_LEGACY
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
    use crate::disc::chd::reader::cue_generator::parse_chd_track_metadata;

    #[test]
    fn padded_track_frames_rounds_to_four() {
        assert_eq!(padded_track_frames(10), 12);
        assert_eq!(padded_track_frames(12), 12);
        assert_eq!(padded_track_frames(1), 4);
    }

    #[test]
    fn track_spans_single_data_track() {
        let tracks = parse_chd_track_metadata("TRACK:1 TYPE:MODE1 FRAMES:20 PREGAP:0").unwrap();
        let spans = chd_track_spans(&tracks, true, CHD_METADATA_TAG_CD, 20).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].start_frame, 0);
        assert_eq!(spans[0].data_frames, 20);
        assert_eq!(spans[0].end_frame, 20);
        assert_eq!(spans[0].datasize, 2048);
        assert!(!spans[0].swap_audio);
        assert_eq!(chd_span_frame(&spans, 19), Some((2048, 0, false)));
        assert_eq!(chd_track_decoded_size(&tracks[0]), 20 * 2048);
    }

    /// A two-track disc with a nonzero pregap on the audio track: the
    /// `FRAMES:` counts are used as-is (pregap frames stored in the CHD
    /// are inside `FRAMES`), and per-frame routing keys on the frame
    /// index so the track boundary lines up.
    #[test]
    fn track_spans_multi_track_with_pregap() {
        let meta =
            "TRACK:1 TYPE:MODE1_RAW FRAMES:300 PREGAP:0 TRACK:2 TYPE:AUDIO FRAMES:500 PREGAP:150";
        let tracks = parse_chd_track_metadata(meta).unwrap();
        let spans = chd_track_spans(&tracks, true, CHD_METADATA_TAG_CD, 800).unwrap();

        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].start_frame, 0);
        assert_eq!(spans[0].end_frame, 300);
        assert_eq!(spans[1].start_frame, 300);
        assert_eq!(spans[1].end_frame, 800);
        assert_eq!(spans[1].track, 1);
        assert_eq!(
            chd_span_frame(&spans, 299),
            Some((2352, 0, false)),
            "data frames of a non-audio track never swap"
        );
        assert_eq!(chd_span_frame(&spans, 300), Some((2352, 1, true)));

        assert_eq!(chd_track_decoded_size(&tracks[0]), 300 * 2352);
        assert_eq!(chd_track_decoded_size(&tracks[1]), 500 * 2352);
    }

    /// Track frame counts that are not 4-frame multiples: each track's
    /// payload frames are followed by width-0 padding frames (10 -> 12,
    /// 5 -> 8, 7 -> 8 physical frames), matching the reference layout.
    #[test]
    fn track_spans_mixed_datasizes() {
        let meta = "TRACK:1 TYPE:MODE1 FRAMES:10 TRACK:2 TYPE:MODE2_FORM1 FRAMES:5 TRACK:3 TYPE:AUDIO FRAMES:7";
        let tracks = parse_chd_track_metadata(meta).unwrap();
        let spans = chd_track_spans(&tracks, true, CHD_METADATA_TAG_CD, 28).unwrap();

        let ends: Vec<u64> = spans.iter().map(|s| s.end_frame).collect();
        assert_eq!(ends, [12, 20, 28]);
        assert_eq!(chd_span_frame(&spans, 9), Some((2048, 0, false)));
        assert_eq!(chd_span_frame(&spans, 11), Some((0, 0, false)));
        assert_eq!(chd_span_frame(&spans, 12), Some((2048, 1, false)));
        assert_eq!(chd_span_frame(&spans, 19), Some((0, 1, false)));
        assert_eq!(chd_span_frame(&spans, 20), Some((2352, 2, true)));
        assert_eq!(chd_span_frame(&spans, 27), Some((0, 2, false)));
    }

    /// CHGD-style track: `PAD:` zero frames sit inside `FRAMES:`, so the
    /// datasize covers only `frames - pad_frames` frames while the
    /// physical stream still spans the padded count (18 -> 20).
    #[test]
    fn track_spans_gd_track_with_pad() {
        let meta = "TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:18 PAD:6 PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0";
        let tracks = parse_chd_track_metadata(meta).unwrap();
        let spans = chd_track_spans(&tracks, true, CHD_METADATA_TAG_CD, 20).unwrap();

        assert_eq!(spans[0].data_frames, 12);
        assert_eq!(spans[0].end_frame, padded_track_frames(18) as u64);
        assert_eq!(chd_span_frame(&spans, 11), Some((2352, 0, false)));
        assert_eq!(chd_span_frame(&spans, 12), Some((0, 0, false)));

        assert_eq!(chd_track_decoded_size(&tracks[0]), 12 * 2352);
        assert!(chd_layout_is_padded(&tracks, 20));
        assert!(!chd_layout_is_padded(&tracks, 18));
    }

    /// A padded track followed by a second track: track 2's frames and
    /// sizes start at the padded frame count of track 1 (18 -> 20), not
    /// at its data frame count (12).
    #[test]
    fn track_spans_second_track_starts_after_the_padded_frames() {
        let meta = "TRACK:1 TYPE:MODE1_RAW FRAMES:18 PAD:6 PREGAP:0 TRACK:2 TYPE:AUDIO FRAMES:8";
        let tracks = parse_chd_track_metadata(meta).unwrap();
        let spans = chd_track_spans(&tracks, true, CHD_METADATA_TAG_CD, 28).unwrap();

        assert_eq!(spans[1].start_frame, 20);
        assert_eq!(spans[1].end_frame, 28);
        assert_eq!(chd_span_frame(&spans, 19), Some((0, 0, false)));
        assert_eq!(chd_span_frame(&spans, 20), Some((2352, 1, true)));

        assert_eq!(chd_track_decoded_size(&tracks[1]), 8 * 2352);
    }

    /// Pre-padding rom-converto builds wrote multi-track streams
    /// unpadded; when the physical frame count matches the raw
    /// `FRAMES:` sum the reader must not inject padding.
    #[test]
    fn track_spans_legacy_unpadded_layout() {
        let meta = "TRACK:1 TYPE:MODE1_RAW FRAMES:10 TRACK:2 TYPE:AUDIO FRAMES:7";
        let tracks = parse_chd_track_metadata(meta).unwrap();
        assert!(!chd_layout_is_padded(&tracks, 17));
        assert!(chd_layout_is_padded(&tracks, 20));

        let spans = chd_track_spans(&tracks, false, CHD_METADATA_TAG_CD, 17).unwrap();
        assert_eq!(spans[0].end_frame, 10);
        assert_eq!(spans[1].end_frame, 17);
        assert_eq!(chd_span_frame(&spans, 9), Some((2352, 0, false)));
        assert_eq!(
            chd_span_frame(&spans, 10),
            Some((2352, 1, true)),
            "the audio track swaps under CD metadata"
        );
    }

    fn metadata_header(tag: [u8; 4], text: &str) -> ChdMetadataHeader {
        let mut data = text.as_bytes().to_vec();
        data.push(0);
        ChdMetadataHeader {
            tag,
            flags: crate::disc::chd::models::CHD_METADATA_FLAG_HASHED,
            reserved: [0; crate::disc::chd::models::CHD_METADATA_RESERVED_BYTES],
            data,
        }
    }

    /// A hostile `FRAMES:` value must be rejected against the CHD's
    /// physical frame count before any span is built.
    #[test]
    fn track_spans_reject_metadata_past_the_physical_frames() {
        let hostile = parse_chd_track_metadata("TRACK:1 TYPE:MODE1_RAW FRAMES:16777215").unwrap();
        assert!(chd_track_spans(&hostile, true, CHD_METADATA_TAG_CD, 12).is_err());
        assert!(chd_track_spans(&hostile, false, CHD_METADATA_TAG_CD, 12).is_err());

        let small = parse_chd_track_metadata("TRACK:1 TYPE:MODE1_RAW FRAMES:10").unwrap();
        assert!(chd_track_spans(&small, true, CHD_METADATA_TAG_CD, 12).is_ok());
        assert!(chd_track_spans(&small, true, CHD_METADATA_TAG_CD, 8).is_err());
    }

    /// A forged header (`logical_bytes` far past the file) cannot
    /// inflate memory: the span table scales with the track count, and
    /// the physical bound rejects the forged total before anything is
    /// built.
    #[test]
    fn track_spans_reject_forged_frame_totals_cheaply() {
        let mut meta = String::new();
        for track in 1..=99 {
            meta.push_str(&format!("TRACK:{track} TYPE:MODE1_RAW FRAMES:16777215 "));
        }
        let tracks = parse_chd_track_metadata(&meta).unwrap();
        assert!(chd_track_spans(&tracks, true, CHD_METADATA_TAG_CD, 2448).is_err());
    }

    /// A CD-mode CHD of a single-layer DVD ISO declares about 2.3M
    /// frames; the span table accepts it without per-frame storage.
    #[test]
    fn track_spans_accept_dvd_sized_cd_metadata() {
        let tracks = parse_chd_track_metadata("TRACK:1 TYPE:MODE1 FRAMES:2295104").unwrap();
        let spans = chd_track_spans(&tracks, true, CHD_METADATA_TAG_CD, 2_295_104).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].data_frames, 2_295_104);
        assert_eq!(spans[0].end_frame, 2_295_104);
        assert_eq!(chd_span_frame(&spans, 2_295_103), Some((2048, 0, false)));
    }

    /// The physical bound covers the last data frame, not the padded
    /// total, so a file whose last track lacks the 4-frame tail padding
    /// still extracts.
    #[test]
    fn track_spans_allow_a_last_track_without_tail_padding() {
        let tracks = parse_chd_track_metadata("TRACK:1 TYPE:MODE1_RAW FRAMES:10").unwrap();
        assert!(chd_track_spans(&tracks, true, CHD_METADATA_TAG_CD, 10).is_ok());
        assert!(chd_track_spans(&tracks, true, CHD_METADATA_TAG_CD, 9).is_err());

        let meta = "TRACK:1 TYPE:MODE1_RAW FRAMES:18 PAD:6 TRACK:2 TYPE:AUDIO FRAMES:8";
        let tracks = parse_chd_track_metadata(meta).unwrap();
        assert!(chd_track_spans(&tracks, true, CHD_METADATA_TAG_CD, 28).is_ok());
        assert!(chd_track_spans(&tracks, true, CHD_METADATA_TAG_CD, 27).is_err());
    }

    /// Legacy `CHGT` GD-ROM metadata stores audio little-endian, so its
    /// AUDIO frames must pass through unswapped. Every other family
    /// swaps them. The decision follows the family the metadata
    /// selector picked: a file carrying both `CHGD` and `CHGT` entries
    /// follows `CHGD`.
    #[test]
    fn track_spans_swap_audio_by_the_chosen_metadata_family() {
        fn family(metadata: &[ChdMetadataHeader]) -> [u8; 4] {
            cd_track_metadata(metadata).unwrap().0
        }

        let tracks = parse_chd_track_metadata("TRACK:1 TYPE:AUDIO FRAMES:10").unwrap();
        let chgt = vec![metadata_header(
            CHD_METADATA_TAG_GD_TRACK_LEGACY,
            "TRACK:1 TYPE:AUDIO FRAMES:10",
        )];
        let spans = chd_track_spans(&tracks, true, family(&chgt), 12).unwrap();
        assert!(!spans[0].swap_audio);
        assert_eq!(chd_span_frame(&spans, 0), Some((2352, 0, false)));

        let chgd = vec![metadata_header(
            CHD_METADATA_TAG_GD_TRACK,
            "TRACK:1 TYPE:AUDIO FRAMES:10",
        )];
        let spans = chd_track_spans(&tracks, true, family(&chgd), 12).unwrap();
        assert!(spans[0].swap_audio);
        assert_eq!(chd_span_frame(&spans, 0), Some((2352, 0, true)));
        assert_eq!(
            chd_span_frame(&spans, 11),
            Some((0, 0, false)),
            "padding frames carry no payload and never swap"
        );

        let mixed = vec![
            metadata_header(CHD_METADATA_TAG_GD_TRACK, "TRACK:1 TYPE:AUDIO FRAMES:10"),
            metadata_header(
                CHD_METADATA_TAG_GD_TRACK_LEGACY,
                "TRACK:1 TYPE:AUDIO FRAMES:10",
            ),
        ];
        assert_eq!(family(&mixed), CHD_METADATA_TAG_GD_TRACK);
        let spans = chd_track_spans(&tracks, true, family(&mixed), 12).unwrap();
        assert!(spans[0].swap_audio);
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
