use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands for CUE/BIN disc images
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum CueCommands {
    Merge(MergeCommand),
}

/// Merges a multi-bin .cue disc image into a single .bin and .cue pair.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct MergeCommand {
    /// Input .cue file referencing multiple .bin files
    #[arg(value_name = "INPUT_CUE")]
    pub input_cue: PathBuf,

    /// Output .cue file path, the merged .bin is named after it
    #[arg(value_name = "OUTPUT_CUE")]
    pub output_cue: PathBuf,

    /// Force overwrite of the output files if they already exist
    #[arg(long, short = 'f', value_name = "FORCE", default_value_t = false)]
    pub force: bool,
}
