//! Errors from building a PS1 `EBOOT.PBP`.

use std::path::PathBuf;

use thiserror::Error;

/// Errors from building a PS1 `EBOOT.PBP`.
#[derive(Debug, Error)]
pub enum PsxPbpError {
    /// Wraps an underlying I/O failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// The blocking build task panicked or was aborted.
    #[error(transparent)]
    JoinError(#[from] tokio::task::JoinError),

    /// The cue sheet could not be parsed.
    #[error(transparent)]
    Cue(#[from] crate::disc::cue::error::CueError),

    /// A CHD disc could not be extracted to its tracks.
    #[error(transparent)]
    Chd(#[from] crate::disc::chd::error::ChdError),

    /// A cue `FILE` is not raw binary sector data.
    #[error("{path}: FILE {file} is not BINARY; a PS1 EBOOT needs raw sectors")]
    NotBinary { path: PathBuf, file: String },

    /// A track mode other than 2352-byte data or audio sectors.
    #[error("{path}: track {track} is {mode}; a PS1 EBOOT needs 2352-byte sectors")]
    UnsupportedTrackMode {
        path: PathBuf,
        track: u8,
        mode: &'static str,
    },

    /// A track has no `INDEX 01`, so its start is unknown.
    #[error("{path}: track {track} has no INDEX 01")]
    MissingTrackStart { path: PathBuf, track: u8 },

    /// A `PREGAP` or `POSTGAP` directive asks for silence the image does
    /// not store.
    #[error(
        "{path}: track {track} has a PREGAP or POSTGAP whose silence is not in the image files"
    )]
    PregapNotStored { path: PathBuf, track: u8 },

    /// A track names no `FILE` line before it.
    #[error("{path}: track {track} comes before any FILE line")]
    TrackWithoutFile { path: PathBuf, track: u8 },

    /// More tracks than a CD's table of contents holds.
    #[error("{path}: {count} tracks; a CD holds at most 99")]
    TooManyTracks { path: PathBuf, count: usize },

    /// A track number a CD's table of contents cannot hold.
    #[error("{path}: track number {track} is outside 1 to 99")]
    TrackNumberOutOfRange { path: PathBuf, track: u8 },

    /// The data track has a stored pregap, so its file system does not
    /// start the image where the PS1 emulator reads it.
    #[error(
        "{path}: track 1 starts {start} sectors into its file; a PS1 EBOOT needs the data track at the start of the image"
    )]
    DataTrackNotAtStart { path: PathBuf, start: u32 },

    /// A disc longer than a table of contents can address.
    #[error("{path}: {sectors} sectors; a CD's table of contents ends at 99:59:74 ({max} sectors)")]
    DiscTooLong {
        path: PathBuf,
        sectors: u64,
        max: u64,
    },

    /// A `.bin` holds a partial sector.
    #[error("{path}: {size} bytes is not a whole number of 2352-byte sectors")]
    PartialSector { path: PathBuf, size: u64 },

    /// A track starts past the end of its file.
    #[error("{path}: track {track} starts past the end of its file")]
    TrackPastEnd { path: PathBuf, track: u8 },

    /// The first track carries no data, so there is no PS1 file system.
    #[error("{0}: the first track is not a data track")]
    NoDataTrack(PathBuf),

    /// The disc is a PS2 or PSP disc, which the PS1 emulator cannot run.
    #[error("{path}: a {kind} disc; a PS1 EBOOT needs a PS1 disc")]
    NotPs1Disc { path: PathBuf, kind: &'static str },

    /// No disc ID could be read from `SYSTEM.CNF` and none was given.
    #[error("{0}: SYSTEM.CNF names no disc ID; pass one with --disc-id")]
    NoDiscId(PathBuf),

    /// A disc ID is not four letters and five digits.
    #[error("invalid disc ID {0:?}: expected four letters and five digits, such as SLUS-00001")]
    InvalidDiscId(String),

    /// A playlist names no discs.
    #[error("{0}: the playlist names no discs")]
    EmptyPlaylist(PathBuf),

    /// An input that is neither a cue sheet, a CHD, nor a playlist.
    #[error("{0}: expected a .cue, .chd or .m3u file")]
    UnsupportedInput(PathBuf),

    /// A disc needs more blocks than the PSISOIMG index holds.
    #[error("{path}: {blocks} blocks; one PS1 EBOOT disc holds at most {max}")]
    IndexOverflow {
        path: PathBuf,
        blocks: u64,
        max: u64,
    },

    /// A header offset past what its 32-bit field can address.
    #[error("{field} {value} is past the 4 GiB a PS1 EBOOT header can address")]
    HeaderFieldOverflow { field: &'static str, value: u64 },

    /// The system temp folder has no room for the CHDs' extracted tracks,
    /// and for the EBOOTs too when they share its disk.
    #[error(
        "not enough free space in {dir} to extract the CHDs: need about {needed} bytes, {available} available; point TMPDIR at a disk with more room"
    )]
    ScratchSpace {
        dir: PathBuf,
        needed: u64,
        available: u64,
    },

    /// The EBOOT would not fit a FAT32 Memory Stick.
    #[error("the EBOOT would be {size} bytes, over the 4 GiB a FAT32 file can hold")]
    ExceedsFat32 { size: u64 },

    /// An image given for the splash or an icon cannot be read.
    #[error("{path}: cannot read the image: {source}")]
    ReadImage {
        path: PathBuf,
        source: std::io::Error,
    },

    /// An image given for the splash or an icon is not a PNG or JPEG the
    /// decoder can read.
    #[error("{path}: not a readable PNG or JPEG image: {source}")]
    InvalidImage {
        path: PathBuf,
        source: image::ImageError,
    },

    /// A fitted image could not be encoded as a PNG.
    #[error("{path}: could not encode the fitted image as a PNG: {source:#}")]
    ImageEncode {
        path: PathBuf,
        source: anyhow::Error,
    },

    /// The operation was cancelled by the caller.
    #[error("{0}")]
    Cancelled(#[from] crate::util::Cancelled),
}

/// Result alias for building a PS1 `EBOOT.PBP`.
pub type PsxPbpResult<T> = Result<T, PsxPbpError>;
