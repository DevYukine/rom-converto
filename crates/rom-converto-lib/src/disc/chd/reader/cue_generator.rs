use crate::disc::chd::error::{ChdError, ChdResult};
use crate::disc::cue::models::Msf;

/// Upper bound on a CD track's `FRAMES:` value: above any real track
/// (the largest, a CD-mode CHD of a dual-layer DVD ISO, declares about
/// 4.2M frames) while keeping the padding math clear of u32 overflow
/// on hostile metadata.
pub(crate) const MAX_TRACK_FRAMES: u32 = 0x00FF_FFFF;

/// Track count a CD can carry: the subchannel only encodes two BCD
/// digits, so more entries mean corrupt or hostile metadata.
pub(crate) const MAX_CD_TRACKS: usize = 99;

#[derive(Debug, Clone, Default)]
pub(crate) struct ChdTrackInfo {
    pub track_number: u8,
    pub track_type: String,
    pub frames: u32,
    /// Zero frames counted inside `FRAMES:` (the GD-ROM `PAD:` field);
    /// they sit at the end of the track and carry no data payload.
    pub pad_frames: u32,
    pub pregap: u32,
    pub subtype: Option<String>,
    pub pgtype: Option<String>,
    pub pgsub: Option<String>,
    pub postgap: Option<u32>,
}

