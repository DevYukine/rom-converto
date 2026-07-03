//! Error type for the GCZ module.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum GczError {
    /// Wraps an underlying I/O failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Wraps a failed worker task join.
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),

    /// Wraps a binary read/write failure from the `binrw` layer.
    #[error(transparent)]
    BinRw(#[from] binrw::Error),

    /// The file does not start with the GCZ magic bytes.
    #[error("invalid GCZ magic: expected 0xb10bc001, got {0:#010x}")]
    InvalidMagic(u32),

    /// The GCZ header is malformed or internally inconsistent.
    #[error("invalid GCZ header: {0}")]
    InvalidHeader(String),

    /// A stored block's Adler-32 checksum does not match the computed value; the file is corrupted.
    #[error(
        "GCZ block {block} checksum mismatch: stored {stored:#010x}, computed {computed:#010x}; \
         the file is corrupted"
    )]
    BlockHashMismatch {
        block: u64,
        stored: u32,
        computed: u32,
    },

    /// A compressed block failed to inflate.
    #[error("GCZ block {block} failed to inflate: {reason}")]
    Inflate { block: u64, reason: String },

    /// The operation was cancelled by the caller.
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
