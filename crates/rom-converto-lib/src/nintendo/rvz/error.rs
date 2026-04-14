//! Error type for the RVZ module.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RvzError {
    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error(transparent)]
    JoinError(#[from] tokio::task::JoinError),

    #[error(transparent)]
    BinRWError(#[from] binrw::Error),

    #[error("invalid RVZ magic: expected b\"RVZ\\x01\", got {0:02X?}")]
    InvalidMagic([u8; 4]),

    #[error("unsupported WIA/RVZ version: {0:#010X}")]
    UnsupportedVersion(u32),

    #[error(
        "unsupported compression method {0}: rom-converto only implements zstd (method 5). \
         Convert the file to RVZ with Dolphin first."
    )]
    UnsupportedCompression(u32),

    #[error("unsupported disc type: {0}")]
    UnsupportedDiscType(u32),

    #[error("file header SHA-1 mismatch")]
    HeaderHashMismatch,

    #[error("disc struct SHA-1 mismatch")]
    DiscHashMismatch,

    #[error("partition table SHA-1 mismatch")]
    PartitionHashMismatch,

    #[error("decompressed size mismatch: expected {expected}, got {actual}")]
    DecompressedSizeMismatch { expected: u64, actual: u64 },

    #[error("invalid chunk size {0}: must be a power of two between {1} and {2} bytes")]
    InvalidChunkSize(u32, u32, u32),

    #[error("input ISO does not look like a GameCube or Wii disc image")]
    UnrecognizedDisc,

    #[error("Wii common key index {0} is out of range (only 0 and 1 are supported)")]
    UnknownCommonKeyIndex(u8),

    #[error("AES operation failed: {0}")]
    AesError(String),

    #[error("{0}")]
    Custom(String),
}

pub type RvzResult<T> = Result<T, RvzError>;
