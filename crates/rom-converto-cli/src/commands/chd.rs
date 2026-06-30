use crate::commands::ConflictPolicyArg;
use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands specific to CHD formats.
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum ChdCommands {
    Compress(CompressCommand),
    Extract(ExtractCommand),
    Verify(VerifyCommand),
    Info(InfoCommand),
}

/// Compress a disc image to a CHD (Compressed Hunks of Data) file.
///
/// A .cue input (with its .bin) becomes a CD-mode CHD. An .iso is
/// probed for its console family: CD-media images (PS1, PS2-CD)
/// become CD-mode CHDs with a single MODE1/2048 track (the chdman
/// createcd equivalent), DVD-media images (PS2-DVD, PSP) become
/// DVD-mode CHDs (the createdvd equivalent). The mode is picked
/// automatically so the createcd/createdvd mixup cannot happen.
/// Default DVD codecs are lzma+zlib, which every emulator reads,
/// including AetherSX2/NetherSX2.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto chd compress game.cue\n  Explicit output: rom-converto chd compress game.iso out.chd\n  Whole folder:    rom-converto chd compress -R ./roms --output-dir ./chd\n"
)]
pub struct CompressCommand {
    /// Input image (.cue, or .iso with CD/DVD media auto-detected), or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output chd file path, defaults to the input path with extension replaced by .chd
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output chd file path, defaults to the input path with extension replaced by .chd
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    /// Write output into this directory using the derived filename. Created if missing. Works with --recursive.
    #[arg(long = "output-dir", value_name = "DIR", conflicts_with_all = ["output", "output_flag"])]
    pub output_dir: Option<PathBuf>,

    /// Output path template applied per file. Tokens: {title}, {titleId}, {region},
    /// {console}, {serial}, {ext}, {basename}. Resolves against extracted metadata;
    /// missing tokens fall back to the input basename. Joined under --output-dir.
    #[arg(long = "output-template", value_name = "TEMPLATE", conflicts_with_all = ["output", "output_flag"])]
    pub output_template: Option<String>,

    /// Force DVD mode (.iso input only)
    #[arg(long, conflicts_with = "cd")]
    pub dvd: bool,

    /// Force CD mode (a .cue, or a CD-media .iso)
    #[arg(long, conflicts_with = "dvd")]
    pub cd: bool,

    /// DVD hunk size in bytes, a multiple of 2048. Defaults to 4096,
    /// or 2048 for detected PSP images (PPSSPP reads 2048-byte blocks)
    #[arg(long, value_name = "BYTES")]
    pub hunk_size: Option<u32>,

    /// Add zstd to the DVD codec set: slightly better ratio, but the
    /// output is rejected by AetherSX2/NetherSX2 (outdated libchdr)
    #[arg(long)]
    pub zstd: bool,

    /// What to do when an output already exists: error, overwrite, skip, or rename to a numbered sibling
    #[arg(long = "on-conflict", value_enum)]
    pub on_conflict: Option<ConflictPolicyArg>,

    /// Alias for --on-conflict overwrite
    #[arg(
        long,
        short = 'f',
        default_value_t = false,
        conflicts_with = "on_conflict"
    )]
    pub force: bool,

    /// Compress every .cue and .iso found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited.
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

    /// Write a run report to FILE. Format inferred from the extension: .csv, .json, .html or .htm. Unknown extensions default to JSON. The file is overwritten directly.
    #[arg(long = "report", value_name = "FILE")]
    pub report: Option<PathBuf>,
}

/// Extract files from a CHD file to a specified output directory.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct ExtractCommand {
    /// Input CHD file, or a directory of .chd files when --recursive is set
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output path for extracted files (ignored with --recursive)
    #[arg(
        value_name = "OUTPUT",
        required_unless_present_any = ["recursive", "output_flag", "output_dir"]
    )]
    pub output: Option<PathBuf>,

    /// Output path for extracted files (ignored with --recursive)
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    /// Write output into this directory using the derived filename. Created if missing. Works with --recursive.
    #[arg(long = "output-dir", value_name = "DIR", conflicts_with_all = ["output", "output_flag"])]
    pub output_dir: Option<PathBuf>,

    /// Output path template applied per file. Tokens: {title}, {titleId}, {region},
    /// {console}, {serial}, {ext}, {basename}. Resolves against extracted metadata;
    /// missing tokens fall back to the input basename. Joined under --output-dir.
    #[arg(long = "output-template", value_name = "TEMPLATE", conflicts_with_all = ["output", "output_flag"])]
    pub output_template: Option<String>,

    /// Optional parent CHD file (for CHDs that reference a parent); not allowed with --recursive
    #[arg(long, short = 'p', value_name = "PARENT", conflicts_with = "recursive")]
    pub parent: Option<PathBuf>,

    /// Extract every .chd in INPUT and its subdirectories; outputs go beside each input
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited.
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

    /// What to do when an output already exists: error, overwrite, skip, or rename to a numbered sibling
    #[arg(long = "on-conflict", value_enum)]
    pub on_conflict: Option<ConflictPolicyArg>,

    /// Alias for --on-conflict overwrite
    #[arg(
        long,
        short = 'f',
        default_value_t = false,
        conflicts_with = "on_conflict"
    )]
    pub force: bool,

    /// Write a run report to FILE. Format inferred from the extension: .csv, .json, .html or .htm. Unknown extensions default to JSON. The file is overwritten directly.
    #[arg(long = "report", value_name = "FILE")]
    pub report: Option<PathBuf>,
}

