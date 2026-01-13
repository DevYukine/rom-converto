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
}

pub type ChdResult<T> = Result<T, ChdError>;
