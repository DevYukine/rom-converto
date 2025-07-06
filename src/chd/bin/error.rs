use thiserror::Error;

#[derive(Debug, Error)]
pub enum BinError {
    #[error(transparent)]
    IoError(#[from] std::io::Error),
}

pub type BinResult<T> = Result<T, BinError>;
