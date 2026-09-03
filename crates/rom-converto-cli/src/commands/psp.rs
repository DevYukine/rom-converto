use crate::commands::ConflictPolicyArg;
use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands for PSP EBOOT.PBP containers
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum PspCommands {
    Info(InfoCommand),
    Extract(ExtractCommand),
    ToIso(ToIsoCommand),
}

/// Extract every segment from a PSP EBOOT.PBP
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Extract every segment from a PSP EBOOT.PBP\n\n\
Writes each present segment (PARAM.SFO, ICON0.PNG, ..., DATA.PSAR) into OUTPUT_DIR under its \
standard name. DATA.PSAR is written as stored, so it stays encrypted for an NPUMDIMG image; \
use `psp to-iso` to decrypt one into an ISO.",
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

/// Convert a PSP EBOOT.PBP to an ISO
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Convert a PSP EBOOT.PBP to an ISO\n\n\
Decrypts the NPUMDIMG image inside DATA.PSAR and writes the UMD ISO it holds. PS1 Classic \
EBOOTs (PSISOIMG/PSTITLEIMG) are not converted. Defaults to <INPUT>.iso next to the input.",
    after_long_help = "EXAMPLES:\n  rom-converto psp to-iso EBOOT.PBP\n  \
rom-converto psp to-iso EBOOT.PBP game.iso\n"
)]
pub struct ToIsoCommand {
    /// Input EBOOT path (.pbp)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output ISO path, defaults to <INPUT>.iso
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output path template applied per file. Tokens: {title}, {titleId}, {region},
    /// {console}, {serial}, {ext}, {basename}. Resolves against extracted metadata;
    /// missing tokens fall back to the input basename
    #[arg(
        long = "output-template",
        value_name = "TEMPLATE",
        conflicts_with = "output"
    )]
    pub output_template: Option<String>,

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

    /// Write a run report to FILE. Format inferred from the extension: .csv, .json, .html or .htm. Unknown extensions default to JSON. The file is overwritten directly
    #[arg(long = "report", value_name = "FILE")]
    pub report: Option<PathBuf>,
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
    fn parses_to_iso_with_and_without_an_output() {
        let h = Harness::parse_from(["bin", "to-iso", "EBOOT.PBP"]);
        let PspCommands::ToIso(c) = h.cmd else {
            panic!("expected ToIso");
        };
        assert_eq!(c.input, PathBuf::from("EBOOT.PBP"));
        assert_eq!(c.output, None);

        let h = Harness::parse_from(["bin", "to-iso", "EBOOT.PBP", "game.iso"]);
        let PspCommands::ToIso(c) = h.cmd else {
            panic!("expected ToIso");
        };
        assert_eq!(c.output, Some(PathBuf::from("game.iso")));
    }

    #[test]
    fn to_iso_defaults_on_conflict_to_error() {
        let h = Harness::parse_from(["bin", "to-iso", "EBOOT.PBP"]);
        let PspCommands::ToIso(c) = h.cmd else {
            panic!("expected ToIso");
        };
        assert_eq!(c.on_conflict, ConflictPolicyArg::Error);
        assert!(!c.force);
    }

    #[test]
    fn to_iso_parses_on_conflict_skip() {
        let h = Harness::parse_from(["bin", "to-iso", "EBOOT.PBP", "--on-conflict", "skip"]);
        let PspCommands::ToIso(c) = h.cmd else {
            panic!("expected ToIso");
        };
        assert_eq!(c.on_conflict, ConflictPolicyArg::Skip);
    }

    #[test]
    fn to_iso_force_still_accepted() {
        let h = Harness::parse_from(["bin", "to-iso", "EBOOT.PBP", "-f"]);
        let PspCommands::ToIso(c) = h.cmd else {
            panic!("expected ToIso");
        };
        assert!(c.force);
        assert_eq!(c.on_conflict, ConflictPolicyArg::Error);
    }

    #[test]
    fn to_iso_force_and_on_conflict_conflict() {
        let result =
            Harness::try_parse_from(["bin", "to-iso", "EBOOT.PBP", "-f", "--on-conflict", "skip"]);
        assert!(result.is_err());
    }

    #[test]
    fn to_iso_parses_output_template() {
        let h = Harness::parse_from([
            "bin",
            "to-iso",
            "EBOOT.PBP",
            "--output-template",
            "{title}.{ext}",
        ]);
        let PspCommands::ToIso(c) = h.cmd else {
            panic!("expected ToIso");
        };
        assert_eq!(c.output_template, Some("{title}.{ext}".to_string()));
    }

    #[test]
    fn to_iso_output_template_conflicts_with_output() {
        let result = Harness::try_parse_from([
            "bin",
            "to-iso",
            "EBOOT.PBP",
            "game.iso",
            "--output-template",
            "{title}.{ext}",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn to_iso_parses_report() {
        let h = Harness::parse_from(["bin", "to-iso", "EBOOT.PBP", "--report", "run.json"]);
        let PspCommands::ToIso(c) = h.cmd else {
            panic!("expected ToIso");
        };
        assert_eq!(c.report, Some(PathBuf::from("run.json")));
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
