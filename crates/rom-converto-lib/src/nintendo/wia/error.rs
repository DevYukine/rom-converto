//! Error type for the WIA module.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum WiaError {
    /// Wraps an underlying I/O failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Wraps a failed worker task join.
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),

    /// Wraps a binary read/write failure from the `binrw` layer.
    #[error(transparent)]
    BinRw(#[from] binrw::Error),

    /// Wraps a failure surfaced by the RVZ pipeline.
    #[error(transparent)]
    Rvz(#[from] Box<crate::nintendo::rvz::error::RvzError>),

    /// The file does not start with the WIA magic bytes.
    #[error("invalid WIA magic: expected 0x57494101, got {:#010x}", u32::from_be_bytes(*.0))]
    InvalidMagic([u8; 4]),

    /// The file declares a version this reader does not support.
    #[error("unsupported WIA version {version:#010x} (compatible {compatible:#010x})")]
    UnsupportedVersion { version: u32, compatible: u32 },

    /// The file declares a compression method this reader does not implement.
    #[error("unsupported WIA compression method {0}")]
    UnsupportedCompression(u32),

    /// The WIA header or a metadata table is malformed.
    #[error("invalid WIA header: {0}")]
    InvalidHeader(String),

    /// A stored SHA-1 in the header chain does not match the computed value; the file is corrupted.
    #[error("WIA {0} SHA-1 mismatch; the file is corrupted")]
    HashChainMismatch(&'static str),

    /// A group failed to decode through its codec.
    #[error("WIA group failed to decode: {0}")]
    Decode(String),

    /// The operation was cancelled by the caller.
    #[error("operation cancelled")]
    Cancelled,

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_magic_renders_lowercase_hex() {
        assert_eq!(
            WiaError::InvalidMagic([0x41, 0x42, 0x43, 0x44]).to_string(),
            "invalid WIA magic: expected 0x57494101, got 0x41424344"
        );
    }
}
