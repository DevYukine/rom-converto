//! Error type for the WBFS module.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum WbfsError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),

    #[error("invalid WBFS magic: expected b\"WBFS\", got {0:02X?}")]
    InvalidMagic([u8; 4]),

    #[error("unsupported WBFS hd sector size shift {0}")]
    UnsupportedHdSectorSize(u8),

    #[error("unsupported WBFS sector size shift {0}: must be >= 15 (0x8000-byte Wii sector)")]
    UnsupportedWbfsSectorSize(u8),

    #[error("WBFS file declares no discs")]
    NoDiscs,

    #[error("input does not look like a GameCube or Wii disc image")]
    UnrecognizedDisc,

    #[error("disc image of {0} bytes is too large to store in a WBFS container")]
    DiscTooLarge(u64),

    #[error("{0}")]
    Custom(String),
}

pub type WbfsResult<T> = Result<T, WbfsError>;
