//! Error type for the WIA module.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum WiaError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),

    #[error(transparent)]
    BinRw(#[from] binrw::Error),

    #[error(transparent)]
    Rvz(#[from] Box<crate::nintendo::rvz::error::RvzError>),

    #[error("invalid WIA magic: expected b\"WIA\\x01\", got {0:02X?}")]
    InvalidMagic([u8; 4]),

    #[error("unsupported WIA version {version:#010X} (compatible {compatible:#010X})")]
    UnsupportedVersion { version: u32, compatible: u32 },

    #[error("unsupported WIA compression method {0}")]
    UnsupportedCompression(u32),

    #[error("invalid WIA header: {0}")]
    InvalidHeader(String),

    #[error("WIA {0} SHA-1 mismatch; the file is corrupted")]
    HashChainMismatch(&'static str),

    #[error("WIA group failed to decode: {0}")]
    Decode(String),

    #[error("{0}")]
    Custom(String),
}

impl From<crate::util::worker_pool::PoolChannelClosed> for WiaError {
    fn from(_: crate::util::worker_pool::PoolChannelClosed) -> Self {
        WiaError::Custom("worker pool channel closed".into())
    }
}

impl From<crate::nintendo::rvz::error::RvzError> for WiaError {
    fn from(e: crate::nintendo::rvz::error::RvzError) -> Self {
        WiaError::Rvz(Box::new(e))
    }
}

pub type WiaResult<T> = Result<T, WiaError>;
