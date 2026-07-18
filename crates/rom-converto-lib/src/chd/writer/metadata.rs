use crate::chd::error::ChdResult;
use crate::chd::models::{CHD_METADATA_FLAG_HASHED, ChdMetadataHeader, SHA1_BYTES};
use crate::cue::models::{CueSheet, TrackType};
use binrw::BinWrite;
use sha1::{Digest, Sha1};
use std::io::Cursor;

const TRACK_INFO_SEPARATOR: char = ' ';
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

/// Per-frame audio flags for the CHD stream: `true` where the frame
/// belongs to an AUDIO track. MAME byte-swaps audio sector samples on
/// ingest and swaps them back on extract; the writer consults this to
/// swap the right frames before hashing and compressing. Track spans
/// use the same primary-index frame offsets as the CHT2 metadata.
pub fn cd_audio_frame_map(cue_sheet: &CueSheet, total_frames: u32) -> Vec<bool> {
    let track_starts: Vec<u32> = cue_sheet
        .tracks
        .iter()
        .map(|track| track.primary_index_lba().unwrap_or(0))
        .collect();

    let mut map = vec![false; total_frames as usize];
    for (idx, track) in cue_sheet.tracks.iter().enumerate() {
        if !matches!(track.track_type, TrackType::Audio) {
            continue;
        }
        let start = track_starts[idx].min(total_frames) as usize;
        let end = track_starts
            .get(idx + 1)
            .copied()
            .unwrap_or(total_frames)
            .min(total_frames)
            .max(start as u32) as usize;
        map[start..end].fill(true);
    }
    map
}

pub fn generate_cd_metadata(cue_sheet: &CueSheet, total_frames: u32) -> ChdResult<MetadataBlock> {
    let mut metadata_buffer = Vec::new();

    // CDs use a single metadata entry that lists every track.
    let mut track_info = String::new();
    let track_starts: Vec<u32> = cue_sheet
        .tracks
        .iter()
        .map(|track| track.primary_index_lba().unwrap_or(0))
        .collect();

    for (idx, track) in cue_sheet.tracks.iter().enumerate() {
        if idx > 0 {
            track_info.push(TRACK_INFO_SEPARATOR);
        }

        let start_frame = track_starts[idx];
        let end_frame = track_starts.get(idx + 1).copied().unwrap_or(total_frames);
        let frames = end_frame.saturating_sub(start_frame);
        let pregap = track.pregap.map(|p| p.to_lba()).unwrap_or(0);

        // Format: TRACK:n TYPE:type SUBTYPE:NONE FRAMES:nnn PREGAP:n PGTYPE:type PGSUB:NONE POSTGAP:0
        track_info.push_str(&format!(
            "TRACK:{} TYPE:{} SUBTYPE:NONE FRAMES:{} PREGAP:{} PGTYPE:{} PGSUB:NONE POSTGAP:0",
            track.number,
            track.track_type.chd_metadata_type(),
            frames,
            pregap,
            PREGAP_TYPE
        ));
    }

    let metadata = ChdMetadataHeader::new_cd_metadata(track_info);
    let mut cursor = Cursor::new(&mut metadata_buffer);
    metadata.write(&mut cursor)?;

    let mut hashes = Vec::new();
    if metadata.flags & CHD_METADATA_FLAG_HASHED != 0 {
        let sha1: [u8; SHA1_BYTES] = Sha1::digest(&metadata.data).into();
        hashes.push(MetadataHash {
            tag: metadata.tag,
            sha1,
        });
    }

    Ok(MetadataBlock {
        bytes: metadata_buffer,
        hashes,
    })
}
