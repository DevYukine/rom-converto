//! Error type for the Original Xbox XISO create/extract paths.

use thiserror::Error;

use crate::microsoft::xdvdfs::XdvdfsError;

/// Errors from Original Xbox XISO creation and extraction.
#[derive(Debug, Error)]
pub enum XboxError {
    #[error(transparent)]
    Xdvdfs(#[from] XdvdfsError),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    JoinError(#[from] tokio::task::JoinError),

    #[error("filename {name:?} cannot be encoded as Windows-1252, which XDVDFS requires")]
    NameNotCp1252 { name: String },

    #[error("{path} collides with a sibling: XDVDFS filenames are case-insensitive")]
    DuplicateName { path: String },

    #[error("filename {name:?} is longer than the 255 bytes a dirent can record")]
    NameTooLong { name: String },

    #[error(
        "directory table for {path} needs {used} bytes, past the 262,140 the u16 entry offsets can address"
    )]
    DirTableTooLarge { path: String, used: u64 },

    #[error("image would need {sectors} sectors, past the u32 sector numbers XDVDFS records")]
    ImageTooLarge { sectors: u64 },

    #[error("operation cancelled")]
    Cancelled,

    #[error("dirent name {name:?} is unsafe to extract (absolute or path-traversing)")]
    UnsafeName { name: String },
}

/// Convenience alias for a [`Result`] with [`XboxError`].
pub type XboxResult<T> = Result<T, XboxError>;
