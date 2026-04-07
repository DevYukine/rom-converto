use crate::chd::cue::error::{CueError, CueResult};
use crate::chd::cue::models::{CueFile, CueSheet, FileType, Index, Msf, Track, TrackType};
use std::io::{BufRead, Cursor};
use std::path::{Path, PathBuf};

pub mod error;
pub mod models;

#[derive(Debug)]
pub struct CueParser {
    cue_path: PathBuf,
}

impl CueParser {
    pub fn new(cue_path: impl AsRef<Path>) -> Self {
        Self {
            cue_path: cue_path.as_ref().to_path_buf(),
        }
    }

    pub async fn parse(&self) -> CueResult<CueSheet> {
        let data = tokio::fs::read(&self.cue_path).await?;
        let reader = Cursor::new(data);

        let mut cue_sheet = CueSheet {
            files: Vec::new(),
            tracks: Vec::new(),
        };

        let mut current_track: Option<Track> = None;

        for line in reader.lines() {
            let line = line?;
            let line = line.trim();

            if line.is_empty() || line.starts_with("REM") {
                continue;
            }

            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.is_empty() {
                continue;
            }

            match parts[0] {
                "FILE" => {
                    if let Some(track) = current_track.take() {
                        cue_sheet.tracks.push(track);
                    }

                    let filename = self.extract_quoted_string(line)?;
                    let file_type = self.parse_file_type(
                        parts
                            .last()
                            .ok_or(CueError::InvalidFileType("empty".to_string()))?,
                    )?;

                    let cue_file = CueFile {
                        filename,
                        file_type,
                    };
                    cue_sheet.files.push(cue_file);
                }
                "TRACK" => {
                    if let Some(track) = current_track.take() {
                        cue_sheet.tracks.push(track);
                    }

                    let number = parts[1].parse::<u8>()?;
                    let track_type = self.parse_track_type(parts[2])?;

                    current_track = Some(Track {
                        number,
                        track_type,
                        indices: Vec::new(),
                        pregap: None,
                        postgap: None,
                    });
                }
                "INDEX" => {
                    if let Some(track) = &mut current_track {
                        let number = parts[1].parse::<u8>()?;
                        let position = self.parse_msf(parts[2])?;

                        track.indices.push(Index { number, position });
                    }
                }
                "PREGAP" => {
                    if let Some(track) = &mut current_track {
                        track.pregap = Some(self.parse_msf(parts[1])?);
                    }
                }
                "POSTGAP" => {
                    if let Some(track) = &mut current_track {
                        track.postgap = Some(self.parse_msf(parts[1])?);
                    }
                }
                _ => {}
            }
        }

        if let Some(track) = current_track {
            cue_sheet.tracks.push(track);
        }

        Ok(cue_sheet)
    }

    fn extract_quoted_string(&self, line: &str) -> CueResult<String> {
        let start = line.find('"').ok_or(CueError::MissingOpeningQuote)?;
        let end = line.rfind('"').ok_or(CueError::MissingClosingQuote)?;
        if start >= end {
            return Err(CueError::InvalidQuotedString(line.to_string()));
        }

        Ok(line[start + 1..end].to_string())
    }

    fn parse_file_type(&self, type_str: &str) -> CueResult<FileType> {
        match type_str {
            "BINARY" => Ok(FileType::Binary),
            "MOTOROLA" => Ok(FileType::Motorola),
            "AIFF" => Ok(FileType::Aiff),
            "WAVE" => Ok(FileType::Wave),
            "MP3" => Ok(FileType::Mp3),
            _ => Err(CueError::InvalidFileType(type_str.to_string())),
        }
    }

    fn parse_track_type(&self, type_str: &str) -> CueResult<TrackType> {
        match type_str {
            "AUDIO" => Ok(TrackType::Audio),
            "CDG" => Ok(TrackType::CdG),
            "MODE1/2048" => Ok(TrackType::Mode1_2048),
            "MODE1/2352" => Ok(TrackType::Mode1_2352),
            "MODE2/2336" => Ok(TrackType::Mode2_2336),
            "MODE2/2352" => Ok(TrackType::Mode2_2352),
            "CDI/2336" => Ok(TrackType::CdI2336),
            "CDI/2352" => Ok(TrackType::CdI2352),
            _ => Err(CueError::InvalidTrackType(type_str.to_string())),
        }
    }

