//! Error type for the CUE sheet parser.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CueError {
    /// Wraps an underlying I/O failure.
    #[error(transparent)]
    IoError(#[from] std::io::Error),

    /// A `FILE` line names a type other than `BINARY`, `WAVE`, `MP3`, or `AIFF`.
    #[error("unknown file type: {0}")]
    InvalidFileType(String),

    /// A `TRACK` line names a mode this parser does not recognize.
    #[error("unknown track type: {0}")]
    InvalidTrackType(String),

    /// An `INDEX`, `PREGAP`, or `POSTGAP` position is not valid `mm:ss:ff`.
    #[error("invalid MSF format: {0}")]
    InvalidMsfFormat(String),

    /// A quoted string field could not be decoded.
    #[error("invalid quoted string: {0}")]
    InvalidQuotedString(String),

    /// Wraps a failure to parse a numeric field (track or index number).
    #[error(transparent)]
    ParseIntError(#[from] std::num::ParseIntError),

    /// A quoted string field is missing its opening quote.
    #[error("missing opening quote")]
    MissingOpeningQuote,

    /// A quoted string field is missing its closing quote.
    #[error("missing closing quote")]
    MissingClosingQuote,
}

pub type CueResult<T> = Result<T, CueError>;
