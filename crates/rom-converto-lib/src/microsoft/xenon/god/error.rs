//! Error type for the Xbox 360 Games on Demand writer.

use thiserror::Error;

use crate::microsoft::xdvdfs::XdvdfsError;

/// Errors from the Xbox 360 Games on Demand writer.
#[derive(Debug, Error)]
pub enum GodError {
    #[error(transparent)]
    Xdvdfs(#[from] XdvdfsError),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),

    #[error("worker pool channel closed")]
    WorkerPool,

    #[error("image has no root-level default.xex")]
    MissingDefaultXex,

    #[error("source image is truncated: expected at least {expected} bytes, found {actual}")]
    TruncatedImage { expected: u64, actual: u64 },

    #[error("invalid XEX2 executable: {reason}")]
    InvalidXex { reason: &'static str },

    #[error("operation cancelled")]
    Cancelled,
}

impl From<crate::util::worker_pool::PoolChannelClosed> for GodError {
    fn from(_: crate::util::worker_pool::PoolChannelClosed) -> Self {
        GodError::WorkerPool
    }
}

/// Convenience alias for a [`Result`] with [`GodError`].
pub type GodResult<T> = Result<T, GodError>;
