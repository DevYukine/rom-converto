use crate::util::worker_pool::PoolChannelClosed;
use std::path::PathBuf;
use thiserror::Error;

/// Errors from CTR (3DS) container parsing, decryption, and conversion.
#[derive(Error, Debug)]
pub enum NintendoCTRError {
    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error("could not find the title file in the specified path: {0}")]
    NoTitleFileFound(PathBuf),

    #[error("could not find at least one TMD file in the specified path: {0}")]
    NoTmdFileFound(PathBuf),

    #[error(
        "content for 0x{0:016X} does not match the TMD hash after decryption: either the title key cannot be derived from its title id, or the content file is corrupt; supply the real ticket (cetk) for this title, or re-dump the content"
    )]
    ForgedTicketKeyMismatch(u64),

    #[error("operation cancelled")]
    Cancelled,

    #[error("worker pool channel closed")]
    WorkerPoolClosed,
}

impl From<PoolChannelClosed> for NintendoCTRError {
    fn from(_: PoolChannelClosed) -> Self {
        NintendoCTRError::WorkerPoolClosed
    }
}

/// Convenience alias for a `Result` with [`NintendoCTRError`].
pub type NintendoCTRResult<T> = Result<T, NintendoCTRError>;
