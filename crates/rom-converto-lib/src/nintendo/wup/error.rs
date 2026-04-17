use std::path::PathBuf;

use thiserror::Error;

use crate::util::worker_pool::PoolChannelClosed;

#[derive(Debug, Error)]
pub enum WupError {
    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error(transparent)]
    JoinError(#[from] tokio::task::JoinError),

    #[error(transparent)]
    BinRwError(#[from] binrw::Error),

    #[error("worker pool channel closed")]
    WorkerPoolClosed,

    #[error("invalid zstd compression level {level}: must be in the range {min}..={max}")]
    InvalidCompressionLevel { level: i32, min: i32, max: i32 },

    #[error("node name is {0} bytes, must be at most 127")]
    NameTooLong(usize),

    #[error("name table would exceed the 2 GiB section limit")]
    NameTableTooLarge,

    #[error("path is empty or malformed: {0:?}")]
    InvalidPath(String),

    #[error("path conflict at {0:?}: cannot place a file under a file or a directory over a file")]
    PathConflict(String),

    #[error("file already exists at {0:?}")]
    DuplicateFile(String),

    #[error("title directory is neither loadiine nor NUS layout: {0}")]
    UnrecognizedTitleDirectory(PathBuf),

    #[error("missing required file: {0}")]
    MissingRequiredFile(PathBuf),

    #[error("could not parse title metadata from {0}")]
    InvalidAppXml(PathBuf),

    #[error("ticket is malformed")]
    InvalidTicket,

    #[error("title metadata (TMD) is malformed")]
    InvalidTmd,

    #[error("FST section is malformed")]
    InvalidFst,

    #[error("unknown common key index {0}")]
    InvalidCommonKeyIndex(u8),

    #[error("failed to decrypt title key")]
    TitleKeyDecryptFailed,

    #[error("TMD references content id {content_id:08x} which is not in the title directory")]
    ContentNotFound { content_id: u32 },

    #[error("content encryption mode is not supported")]
    UnsupportedContentMode,

    #[error("AES operation failed: {0}")]
    AesError(String),

    #[error("disc key file missing or unreadable: {0}")]
    DiscKeyMissing(PathBuf),

    #[error("disc key file is malformed: {0}")]
    DiscKeyMalformed(String),

    #[error("disc key did not decrypt the partition table; wrong key?")]
    DiscKeyWrong,

    #[error("unsupported disc container format: {0}")]
    UnsupportedDiscFormat(PathBuf),

    #[error("disc contains no GM (game) partition")]
    NoGamePartitionFound,

    #[error("disc image is truncated: expected {expected} bytes, got {actual}")]
    DiscTruncated { expected: u64, actual: u64 },

    #[error("partition header missing or corrupt")]
    InvalidPartitionHeader,

    /// Virtual file extends past the decrypted cluster bytes. Update
    /// titles that describe the merged game but only ship delta
    /// clusters hit this; the NUS orchestrator skips the file so a
    /// stacked base `.wua` can fill it in.
    #[error(
        "file {path:?} (cluster {cluster_index}) extends past the cluster's available bytes; likely inherited from another title"
    )]
    FileInheritedFromOtherTitle { path: String, cluster_index: u16 },
}

impl From<PoolChannelClosed> for WupError {
    fn from(_: PoolChannelClosed) -> Self {
        WupError::WorkerPoolClosed
    }
}

pub type WupResult<T> = Result<T, WupError>;
