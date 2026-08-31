use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands specific to the original Xbox XISO format
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum XboxCommands {
    Convert(ConvertCommand),
    Extract(ExtractCommand),
    Info(InfoCommand),
}

/// Convert to an Xbox XISO
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Convert to an Xbox XISO\n\n\
Accepts either a full disc image (.iso), which is trimmed down to the game partition and re-laid \
out if it carries a video partition, or a directory of already-extracted game files, which is \
packed fresh.\n\n\
Every .xbe in the output has its XDK media-type check patched by default: xdvdfs-built images are \
known not to boot on some BIOSes without it, and the patch is inert on the ones that do not need \
it. Pass --no-media-patch to leave .xbe files untouched.\n\n\
Output defaults to the input path with the extension replaced by .xiso.",
    after_long_help = "EXAMPLES:\n  From a full disc image: rom-converto xbox convert game.iso\n  From a directory:       rom-converto xbox convert ./gamefiles game.xiso\n  No media patch:         rom-converto xbox convert game.iso --no-media-patch\n"
)]
pub struct ConvertCommand {
    /// Input full disc image (.iso), or a directory of already-extracted game files
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output XISO path, defaults to the input path with extension replaced by .xiso
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output XISO path, defaults to the input path with extension replaced by .xiso
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    /// Skip patching the XDK media-type check in every .xbe
    #[arg(long = "no-media-patch", default_value_t = false)]
    pub no_media_patch: bool,

    /// Overwrite the output file if it already exists
    #[arg(long, short = 'f', default_value_t = false)]
    pub force: bool,
}

/// Extract every file from an XISO
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Extract every file from an XISO\n\nWalks the disc's XDVDFS directory tree and writes every file into OUTPUT_DIR, mirroring the disc's layout.",
    after_long_help = "EXAMPLES:\n  rom-converto xbox extract game.xiso ./out\n"
)]
pub struct ExtractCommand {
    /// Input XISO path (.xiso or .iso)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Directory to extract into, created if missing
    #[arg(value_name = "OUTPUT_DIR")]
    pub output_dir: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Parser, Debug)]
    struct Harness {
        #[command(subcommand)]
        cmd: XboxCommands,
    }

    #[test]
    fn parses_convert_defaults() {
        let h = Harness::parse_from(["bin", "convert", "game.iso"]);
        let XboxCommands::Convert(c) = h.cmd else {
            panic!("expected Convert");
        };
        assert_eq!(c.output, None);
        assert_eq!(c.output_flag, None);
        assert!(!c.no_media_patch);
        assert!(!c.force);
    }

    #[test]
    fn parses_convert_no_media_patch_and_force() {
        let h = Harness::parse_from(["bin", "convert", "game.iso", "--no-media-patch", "-f"]);
        let XboxCommands::Convert(c) = h.cmd else {
            panic!("expected Convert");
        };
        assert!(c.no_media_patch);
        assert!(c.force);
    }

    #[test]
    fn parses_convert_output_flag() {
        let h = Harness::parse_from(["bin", "convert", "src_dir", "-o", "out.xiso"]);
        let XboxCommands::Convert(c) = h.cmd else {
            panic!("expected Convert");
        };
        assert_eq!(c.output, None);
        assert_eq!(c.output_flag, Some(PathBuf::from("out.xiso")));
    }

    #[test]
    fn convert_output_flag_conflicts_with_positional() {
        let result =
            Harness::try_parse_from(["bin", "convert", "game.iso", "pos.xiso", "-o", "flag.xiso"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_extract() {
        let h = Harness::parse_from(["bin", "extract", "game.xiso", "./out"]);
        let XboxCommands::Extract(c) = h.cmd else {
            panic!("expected Extract");
        };
        assert_eq!(c.input, PathBuf::from("game.xiso"));
        assert_eq!(c.output_dir, PathBuf::from("./out"));
    }

    #[test]
    fn extract_requires_output_dir() {
        let result = Harness::try_parse_from(["bin", "extract", "game.xiso"]);
        assert!(result.is_err());
    }
}
