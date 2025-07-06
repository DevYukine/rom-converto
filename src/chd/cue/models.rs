// src/cue/models
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
    pub pregap: Option<MSF>,
    pub postgap: Option<MSF>,
}

#[derive(Debug, Clone, Copy)]
pub struct Index {
    pub number: u8,
    pub position: MSF,
}

#[derive(Debug, Clone, Copy)]
pub struct MSF {
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
}

impl MSF {
    pub fn to_lba(&self) -> u32 {
        (self.minutes as u32 * 60 + self.seconds as u32) * 75 + self.frames as u32 - 150
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

#[derive(Debug, Clone, Copy)]
pub enum FileType {
    Binary,
    Motorola,
    Aiff,
    Wave,
    Mp3,
}
