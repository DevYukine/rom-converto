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
    InvalidMsfFormat(String),

    #[error("Invalid quoted string: {0}")]
    InvalidQuotedString(String),

    #[error(transparent)]
    ParseIntError(#[from] std::num::ParseIntError),

    #[error("Missing opening quote")]
    MissingOpeningQuote,

    #[error("Missing closing quote")]
    MissingClosingQuote,
}

pub type CueResult<T> = Result<T, CueError>;
