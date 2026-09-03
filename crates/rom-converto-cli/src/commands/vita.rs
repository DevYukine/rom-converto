use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands for PS Vita packages: VPK and PKG info, PKG extraction
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum VitaCommands {
    Info(InfoCommand),
    Extract(ExtractCommand),
}

/// Extract every file item from a PS Vita PKG
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Extract every file item from a PS Vita PKG\n\n\
Decrypts the package with its embedded key index and writes every file item into OUTPUT_DIR, \
keeping the paths the item table names.",
    after_long_help = "EXAMPLES:\n  rom-converto vita extract game.pkg ./out\n"
)]
pub struct ExtractCommand {
    /// Input package path (.pkg)
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
        cmd: VitaCommands,
    }

    #[test]
    fn parses_extract() {
        let h = Harness::parse_from(["bin", "extract", "game.pkg", "./out"]);
        let VitaCommands::Extract(c) = h.cmd else {
            panic!("expected Extract");
        };
        assert_eq!(c.input, PathBuf::from("game.pkg"));
        assert_eq!(c.output_dir, PathBuf::from("./out"));
    }

    #[test]
    fn extract_requires_output_dir() {
        assert!(Harness::try_parse_from(["bin", "extract", "game.pkg"]).is_err());
    }

    #[test]
    fn parses_info() {
        let h = Harness::parse_from(["bin", "info", "game.vpk"]);
        let VitaCommands::Info(c) = h.cmd else {
            panic!("expected Info");
        };
        assert_eq!(c.input, Some(PathBuf::from("game.vpk")));
    }
}
