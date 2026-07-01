//! Error type for the NKit module.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum NkitError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),

    #[error(transparent)]
    Gcz(#[from] crate::nintendo::gcz::error::GczError),

    #[error("invalid NKit image: {0}")]
    InvalidHeader(String),

    #[error("invalid NKit gap encoding: {0}")]
    InvalidGap(String),

    #[error(
        "NKit CRC32 mismatch: stored {stored:#010X}, computed {computed:#010X} over {what}; \
         the file is corrupted or was not produced by a compatible NKit version"
    )]
    CrcMismatch {
        what: &'static str,
        stored: u32,
        computed: u32,
    },

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