/// Verify the integrity of a CHD file.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct VerifyCommand {
    /// Input CHD file, or a directory of .chd files when --recursive is set
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Optional parent CHD file (for CHDs that reference a parent); not allowed with --recursive
    #[arg(long, short = 'p', value_name = "PARENT", conflicts_with = "recursive")]
    pub parent: Option<PathBuf>,

    /// Fix incorrect SHA1 values in the header
    #[arg(long)]
    pub fix: bool,

    /// Verify every .chd in INPUT and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited.
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Parser, Debug)]
    struct Harness {
        #[command(subcommand)]
        cmd: ChdCommands,
    }

    #[test]
    fn parses_compress_defaults() {
        let h = Harness::parse_from(["bin", "compress", "game.iso"]);
        let ChdCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.input, PathBuf::from("game.iso"));
        assert_eq!(c.output, None);
        assert!(!c.dvd && !c.cd && !c.zstd && !c.force && !c.recursive);
        assert_eq!(c.hunk_size, None);
    }

    #[test]
    fn parses_compress_dvd_flags() {
        let h = Harness::parse_from([
            "bin",
            "compress",
            "game.iso",
            "game.chd",
            "--dvd",
            "--hunk-size",
            "2048",
            "--zstd",
            "-R",
        ]);
        let ChdCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.output, Some(PathBuf::from("game.chd")));
        assert!(c.dvd && c.zstd && c.recursive);
        assert_eq!(c.hunk_size, Some(2048));
    }

    #[test]
    fn rejects_cd_and_dvd_together() {
        let result = Harness::try_parse_from(["bin", "compress", "x.cue", "--cd", "--dvd"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_compress_cd_flag_on_iso() {
        let h = Harness::parse_from(["bin", "compress", "--cd", "game.iso"]);
        let ChdCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.cd && !c.dvd);
        assert_eq!(c.input, PathBuf::from("game.iso"));
    }

    #[test]
    fn verify_parses_recursive_flag() {
        let h = Harness::parse_from(["bin", "verify", "-R", "dir"]);
        let ChdCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert!(c.recursive);
        assert_eq!(c.input, PathBuf::from("dir"));
    }

    #[test]
    fn extract_output_optional_with_recursive() {
        let h = Harness::parse_from(["bin", "extract", "-R", "dir"]);
        let ChdCommands::Extract(c) = h.cmd else {
            panic!("expected Extract");
        };
        assert!(c.recursive);
        assert!(c.output.is_none());
        assert!(Harness::try_parse_from(["bin", "extract", "in.chd"]).is_err());
    }

    #[test]
    fn extract_output_flag_satisfies_requirement() {
        let h = Harness::parse_from(["bin", "extract", "in.chd", "-o", "out.cue"]);
        let ChdCommands::Extract(c) = h.cmd else {
            panic!("expected Extract");
        };
        assert!(c.output.is_none());
        assert_eq!(c.output_flag, Some(PathBuf::from("out.cue")));
    }

    #[test]
    fn extract_output_flag_conflicts_with_positional() {
        let result =
            Harness::try_parse_from(["bin", "extract", "in.chd", "pos.cue", "-o", "flag.cue"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_compress_output_dir() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--output-dir", "out"]);
        let ChdCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.output_dir, Some(PathBuf::from("out")));
        assert_eq!(c.output, None);
    }

    #[test]
    fn max_depth_parses_with_recursive() {
        let h = Harness::parse_from(["bin", "verify", "-R", "--max-depth", "2", "dir"]);
        let ChdCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert!(c.recursive);
        assert_eq!(c.max_depth, Some(2));
    }

    #[test]
    fn max_depth_requires_recursive() {
        let result = Harness::try_parse_from(["bin", "verify", "--max-depth", "2", "dir"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_on_conflict_skip() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--on-conflict", "skip"]);
        let ChdCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.on_conflict, Some(ConflictPolicyArg::Skip));
    }

    #[test]
    fn parses_on_conflict_rename() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--on-conflict", "rename"]);
        let ChdCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.on_conflict, Some(ConflictPolicyArg::Rename));
    }

    #[test]
    fn force_still_accepted() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "-f"]);
        let ChdCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.force);
        assert!(c.on_conflict.is_none());
    }

    #[test]
    fn force_and_on_conflict_conflict() {
        let result =
            Harness::try_parse_from(["bin", "compress", "game.iso", "-f", "--on-conflict", "skip"]);
        assert!(result.is_err());
    }

    #[test]
    fn on_conflict_absent_is_none() {
        let h = Harness::parse_from(["bin", "compress", "game.iso"]);
        let ChdCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.on_conflict.is_none());
    }

    #[test]
    fn extract_output_dir_satisfies_requirement() {
        let h = Harness::parse_from(["bin", "extract", "in.chd", "--output-dir", "out"]);
        let ChdCommands::Extract(c) = h.cmd else {
            panic!("expected Extract");
        };
        assert!(c.output.is_none());
        assert_eq!(c.output_dir, Some(PathBuf::from("out")));
    }

    #[test]
    fn parses_compress_report_flag() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--report", "out.json"]);
        let ChdCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.report, Some(PathBuf::from("out.json")));
    }

    #[test]
    fn parses_extract_report_flag() {
        let h = Harness::parse_from(["bin", "extract", "in.chd", "out", "--report", "out.csv"]);
        let ChdCommands::Extract(c) = h.cmd else {
            panic!("expected Extract");
        };
        assert_eq!(c.report, Some(PathBuf::from("out.csv")));
    }
}
