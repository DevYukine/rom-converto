//! Error type for the self-update flow.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum UpdaterError {
    /// No release asset name matches the current OS/architecture.
    #[error("no prebuild found for the current platform")]
    NoPrebuildFoundError,
}
