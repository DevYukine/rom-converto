//! Error type for the NKit module.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum NkitError {
    /// Wraps an underlying I/O failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Wraps a failed worker task join.
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),

    /// Wraps a failure from the underlying GCZ reader (for `.nkit.gcz` input).
    #[error(transparent)]
    Gcz(#[from] crate::nintendo::gcz::error::GczError),

    /// The NKit header or FST is malformed.
    #[error("invalid NKit image: {0}")]
    InvalidHeader(String),

    /// A gap or junk-file record is malformed.
    #[error("invalid NKit gap encoding: {0}")]
    InvalidGap(String),

    /// The computed CRC32 does not match the value stored in the NKit header; the file is corrupted or was produced by an incompatible NKit version.
    #[error(
        "NKit CRC32 mismatch: stored {stored:#010x}, computed {computed:#010x} over {what}; \
         the file is corrupted or was not produced by a compatible NKit version"
    )]
    CrcMismatch {
        what: &'static str,
        stored: u32,
        computed: u32,
    },

    /// The operation was cancelled by the caller.
    #[error("operation cancelled")]
    Cancelled,

    #[error("{0}")]
    Custom(String),
}

impl From<crate::util::worker_pool::PoolChannelClosed> for NkitError {
    fn from(_: crate::util::worker_pool::PoolChannelClosed) -> Self {
        NkitError::Custom("worker pool channel closed".into())
    }
}

pub type NkitResult<T> = Result<T, NkitError>;
