use std::path::PathBuf;

use thiserror::Error;

use crate::util::worker_pool::PoolChannelClosed;

#[derive(Debug, Error)]
pub enum NxError {
    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error(transparent)]
    JoinError(#[from] tokio::task::JoinError),

    #[error(transparent)]
    BinRwError(#[from] binrw::Error),

    #[error("worker pool channel closed")]
    WorkerPoolClosed,

    #[error("prod.keys not found; tried: {}", format_paths(.0))]
    KeyfileMissing(Vec<PathBuf>),

    #[error("malformed line in keys file: {line:?}")]
    KeyfileParse { line: String },

    #[error("missing required key {name:?} in keys file")]
    MissingKey { name: String },

    #[error("invalid hex value in keys file for {name:?}: {value:?}")]
    InvalidKeyHex { name: String, value: String },

    #[error("NCA header is invalid (wrong magic or unsupported version)")]
    InvalidNcaHeader,

    #[error("unsupported NCA version {0} (only NCA3 is supported)")]
    UnsupportedNcaVersion(u8),

    #[error("unsupported NCA section encryption type {0}")]
    UnsupportedEncryption(u8),

    #[error("PFS0 container has wrong magic")]
    Pfs0BadMagic,

    #[error("HFS0 container has wrong magic")]
    Hfs0BadMagic,

    #[error("NCZ block has wrong magic: {0:?}")]
    NczBadMagic([u8; 8]),

    #[error("NCZ block size exponent {0} out of range (must be 14..=32)")]
    BlockSizeOutOfRange(u8),

    #[error("NCZ section is incomplete or truncated")]
    IncompleteSection,

    #[error("invalid zstd compression level {level}: must be in the range {min}..={max}")]
    InvalidCompressionLevel { level: i32, min: i32, max: i32 },

    #[error("zstd error: {0}")]
    ZstdError(String),

    #[error("AES operation failed: {0}")]
    AesError(String),

    #[error("input is not a recognised Switch container (NSP/XCI/NSZ/XCZ)")]
    UnknownContainer,

    #[error("input container kind {0:?} cannot be the source of a {1} operation")]
    WrongContainerKind(String, &'static str),

    #[error("XCI cartridge image is truncated or malformed")]
    InvalidXci,

    #[error("ticket file is truncated or has unknown signature type")]
    InvalidTicket,

    #[error("no ticket found for rights_id {0}")]
    MissingTicket(String),

    #[error("operation cancelled")]
    Cancelled,
}

fn format_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

impl From<PoolChannelClosed> for NxError {
    fn from(_: PoolChannelClosed) -> Self {
        NxError::WorkerPoolClosed
    }
}

pub type NxResult<T> = Result<T, NxError>;
