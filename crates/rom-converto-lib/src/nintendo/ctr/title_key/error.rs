use hmac::digest::InvalidLength;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TitleKeyError {
    #[error(transparent)]
    InvalidLength(#[from] InvalidLength),

    #[error(transparent)]
    FromHexError(#[from] hex::FromHexError),

    #[error("Padding invalid: {0}")]
    PadError(String),
}

pub type TitleKeyResult<T> = Result<T, TitleKeyError>;
