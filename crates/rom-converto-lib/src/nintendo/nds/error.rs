//! Error type for the Nintendo DS secure-area module.

use thiserror::Error;

/// Errors from Nintendo DS secure-area encryption and decryption.
#[derive(Debug, Error)]
pub enum NdsError {
    /// Wraps an underlying I/O failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Wraps a failed worker task join.
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),

    /// The input is shorter than the header plus the secure-area block.
    #[error("NDS ROM is too small to hold a secure area")]
    TooSmall,

    /// The ARM9 code starts outside the secure-area window, or the secure
    /// area is blank, so there is no secure area to work on.
    #[error(
        "NDS ROM has no secure area (ARM9 offset outside 0x4000..0x8000, or blank secure area); nothing to encrypt or decrypt"
    )]
    NoSecureArea,

    /// The secure area is already plaintext, so there is nothing to do.
    #[error("NDS secure area is already decrypted")]
    AlreadyDecrypted,

    /// The secure area is already ciphertext, so there is nothing to do.
    #[error("NDS secure area is already encrypted")]
    AlreadyEncrypted,

    /// The secure area is neither valid plaintext nor decryptable with the
    /// key derived from the header id code.
    #[error("NDS secure area is corrupt or was built with an unknown key")]
    SecureAreaCorrupt,

    /// The output ROM already exists and no overwrite was requested.
    #[error("output already exists; pass --on-conflict overwrite to replace it")]
    OutputAlreadyExists,

    /// The operation was cancelled by the caller.
    #[error("operation cancelled")]
    Cancelled,
}

/// Convenience alias for a [`Result`] with [`NdsError`].
pub type NdsResult<T> = Result<T, NdsError>;
