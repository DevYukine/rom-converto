//! Parsed CUE sheet data model: files, tracks, indices, and time positions.

use crate::disc::cd::{FRAMES_PER_SECOND, SECONDS_PER_MINUTE};

const PRIMARY_INDEX: u8 = 1;
const PREGAP_INDEX: u8 = 0;

/// A parsed CUE sheet: the files it references and the tracks laid out
/// across them.
#[derive(Debug, Clone)]
pub struct CueSheet {
    pub files: Vec<CueFile>,
    pub tracks: Vec<Track>,
}

/// One `FILE` entry in a CUE sheet.
#[derive(Debug, Clone)]
pub struct CueFile {
    pub filename: String,
    pub file_type: FileType,
}

/// One `TRACK` entry in a CUE sheet.
#[derive(Debug, Clone)]
pub struct Track {
    pub number: u8,
    pub track_type: TrackType,
    pub indices: Vec<Index>,
    pub pregap: Option<Msf>,
    pub postgap: Option<Msf>,
    /// Index into `CueSheet::files` of the FILE entry this track belongs to.
    pub file_index: usize,
}

/// One `INDEX` entry within a track.
#[derive(Debug, Clone, Copy)]
pub struct Index {
    /// The CUE `INDEX` number (0 = pregap start, 1 = track start, and so on),
    /// not a position in `Track::indices`.
    pub number: u8,
    pub position: Msf,
}

/// A minutes:seconds:frames CD time position (75 frames per second).
#[derive(Debug, Clone, Copy)]
pub struct Msf {
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
}

impl Msf {
    /// Converts a logical block address to a minutes:seconds:frames position.
    pub fn from_lba(lba: u32) -> Self {
        let frames = (lba % FRAMES_PER_SECOND) as u8;
        let total_seconds = lba / FRAMES_PER_SECOND;
        let seconds = (total_seconds % SECONDS_PER_MINUTE) as u8;
        let minutes = (total_seconds / SECONDS_PER_MINUTE) as u8;
        Self {
            minutes,
            seconds,
            frames,
        }
    }

    /// Converts the position to a logical block address.
    pub fn to_lba(self) -> u32 {
        (self.minutes as u32 * SECONDS_PER_MINUTE + self.seconds as u32) * FRAMES_PER_SECOND
            + self.frames as u32
    }
}

impl std::fmt::Display for Msf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:02}:{:02}:{:02}",
            self.minutes, self.seconds, self.frames
        )
    }
}

impl Track {
    /// Returns the LBA of the track's INDEX 01 (the track's start), or
    /// `None` if it has no INDEX 01.
    pub fn primary_index_lba(&self) -> Option<u32> {
        self.indices
            .iter()
            .find(|index| index.number == PRIMARY_INDEX)
            .map(|index| index.position.to_lba())
    }

    /// Returns the LBA of chdman's track boundary inside a shared FILE:
    /// the INDEX 00 position when present, else the INDEX 01 position.
    pub fn boundary_lba(&self) -> Option<u32> {
        self.indices
            .iter()
            .find(|index| index.number == PREGAP_INDEX)
            .or_else(|| {
                self.indices
                    .iter()
                    .find(|index| index.number == PRIMARY_INDEX)
            })
            .map(|index| index.position.to_lba())
    }

    /// Returns the frames of pregap stored inside the track's own data,
    /// chdman's rule: an INDEX 00 present and no nonzero `PREGAP`
    /// directive. The value is the INDEX 01 LBA minus the INDEX 00 LBA
    /// (saturating; `Some(0)` when the indices are equal), or `None`
    /// when the pregap is not stored: no INDEX 00, no INDEX 01, or a
    /// `PREGAP` directive with a nonzero position.
    pub fn stored_pregap(&self) -> Option<u32> {
        if self.pregap.is_some_and(|pregap| pregap.to_lba() > 0) {
            return None;
        }
        let pregap_start = self
            .indices
            .iter()
            .find(|index| index.number == PREGAP_INDEX)?;
        let track_start = self
            .indices
            .iter()
            .find(|index| index.number == PRIMARY_INDEX)?;
        Some(
            track_start
                .position
                .to_lba()
                .saturating_sub(pregap_start.position.to_lba()),
        )
    }
}

/// CD track mode, as named in a CUE sheet's `TRACK` line.
#[derive(Debug, Clone, Copy)]
pub enum TrackType {
    Audio,
    CdG,
    Mode1_2048,
    Mode1_2352,
    Mode2_2336,
    Mode2_2352,
    CdI2336,
    CdI2352,
}

