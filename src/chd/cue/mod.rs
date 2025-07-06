use crate::chd::cue::error::{CueError, CueResult};
use crate::chd::cue::models::{CueFile, CueSheet, FileType, Index, MSF, Track, TrackType};
use std::io::{BufRead, Cursor};
use std::path::{Path, PathBuf};

pub mod error;
pub mod models;

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

        let mut current_file: Option<CueFile> = None;
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

                    let filename = self.extract_quoted_string(&line)?;
                    let file_type = self.parse_file_type(parts.last().unwrap())?;

                    current_file = Some(CueFile {
                        filename,
                        file_type,
                    });
                    if let Some(file) = &current_file {
                        cue_sheet.files.push(file.clone());
                    }
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
        let start = line.find('"').ok_or(CueError::MissingQuoteError(
            "Missing opening quote".to_string(),
        ))?;
        let end = line.rfind('"').ok_or(CueError::MissingQuoteError(
            "Missing closing quote".to_string(),
        ))?;
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

    fn parse_msf(&self, msf_str: &str) -> CueResult<MSF> {
        let parts: Vec<&str> = msf_str.split(':').collect();
        if parts.len() != 3 {
            return Err(CueError::InvalidMSFFormat(msf_str.to_string()));
        }

        Ok(MSF {
            minutes: parts[0].parse()?,
            seconds: parts[1].parse()?,
            frames: parts[2].parse()?,
        })
    }
}
