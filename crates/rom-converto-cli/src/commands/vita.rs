use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::commands::support::{
    DispatchCtx, log_single_summary, require_info_input, save_info_icon,
};
use crate::info_print;
use crate::util::{WriteDecision, ensure_input_exists, log_skipped, resolve_output_dir};
use anyhow::Result;
use rom_converto_lib::util::TallyDirection;
use std::time::Instant;

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

/// Runs one `vita` subcommand.
pub async fn run(command: VitaCommands, ctx: DispatchCtx<'_>) -> Result<()> {
    let DispatchCtx { progress, .. } = ctx;
    match command {
        VitaCommands::Info(cmd) => {
            if cmd.keys.is_some() {
                anyhow::bail!("--keys is only supported by nx, wup, and ps3 info");
            }
            let input = require_info_input(&cmd.input)?;
            ensure_input_exists(input)?;
            let resolved = rom_converto_lib::util::resolve_input(input, &["vpk", "pkg"])?;
            let info = rom_converto_lib::info::read_info(
                resolved.path(),
                &rom_converto_lib::info::InfoOptions::default(),
            )?;
            if let Some(dir) = &cmd.save_icon {
                save_info_icon(&info, dir)?;
            }
            info_print::print(&info, cmd.json)?;
        }
        VitaCommands::Extract(cmd) => {
            ensure_input_exists(&cmd.input)?;
            let policy = rom_converto_lib::util::ConflictPolicy::Error;
            match resolve_output_dir(&cmd.output_dir, policy)? {
                WriteDecision::Skip => {
                    log_skipped(&cmd.output_dir);
                    return Ok(());
                }
                WriteDecision::Write(_) => {}
            }
            let resolved = rom_converto_lib::util::resolve_input(&cmd.input, &["pkg"])?;
            let started = Instant::now();
            rom_converto_lib::sony::vita::pkg::extract(
                resolved.path(),
                &cmd.output_dir,
                &progress,
            )?;
            log_single_summary(
                &cmd.input,
                &cmd.output_dir,
                TallyDirection::CountOnly,
                started,
            );
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