impl TrackType {
    /// Bytes per sector for this track type: 2352 for most modes, 2448 for
    /// `CdG`, 2048 for `Mode1_2048`, 2336 for `Mode2_2336`/`CdI2336`.
    pub fn block_size(self) -> u32 {
        match self {
            TrackType::Audio
            | TrackType::Mode1_2352
            | TrackType::Mode2_2352
            | TrackType::CdI2352 => 2352,
            TrackType::CdG => 2448,
            TrackType::Mode1_2048 => 2048,
            TrackType::Mode2_2336 | TrackType::CdI2336 => 2336,
        }
    }

    /// Returns the CUE sheet `TRACK` type string, e.g. `"MODE1/2048"`.
    pub fn cue_string(self) -> &'static str {
        match self {
            TrackType::Audio => "AUDIO",
            TrackType::CdG => "CDG",
            TrackType::Mode1_2048 => "MODE1/2048",
            TrackType::Mode1_2352 => "MODE1/2352",
            TrackType::Mode2_2336 => "MODE2/2336",
            TrackType::Mode2_2352 => "MODE2/2352",
            TrackType::CdI2336 => "CDI/2336",
            TrackType::CdI2352 => "CDI/2352",
        }
    }

    /// Returns the CHD track metadata type string, matching chdman's
    /// `get_info_from_type_string` (`MODE2/2336` is `MODE2`, `CDI/2352` is
    /// `MODE2_RAW`). Types with no direct CHD equivalent (`CdG`, `CdI2336`)
    /// fall back to `"MODE1_RAW"`.
    pub fn chd_metadata_type(self) -> &'static str {
        match self {
            TrackType::Audio => "AUDIO",
            TrackType::Mode1_2352 => "MODE1_RAW",
            TrackType::Mode1_2048 => "MODE1",
            TrackType::Mode2_2352 | TrackType::CdI2352 => "MODE2_RAW",
            TrackType::Mode2_2336 => "MODE2",
            _ => "MODE1_RAW",
        }
    }
}

/// CD track data type, as named in a CUE sheet's `FILE` line.
#[derive(Debug, Clone, Copy)]
pub enum FileType {
    Binary,
    Motorola,
    Aiff,
    Wave,
    Mp3,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msf_from_lba_zero() {
        let msf = Msf::from_lba(0);
        assert_eq!((msf.minutes, msf.seconds, msf.frames), (0, 0, 0));
    }

    #[test]
    fn msf_from_lba_one_second() {
        // 75 frames = 1 second
        let msf = Msf::from_lba(75);
        assert_eq!((msf.minutes, msf.seconds, msf.frames), (0, 1, 0));
    }

    #[test]
    fn msf_from_lba_one_minute() {
        // 75 * 60 = 4500
        let msf = Msf::from_lba(4500);
        assert_eq!((msf.minutes, msf.seconds, msf.frames), (1, 0, 0));
    }

    #[test]
    fn msf_from_lba_mixed() {
        // 2 minutes + 33 seconds + 12 frames = (2 * 60 + 33) * 75 + 12 = 11487
        let msf = Msf::from_lba(11487);
        assert_eq!((msf.minutes, msf.seconds, msf.frames), (2, 33, 12));
    }

    #[test]
    fn msf_round_trip() {
        for lba in [0, 1, 74, 75, 150, 4500, 11487, 33750] {
            let msf = Msf::from_lba(lba);
            assert_eq!(msf.to_lba(), lba, "round trip failed for lba={lba}");
        }
    }

    #[test]
    fn to_lba_manual() {
        let msf = Msf {
            minutes: 1,
            seconds: 2,
            frames: 3,
        };
        // (1*60 + 2) * 75 + 3 = 62 * 75 + 3 = 4653
        assert_eq!(msf.to_lba(), 4653);
    }

    #[test]
    fn primary_index_found() {
        let track = Track {
            number: 1,
            track_type: TrackType::Mode1_2352,
            indices: vec![
                Index {
                    number: 0,
                    position: Msf {
                        minutes: 0,
                        seconds: 0,
                        frames: 0,
                    },
                },
                Index {
                    number: 1,
                    position: Msf {
                        minutes: 0,
                        seconds: 2,
                        frames: 0,
                    },
                },
            ],
            pregap: None,
            postgap: None,
            file_index: 0,
        };
        assert_eq!(track.primary_index_lba(), Some(150)); // 2 seconds = 150 frames
    }

