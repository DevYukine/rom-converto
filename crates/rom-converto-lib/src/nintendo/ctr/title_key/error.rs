use hmac::digest::InvalidLength;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TitleKeyError {
    #[error(transparent)]
    InvalidLength(#[from] InvalidLength),

    #[error(transparent)]
    FromHexError(#[from] hex::FromHexError),

    #[error("padding invalid: {0}")]
    PadError(String),

    #[error(transparent)]
    NoEmbeddedKeys(#[from] crate::util::embedded_keys::NoEmbeddedKeys),
}

pub type TitleKeyResult<T> = Result<T, TitleKeyError>;
