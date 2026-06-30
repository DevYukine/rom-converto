//! Error type for the CSO module.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CsoError {
    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error(transparent)]
    JoinError(#[from] tokio::task::JoinError),

    #[error(transparent)]
    BinRWError(#[from] binrw::Error),

    #[error("Output file already exists, use --force to overwrite")]
    OutputAlreadyExists,

    #[error("invalid CSO/ZSO image: {0}")]
    InvalidHeader(String),

    #[error("invalid block size {0}: must be a power of two between 2 KiB and 1 MiB")]
    InvalidBlockSize(u32),

    #[error("block {block} decompressed to {actual} bytes, expected {expected}")]
    BlockSizeMismatch {
        block: u64,
        expected: usize,
        actual: usize,
    },

    #[error("corrupt index: {0}")]
    CorruptIndex(String),

    #[error("worker pool channel closed")]
    WorkerPoolClosed,

    #[error("worker pool writer thread panicked")]
    WorkerPoolPanic,

    #[error("operation cancelled")]
    Cancelled,
}

impl From<crate::util::worker_pool::PoolChannelClosed> for CsoError {
    fn from(_: crate::util::worker_pool::PoolChannelClosed) -> Self {
        CsoError::WorkerPoolClosed
    }
}

pub type CsoResult<T> = Result<T, CsoError>;