    #[test]
    fn primary_index_missing() {
        let track = Track {
            number: 1,
            track_type: TrackType::Audio,
            indices: vec![Index {
                number: 0,
                position: Msf {
                    minutes: 0,
                    seconds: 0,
                    frames: 0,
                },
            }],
            pregap: None,
            postgap: None,
            file_index: 0,
        };
        assert_eq!(track.primary_index_lba(), None);
    }

    /// Audio track with the given `(index number, LBA)` pairs and an
    /// optional `PREGAP` directive in frames.
    fn track(indices: &[(u8, u32)], pregap: Option<u32>) -> Track {
        Track {
            number: 2,
            track_type: TrackType::Audio,
            indices: indices
                .iter()
                .map(|&(number, lba)| Index {
                    number,
                    position: Msf::from_lba(lba),
                })
                .collect(),
            pregap: pregap.map(Msf::from_lba),
            postgap: None,
            file_index: 0,
        }
    }

    #[test]
    fn boundary_lba_prefers_index_00_over_index_01() {
        assert_eq!(track(&[(0, 0), (1, 150)], None).boundary_lba(), Some(0));
        assert_eq!(track(&[(1, 150)], None).boundary_lba(), Some(150));
        assert_eq!(track(&[], None).boundary_lba(), None);
    }

    #[test]
    fn stored_pregap_is_the_index_00_to_01_gap() {
        assert_eq!(track(&[(0, 0), (1, 150)], None).stored_pregap(), Some(150));
        assert_eq!(track(&[(0, 150), (1, 150)], None).stored_pregap(), Some(0));
        assert_eq!(track(&[(1, 150)], None).stored_pregap(), None);
        assert_eq!(track(&[(0, 0)], None).stored_pregap(), None);
    }

    /// A nonzero `PREGAP` directive means the gap is not in the bin even
    /// when an INDEX 00 is present; a zero one changes nothing.
    #[test]
    fn stored_pregap_yields_to_a_nonzero_pregap_directive() {
        assert_eq!(track(&[(0, 0), (1, 150)], Some(150)).stored_pregap(), None);
        assert_eq!(
            track(&[(0, 0), (1, 150)], Some(0)).stored_pregap(),
            Some(150)
        );
    }

    #[test]
    fn chd_metadata_type_mappings() {
        assert_eq!(TrackType::Audio.chd_metadata_type(), "AUDIO");
        assert_eq!(TrackType::Mode1_2352.chd_metadata_type(), "MODE1_RAW");
        assert_eq!(TrackType::Mode1_2048.chd_metadata_type(), "MODE1");
        assert_eq!(TrackType::Mode2_2352.chd_metadata_type(), "MODE2_RAW");
        assert_eq!(TrackType::Mode2_2336.chd_metadata_type(), "MODE2");
        assert_eq!(TrackType::CdI2352.chd_metadata_type(), "MODE2_RAW");
    }

    #[test]
    fn chd_metadata_type_fallback() {
        // CdG and CdI2336 fall back to MODE1_RAW
        assert_eq!(TrackType::CdG.chd_metadata_type(), "MODE1_RAW");
        assert_eq!(TrackType::CdI2336.chd_metadata_type(), "MODE1_RAW");
    }

    #[test]
    fn block_size_mappings() {
        assert_eq!(TrackType::Audio.block_size(), 2352);
        assert_eq!(TrackType::Mode1_2352.block_size(), 2352);
        assert_eq!(TrackType::Mode2_2352.block_size(), 2352);
        assert_eq!(TrackType::CdI2352.block_size(), 2352);
        assert_eq!(TrackType::CdG.block_size(), 2448);
        assert_eq!(TrackType::Mode1_2048.block_size(), 2048);
        assert_eq!(TrackType::Mode2_2336.block_size(), 2336);
        assert_eq!(TrackType::CdI2336.block_size(), 2336);
    }

    #[test]
    fn cue_string_round_trips_with_parser() {
        let parser = crate::disc::cue::CueParser::new("/dev/null");
        for track_type in [
            TrackType::Audio,
            TrackType::CdG,
            TrackType::Mode1_2048,
            TrackType::Mode1_2352,
            TrackType::Mode2_2336,
            TrackType::Mode2_2352,
            TrackType::CdI2336,
            TrackType::CdI2352,
        ] {
            let parsed = parser.parse_track_type(track_type.cue_string()).unwrap();
            assert_eq!(parsed.cue_string(), track_type.cue_string());
        }
    }
}
