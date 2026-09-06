use crate::commands::ConflictPolicyArg;
use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::commands::support::{
    DispatchCtx, finish_single, log_single_summary, require_info_input, save_pbp_icon,
};
use crate::util::{
    WriteDecision, ensure_input_exists, file_len, log_skipped, resolve_output, resolve_output_dir,
};
use crate::{batch, dry_run, info_print};
use anyhow::Result;
use rom_converto_lib::util::TallyDirection;
use std::path::Path;
use std::time::Instant;

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

/// Convert a PSP EBOOT.PBP or PSN .pkg to an ISO
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Convert a PSP EBOOT.PBP or PSN .pkg to an ISO\n\n\
Decrypts the NPUMDIMG image inside DATA.PSAR and writes the UMD ISO it holds. A PSN .pkg is \
read in place, so its EBOOT.PBP need not be extracted first. PS1 Classic EBOOTs and packages \
(PSISOIMG/PSTITLEIMG) are not converted. Defaults to <INPUT>.iso next to the input.",
    after_long_help = "EXAMPLES:\n  rom-converto psp to-iso EBOOT.PBP\n  \
rom-converto psp to-iso game.pkg\n  rom-converto psp to-iso EBOOT.PBP game.iso\n"
)]
pub struct ToIsoCommand {
    /// Input EBOOT or package path (.pbp or .pkg)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,
    /// Output ISO path, defaults to <INPUT>.iso
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,
    /// Output path template applied per file. Tokens: {title}, {titleId}, {region}, /// {console}, {serial}, {ext}, {basename}. Resolves against extracted metadata};
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

/// Runs one `psp` subcommand.
pub async fn run(command: PspCommands, ctx: DispatchCtx<'_>) -> Result<()> {
    let DispatchCtx {
        progress,
        dry_run,
        skip_space_check,
        ..
    } = ctx;
    match command {
        PspCommands::Info(cmd) => {
            if cmd.keys.is_some() {
                anyhow::bail!("--keys is only supported by nx, wup, and ps3 info");
            }
            let input = require_info_input(&cmd.input)?;
            ensure_input_exists(input)?;
            let resolved = rom_converto_lib::util::resolve_input(input, &["pbp"])?;
            let info = rom_converto_lib::sony::psp::read_info(resolved.path())?;
            if let Some(dir) = &cmd.save_icon {
                save_pbp_icon(&info, dir)?;
            }
            info_print::print(&rom_converto_lib::info::InfoResult::Pbp(info), cmd.json)?;
        }
        PspCommands::Extract(cmd) => {
            ensure_input_exists(&cmd.input)?;
            let policy = rom_converto_lib::util::ConflictPolicy::Error;
            match resolve_output_dir(&cmd.output_dir, policy)? {
                WriteDecision::Skip => {
                    log_skipped(&cmd.output_dir);
                    return Ok(());
                }
                WriteDecision::Write(_) => {}
            }
            let resolved = rom_converto_lib::util::resolve_input(&cmd.input, &["pbp"])?;
            let started = Instant::now();
            rom_converto_lib::sony::psp::extract_segments(
                &progress,
                resolved.path(),
                &cmd.output_dir,
            )?;
            log_single_summary(
                &cmd.input,
                &cmd.output_dir,
                TallyDirection::CountOnly,
                started,
            );
        }
        PspCommands::ToIso(cmd) => {
            ensure_input_exists(&cmd.input)?;
            let resolved = rom_converto_lib::util::resolve_input(&cmd.input, &["pbp", "pkg"])?;
            let input = resolved.path();
            let output = match cmd.output.clone() {
                Some(p) => p,
                None => match cmd.output_template.as_deref() {
                    Some(tmpl) => {
                        crate::util::templated_output(tmpl, input, None, "iso", None, dry_run)?
                    }
                    None => resolved.output_basis().with_extension("iso"),
                },
            };
            let policy = if cmd.force {
                rom_converto_lib::util::ConflictPolicy::Overwrite
            } else {
                cmd.on_conflict.into()
            };
            let decision = resolve_output(&output, policy)?;
            if dry_run {
                return dry_run::single(
                    "convert",
                    &cmd.input,
                    &output,
                    &decision,
                    None,
                    None,
                    cmd.report.as_deref(),
                );
            }
            let output = match decision {
                WriteDecision::Skip => {
                    log_skipped(&output);
                    return Ok(());
                }
                WriteDecision::Write(p) => p,
            };
            if !skip_space_check {
                let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                batch::space_preflight_for_size(file_len(input), check_dir)?;
            }
            let started = Instant::now();
            rom_converto_lib::sony::psp::to_iso(&progress, input, &output)?;
            finish_single(
                &cmd.input,
                &output,
                TallyDirection::Convert,
                "convert",
                started,
                cmd.report.as_deref(),
            )?;
        }
    }
    Ok(())
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
