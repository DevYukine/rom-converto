use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands specific to CHD formats
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum ChdCommands {
    Compress(CompressCommand),
    Extract(ExtractCommand),
    Verify(VerifyCommand),
    Info(InfoCommand),
}

/// Compresses a disc image to a CHD (Compressed Hunks of Data) file.
///
/// A .cue input (with its .bin) becomes a CD-mode CHD; a PS2/PSP .iso
/// becomes a DVD-mode CHD, the chdman createdvd equivalent. The mode
/// is picked automatically so the createcd/createdvd mixup cannot
/// happen. Default DVD codecs are lzma+zlib, which every emulator
/// reads, including AetherSX2/NetherSX2.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct CompressCommand {
    /// Input image (.cue for CD, .iso for PS2/PSP DVD), or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output chd file path, defaults to the input path with extension replaced by .chd
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Force DVD mode regardless of the input extension
    #[arg(long, conflicts_with = "cd")]
    pub dvd: bool,

    /// Force CD mode (the input must be a cue sheet)
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

/// Extracts files from a CHD file to a specified output directory.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct ExtractCommand {
    /// Input path containing the CHD file
    pub input: PathBuf,

    /// Output path for extracted files
    pub output: PathBuf,

    /// Optional parent CHD file (for CHDs that reference a parent)
    #[arg(long, short = 'p', value_name = "PARENT")]
    pub parent: Option<PathBuf>,
}

/// Verifies the integrity of a CHD file.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct VerifyCommand {
    /// Input path containing the CHD file
    pub input: PathBuf,

    /// Optional parent CHD file (for CHDs that reference a parent)
    #[arg(long, short = 'p', value_name = "PARENT")]
    pub parent: Option<PathBuf>,

    /// Fix incorrect SHA1 values in the header
    #[arg(long)]
    pub fix: bool,
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
}
