//! Error type and result alias for the Playmatch API client.

use thiserror::Error;

/// Errors from the Playmatch API client.
#[derive(Debug, Error)]
pub enum DatError {
    #[error(transparent)]
    IoError(#[from] std::io::Error),
    #[error(transparent)]
    HttpError(#[from] reqwest::Error),
    #[error("network error: {0}")]
    Transport(String),
    #[error("Playmatch API error ({code}): {message}")]
    Api { code: String, message: String },
    #[error("pagination exceeded {0} pages with more remaining; refusing incomplete results")]
    Truncated(usize),
    #[error("invalid response from Playmatch: {0}")]
    BadResponse(String),
    #[error("inner-stream hashing is not supported for {format} yet; decompress the file first")]
    UnsupportedInnerHash { format: &'static str },
    #[error("decode error: {0}")]
    Container(String),
    #[error("{0}")]
    InvalidInput(String),
    #[error("{0}")]
    Cancelled(#[from] crate::util::Cancelled),
}

impl From<tokio::task::JoinError> for DatError {
    fn from(err: tokio::task::JoinError) -> Self {
        DatError::Container(err.to_string())
    }
}

/// Result alias for Playmatch API client operations.
pub type DatResult<T> = Result<T, DatError>;
