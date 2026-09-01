//! Error type for the XDVDFS reader.

use thiserror::Error;

/// Errors from reading an XDVDFS volume.
#[derive(Debug, Error)]
pub enum XdvdfsError {
    #[error("no XDVDFS volume descriptor found at any probed base")]
    NoVolumeDescriptor,

    #[error("XDVDFS tail magic mismatch at base {base:#x}: image is corrupt")]
    TailMagicMismatch { base: u64 },

    #[error("invalid directory entry at dirtab offset {offset:#x}: {reason}")]
    InvalidDirent { offset: usize, reason: &'static str },

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Convenience alias for a [`Result`] with [`XdvdfsError`].
pub type XdvdfsResult<T> = Result<T, XdvdfsError>;
