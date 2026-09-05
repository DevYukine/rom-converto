use crate::commands::ConflictPolicyArg;
use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands specific to Xbox 360 disc images and the ZArchive format
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum XenonCommands {
    Compress(CompressCommand),
    Convert(ConvertCommand),
    Extract(ExtractCommand),
    Verify(VerifyCommand),
    Info(InfoCommand),
}

/// Pack an Xbox 360 ZArchive
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Pack an Xbox 360 ZArchive\n\n\
Accepts either a full disc image (an XDVDFS-formatted .iso) or a directory of already-extracted \
game files. Content always lands at the archive root, matching what Xenia expects to mount.\n\n\
Output defaults to the input path with the extension replaced by .zar.",
    after_long_help = "EXAMPLES:\n  From a full disc image: rom-converto xenon compress game.iso\n  From a directory:       rom-converto xenon compress ./gamefiles game.zar\n"
)]
pub struct CompressCommand {
    /// Input full disc image (.iso), or a directory of already-extracted game files
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output ZArchive path, defaults to the input path with extension replaced by .zar
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output ZArchive path, defaults to the input path with extension replaced by .zar
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    /// Overwrite the output file if it already exists
    #[arg(long, short = 'f', default_value_t = false)]
    pub force: bool,
}

/// Convert an Xbox 360 ISO into a Games on Demand (GoD) container
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Convert an Xbox 360 ISO into a Games on Demand (GoD) container\n\n\
Copies the game partition out of the XDVDFS image into the hash-chained part-file layout a console \
installs under its content directory. The output installs on modified consoles and emulators, not \
on unmodified retail systems.\n\n\
Output defaults to a directory next to the input, named after it with _god appended.",
    after_long_help = "EXAMPLES:\n  rom-converto xenon convert game.iso\n  rom-converto xenon convert game.iso ./out --title \"Custom Title\"\n"
)]
pub struct ConvertCommand {
    /// Input full disc image (.iso)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output directory, defaults to the input path with _god appended
    #[arg(value_name = "OUTPUT_DIR")]
    pub output_dir: Option<PathBuf>,

    /// Display name written into the GoD container header, overriding the name read from the executable
    #[arg(long, value_name = "NAME")]
    pub title: Option<String>,

    /// What to do when the output directory already exists: error, overwrite, skip, rename, or overwrite-invalid. rename is rejected for directory outputs, and overwrite-invalid behaves like skip
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

/// Extract every file from an Xbox 360 ZArchive
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Extract every file from an Xbox 360 ZArchive\n\nWrites every file in the archive into OUTPUT_DIR.",
    after_long_help = "EXAMPLES:\n  rom-converto xenon extract game.zar ./out\n"
)]
pub struct ExtractCommand {
    /// Input ZArchive path (.zar)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Directory to extract into, created if missing
    #[arg(value_name = "OUTPUT_DIR")]
    pub output_dir: PathBuf,
}

/// Verify an Xbox 360 ZArchive
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Verify an Xbox 360 ZArchive\n\nRe-hashes the archive's stored digest and decodes every block to prove the compressed data is intact.",
    after_long_help = "EXAMPLES:\n  rom-converto xenon verify game.zar\n"
)]
pub struct VerifyCommand {
    /// Input ZArchive path (.zar)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Parser, Debug)]
    struct Harness {
        #[command(subcommand)]
        cmd: XenonCommands,
    }

    #[test]
    fn parses_compress_defaults() {
        let h = Harness::parse_from(["bin", "compress", "game.iso"]);
        let XenonCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.output, None);
        assert_eq!(c.output_flag, None);
        assert!(!c.force);
    }

    #[test]
    fn parses_compress_output_flag_and_force() {
        let h = Harness::parse_from(["bin", "compress", "src_dir", "-o", "out.zar", "-f"]);
        let XenonCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.output, None);
        assert_eq!(c.output_flag, Some(PathBuf::from("out.zar")));
        assert!(c.force);
    }

    #[test]
    fn compress_output_flag_conflicts_with_positional() {
        let result =
            Harness::try_parse_from(["bin", "compress", "game.iso", "pos.zar", "-o", "flag.zar"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_convert_defaults() {
        let h = Harness::parse_from(["bin", "convert", "game.iso"]);
        let XenonCommands::Convert(c) = h.cmd else {
            panic!("expected Convert");
        };
        assert_eq!(c.output_dir, None);
        assert_eq!(c.title, None);
        assert_eq!(c.on_conflict, ConflictPolicyArg::Error);
        assert!(!c.force);
    }

    #[test]
    fn parses_convert_output_dir_and_title() {
        let h = Harness::parse_from(["bin", "convert", "game.iso", "./out", "--title", "Name"]);
        let XenonCommands::Convert(c) = h.cmd else {
            panic!("expected Convert");
        };
        assert_eq!(c.output_dir, Some(PathBuf::from("./out")));
        assert_eq!(c.title, Some("Name".to_string()));
    }

    #[test]
    fn convert_force_and_on_conflict_conflict() {
        let result =
            Harness::try_parse_from(["bin", "convert", "game.iso", "-f", "--on-conflict", "skip"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_extract() {
        let h = Harness::parse_from(["bin", "extract", "game.zar", "./out"]);
        let XenonCommands::Extract(c) = h.cmd else {
            panic!("expected Extract");
        };
        assert_eq!(c.input, PathBuf::from("game.zar"));
        assert_eq!(c.output_dir, PathBuf::from("./out"));
    }

    #[test]
    fn extract_requires_output_dir() {
        let result = Harness::try_parse_from(["bin", "extract", "game.zar"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_verify() {
        let h = Harness::parse_from(["bin", "verify", "game.zar"]);
        let XenonCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert_eq!(c.input, PathBuf::from("game.zar"));
    }
}
