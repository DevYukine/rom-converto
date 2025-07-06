use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands specific to CHD formats
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum ChdCommands {
    Compress(CompressCommand),
    Extract(ExtractCommand),
    Verify(VerifyCommand),
}

/// Compresses a .bin and .cue file to a CHD (Compressed Hunks of Data) file.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct CompressCommand {
    /// Input path containing the .bin and .cue file
    #[arg(value_name = "INPUT_CUE")]
    pub input_cue: PathBuf,

    /// Output chd file path
    #[arg(value_name = "OUTPUT")]
    pub output: PathBuf,

    /// Force overwrite of the output file if it already exists
    #[arg(long, short = 'f', value_name = "FORCE", default_value_t = false)]
    pub force: bool,
}

/// Extracts files from a CHD file to a specified output directory.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct ExtractCommand {
    /// Input path containing the CHD file
    pub input: PathBuf,

    /// Output path for extracted files
    pub output: PathBuf,
}

/// Verifies the integrity of a CHD file.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct VerifyCommand {
    /// Input path containing the CHD file
    pub input: PathBuf,
}
