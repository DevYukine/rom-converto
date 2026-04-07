use thiserror::Error;

#[derive(Debug, Error)]
pub enum Z3dsError {
    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error(transparent)]
    JoinError(#[from] tokio::task::JoinError),

    #[error(transparent)]
    BinRWError(#[from] binrw::Error),

    #[error(transparent)]
    TemplateError(#[from] indicatif::style::TemplateError),

    #[error("unsupported Z3DS version: {0}")]
    UnsupportedVersion(u8),

    #[error("input ROM appears to be encrypted, only decrypted ROMs can be compressed")]
    InputNotDecrypted,

    #[error("unsupported input format: {0}")]
    UnsupportedInputFormat(String),

    #[error("decompressed size mismatch: expected {expected}, got {actual}")]
    DecompressedSizeMismatch { expected: u64, actual: u64 },
}

pub type Z3dsResult<T> = Result<T, Z3dsError>;
