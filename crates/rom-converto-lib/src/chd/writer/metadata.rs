use crate::chd::error::ChdResult;
use crate::chd::models::{
    CHD_METADATA_FLAG_HASHED, CHD_METADATA_HEADER_BYTES, CHD_METADATA_RESERVED_BYTES,
    CHD_METADATA_TAG_AV, CHD_METADATA_TAG_AV_LD, CHD_V5_HEADER_SIZE, ChdMetadataHeader, SHA1_BYTES,
};
use crate::cue::models::{CueSheet, TrackType};
use crate::laserdisc::avi::LdParams;
use crate::laserdisc::vbi::VBI_PACKED_BYTES;
use binrw::BinWrite;
use sha1::{Digest, Sha1};
use std::io::Cursor;

// chdman leaves PGTYPE at its MODE1 default unless the pregap data is
// stored in-file, which this writer never does.
const PREGAP_TYPE: &str = "MODE1";

#[derive(Debug, Clone)]
pub struct MetadataHash {
    pub tag: [u8; 4],
    pub sha1: [u8; SHA1_BYTES],
}

#[derive(Debug)]
pub struct MetadataBlock {
    pub bytes: Vec<u8>,
    pub hashes: Vec<MetadataHash>,
}

/// Serialized `DVD ` marker block: chdman's whole DVD metadata is the
/// hashed empty string.
pub fn generate_dvd_metadata() -> ChdResult<MetadataBlock> {
    let metadata = ChdMetadataHeader::new_dvd_metadata();
    let mut bytes = Vec::new();
    metadata.write(&mut Cursor::new(&mut bytes))?;

    let sha1: [u8; SHA1_BYTES] = Sha1::digest(&metadata.data).into();
    Ok(MetadataBlock {
        bytes,
        hashes: vec![MetadataHash {
            tag: metadata.tag,
            sha1,
        }],
    })
}

/// Per-track `(start, frames)` spans in source frames, derived from
/// each track's INDEX 01 offset. The first track always starts at 0
/// so a nonzero first INDEX cannot silently drop the head of the bin.
fn track_spans(cue_sheet: &CueSheet, data_frames: u32) -> Vec<(u32, u32)> {
    let starts: Vec<u32> = cue_sheet
        .tracks
        .iter()
        .enumerate()
        .map(|(idx, track)| {
            if idx == 0 {
                0
            } else {
                track.primary_index_lba().unwrap_or(0).min(data_frames)
            }
        })
        .collect();
    (0..cue_sheet.tracks.len())
        .map(|idx| {
            let start = starts[idx];
            let end = starts
                .get(idx + 1)
                .copied()
                .unwrap_or(data_frames)
                .clamp(start, data_frames);
            (start, end - start)
        })
        .collect()
}

/// Per-frame maps for the physical CHD stream, each track padded to
/// chdman's 4-frame boundary. `.0` is `true` where the frame carries
/// source data (`false` for the zero padding frames appended per
/// track); `.1` is `true` where the frame belongs to an AUDIO track.
/// MAME byte-swaps audio sector samples on ingest and swaps them back
/// on extract; the writer consults `.1` to swap the right frames
/// before hashing and compressing. Track spans use the same
/// primary-index frame offsets as the CHT2 metadata; `data_frames` is
/// the unpadded source frame count.
pub fn cd_frame_layout(cue_sheet: &CueSheet, data_frames: u32) -> (Vec<bool>, Vec<bool>) {
    let mut is_data = Vec::new();
    let mut is_audio = Vec::new();
    for (track, (_, frames)) in cue_sheet
        .tracks
        .iter()
        .zip(track_spans(cue_sheet, data_frames))
    {
        let padded = crate::chd::padded_track_frames(frames);
        let audio = matches!(track.track_type, TrackType::Audio);
        is_data.extend(std::iter::repeat_n(true, frames as usize));
        is_data.extend(std::iter::repeat_n(false, (padded - frames) as usize));
        is_audio.extend(std::iter::repeat_n(audio, padded as usize));
    }
    (is_data, is_audio)
}

