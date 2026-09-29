//! CUE sheet parsing and multi-bin merging for CD disc images.

use crate::disc::cue::error::{CueError, CueResult};
use crate::disc::cue::models::{CueFile, CueSheet, FileType, Index, Msf, Track, TrackType};
use crate::util::bounded_line;
use std::io::{BufRead, BufReader, Cursor};
use std::path::{Path, PathBuf};

pub mod error;
pub mod merge;
pub mod models;
pub mod to_iso;

/// Parses a `.cue` sheet file into a [`CueSheet`].
#[derive(Debug)]
pub struct CueParser {
    cue_path: PathBuf,
}

impl CueParser {
    /// Creates a parser for the `.cue` file at `cue_path`.
    pub fn new(cue_path: impl AsRef<Path>) -> Self {
        Self {
            cue_path: cue_path.as_ref().to_path_buf(),
        }
    }

    pub async fn parse(&self) -> CueResult<CueSheet> {
        let file = tokio::fs::File::open(&self.cue_path).await?;
        let file = file.into_std().await;
        self.parse_reader(BufReader::new(file))
    }

    /// Parses a CUE sheet from a buffered reader, handling `FILE`, `TRACK`,
    /// `INDEX`, `PREGAP`, and `POSTGAP` directives; `REM` lines and blank
    /// lines are skipped. Retains track metadata but not the complete text.
    pub fn parse_reader<R: BufRead>(&self, mut reader: R) -> CueResult<CueSheet> {
        let mut cue_sheet = CueSheet {
            files: Vec::new(),
            tracks: Vec::new(),
        };

        let mut current_track: Option<Track> = None;

        while let Some(line) = bounded_line::read_line(&mut reader)? {
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

                    let filename = if line.contains('"') {
                        self.extract_quoted_string(line)?
                    } else {
                        parts
                            .get(1)
                            .filter(|_| parts.len() >= 3)
                            .ok_or(CueError::MissingOpeningQuote)?
                            .to_string()
                    };
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
                    if parts.len() < 3 {
                        return Err(CueError::InvalidTrackType(line.to_string()));
                    }
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
                        file_index: cue_sheet.files.len().saturating_sub(1),
                    });
                }
                "INDEX" => {
                    if parts.len() < 3 {
                        return Err(CueError::InvalidMsfFormat(line.to_string()));
                    }
                    if let Some(track) = &mut current_track {
                        let number = parts[1].parse::<u8>()?;
                        let position = self.parse_msf(parts[2])?;

                        track.indices.push(Index { number, position });
                    }
                }
                "PREGAP" => {
                    if parts.len() < 2 {
                        return Err(CueError::InvalidMsfFormat(line.to_string()));
                    }
                    if let Some(track) = &mut current_track {
                        track.pregap = Some(self.parse_msf(parts[1])?);
                    }
                }
                "POSTGAP" => {
                    if parts.len() < 2 {
                        return Err(CueError::InvalidMsfFormat(line.to_string()));
                    }
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

    /// Parses CUE sheet bytes line by line; see [`Self::parse_reader`].
    pub fn parse_bytes(&self, data: &[u8]) -> CueResult<CueSheet> {
        self.parse_reader(Cursor::new(data))
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

/// Sums the on-disk size of the FILE entries a CUE sheet references, resolved
/// relative to the CUE's own directory. Used to estimate output size for space
/// preflight checks (raw sectors are larger than the ISO/output they produce,
/// so this is a safe overestimate).
pub async fn referenced_files_size(cue_path: impl AsRef<Path>) -> CueResult<u64> {
    let cue_path = cue_path.as_ref();
    let cue_sheet = CueParser::new(cue_path).parse().await?;
    let cue_dir = cue_path.parent().unwrap_or(Path::new("."));

    let mut total = 0u64;
    for file in &cue_sheet.files {
        let bin_path = cue_dir.join(&file.filename);
        total += tokio::fs::metadata(&bin_path).await?.len();
    }
    Ok(total)
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

    #[tokio::test]
    async fn parse_truncated_track_line_errors_not_panics() {
        let dir = tempfile::tempdir().unwrap();
        let cue = dir.path().join("bad.cue");
        tokio::fs::write(&cue, "FILE \"a.bin\" BINARY\r\n  TRACK 01\r\n")
            .await
            .unwrap();
        assert!(CueParser::new(&cue).parse().await.is_err());
    }

    #[tokio::test]
    async fn parse_truncated_index_line_errors_not_panics() {
        let dir = tempfile::tempdir().unwrap();
        let cue = dir.path().join("bad.cue");
        tokio::fs::write(
            &cue,
            "FILE \"a.bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01\r\n",
        )
        .await
        .unwrap();
        assert!(CueParser::new(&cue).parse().await.is_err());
    }
    #[test]
    fn streamed_sheet_preserves_quoted_files_and_multitrack_indices() {
        let mut text = String::with_capacity(64 * 1024);
        for _ in 0..4096 {
            text.push_str("REM generated comment line\n");
        }
        text.push_str(
            "FILE \"first track.bin\" BINARY\n\
             TRACK 01 MODE1/2352\n\
             INDEX 00 00:00:00\n\
             INDEX 01 00:02:00\n\
             FILE \"second track.bin\" BINARY\n\
             TRACK 02 AUDIO\n\
             INDEX 01 00:00:00\n",
        );
        let sheet = parser().parse_reader(Cursor::new(text.as_bytes())).unwrap();
        assert_eq!(sheet.files[0].filename, "first track.bin");
        assert_eq!(sheet.files[1].filename, "second track.bin");
        assert_eq!(sheet.tracks.len(), 2);
        assert_eq!(sheet.tracks[0].file_index, 0);
        assert_eq!(sheet.tracks[0].indices.len(), 2);
        assert_eq!(sheet.tracks[1].file_index, 1);
        let unquoted = parser()
            .parse_bytes(b"FILE disk.bin BINARY\nTRACK 01 MODE1/2048\nINDEX 01 00:00:00\n")
            .unwrap();
        assert_eq!(unquoted.files[0].filename, "disk.bin");
    }
    #[test]
    fn accepts_cue_rem_lines_over_64_kib() {
        let mut cue = String::from("REM ");
        cue.push_str(&"x".repeat(64 * 1024));
        // A directive padded far past any sane width must still parse whole.
        cue.push_str("\nFILE \"disk.bin\"");
        cue.push_str(&" ".repeat(65 * 1024));
        cue.push_str("BINARY\nTRACK 01 MODE1/2048\nINDEX 01 00:00:00\n");
        let parsed = parser().parse_reader(Cursor::new(cue.as_bytes())).unwrap();
        let expected = parser()
            .parse_bytes(b"FILE disk.bin BINARY\nTRACK 01 MODE1/2048\nINDEX 01 00:00:00\n")
            .unwrap();
        assert_eq!(parsed.files[0].filename, expected.files[0].filename);
        let parsed_msf = parsed.tracks[0].indices[0].position;
        let expected_msf = expected.tracks[0].indices[0].position;
        assert_eq!(
            (parsed_msf.minutes, parsed_msf.seconds, parsed_msf.frames),
            (
                expected_msf.minutes,
                expected_msf.seconds,
                expected_msf.frames
            )
        );
    }
}
