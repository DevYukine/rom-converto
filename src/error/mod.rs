use std::path::PathBuf;
use std::result;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum RomConvertoError {
    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error("Could not find the title file in the specified path: {0}")]
    NoTitleFileFound(PathBuf),

    #[error("Could not find at least one TMD file in the specified path: {0}")]
    NoTmdFileFound(PathBuf),
}

pub type RomConvertoResult<T> = result::Result<T, RomConvertoError>;