pub(crate) fn parse_chd_track_metadata(metadata_str: &str) -> ChdResult<Vec<ChdTrackInfo>> {
    let mut tracks = Vec::new();
    let mut current: Option<ChdTrackInfo> = None;

    for token in metadata_str.split_whitespace() {
        if let Some((key, value)) = token.split_once(':') {
            match key {
                "TRACK" => {
                    if let Some(track) = current.take() {
                        validate_track(&track)?;
                        tracks.push(track);
                    }
                    let number = value
                        .parse::<u8>()
                        .map_err(|_| ChdError::InvalidTrackMetadata(token.to_string()))?;
                    current = Some(ChdTrackInfo {
                        track_number: number,
                        ..ChdTrackInfo::default()
                    });
                }
                "TYPE" => {
                    if let Some(track) = &mut current {
                        track.track_type = value.to_string();
                    }
                }
                "FRAMES" => {
                    if let Some(track) = &mut current {
                        let frames: u32 = value
                            .parse()
                            .map_err(|_| ChdError::InvalidTrackMetadata(token.to_string()))?;
                        if frames > MAX_TRACK_FRAMES {
                            return Err(ChdError::InvalidTrackMetadata(token.to_string()));
                        }
                        track.frames = frames;
                    }
                }
                "PAD" => {
                    if let Some(track) = &mut current {
                        track.pad_frames = value
                            .parse()
                            .map_err(|_| ChdError::InvalidTrackMetadata(token.to_string()))?;
                    }
                }
                "PREGAP" => {
                    if let Some(track) = &mut current {
                        track.pregap = value
                            .parse()
                            .map_err(|_| ChdError::InvalidTrackMetadata(token.to_string()))?;
                    }
                }
                "SUBTYPE" => {
                    if let Some(track) = &mut current {
                        track.subtype = Some(value.to_string());
                    }
                }
                "PGTYPE" => {
                    if let Some(track) = &mut current {
                        track.pgtype = Some(value.to_string());
                    }
                }
                "PGSUB" => {
                    if let Some(track) = &mut current {
                        track.pgsub = Some(value.to_string());
                    }
                }
                "POSTGAP" => {
                    if let Some(track) = &mut current {
                        track.postgap = Some(
                            value
                                .parse()
                                .map_err(|_| ChdError::InvalidTrackMetadata(token.to_string()))?,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    if let Some(track) = current {
        validate_track(&track)?;
        tracks.push(track);
    }

    if tracks.is_empty() {
        return Err(ChdError::InvalidTrackMetadata(
            "no tracks found".to_string(),
        ));
    }

    if tracks.len() > MAX_CD_TRACKS {
        return Err(ChdError::InvalidTrackMetadata(format!(
            "CD has {} tracks, maximum is {MAX_CD_TRACKS}",
            tracks.len()
        )));
    }

    Ok(tracks)
}

/// `PAD:` zero frames are counted inside `FRAMES:`, so a track claiming
/// more pad than frames has contradictory metadata. Key order within an
/// entry is not fixed, so this runs once the entry is complete.
fn validate_track(track: &ChdTrackInfo) -> ChdResult<()> {
    if track.pad_frames > track.frames {
        return Err(ChdError::InvalidTrackMetadata(format!(
            "TRACK:{} PAD:{} > FRAMES:{}",
            track.track_number, track.pad_frames, track.frames
        )));
    }
    Ok(())
}

pub(crate) fn generate_cue_sheet(bin_names: &[String], tracks: &[ChdTrackInfo]) -> String {
    // The line content and indentation below follow the upstream
    // `output_track_metadata`. CRLF endings follow its older builds
    // (0.289 writes LF) and are kept so existing extracts stay
    // byte-identical for single-track discs. Each track gets its own FILE.
    // A `V` prefix on PGTYPE marks a pregap stored inside the track's
    // own frames, so it is written as INDEX 00 rather than a PREGAP line.
    debug_assert_eq!(bin_names.len(), tracks.len());
    let mut cue = String::new();

    for (bin_name, track) in bin_names.iter().zip(tracks) {
        cue.push_str(&format!("FILE \"{bin_name}\" BINARY\r\n"));
        let cue_type = chd_type_to_cue_type(&track.track_type);
        cue.push_str(&format!(
            "  TRACK {:02} {}\r\n",
            track.track_number, cue_type
        ));

        let stored_pregap =
            track.pregap > 0 && track.pgtype.as_deref().is_some_and(|t| t.starts_with('V'));
        if stored_pregap {
            let msf = Msf::from_lba(0);
            cue.push_str(&format!(
                "    INDEX 00 {:02}:{:02}:{:02}\r\n",
                msf.minutes, msf.seconds, msf.frames
            ));
        } else if track.pregap > 0 {
            let msf = Msf::from_lba(track.pregap);
            cue.push_str(&format!(
                "    PREGAP {:02}:{:02}:{:02}\r\n",
                msf.minutes, msf.seconds, msf.frames
            ));
        }

        let index01 = if stored_pregap { track.pregap } else { 0 };
        let msf = Msf::from_lba(index01);
        cue.push_str(&format!(
            "    INDEX 01 {:02}:{:02}:{:02}\r\n",
            msf.minutes, msf.seconds, msf.frames
        ));

        // The upstream `output_track_metadata` writes a POSTGAP line only
        // for a nonzero frame count, so zero stays silent.
        if let Some(postgap) = track.postgap.filter(|postgap| *postgap > 0) {
            let msf = Msf::from_lba(postgap);
            cue.push_str(&format!(
                "    POSTGAP {:02}:{:02}:{:02}\r\n",
                msf.minutes, msf.seconds, msf.frames
            ));
        }
    }

    cue
}

fn chd_type_to_cue_type(chd_type: &str) -> &'static str {
    match chd_type {
        "AUDIO" => "AUDIO",
        "MODE1_RAW" => "MODE1/2352",
        "MODE1" => "MODE1/2048",
        "MODE2_RAW" => "MODE2/2352",
        "MODE2" | "MODE2_FORM_MIX" => "MODE2/2336",
        "MODE2_FORM1" => "MODE2/2048",
        "MODE2_FORM2" => "MODE2/2324",
        _ => "MODE1/2352",
    }
}

/// Bytes of frame payload the extracted bin carries per track type,
/// matching the datasizes in the upstream `get_info_from_type_string`
/// (`cdrom.cpp`). Mirrors [`chd_type_to_cue_type`] so the generated cue
/// sheet and the bin widths always agree.
pub(crate) fn chd_type_datasize(chd_type: &str) -> usize {
    match chd_type {
        "MODE1" | "MODE2_FORM1" => 2048,
        "MODE2_FORM2" => 2324,
        "MODE2" | "MODE2_FORM_MIX" => 2336,
        _ => 2352,
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
        assert_eq!(tracks[0].subtype.as_deref(), Some("NONE"));
        assert_eq!(tracks[0].pgtype.as_deref(), Some("MODE1"));
        assert_eq!(tracks[0].pgsub.as_deref(), Some("NONE"));
        assert_eq!(tracks[0].postgap, Some(0));
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
    fn parse_chtr_track_has_no_gap_fields() {
        // CHTR shape: "TRACK:%d TYPE:%s SUBTYPE:%s FRAMES:%d", no gap fields at all.
        let meta = "TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:300";
        let tracks = parse_chd_track_metadata(meta).unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].track_number, 1);
        assert_eq!(tracks[0].track_type, "MODE1_RAW");
        assert_eq!(tracks[0].frames, 300);
        assert_eq!(tracks[0].pregap, 0);
        assert_eq!(tracks[0].pgtype, None);
        assert_eq!(tracks[0].pgsub, None);
        assert_eq!(tracks[0].postgap, None);
    }

    #[test]
    fn parse_chgd_track_parses_pad() {
        // CHGD shape adds a "PAD:%d" key ahead of the gap fields.
        let meta = "TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:300 PAD:12 PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0";
        let tracks = parse_chd_track_metadata(meta).unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].frames, 300);
        assert_eq!(tracks[0].pad_frames, 12);
        assert_eq!(tracks[0].pregap, 0);
        assert_eq!(tracks[0].pgtype.as_deref(), Some("MODE1"));
        assert_eq!(tracks[0].pgsub.as_deref(), Some("NONE"));
        assert_eq!(tracks[0].postgap, Some(0));
    }

    #[test]
    fn parse_chd_track_without_pad_defaults_to_zero() {
        let tracks =
            parse_chd_track_metadata("TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:300").unwrap();
        assert_eq!(tracks[0].pad_frames, 0);
    }

    #[test]
    fn parse_pad_exceeding_frames_fails() {
        // Key order is not fixed: PAD seen before FRAMES must still be
        // rejected once the entry is complete.
        let result =
            parse_chd_track_metadata("TRACK:1 TYPE:MODE1_RAW PAD:12 FRAMES:10 SUBTYPE:NONE");
        assert!(result.is_err());
    }

    /// The entry is only validated once it is complete, so a `PAD:` key
    /// seen before `FRAMES:` parses like any other key.
    #[test]
    fn parse_pad_before_frames_is_accepted() {
        let tracks =
            parse_chd_track_metadata("TRACK:1 TYPE:MODE1_RAW PAD:6 FRAMES:10 SUBTYPE:NONE")
                .unwrap();
        assert_eq!(tracks[0].frames, 10);
        assert_eq!(tracks[0].pad_frames, 6);
    }

    /// `PAD:` frames are counted inside `FRAMES:`, so `PAD == FRAMES`
    /// is the boundary case of a track with no data frames at all.
    #[test]
    fn parse_pad_equal_to_frames_is_accepted() {
        let tracks =
            parse_chd_track_metadata("TRACK:1 TYPE:MODE1_RAW FRAMES:6 PAD:6 SUBTYPE:NONE").unwrap();
        assert_eq!(tracks[0].frames, 6);
        assert_eq!(tracks[0].pad_frames, 6);
    }

    #[test]
    fn parse_invalid_track_number_fails() {
        let result = parse_chd_track_metadata("TRACK:abc TYPE:MODE1_RAW FRAMES:100");
        assert!(result.is_err());
    }

    #[test]
    fn parse_oversized_frames_fails() {
        let result = parse_chd_track_metadata("TRACK:1 TYPE:MODE1_RAW FRAMES:4294967295");
        assert!(result.is_err());
    }

    /// A CD cannot carry more than [`MAX_CD_TRACKS`] tracks; every
    /// reader that parses the metadata shares the cap.
    #[test]
    fn parse_rejects_more_than_the_cd_track_cap() {
        let ok: String = (1..=MAX_CD_TRACKS)
            .map(|t| format!("TRACK:{t} TYPE:MODE1_RAW FRAMES:10 "))
            .collect();
        assert_eq!(parse_chd_track_metadata(&ok).unwrap().len(), MAX_CD_TRACKS);

        let hostile: String = (1..=MAX_CD_TRACKS + 1)
            .map(|t| format!("TRACK:{t} TYPE:MODE1_RAW FRAMES:10 "))
            .collect();
        assert!(parse_chd_track_metadata(&hostile).is_err());
    }

    #[test]
    fn type_mappings() {
        assert_eq!(chd_type_to_cue_type("AUDIO"), "AUDIO");
        assert_eq!(chd_type_to_cue_type("MODE1_RAW"), "MODE1/2352");
        assert_eq!(chd_type_to_cue_type("MODE1"), "MODE1/2048");
        assert_eq!(chd_type_to_cue_type("MODE2_RAW"), "MODE2/2352");
        assert_eq!(chd_type_to_cue_type("MODE2"), "MODE2/2336");
        assert_eq!(chd_type_to_cue_type("MODE2_FORM1"), "MODE2/2048");
        assert_eq!(chd_type_to_cue_type("MODE2_FORM2"), "MODE2/2324");
        assert_eq!(chd_type_to_cue_type("MODE2_FORM_MIX"), "MODE2/2336");
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
            ..ChdTrackInfo::default()
        }];
        let cue = generate_cue_sheet(&["game.bin".to_string()], &tracks);
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
                ..ChdTrackInfo::default()
            },
            ChdTrackInfo {
                track_number: 2,
                track_type: "AUDIO".to_string(),
                frames: 5000,
                pregap: 150,
                ..ChdTrackInfo::default()
            },
        ];
        let cue = generate_cue_sheet(
            &[
                "game (Track 1).bin".to_string(),
                "game (Track 2).bin".to_string(),
            ],
            &tracks,
        );
        assert!(cue.contains(
            "FILE \"game (Track 2).bin\" BINARY\r\n  TRACK 02 AUDIO\r\n    PREGAP 00:02:00\r\n    INDEX 01 00:00:00\r\n"
        ));
    }

    #[test]
    fn generate_cue_with_stored_pregap() {
        let tracks = vec![
            ChdTrackInfo {
                track_number: 1,
                track_type: "MODE1_RAW".to_string(),
                frames: 300,
                pregap: 0,
                ..ChdTrackInfo::default()
            },
            ChdTrackInfo {
                track_number: 2,
                track_type: "AUDIO".to_string(),
                frames: 5000,
                pregap: 150,
                pgtype: Some("VAUDIO".to_string()),
                ..ChdTrackInfo::default()
            },
        ];
        let cue = generate_cue_sheet(
            &[
                "game (Track 1).bin".to_string(),
                "game (Track 2).bin".to_string(),
            ],
            &tracks,
        );
        assert!(cue.contains("    INDEX 00 00:00:00\r\n    INDEX 01 00:02:00\r\n"));
        assert!(!cue.contains("PREGAP"));
    }

    #[test]
    fn generate_cue_tracks_start_in_their_own_files() {
        let tracks = vec![
            ChdTrackInfo {
                track_number: 1,
                track_type: "MODE1_RAW".to_string(),
                frames: 75,
                pregap: 0,
                ..ChdTrackInfo::default()
            },
            ChdTrackInfo {
                track_number: 2,
                track_type: "AUDIO".to_string(),
                frames: 150,
                pregap: 0,
                ..ChdTrackInfo::default()
            },
        ];
        let cue = generate_cue_sheet(
            &[
                "game (Track 1).bin".to_string(),
                "game (Track 2).bin".to_string(),
            ],
            &tracks,
        );
        assert_eq!(
            cue,
            "FILE \"game (Track 1).bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\n\
             FILE \"game (Track 2).bin\" BINARY\r\n  TRACK 02 AUDIO\r\n    INDEX 01 00:00:00\r\n"
        );
    }

    #[test]
    fn datasize_matches_cue_type_widths() {
        assert_eq!(chd_type_datasize("MODE1"), 2048);
        assert_eq!(chd_type_datasize("MODE1_RAW"), 2352);
        assert_eq!(chd_type_datasize("MODE2_RAW"), 2352);
        assert_eq!(chd_type_datasize("MODE2"), 2336);
        assert_eq!(chd_type_datasize("MODE2_FORM1"), 2048);
        assert_eq!(chd_type_datasize("MODE2_FORM2"), 2324);
        assert_eq!(chd_type_datasize("MODE2_FORM_MIX"), 2336);
        assert_eq!(chd_type_datasize("AUDIO"), 2352);
        assert_eq!(chd_type_datasize("BOGUS"), 2352);
    }
}
