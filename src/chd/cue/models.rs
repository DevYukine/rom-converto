// src/cue/models
const SECONDS_PER_MINUTE: u32 = 60;
const FRAMES_PER_SECOND: u32 = 75;
const PRIMARY_INDEX: u8 = 1;

#[derive(Debug, Clone)]
pub struct CueSheet {
    pub files: Vec<CueFile>,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone)]
pub struct CueFile {
    pub filename: String,
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