    fn parse_msf(&self, msf_str: &str) -> CueResult<Msf> {
        let parts: Vec<&str> = msf_str.split(':').collect();
        if parts.len() != 3 {
            return Err(CueError::InvalidMsfFormat(msf_str.to_string()));
        }

        Ok(Msf {
            minutes: parts[0].parse()?,
            seconds: parts[1].parse()?,
            frames: parts[2].parse()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser() -> CueParser {
        CueParser::new("/dev/null")
    }

    #[test]
    fn extract_quoted_string_normal() {
        let result = parser()
            .extract_quoted_string(r#"FILE "track.bin" BINARY"#)
            .unwrap();
        assert_eq!(result, "track.bin");
    }

    #[test]
    fn extract_quoted_string_with_spaces() {
        let result = parser()
            .extract_quoted_string(r#"FILE "my game file.bin" BINARY"#)
            .unwrap();
        assert_eq!(result, "my game file.bin");
    }

    #[test]
    fn extract_quoted_string_no_quotes_fails() {
        assert!(
            parser()
                .extract_quoted_string("FILE track.bin BINARY")
                .is_err()
        );
    }

    #[test]
    fn extract_quoted_string_single_quote_fails() {
        // Only one quote, rfind == find, start >= end
        assert!(
            parser()
                .extract_quoted_string(r#"FILE "track.bin BINARY"#)
                .is_err()
        );
    }

    #[test]
    fn parse_file_type_all_variants() {
        let p = parser();
        assert!(matches!(p.parse_file_type("BINARY"), Ok(FileType::Binary)));
        assert!(matches!(
            p.parse_file_type("MOTOROLA"),
            Ok(FileType::Motorola)
        ));
        assert!(matches!(p.parse_file_type("AIFF"), Ok(FileType::Aiff)));
        assert!(matches!(p.parse_file_type("WAVE"), Ok(FileType::Wave)));
        assert!(matches!(p.parse_file_type("MP3"), Ok(FileType::Mp3)));
    }

    #[test]
    fn parse_file_type_unknown_fails() {
        assert!(parser().parse_file_type("FLAC").is_err());
    }

    #[test]
    fn parse_track_type_all_variants() {
        let p = parser();
        assert!(matches!(p.parse_track_type("AUDIO"), Ok(TrackType::Audio)));
        assert!(matches!(p.parse_track_type("CDG"), Ok(TrackType::CdG)));
        assert!(matches!(
            p.parse_track_type("MODE1/2048"),
            Ok(TrackType::Mode1_2048)
        ));
        assert!(matches!(
            p.parse_track_type("MODE1/2352"),
            Ok(TrackType::Mode1_2352)
        ));
        assert!(matches!(
            p.parse_track_type("MODE2/2336"),
            Ok(TrackType::Mode2_2336)
        ));
        assert!(matches!(
            p.parse_track_type("MODE2/2352"),
            Ok(TrackType::Mode2_2352)
        ));
        assert!(matches!(
            p.parse_track_type("CDI/2336"),
            Ok(TrackType::CdI2336)
        ));
        assert!(matches!(
            p.parse_track_type("CDI/2352"),
            Ok(TrackType::CdI2352)
        ));
    }

    #[test]
    fn parse_track_type_unknown_fails() {
        assert!(parser().parse_track_type("MODE3/2048").is_err());
    }

    #[test]
    fn parse_msf_valid() {
        let msf = parser().parse_msf("00:02:33").unwrap();
        assert_eq!((msf.minutes, msf.seconds, msf.frames), (0, 2, 33));
    }

    #[test]
    fn parse_msf_zeros() {
        let msf = parser().parse_msf("00:00:00").unwrap();
        assert_eq!((msf.minutes, msf.seconds, msf.frames), (0, 0, 0));
    }

    #[test]
    fn parse_msf_wrong_format_fails() {
        assert!(parser().parse_msf("00:02").is_err());
        assert!(parser().parse_msf("00:02:33:44").is_err());
    }
}
