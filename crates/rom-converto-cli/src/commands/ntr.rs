use crate::commands::info_command::InfoCommand;
use crate::commands::{BatchArgs, ConflictArgs, OutputArgs};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::commands::support::{DispatchCtx, require_info_input, require_input, save_ntr_icon};
use crate::util::{ensure_input_exists, resolve_policy};
use crate::{batch, info_print};
use anyhow::Result;
use rom_converto_lib::runner::models::RunOptions;
use rom_converto_lib::util::ConflictPolicy;

/// Commands for Nintendo DS secure-area crypto
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum NtrCommands {
    Encrypt(EncryptNtrCommand),
    Decrypt(DecryptNtrCommand),
    Info(InfoCommand),
}

/// Encrypt an NDS ROM's secure area
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Encrypt an NDS ROM's secure area\n\nThe KEY1-covered 2 KiB block at 0x4000 is encrypted with the key derived from the header id code; the rest of the ROM is copied unchanged. No key file is ever needed.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto ntr encrypt game.nds\n  Whole folder:    rom-converto ntr encrypt -R ./roms --output-dir ./encrypted\n"
)]
pub struct EncryptNtrCommand {
    /// Input NDS ROM path, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output ROM path, defaults to the input with `.encrypted` inserted before the extension
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Alias for the positional OUTPUT argument
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    #[command(flatten)]
    pub out: OutputArgs,

    #[command(flatten)]
    pub conflict: ConflictArgs,

    /// Encrypt every .nds found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    #[command(flatten)]
    pub batch: BatchArgs,
}

/// Decrypt an NDS ROM's secure area
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Decrypt an NDS ROM's secure area\n\nThe KEY1-covered 2 KiB block at 0x4000 is decrypted with the key derived from the header id code; the rest of the ROM is copied unchanged. No key file is ever needed.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto ntr decrypt game.nds\n  Whole folder:    rom-converto ntr decrypt -R ./roms --output-dir ./decrypted\n"
)]
pub struct DecryptNtrCommand {
    /// Input NDS ROM path, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output ROM path, defaults to the input with `.decrypted` inserted before the extension
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Alias for the positional OUTPUT argument
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    #[command(flatten)]
    pub out: OutputArgs,

    #[command(flatten)]
    pub conflict: ConflictArgs,

    /// Decrypt every .nds found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    #[command(flatten)]
    pub batch: BatchArgs,
}

/// Runs one `ntr` subcommand.
pub async fn run(command: NtrCommands, ctx: DispatchCtx<'_>) -> Result<()> {
    let DispatchCtx {
        progress,
        total_progress,
        dry_run,
        skip_space_check,
        cancel,
        cache,
        config,
        preset,
        ..
    } = ctx;
    let run = batch::BatchRun {
        progress: &progress,
        total_progress: &total_progress,
        cache,
        cancel: &cancel,
        config,
        preset,
        dry_run,
    };
    match command {
        NtrCommands::Encrypt(cmd) => {
            require_input(&cmd.input, cmd.recursive)?;
            let options = RunOptions::from(batch::Common {
                recursive: cmd.recursive,
                output_dir: cmd.out.output_dir,
                output_template: cmd.out.output_template,
                max_depth: cmd.batch.max_depth,
                report: cmd.batch.report,
                policy: resolve_policy(
                    cmd.conflict.on_conflict,
                    cmd.conflict.force,
                    ConflictPolicy::Error,
                ),
                skip_space_check,
            });
            batch::run(
                &run,
                "ntr.encrypt",
                cmd.input,
                cmd.output_flag.or(cmd.output),
                options,
            )
            .await?;
        }
        NtrCommands::Decrypt(cmd) => {
            require_input(&cmd.input, cmd.recursive)?;
            let options = RunOptions::from(batch::Common {
                recursive: cmd.recursive,
                output_dir: cmd.out.output_dir,
                output_template: cmd.out.output_template,
                max_depth: cmd.batch.max_depth,
                report: cmd.batch.report,
                policy: resolve_policy(
                    cmd.conflict.on_conflict,
                    cmd.conflict.force,
                    ConflictPolicy::Error,
                ),
                skip_space_check,
            });
            batch::run(
                &run,
                "ntr.decrypt",
                cmd.input,
                cmd.output_flag.or(cmd.output),
                options,
            )
            .await?;
        }
        NtrCommands::Info(cmd) => {
            if cmd.keys.is_some() {
                anyhow::bail!("--keys is only supported by nx, wup, and ps3 info");
            }
            let input = require_info_input(&cmd.input)?;
            ensure_input_exists(input)?;
            let resolved = rom_converto_lib::util::resolve_input(input, &["nds", "dsi"])?;
            let info = rom_converto_lib::nintendo::ntr::info::read_info(resolved.path())?;
            if let Some(dir) = &cmd.save_icon {
                save_ntr_icon(&info, dir)?;
            }
            info_print::print(&rom_converto_lib::info::InfoResult::Ntr(info), cmd.json)?;
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
        cmd: NtrCommands,
    }

    #[test]
    fn parses_decrypt() {
        let h = Harness::parse_from(["bin", "decrypt", "game.nds"]);
        let NtrCommands::Decrypt(c) = h.cmd else {
            panic!("expected Decrypt");
        };
        assert_eq!(c.input, PathBuf::from("game.nds"));
        assert_eq!(c.output, None);
    }

    #[test]
    fn parses_encrypt_force() {
        let h = Harness::parse_from(["bin", "encrypt", "game.nds", "-f"]);
        let NtrCommands::Encrypt(c) = h.cmd else {
            panic!("expected Encrypt");
        };
        assert!(c.conflict.force);
        assert!(c.conflict.on_conflict.is_none());
    }

    #[test]
    fn parses_decrypt_recursive() {
        let h = Harness::parse_from(["bin", "decrypt", "roms", "-R"]);
        let NtrCommands::Decrypt(c) = h.cmd else {
            panic!("expected Decrypt");
        };
        assert!(c.recursive);
    }

    #[test]
    fn decrypt_output_flag_overrides_positional() {
        let h = Harness::parse_from(["bin", "decrypt", "game.nds", "-o", "out.nds"]);
        let NtrCommands::Decrypt(c) = h.cmd else {
            panic!("expected Decrypt");
        };
        assert_eq!(c.output, None);
        assert_eq!(c.output_flag, Some(PathBuf::from("out.nds")));
    }

    #[test]
    fn decrypt_output_flag_conflicts_with_positional() {
        let result =
            Harness::try_parse_from(["bin", "decrypt", "game.nds", "pos.nds", "-o", "flag.nds"]);
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_output_dir_conflicts_with_positional() {
        let result = Harness::try_parse_from([
            "bin",
            "decrypt",
            "game.nds",
            "pos.nds",
            "--output-dir",
            "out",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn max_depth_requires_recursive() {
        let result = Harness::try_parse_from(["bin", "decrypt", "roms", "--max-depth", "2"]);
        assert!(result.is_err());
    }
}
