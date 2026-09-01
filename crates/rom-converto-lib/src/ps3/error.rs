//! Error type for the PS3 ISO module.

use std::path::PathBuf;

use thiserror::Error;

/// Errors from PS3 ISO decryption and metadata reading.
#[derive(Debug, Error)]
pub enum Ps3Error {
    /// Wraps an underlying I/O failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Wraps a failed worker task join.
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),

    /// The output ISO already exists and no overwrite was requested.
    #[error("output already exists; pass --on-conflict overwrite to replace it")]
    OutputAlreadyExists,

    /// The sector 0 region table is malformed or internally inconsistent.
    #[error("invalid PS3 region table: {0}")]
    InvalidRegionTable(String),

    /// A `PARAM.SFO` (`\0PSF`) blob is malformed or truncated.
    #[error("invalid PS3 PARAM.SFO: {0}")]
    InvalidSfo(String),

    /// A `PS3_DISC.SFB` (`.SFB`) blob is malformed or truncated.
    #[error("invalid PS3 PS3_DISC.SFB: {0}")]
    InvalidSfb(String),

    /// The supplied `.dkey` is not 32 hex characters.
    #[error("malformed PS3 disc key: {0}")]
    KeyMalformed(String),

    /// No `.dkey` could be resolved for the input ISO.
    #[error("no PS3 disc key (.dkey) found for {0}")]
    KeyMissing(PathBuf),

    /// The input holds no encrypted sectors, so there is nothing to do.
    #[error("PS3 ISO is already decrypted")]
    AlreadyDecrypted,

    /// The disc key did not turn any sampled encrypted sector into
    /// plausible plaintext.
    #[error("PS3 disc key does not match this ISO")]
    KeyMismatch,

    /// The worker pool's channel closed before the task could be submitted.
    #[error("worker pool channel closed")]
    WorkerPoolClosed,

    /// The operation was cancelled by the caller.
    #[error("operation cancelled")]
    Cancelled,
}

impl From<crate::util::worker_pool::PoolChannelClosed> for Ps3Error {
    fn from(_: crate::util::worker_pool::PoolChannelClosed) -> Self {
        Ps3Error::WorkerPoolClosed
    }
}

/// Convenience alias for a [`Result`] with [`Ps3Error`].
pub type Ps3Result<T> = Result<T, Ps3Error>;
