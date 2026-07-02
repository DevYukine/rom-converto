use thiserror::Error;

#[derive(Debug, Error)]
pub enum DatError {
    #[error(transparent)]
    IoError(#[from] std::io::Error),
    #[error(transparent)]
    HttpError(#[from] reqwest::Error),
    #[error("network error: {0}")]
    Transport(String),
    #[error("playmatch api error ({code}): {message}")]
    Api { code: String, message: String },
    #[error("pagination exceeded {0} pages with more remaining; refusing incomplete results")]
    Truncated(usize),
    #[error("invalid response from playmatch: {0}")]
    BadResponse(String),
    #[error("inner-stream hashing is not supported for {format} yet; decompress the file first")]
    UnsupportedInnerHash { format: &'static str },
    #[error("container error: {0}")]
    Container(String),
    #[error("{0}")]
    InvalidInput(String),
    #[error("operation cancelled")]
    Cancelled,
}

pub type DatResult<T> = Result<T, DatError>;
