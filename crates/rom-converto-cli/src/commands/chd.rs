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

    /// Force overwrite of the output file if it already exists
    #[arg(long, short = 'f', value_name = "FORCE", default_value_t = false)]
    pub force: bool,

    /// Compress every .cue and .iso found in the INPUT directory
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,
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
        required_unless_present_any = ["recursive", "output_flag"]
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

    /// Optional parent CHD file (for CHDs that reference a parent); not allowed with --recursive
    #[arg(long, short = 'p', value_name = "PARENT")]
    pub parent: Option<PathBuf>,

    /// Extract every .chd in INPUT (top-level only); outputs go beside each input
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Overwrite the output file if it already exists
    #[arg(long, short = 'f', default_value_t = false)]
    pub force: bool,
}

/// Verify the integrity of a CHD file.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct VerifyCommand {
    /// Input CHD file, or a directory of .chd files when --recursive is set
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Optional parent CHD file (for CHDs that reference a parent); not allowed with --recursive
    #[arg(long, short = 'p', value_name = "PARENT")]
    pub parent: Option<PathBuf>,

    /// Fix incorrect SHA1 values in the header
    #[arg(long)]
    pub fix: bool,

    /// Verify every .chd in INPUT (top-level only)
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,
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
}
