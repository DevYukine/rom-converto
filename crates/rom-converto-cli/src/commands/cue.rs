use crate::commands::ConflictPolicyArg;
use crate::commands::cso::CsoFormatArg;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands for CUE/BIN disc images
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum CueCommands {
    Merge(MergeCommand),
    ToIso(ToIsoCommand),
    ToCso(ToCsoCommand),
}

/// Merge a multi-bin .cue disc image into a single .bin and .cue pair
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  Merge tracks: rom-converto cue merge game.cue merged.cue\n"
)]
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

/// Convert a .cue/.bin disc image's data track to a plain .iso
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Convert a .cue/.bin disc image's data track to a plain .iso\n\nExtracts the first track (which must be a MODE1/MODE2 data track) to 2048-byte ISO sectors. Any audio tracks are skipped.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto cue to-iso game.cue\n  Explicit output: rom-converto cue to-iso game.cue game.iso\n"
)]
pub struct ToIsoCommand {
    /// Input .cue file
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output .iso path, defaults to the input with extension replaced by .iso
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

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

/// Convert a .cue/.bin disc image's data track straight to a .cso or .zso
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Convert a .cue/.bin disc image's data track straight to a .cso or .zso\n\nExtracts the data track to a temporary ISO, then compresses it, always removing the temporary ISO afterward.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto cue to-cso game.cue\n  Explicit output: rom-converto cue to-cso game.cue game.cso --format cso\n"
)]
pub struct ToCsoCommand {
    /// Input .cue file
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output path, defaults to the input with the format's extension
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output container format
    #[arg(long, value_enum, default_value_t = CsoFormatArg::Zso)]
    pub format: CsoFormatArg,

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
        let CueCommands::Merge(c) = h.cmd else {
            panic!("expected Merge");
        };
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
        let CueCommands::Merge(c) = h.cmd else {
            panic!("expected Merge");
        };
        assert_eq!(c.on_conflict, ConflictPolicyArg::Rename);
    }

    #[test]
    fn force_still_accepted() {
        let h = Harness::parse_from(["bin", "merge", "in.cue", "out.cue", "-f"]);
        let CueCommands::Merge(c) = h.cmd else {
            panic!("expected Merge");
        };
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
        let CueCommands::Merge(c) = h.cmd else {
            panic!("expected Merge");
        };
        assert_eq!(c.on_conflict, ConflictPolicyArg::Error);
    }

    #[test]
    fn parses_to_iso_defaults() {
        let h = Harness::parse_from(["bin", "to-iso", "game.cue"]);
        let CueCommands::ToIso(c) = h.cmd else {
            panic!("expected ToIso");
        };
        assert_eq!(c.input, PathBuf::from("game.cue"));
        assert_eq!(c.output, None);
        assert_eq!(c.on_conflict, ConflictPolicyArg::Error);
        assert!(!c.force);
    }

    #[test]
    fn parses_to_cso_defaults_to_zso() {
        let h = Harness::parse_from(["bin", "to-cso", "game.cue"]);
        let CueCommands::ToCso(c) = h.cmd else {
            panic!("expected ToCso");
        };
        assert_eq!(c.format, CsoFormatArg::Zso);
        assert_eq!(c.output, None);
    }

    #[test]
    fn parses_to_cso_with_format_and_output() {
        let h = Harness::parse_from(["bin", "to-cso", "game.cue", "game.cso", "--format", "cso"]);
        let CueCommands::ToCso(c) = h.cmd else {
            panic!("expected ToCso");
        };
        assert_eq!(c.format, CsoFormatArg::Cso);
        assert_eq!(c.output, Some(PathBuf::from("game.cso")));
    }
}
