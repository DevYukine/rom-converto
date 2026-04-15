use crate::chd::cue::models::Msf;
use crate::chd::error::{ChdError, ChdResult};

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
                _ => {} // SUBTYPE, PGTYPE, PGSUB, POSTGAP: not needed for CUE reconstruction.
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
    // CRLF line endings and the exact indentation below match
    // chdman's `output_track_metadata` in `src/tools/chdman.cpp`
    // so `chd extract` output is byte-identical to `chdman
    // extractcd` for the same input.
    let mut cue = format!("FILE \"{bin_filename}\" BINARY\r\n");
    let mut frame_offset: u32 = 0;

    for track in tracks {
        let cue_type = chd_type_to_cue_type(&track.track_type);
        cue.push_str(&format!(
            "  TRACK {:02} {}\r\n",
            track.track_number, cue_type
        ));

        if track.pregap > 0 {
            let msf = Msf::from_lba(track.pregap);
            cue.push_str(&format!(
                "    PREGAP {:02}:{:02}:{:02}\r\n",
                msf.minutes, msf.seconds, msf.frames
            ));
        }

        let msf = Msf::from_lba(frame_offset);
        cue.push_str(&format!(
            "    INDEX 01 {:02}:{:02}:{:02}\r\n",
            msf.minutes, msf.seconds, msf.frames
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_track() {
        let meta = "TRACK:1 TYPE:MODE1_RAW FRAMES:300 PREGAP:0 SUBTYPE:NONE PGTYPE:MODE1 PGSUB:NONE POSTGAP:0";
        let tracks = parse_chd_track_metadata(meta).unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].track_number, 1);
        assert_eq!(tracks[0].track_type, "MODE1_RAW");
        assert_eq!(tracks[0].frames, 300);
        assert_eq!(tracks[0].pregap, 0);
    }

    #[test]
    fn parse_multiple_tracks() {
        let meta =
            "TRACK:1 TYPE:MODE1_RAW FRAMES:300 PREGAP:0 TRACK:2 TYPE:AUDIO FRAMES:5000 PREGAP:150";
        let tracks = parse_chd_track_metadata(meta).unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].track_number, 1);
        assert_eq!(tracks[0].track_type, "MODE1_RAW");
        assert_eq!(tracks[1].track_number, 2);
        assert_eq!(tracks[1].track_type, "AUDIO");
        assert_eq!(tracks[1].frames, 5000);
        assert_eq!(tracks[1].pregap, 150);
    }

    #[test]
    fn parse_empty_string_fails() {
        let result = parse_chd_track_metadata("");
        assert!(result.is_err());
    }

    #[test]
    fn parse_no_tracks_fails() {
        let result = parse_chd_track_metadata("SUBTYPE:NONE PGTYPE:MODE1");
        assert!(result.is_err());
    }

    #[test]
    fn parse_invalid_track_number_fails() {
        let result = parse_chd_track_metadata("TRACK:abc TYPE:MODE1_RAW FRAMES:100");
        assert!(result.is_err());
    }

    #[test]
    fn type_mappings() {
        assert_eq!(chd_type_to_cue_type("AUDIO"), "AUDIO");
        assert_eq!(chd_type_to_cue_type("MODE1_RAW"), "MODE1/2352");
        assert_eq!(chd_type_to_cue_type("MODE1"), "MODE1/2048");
        assert_eq!(chd_type_to_cue_type("MODE2_RAW"), "MODE2/2352");
        assert_eq!(chd_type_to_cue_type("MODE2_FORM1"), "MODE2/2336");
        assert_eq!(chd_type_to_cue_type("MODE2_FORM2"), "MODE2/2352");
    }

    #[test]
    fn type_unknown_falls_back() {
        assert_eq!(chd_type_to_cue_type("SOMETHING_ELSE"), "MODE1/2352");
    }

    #[test]
    fn generate_single_track_cue() {
        let tracks = vec![ChdTrackInfo {
            track_number: 1,
            track_type: "MODE1_RAW".to_string(),
            frames: 300,
            pregap: 0,
        }];
        let cue = generate_cue_sheet("game.bin", &tracks);
        assert!(cue.starts_with("FILE \"game.bin\" BINARY\r\n"));
        assert!(cue.contains("TRACK 01 MODE1/2352"));
        assert!(cue.contains("INDEX 01 00:00:00"));
        assert!(!cue.contains("PREGAP"));
    }

    #[test]
    fn generate_cue_with_pregap() {
        let tracks = vec![
            ChdTrackInfo {
                track_number: 1,
                track_type: "MODE1_RAW".to_string(),
                frames: 300,
                pregap: 0,
            },
            ChdTrackInfo {
                track_number: 2,
                track_type: "AUDIO".to_string(),
                frames: 5000,
                pregap: 150,
            },
        ];
        let cue = generate_cue_sheet("game.bin", &tracks);
        assert!(cue.contains("TRACK 02 AUDIO"));
        assert!(cue.contains("PREGAP 00:02:00")); // 150 frames = 2 seconds
        // Track 2 starts at frame 300 = 00:04:00
        assert!(cue.contains("INDEX 01 00:04:00"));
    }

    #[test]
    fn generate_cue_frame_offset_advances() {
        let tracks = vec![
            ChdTrackInfo {
                track_number: 1,
                track_type: "MODE1_RAW".to_string(),
                frames: 75,
                pregap: 0,
            },
            ChdTrackInfo {
                track_number: 2,
                track_type: "AUDIO".to_string(),
                frames: 150,
                pregap: 0,
            },
        ];
        let cue = generate_cue_sheet("game.bin", &tracks);
        // Track 1 at 00:00:00, track 2 at 00:01:00 (75 frames = 1 second)
        let lines: Vec<&str> = cue.lines().collect();
        let idx_lines: Vec<&&str> = lines.iter().filter(|l| l.contains("INDEX 01")).collect();
        assert_eq!(idx_lines.len(), 2);
        assert!(idx_lines[0].contains("00:00:00"));
        assert!(idx_lines[1].contains("00:01:00"));
    }
}
