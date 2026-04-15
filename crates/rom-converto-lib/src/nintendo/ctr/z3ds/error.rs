use thiserror::Error;

#[derive(Debug, Error)]
pub enum Z3dsError {
    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error(transparent)]
    JoinError(#[from] tokio::task::JoinError),

    #[error(transparent)]
    BinRWError(#[from] binrw::Error),

    #[error("unsupported Z3DS version: {0}")]
    UnsupportedVersion(u8),

    #[error("input ROM appears to be encrypted, only decrypted ROMs can be compressed")]
    InputNotDecrypted,

    #[error("unsupported input format: {0}")]
    UnsupportedInputFormat(String),

    #[error("invalid zstd compression level {level}: must be in the range {min}..={max}")]
    InvalidCompressionLevel { level: i32, min: i32, max: i32 },

    #[error("decompressed size mismatch: expected {expected}, got {actual}")]
    DecompressedSizeMismatch { expected: u64, actual: u64 },

    #[error("worker pool channel closed")]
    WorkerPoolClosed,

    #[error("worker pool writer thread panicked")]
    WorkerPoolPanic,
}

impl From<crate::util::worker_pool::PoolChannelClosed> for Z3dsError {
    fn from(_: crate::util::worker_pool::PoolChannelClosed) -> Self {
        Z3dsError::WorkerPoolClosed
    }
}

pub type Z3dsResult<T> = Result<T, Z3dsError>;
