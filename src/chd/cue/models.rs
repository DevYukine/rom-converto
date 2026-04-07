use crate::cd::{FRAMES_PER_SECOND, SECONDS_PER_MINUTE};

const PRIMARY_INDEX: u8 = 1;

#[derive(Debug, Clone)]
pub struct CueSheet {
    pub files: Vec<CueFile>,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone)]
pub struct CueFile {
    pub filename: String,
    #[allow(dead_code)] // Parsed from CUE, used for format validation
    pub file_type: FileType,
}

#[derive(Debug, Clone)]
pub struct Track {
    pub number: u8,
    pub track_type: TrackType,
    pub indices: Vec<Index>,
    pub pregap: Option<Msf>,
    pub postgap: Option<Msf>,
}

#[derive(Debug, Clone, Copy)]
pub struct Index {
    pub number: u8,
    pub position: Msf,
}

#[derive(Debug, Clone, Copy)]
pub struct Msf {
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
}

impl Msf {
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

    pub fn to_lba(self) -> u32 {
        (self.minutes as u32 * SECONDS_PER_MINUTE + self.seconds as u32) * FRAMES_PER_SECOND
            + self.frames as u32
    }
}

impl Track {
    pub fn primary_index_lba(&self) -> Option<u32> {
        self.indices
            .iter()
            .find(|index| index.number == PRIMARY_INDEX)
            .map(|index| index.position.to_lba())
    }
}

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
    pub fn chd_metadata_type(self) -> &'static str {
        match self {
            TrackType::Audio => "AUDIO",
            TrackType::Mode1_2352 => "MODE1_RAW",
            TrackType::Mode1_2048 => "MODE1",
            TrackType::Mode2_2352 => "MODE2_RAW",
            TrackType::Mode2_2336 => "MODE2_FORM1",
            _ => "MODE1_RAW",
        }
    }
}

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
        };
        assert_eq!(track.primary_index_lba(), None);
    }

    #[test]
    fn chd_metadata_type_mappings() {
        assert_eq!(TrackType::Audio.chd_metadata_type(), "AUDIO");
        assert_eq!(TrackType::Mode1_2352.chd_metadata_type(), "MODE1_RAW");
        assert_eq!(TrackType::Mode1_2048.chd_metadata_type(), "MODE1");
        assert_eq!(TrackType::Mode2_2352.chd_metadata_type(), "MODE2_RAW");
        assert_eq!(TrackType::Mode2_2336.chd_metadata_type(), "MODE2_FORM1");
    }

    #[test]
    fn chd_metadata_type_fallback() {
        // CdG, CdI2336, CdI2352 all fall back to MODE1_RAW
        assert_eq!(TrackType::CdG.chd_metadata_type(), "MODE1_RAW");
        assert_eq!(TrackType::CdI2336.chd_metadata_type(), "MODE1_RAW");
        assert_eq!(TrackType::CdI2352.chd_metadata_type(), "MODE1_RAW");
    }
}
