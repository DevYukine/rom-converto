//! Error type for the Xbox 360 (xenon) ISO/ZArchive pipeline.

use thiserror::Error;

use crate::microsoft::xdvdfs::XdvdfsError;
use crate::microsoft::zar::format::ZarError;

/// Errors from the Xbox 360 (Xenon) ISO/ZArchive pipeline.
#[derive(Debug, Error)]
pub enum XenonError {
    #[error(transparent)]
    Xdvdfs(#[from] XdvdfsError),

    #[error(transparent)]
    Zar(#[from] ZarError),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),

    #[error("worker pool channel closed")]
    WorkerPool,

    #[error("archive entry path {path:?} is unsafe to extract")]
    UnsafePath { path: String },

    #[error(
        "archive entry at recorded offset {actual} does not match the expected stream position {expected}"
    )]
    OffsetMismatch { expected: u64, actual: u64 },

    #[error("operation cancelled")]
    Cancelled,
}

impl From<crate::util::worker_pool::PoolChannelClosed> for XenonError {
    fn from(_: crate::util::worker_pool::PoolChannelClosed) -> Self {
        XenonError::WorkerPool
    }
}

/// Convenience alias for a [`Result`] with [`XenonError`].
pub type XenonResult<T> = Result<T, XenonError>;
