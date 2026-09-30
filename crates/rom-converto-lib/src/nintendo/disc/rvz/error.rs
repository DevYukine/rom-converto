//! Error type for the RVZ module.

use thiserror::Error;

/// Errors from reading, decoding, or writing an RVZ container.
#[derive(Debug, Error)]
pub enum RvzError {
    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error(transparent)]
    JoinError(#[from] tokio::task::JoinError),

    #[error(transparent)]
    BinRWError(#[from] binrw::Error),

    #[error(transparent)]
    Wbfs(#[from] crate::nintendo::disc::wbfs::error::WbfsError),

    #[error(transparent)]
    Gcz(#[from] crate::nintendo::disc::gcz::error::GczError),

    #[error(transparent)]
    Wia(#[from] Box<crate::nintendo::disc::wia::error::WiaError>),

    #[error(transparent)]
    Nkit(#[from] crate::nintendo::disc::nkit::error::NkitError),

    #[error("invalid RVZ magic: expected b\"RVZ\\x01\", got {0:02X?}")]
    InvalidMagic([u8; 4]),

    #[error("unsupported WIA/RVZ version: {0:#010x}")]
    UnsupportedVersion(u32),

    #[error(
        "unsupported compression method {0}: rom-converto only implements zstd (method 5). \
         Recompress it as a zstd RVZ first."
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

    #[error("RVZ file is truncated: expected at least {expected} bytes, found {actual}")]
    Truncated { expected: u64, actual: u64 },

    #[error(
        "{table} table needs {entries} entries, more than the container geometry allows or provides"
    )]
    TableTooLarge { table: &'static str, entries: u32 },

    #[error("decompressed size mismatch: expected {expected}, got {actual}")]
    DecompressedSizeMismatch { expected: u64, actual: u64 },

    #[error(
        "implausible ISO size {0}: no GameCube or Wii disc image is this large \
         (the largest ever shipped is a dual-layer image at ~8.5 GB)"
    )]
    ImplausibleIsoSize(u64),

    #[error(
        "invalid chunk size {0}: must be a power of two of at least {1} bytes \
         or a multiple of {2} bytes"
    )]
    InvalidChunkSize(u32, u32, u32),

    /// A partitioned container whose `chunk_size` exceeds the 2 MiB
    /// partition-decode limit: structurally sound, but the partition
    /// decoder walks one 2 MiB cluster of sectors per chunk and cannot
    /// feed it. Verification reports this as unverifiable instead of
    /// invalid.
    #[error(
        "partitioned RVZ chunk size {0} exceeds the {1}-byte limit the partition decoder supports"
    )]
    PartitionChunkTooLarge(u32, u32),

    #[error("input ISO does not look like a GameCube or Wii disc image")]
    UnrecognizedDisc,

    #[error("Wii common key index {0} is out of range (only 0 and 1 are supported)")]
    UnknownCommonKeyIndex(u8),

    #[error("AES operation failed: {0}")]
    AesError(String),

    #[error("{0}")]
    Custom(String),

    /// The worker pool's channel closed before the task could be submitted.
    #[error(transparent)]
    WorkerPoolClosed(#[from] crate::util::worker_pool::PoolChannelClosed),

    #[error("{0}")]
    Cancelled(#[from] crate::util::Cancelled),
}

impl From<crate::nintendo::disc::wia::error::WiaError> for RvzError {
    fn from(e: crate::nintendo::disc::wia::error::WiaError) -> Self {
        RvzError::Wia(Box::new(e))
    }
}

/// Result alias for RVZ operations.
pub type RvzResult<T> = Result<T, RvzError>;
