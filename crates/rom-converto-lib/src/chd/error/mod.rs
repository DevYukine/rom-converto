use crate::chd::bin::error::BinError;
use crate::chd::cue::error::CueError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ChdError {
    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error(transparent)]
    JoinError(#[from] tokio::task::JoinError),

    #[error(transparent)]
    BinRWError(#[from] binrw::Error),

    #[error(transparent)]
    CueError(#[from] CueError),

    #[error(transparent)]
    BinError(#[from] BinError),

    #[error("Chd file already exists, use --force to overwrite")]
    ChdFileAlreadyExists,

    #[error("No files are referenced in the CUE sheet")]
    NoFileReferencedInCueSheet,

    #[error("Invalid hunk size for CHD data")]
    InvalidHunkSize,

    #[error("CHD map compression failed")]
    MapCompressionError,

    #[error("CHD map decompression failed")]
    MapDecompressionError,

    #[error("Unsupported CHD version: expected V5")]
    UnsupportedChdVersion,

    #[error("Unknown compression codec: {0:02x?}")]
    UnknownCompressionCodec([u8; 4]),

    #[error("CRC mismatch for hunk {hunk}: expected {expected:#06x}, got {actual:#06x}")]
    HunkCrcMismatch {
        hunk: u32,
        expected: u16,
        actual: u16,
    },

    #[error("SHA1 mismatch: expected {expected}, got {actual}")]
    Sha1Mismatch { expected: String, actual: String },

    #[error("Decompression produced wrong size: expected {expected}, got {actual}")]
    DecompressionSizeMismatch { expected: usize, actual: usize },

    #[error("Parent CHD references are not supported")]
    ParentChdNotSupported,

    #[error("Invalid CHD track metadata: {0}")]
    InvalidTrackMetadata(String),
}

pub type ChdResult<T> = Result<T, ChdError>;
