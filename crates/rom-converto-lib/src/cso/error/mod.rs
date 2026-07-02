//! Error type for the CSO module.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CsoError {
    /// Wraps an underlying I/O failure.
    #[error(transparent)]
    IoError(#[from] std::io::Error),

    /// Wraps a failed worker task join.
    #[error(transparent)]
    JoinError(#[from] tokio::task::JoinError),

    /// Wraps a binary read/write failure from the `binrw` layer.
    #[error(transparent)]
    BinRWError(#[from] binrw::Error),

    /// The output CSO/ZSO file already exists and no overwrite was requested.
    #[error("output already exists; pass --on-conflict overwrite to replace it")]
    OutputAlreadyExists,

    /// The CSO/ZSO header is malformed or its magic bytes are not recognized.
    #[error("invalid CSO/ZSO image: {0}")]
    InvalidHeader(String),

    /// The requested block size is not a power of two in the supported range.
    #[error("invalid block size {0}: must be a power of two between 2 KiB and 1 MiB")]
    InvalidBlockSize(u32),

    /// A decompressed block's byte length does not match the configured block size.
    #[error("block {block} decompressed to {actual} bytes, expected {expected}")]
    BlockSizeMismatch {
        block: u64,
        expected: usize,
        actual: usize,
    },

    /// The block index table is malformed or internally inconsistent.
    #[error("corrupt index: {0}")]
    CorruptIndex(String),

    /// The worker pool's channel closed before the task could be submitted.
    #[error("worker pool channel closed")]
    WorkerPoolClosed,

    /// The worker pool's writer thread panicked.
    #[error("worker pool writer thread panicked")]
    WorkerPoolPanic,

    /// The operation was cancelled by the caller.
    #[error("operation cancelled")]
    Cancelled,
}

impl From<crate::util::worker_pool::PoolChannelClosed> for CsoError {
    fn from(_: crate::util::worker_pool::PoolChannelClosed) -> Self {
        CsoError::WorkerPoolClosed
    }
}

pub type CsoResult<T> = Result<T, CsoError>;
