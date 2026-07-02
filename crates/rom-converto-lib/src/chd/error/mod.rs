//! Error type for the CHD module.

use crate::cue::error::CueError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ChdError {
    /// Wraps an underlying I/O failure.
    #[error(transparent)]
    IoError(#[from] std::io::Error),

    /// Wraps a failed worker task join.
    #[error(transparent)]
    JoinError(#[from] tokio::task::JoinError),

    /// Wraps a binary read/write failure from the `binrw` layer.
    #[error(transparent)]
    BinRWError(#[from] binrw::Error),

    /// Wraps a CUE sheet parsing or validation failure.
    #[error(transparent)]
    CueError(#[from] CueError),

    /// The output CHD file already exists and no overwrite was requested.
    #[error("output already exists; pass --on-conflict overwrite to replace it")]
    ChdFileAlreadyExists,

    /// The CUE sheet does not reference any files to compress.
    #[error("no files are referenced in the CUE sheet")]
    NoFileReferencedInCueSheet,

    /// The computed hunk size for the CHD data is not valid.
    #[error("invalid hunk size for CHD data")]
    InvalidHunkSize,

    /// The raw ISO input size is not a multiple of the 2048-byte sector size.
    #[error(
        "input size {size} is not a multiple of 2048; not a 2048-byte-sector image \
         (raw 2352-byte CD dumps need bin/cue input)"
    )]
    IsoNotSectorAligned { size: u64 },

    /// A `.cue` input was given with `--dvd`, which needs a flat `.iso` instead.
    #[error("DVD mode needs a flat .iso input; a .cue describes a CD-layout disc, drop --dvd")]
    DvdModeNeedsIso,

    /// Compressing a CHD map hunk failed.
    #[error("CHD map compression failed")]
    MapCompressionError,

    /// Decompressing a CHD map hunk failed.
    #[error("CHD map decompression failed")]
    MapDecompressionError,

    /// The CHD file is not version 5, the only version this crate reads and writes.
    #[error("unsupported CHD version: expected V5")]
    UnsupportedChdVersion,

    /// The CHD map references a compression codec this crate does not implement.
    #[error("unknown compression codec: {0:02x?}")]
    UnknownCompressionCodec([u8; 4]),

    /// A decompressed hunk's CRC does not match the value stored in the CHD map.
    #[error("CRC mismatch for hunk {hunk}: expected {expected:#06x}, got {actual:#06x}")]
    HunkCrcMismatch {
        hunk: u32,
        expected: u16,
        actual: u16,
    },

    /// A decompressed hunk's SHA-1 does not match the value stored in the CHD map.
    #[error("SHA-1 mismatch: expected {expected}, got {actual}")]
    Sha1Mismatch { expected: String, actual: String },

    /// A decompressed hunk's byte length does not match the size recorded in the header.
    #[error("decompression produced wrong size: expected {expected}, got {actual}")]
    DecompressionSizeMismatch { expected: usize, actual: usize },

    /// The CHD file declares a parent CHD, which this crate does not support.
    #[error("parent CHD references are not supported")]
    ParentChdNotSupported,

    /// The CD track metadata embedded in the CHD could not be parsed.
    #[error("invalid CHD track metadata: {0}")]
    InvalidTrackMetadata(String),

    /// The worker pool's channel closed before the task could be submitted.
    #[error("worker pool channel closed")]
    WorkerPoolClosed,

    /// The worker pool's writer thread panicked.
    #[error("worker pool writer thread panicked")]
    WorkerPoolPanic,

    /// The operation was cancelled by the caller.
    #[error("operation cancelled")]
    Cancelled,
}

impl From<crate::util::worker_pool::PoolChannelClosed> for ChdError {
    fn from(_: crate::util::worker_pool::PoolChannelClosed) -> Self {
        ChdError::WorkerPoolClosed
    }
}

pub type ChdResult<T> = Result<T, ChdError>;
