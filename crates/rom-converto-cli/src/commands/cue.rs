use crate::commands::ConflictPolicyArg;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands for CUE/BIN disc images.
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum CueCommands {
    Merge(MergeCommand),
}

/// Merge a multi-bin .cue disc image into a single .bin and .cue pair.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct MergeCommand {
    /// Input .cue file referencing multiple .bin files
    #[arg(value_name = "INPUT_CUE")]
    pub input_cue: PathBuf,

    /// Output .cue file path, the merged .bin is named after it
    #[arg(value_name = "OUTPUT_CUE")]
    pub output_cue: PathBuf,

    /// What to do when an output already exists: error, overwrite, skip, or rename to a numbered sibling
    #[arg(long = "on-conflict", value_enum, default_value_t = ConflictPolicyArg::Error)]
    pub on_conflict: ConflictPolicyArg,

    /// Alias for --on-conflict overwrite
    #[arg(
        long,
        short = 'f',
        default_value_t = false,
        conflicts_with = "on_conflict"
    )]
    pub force: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Parser, Debug)]
    struct Harness {
        #[command(subcommand)]
        cmd: CueCommands,
    }

    #[test]
    fn parses_on_conflict_skip() {
        let h = Harness::parse_from(["bin", "merge", "in.cue", "out.cue", "--on-conflict", "skip"]);
        let CueCommands::Merge(c) = h.cmd;
        assert_eq!(c.on_conflict, ConflictPolicyArg::Skip);
    }

    #[test]
    fn parses_on_conflict_rename() {
        let h = Harness::parse_from([
            "bin",
            "merge",
            "in.cue",
            "out.cue",
            "--on-conflict",
            "rename",
        ]);
        let CueCommands::Merge(c) = h.cmd;
        assert_eq!(c.on_conflict, ConflictPolicyArg::Rename);
    }

    #[test]
    fn force_still_accepted() {
        let h = Harness::parse_from(["bin", "merge", "in.cue", "out.cue", "-f"]);
        let CueCommands::Merge(c) = h.cmd;
        assert!(c.force);
        assert_eq!(c.on_conflict, ConflictPolicyArg::Error);
    }

    #[test]
    fn force_and_on_conflict_conflict() {
        let result = Harness::try_parse_from([
            "bin",
            "merge",
            "in.cue",
            "out.cue",
            "-f",
            "--on-conflict",
            "skip",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn defaults_on_conflict_to_error() {
        let h = Harness::parse_from(["bin", "merge", "in.cue", "out.cue"]);
        let CueCommands::Merge(c) = h.cmd;
        assert_eq!(c.on_conflict, ConflictPolicyArg::Error);
    }
}
