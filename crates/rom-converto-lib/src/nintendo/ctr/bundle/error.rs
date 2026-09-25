use std::path::PathBuf;

use thiserror::Error;

use super::MAX_MEMBERS;

/// Errors from Azahar bundle (`.bcia`/`.bcci`/`.bcxi`) planning, bundling,
/// and unbundling.
#[derive(Debug, Error)]
pub enum CtrBundleError {
    #[error("bundling needs at least one input ROM")]
    NoInputs,

    #[error("bundle output {0} is also one of the inputs")]
    OutputIsInput(PathBuf),

    #[error(
        "unsupported bundle member {0}: expected .cia/.zcia files plus at most one .cci/.3ds/.cxi (or Z-prefixed) main"
    )]
    UnsupportedMember(PathBuf),

    #[error(
        "more than one main ROM: {} and {}; a bundle holds exactly one .cci/.3ds/.cxi main plus .cia updates and DLC",
        .0.display(),
        .1.display()
    )]
    MultipleMains(PathBuf, PathBuf),

    #[error("member name {0:?} exceeds the 100-byte tar name limit; rename the file shorter")]
    NameTooLong(String),

    #[error("duplicate member name {0:?}; member names must be unique ignoring case")]
    DuplicateName(String),

    #[error("a bundle holds at most {MAX_MEMBERS} members, got {0}")]
    TooManyMembers(usize),

    #[error(
        "member {0} appears to be encrypted; Azahar plays decrypted ROMs only. Decrypt it first with: rom-converto ctr decrypt <INPUT>"
    )]
    Encrypted(PathBuf),

    #[error(
        "bundle member name {0:?} is not a plain file name; refusing to write outside the output directory"
    )]
    UnsafeName(String),

    #[error("{0} is not a bundle (not a valid tar archive)")]
    NotABundle(PathBuf),

    #[error("{path} is a malformed tar bundle: {reason}")]
    InvalidTar { path: PathBuf, reason: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    JoinError(#[from] tokio::task::JoinError),

    #[error("{0}")]
    Cancelled(#[from] crate::util::Cancelled),
}

/// Convenience alias for a `Result` with [`CtrBundleError`].
pub type CtrBundleResult<T> = Result<T, CtrBundleError>;

#[cfg(test)]
mod tests {
    use super::CtrBundleError;
    use crate::util::Cancelled;

    #[test]
    fn cancelled_is_discoverable_through_the_anyhow_chain() {
        let err = anyhow::Error::from(CtrBundleError::from(Cancelled));
        assert!(Cancelled::in_chain(&err));
    }
}