/// One CHT2 metadata entry per track, chained through the reserved
/// bytes (the on-disk `next` offset), exactly as chdman's
/// `write_metadata` lays them out right after the V5 header.
pub fn generate_cd_metadata(cue_sheet: &CueSheet, total_frames: u32) -> ChdResult<MetadataBlock> {
    let mut entries = Vec::new();
    for (track, (_, frames)) in cue_sheet
        .tracks
        .iter()
        .zip(track_spans(cue_sheet, total_frames))
    {
        let pregap = track.pregap.map(|p| p.to_lba()).unwrap_or(0);

        // Format: TRACK:n TYPE:type SUBTYPE:NONE FRAMES:nnn PREGAP:n PGTYPE:type PGSUB:NONE POSTGAP:0
        entries.push(ChdMetadataHeader::new_cd_metadata(format!(
            "TRACK:{} TYPE:{} SUBTYPE:NONE FRAMES:{} PREGAP:{} PGTYPE:{} PGSUB:NONE POSTGAP:0",
            track.number,
            track.track_type.chd_metadata_type(),
            frames,
            pregap,
            PREGAP_TYPE
        )));
    }

    chain_and_serialize(entries)
}

/// NTSC and PAL field heights. chdman emits the `AVLD` blob only for
/// these two, so anything else is a plain A/V CHD.
const LD_VBI_FIELD_HEIGHTS: [u32; 2] = [524 / 2, 624 / 2];

/// Size of the `AVLD` VBI blob these parameters call for: one packed
/// record per field at NTSC and PAL field heights, nothing otherwise.
pub fn ld_vbi_bytes(params: &LdParams, vbi_frames: usize) -> usize {
    if LD_VBI_FIELD_HEIGHTS.contains(&params.height) {
        vbi_frames * VBI_PACKED_BYTES
    } else {
        0
    }
}

/// `AVAV` A/V metadata, plus a reserved `AVLD` VBI blob of
/// `vbi_frames` packed records when the field height is NTSC or PAL.
///
/// The blob is emitted zero-filled; the writer backfills it once every
/// field has been parsed. That is safe because the `AVLD` entry is not
/// hashed, so it never feeds the overall SHA-1.
pub fn generate_ld_metadata(params: &LdParams, vbi_frames: usize) -> ChdResult<MetadataBlock> {
    let mut av_data = params.av_metadata().into_bytes();
    av_data.push(0);

    let mut entries = vec![ChdMetadataHeader {
        tag: CHD_METADATA_TAG_AV,
        flags: CHD_METADATA_FLAG_HASHED,
        reserved: [0; CHD_METADATA_RESERVED_BYTES],
        data: av_data,
    }];
    let vbi_bytes = ld_vbi_bytes(params, vbi_frames);
    if vbi_bytes > 0 {
        entries.push(ChdMetadataHeader {
            tag: CHD_METADATA_TAG_AV_LD,
            flags: 0,
            reserved: [0; CHD_METADATA_RESERVED_BYTES],
            data: vec![0; vbi_bytes],
        });
    }

    chain_and_serialize(entries)
}

/// Link the entries through their reserved `next` offsets (the metadata
/// list starts right after the V5 header) and serialize them, hashing
/// every entry that carries the checksum flag.
fn chain_and_serialize(mut entries: Vec<ChdMetadataHeader>) -> ChdResult<MetadataBlock> {
    let mut offset = CHD_V5_HEADER_SIZE as u64;
    let count = entries.len();
    for (i, entry) in entries.iter_mut().enumerate() {
        let size = CHD_METADATA_HEADER_BYTES as u64 + entry.data.len() as u64;
        if i + 1 < count {
            entry.reserved = (offset + size).to_be_bytes();
        }
        offset += size;
    }

    let mut metadata_buffer = Vec::new();
    let mut hashes = Vec::new();
    let mut cursor = Cursor::new(&mut metadata_buffer);
    for entry in &entries {
        entry.write(&mut cursor)?;
        if entry.flags & CHD_METADATA_FLAG_HASHED != 0 {
            let sha1: [u8; SHA1_BYTES] = Sha1::digest(&entry.data).into();
            hashes.push(MetadataHash {
                tag: entry.tag,
                sha1,
            });
        }
    }

    Ok(MetadataBlock {
        bytes: metadata_buffer,
        hashes,
    })
}
