use thiserror::Error;

#[derive(Debug, Error)]
pub enum CueError {
    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error("Unknown file type: {0}")]
    InvalidFileType(String),

    #[error("Unknown track type: {0}")]
    InvalidTrackType(String),

    #[error("Invalid MSF format: {0}")]
    InvalidMSFFormat(String),

    #[error("Invalid quoted string: {0}")]
    InvalidQuotedString(String),

    #[error(transparent)]
    ParseIntError(#[from] std::num::ParseIntError),

    #[error("{0}")]
    MissingQuoteError(String),
}

pub type CueResult<T> = Result<T, CueError>;
