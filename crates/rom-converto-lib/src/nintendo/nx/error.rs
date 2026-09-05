use std::path::PathBuf;

use thiserror::Error;

use crate::util::worker_pool::PoolChannelClosed;

/// Errors from parsing, encrypting, compressing, or decompressing
/// Nintendo Switch (NX) containers and their NCA/PFS0/HFS0 contents.
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

    #[error("input is not a recognized Switch container (NSP/XCI/NSZ/XCZ)")]
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

    #[error("compressed container {0} is not supported here; decompress it to NSP/XCI first")]
    CompressedInputUnsupported(PathBuf),

    #[error("no title content (CNMT) found in {0}")]
    NoContentInInput(PathBuf),

    #[error("super-XCI merge requires XCI inputs; {0} is an NSP (use --format nsp)")]
    XciMergeRequiresXciInputs(PathBuf),

    #[error("input {input} is missing NCA {nca_id} referenced by its CNMT", input = .input.display())]
    MissingReferencedNca { input: PathBuf, nca_id: String },

    #[error("merge needs at least one input container")]
    NoInputs,

    #[error("merge output {0} is also one of the inputs")]
    OutputIsInput(PathBuf),

    #[error("NCA content type {content_type} is not meta (expected a .cnmt.nca)")]
    NotMetaNca { content_type: u8 },

    #[error("meta NCA has no filesystem sections")]
    MetaNcaNoSections,

    #[error("meta NCA holds no .cnmt file")]
    MetaMissingCnmt,

    #[error("meta NCA .cnmt entry runs past the end of its section")]
    MetaCnmtTruncated,
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

/// Convenience alias for results returned by the NX (Switch) module.
pub type NxResult<T> = Result<T, NxError>;
