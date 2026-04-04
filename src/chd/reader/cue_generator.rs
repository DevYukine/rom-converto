use crate::chd::error::{ChdError, ChdResult};

const FRAMES_PER_SECOND: u32 = 75;
const SECONDS_PER_MINUTE: u32 = 60;

#[derive(Debug)]
pub(crate) struct ChdTrackInfo {
    pub track_number: u8,
    pub track_type: String,
    pub frames: u32,
    pub pregap: u32,
}

pub(crate) fn parse_chd_track_metadata(metadata_str: &str) -> ChdResult<Vec<ChdTrackInfo>> {
    let mut tracks = Vec::new();
    let mut current: Option<ChdTrackInfo> = None;

    for token in metadata_str.split_whitespace() {
        if let Some((key, value)) = token.split_once(':') {
            match key {
                "TRACK" => {
                    if let Some(track) = current.take() {
                        tracks.push(track);
                    }
                    let number = value
                        .parse::<u8>()
                        .map_err(|_| ChdError::InvalidTrackMetadata(token.to_string()))?;
                    current = Some(ChdTrackInfo {
                        track_number: number,
                        track_type: String::new(),
                        frames: 0,
                        pregap: 0,
                    });
                }
                "TYPE" => {
                    if let Some(ref mut track) = current {
                        track.track_type = value.to_string();
                    }
                }
                "FRAMES" => {
                    if let Some(ref mut track) = current {
                        track.frames = value
                            .parse()
                            .map_err(|_| ChdError::InvalidTrackMetadata(token.to_string()))?;
                    }
                }
                "PREGAP" => {
                    if let Some(ref mut track) = current {
                        track.pregap = value
                            .parse()
                            .map_err(|_| ChdError::InvalidTrackMetadata(token.to_string()))?;
                    }
                }
                _ => {} // SUBTYPE, PGTYPE, PGSUB, POSTGAP — not needed for CUE reconstruction
            }
        }
    }

    if let Some(track) = current {
        tracks.push(track);
    }

    if tracks.is_empty() {
        return Err(ChdError::InvalidTrackMetadata(
            "no tracks found".to_string(),
        ));
    }

    Ok(tracks)
}

pub(crate) fn generate_cue_sheet(bin_filename: &str, tracks: &[ChdTrackInfo]) -> String {
    let mut cue = format!("FILE \"{bin_filename}\" BINARY\n");
    let mut frame_offset: u32 = 0;

    for track in tracks {
        let cue_type = chd_type_to_cue_type(&track.track_type);
        cue.push_str(&format!(
            "  TRACK {:02} {}\n",
            track.track_number, cue_type
        ));

        if track.pregap > 0 {
            let msf = lba_to_msf(track.pregap);
            cue.push_str(&format!("    PREGAP {:02}:{:02}:{:02}\n", msf.0, msf.1, msf.2));
        }

        let msf = lba_to_msf(frame_offset);
        cue.push_str(&format!(
            "    INDEX 01 {:02}:{:02}:{:02}\n",
            msf.0, msf.1, msf.2
        ));

        frame_offset += track.frames;
    }

    cue
}

fn chd_type_to_cue_type(chd_type: &str) -> &'static str {
    match chd_type {
        "AUDIO" => "AUDIO",
        "MODE1_RAW" => "MODE1/2352",
        "MODE1" => "MODE1/2048",
        "MODE2_RAW" => "MODE2/2352",
        "MODE2_FORM1" => "MODE2/2336",
        "MODE2_FORM2" => "MODE2/2352",
        _ => "MODE1/2352",
    }
}

fn lba_to_msf(lba: u32) -> (u8, u8, u8) {
    let frames = lba % FRAMES_PER_SECOND;
    let total_seconds = lba / FRAMES_PER_SECOND;
    let seconds = total_seconds % SECONDS_PER_MINUTE;
    let minutes = total_seconds / SECONDS_PER_MINUTE;
    (minutes as u8, seconds as u8, frames as u8)
}
