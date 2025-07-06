use crate::chd::cue::models::{CueSheet, TrackType};
use crate::chd::error::ChdResult;
use crate::chd::models::ChdMetadataHeader;
use binrw::BinWrite;
use std::io::Cursor;

pub fn generate_cd_metadata(cue_sheet: &CueSheet, total_frames: u32) -> ChdResult<Vec<u8>> {
    let mut metadata_buffer = Vec::new();

    // For CDs, we typically have one metadata entry for all tracks
    let mut track_info = String::new();

    for (idx, track) in cue_sheet.tracks.iter().enumerate() {
        if idx > 0 {
            track_info.push(' '); // Space between tracks
        }

        let track_type = match track.track_type {
            TrackType::Audio => "AUDIO",
            TrackType::Mode1_2352 => "MODE1_RAW",
            TrackType::Mode1_2048 => "MODE1",
            TrackType::Mode2_2352 => "MODE2_RAW",
            TrackType::Mode2_2336 => "MODE2_FORM1",
            _ => "MODE1_RAW",
        };

        // Calculate frames
        let start_frame = if let Some(index) = track.indices.iter().find(|i| i.number == 1) {
            index.position.to_lba()
        } else {
            0
        };

        let end_frame = if idx + 1 < cue_sheet.tracks.len() {
            if let Some(next_track) = cue_sheet.tracks.get(idx + 1) {
                if let Some(index) = next_track.indices.iter().find(|i| i.number == 1) {
                    index.position.to_lba()
                } else {
                    total_frames
                }
            } else {
                total_frames
            }
        } else {
            total_frames
        };

        let frames = end_frame - start_frame;
        let pregap = track.pregap.map(|p| p.to_lba()).unwrap_or(0);
        let pgtype = if pregap > 0 { "MODE1" } else { "V" };

        // Format: TRACK:n TYPE:type SUBTYPE:NONE FRAMES:nnn PREGAP:n PGTYPE:type PGSUB:NONE POSTGAP:0
        track_info.push_str(&format!(
            "TRACK:{} TYPE:{} SUBTYPE:NONE FRAMES:{} PREGAP:{} PGTYPE:{} PGSUB:NONE POSTGAP:0",
            track.number, track_type, frames, pregap, pgtype
        ));
    }

    let metadata = ChdMetadataHeader::new_cd_metadata(track_info);
    let mut cursor = Cursor::new(&mut metadata_buffer);
    metadata.write(&mut cursor)?;

    Ok(metadata_buffer)
}
