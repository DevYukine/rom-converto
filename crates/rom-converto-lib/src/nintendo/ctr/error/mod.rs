use crate::util::worker_pool::PoolChannelClosed;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum NintendoCTRError {
    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error("Could not find the title file in the specified path: {0}")]
    NoTitleFileFound(PathBuf),

    #[error("Could not find at least one TMD file in the specified path: {0}")]
    NoTmdFileFound(PathBuf),

    #[error("operation cancelled")]
    Cancelled,

    #[error("worker pool channel closed")]
    WorkerPoolClosed,
}

impl From<PoolChannelClosed> for NintendoCTRError {
    fn from(_: PoolChannelClosed) -> Self {
        NintendoCTRError::WorkerPoolClosed
    }
}

pub type NintendoCTRResult<T> = Result<T, NintendoCTRError>;
