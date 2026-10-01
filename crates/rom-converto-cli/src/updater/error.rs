//! Error type for the self-update flow.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum UpdaterError {
    /// No release asset name matches the current OS/architecture.
    #[error("no prebuild found for the current platform")]
    NoPrebuildFoundError,

    /// The downloaded release asset does not hash to its published `.sha256`.
    #[error(
        "downloaded release does not match its published SHA-256 (expected {expected}, got {actual})"
    )]
    ChecksumMismatch { expected: String, actual: String },
}
