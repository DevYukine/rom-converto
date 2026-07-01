//! Error type for the GCZ module.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum GczError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),

    #[error(transparent)]
    BinRw(#[from] binrw::Error),

    #[error("invalid GCZ magic: expected 0xB10BC001, got {0:#010X}")]
    InvalidMagic(u32),

    #[error("invalid GCZ header: {0}")]
    InvalidHeader(String),

    #[error(
        "GCZ block {block} checksum mismatch: stored {stored:#010X}, computed {computed:#010X}; \
         the file is corrupted"
    )]
    BlockHashMismatch {
        block: u64,
        stored: u32,
        computed: u32,
    },

    #[error("GCZ block {block} failed to inflate: {reason}")]
    Inflate { block: u64, reason: String },

    #[error("operation cancelled")]
    Cancelled,

    #[error("{0}")]
    Custom(String),
}

impl From<crate::util::worker_pool::PoolChannelClosed> for GczError {
    fn from(_: crate::util::worker_pool::PoolChannelClosed) -> Self {
        GczError::Custom("worker pool channel closed".into())
    }
}

pub type GczResult<T> = Result<T, GczError>;
