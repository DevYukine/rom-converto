use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands for PSP EBOOT.PBP containers
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum PspCommands {
    Info(InfoCommand),
    Extract(ExtractCommand),
}

/// Extract every segment from a PSP EBOOT.PBP
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Extract every segment from a PSP EBOOT.PBP\n\n\
Writes each present segment (PARAM.SFO, ICON0.PNG, ..., DATA.PSAR) into OUTPUT_DIR under its \
standard name. DATA.PSAR is written as stored, so it stays encrypted for an NPUMDIMG image; \
there is no EBOOT to ISO conversion.",
    after_long_help = "EXAMPLES:\n  rom-converto psp extract EBOOT.PBP ./out\n"
)]
pub struct ExtractCommand {
    /// Input EBOOT path (.pbp)
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
        cmd: PspCommands,
    }

    #[test]
    fn parses_extract() {
        let h = Harness::parse_from(["bin", "extract", "EBOOT.PBP", "./out"]);
        let PspCommands::Extract(c) = h.cmd else {
            panic!("expected Extract");
        };
        assert_eq!(c.input, PathBuf::from("EBOOT.PBP"));
        assert_eq!(c.output_dir, PathBuf::from("./out"));
    }

    #[test]
    fn extract_requires_output_dir() {
        assert!(Harness::try_parse_from(["bin", "extract", "EBOOT.PBP"]).is_err());
    }

    #[test]
    fn parses_info() {
        let h = Harness::parse_from(["bin", "info", "EBOOT.PBP"]);
        let PspCommands::Info(c) = h.cmd else {
            panic!("expected Info");
        };
        assert_eq!(c.input, Some(PathBuf::from("EBOOT.PBP")));
    }
}
