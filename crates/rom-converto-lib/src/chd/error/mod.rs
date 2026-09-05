//! Error type for the CHD module.

use crate::chd::DiscMode;
use crate::chd::compression::ChdCodec;
use crate::cue::error::CueError;
use thiserror::Error;

/// Errors from CHD creation, extraction, and verification.
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

    /// A cue track whose sector width is not 2352 bytes; the CD ingest
    /// reads a uniform raw-sector bin.
    #[error(
        "cue track type {cue_type} is not a 2352-byte raw track; convert from a raw bin/cue \
         dump (or a flat .iso for 2048-byte data discs)"
    )]
    UnsupportedCueTrackWidth { cue_type: &'static str },

    /// Compressing a CHD map hunk failed.
    #[error("CHD map compression failed")]
    MapCompressionError,

    /// Decompressing a CHD map hunk failed.
    #[error("CHD map decompression failed")]
    MapDecompressionError,

    /// The CHD file is not version 5, the only version this operation handles.
    #[error("unsupported CHD version: expected V5, run `chd migrate` to convert a V1-V4 CHD")]
    UnsupportedChdVersion,

    /// A V1-V4 CHD header failed one of chdman's validity checks.
    #[error("invalid CHD v{version} header: {reason}")]
    InvalidLegacyHeader { version: u8, reason: String },

    /// A V1-V4 map entry uses a hunk type this crate does not decode.
    #[error("unsupported CHD v1-v4 map entry type: {0}")]
    UnsupportedLegacyMapEntry(u8),

    /// A decompressed V1-V4 hunk's CRC-32 does not match the map entry.
    #[error("CRC-32 mismatch for hunk {hunk}: expected {expected:#010x}, got {actual:#010x}")]
    LegacyHunkCrcMismatch {
        hunk: u32,
        expected: u32,
        actual: u32,
    },

    /// A migrate target is already the version it would be migrated to.
    #[error("CHD is already version 5")]
    ChdAlreadyV5,

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

    /// A codec list named a codec chdman does not implement.
    #[error("unknown compression codec name: {0}")]
    UnknownCodecName(String),

    /// A codec list was empty; at least one codec is required.
    #[error("codec list must not be empty")]
    EmptyCodecList,

    /// A codec list has more entries than a CHD header's 4 compressor slots.
    #[error("codec list has {0} entries; a CHD header supports at most 4")]
    TooManyCodecs(usize),

    /// A codec list names the same codec more than once.
    #[error("duplicate codec in list: {0}")]
    DuplicateCodec(ChdCodec),

    /// A CD-only codec (cdzl/cdzs/cdlz/cdfl) was requested for a DVD-mode CHD.
    #[error("{0} is a CD-only codec and cannot be used for a DVD-mode CHD")]
    CdCodecOnDvd(ChdCodec),

    /// A `--level` value outside the accepted compression level range.
    #[error("compression level {0} out of range 1..=22")]
    InvalidCompressionLevel(i32),

    /// A compressed hunk is shorter than its header or length fields require.
    #[error("malformed compressed hunk: truncated or corrupt")]
    MalformedHunk,

    /// Wraps an AVI parse failure from the laserdisc reader.
    #[error("laserdisc AVI error: {0}")]
    LdAviError(#[from] anyhow::Error),

    /// `--mode ld` was given with a non-`.avi` input.
    #[error("LD mode needs a .avi input")]
    LdModeNeedsAvi,

    /// `--mode cd`/`--mode dvd` was given with an `.avi` input.
    #[error("{0:?} mode cannot be used with a .avi input; laserdisc AVIs need LD mode")]
    AviNeedsLdMode(DiscMode),

    /// LD mode does not accept a codec, level, or hunk-size override;
    /// the `avhu` codec and per-field hunk size are fixed by the AVI.
    #[error("LD mode does not support overriding {knob}")]
    LdRejectsOverride { knob: &'static str },

    /// Extracting a laserdisc (`AVAV`-tagged) CHD is not implemented.
    #[error("LaserDisc CHDs cannot be extracted yet")]
    LdExtractionUnsupported,
}

impl From<crate::util::worker_pool::PoolChannelClosed> for ChdError {
    fn from(_: crate::util::worker_pool::PoolChannelClosed) -> Self {
        ChdError::WorkerPoolClosed
    }
}

/// Convenience alias for a [`Result`] with [`ChdError`].
pub type ChdResult<T> = Result<T, ChdError>;
