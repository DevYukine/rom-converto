use crate::commands::info_command::InfoCommand;
use crate::commands::{BatchArgs, ConflictArgs, OutputArgs};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::commands::support::{
    DispatchCtx, finish_single, require_dir, require_info_input, save_nds_icon, skipped_single,
};
use crate::util::{
    SingleOutput, ensure_input_exists, file_len, resolve_policy, resolve_single_output,
};
use crate::{batch, info_print};
use anyhow::Result;
use rom_converto_lib::nintendo::nds::{
    NdsError, decrypt_nds_rom, derive_decrypted_path as nds_derive_decrypted_path,
    derive_encrypted_path as nds_derive_encrypted_path, encrypt_nds_rom,
};
use rom_converto_lib::util::TallyDirection;
use std::path::Path;
use std::time::Instant;

/// Commands for Nintendo DS secure-area crypto
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum NdsCommands {
    Encrypt(EncryptNdsCommand),
    Decrypt(DecryptNdsCommand),
    Info(InfoCommand),
}

/// Encrypt an NDS ROM's secure area
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Encrypt an NDS ROM's secure area\n\nThe KEY1-covered 2 KiB block at 0x4000 is encrypted with the key derived from the header id code; the rest of the ROM is copied unchanged. No key file is ever needed.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto nds encrypt game.nds\n  Whole folder:    rom-converto nds encrypt -R ./roms --output-dir ./encrypted\n"
)]
pub struct EncryptNdsCommand {
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
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto nds decrypt game.nds\n  Whole folder:    rom-converto nds decrypt -R ./roms --output-dir ./decrypted\n"
)]
pub struct DecryptNdsCommand {
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

/// Runs one `nds` subcommand.
pub async fn run(command: NdsCommands, ctx: DispatchCtx<'_>) -> Result<()> {
    let DispatchCtx {
        progress,
        total_progress,
        dry_run,
        skip_space_check,
        cancel,
        ..
    } = ctx;
    match command {
        NdsCommands::Encrypt(cmd) => {
            let output_dir = cmd.out.output_dir.clone();
            let report = cmd.batch.report.clone();
            if cmd.recursive {
                require_dir(&cmd.input)?;
                let policy = resolve_policy(
                    cmd.conflict.on_conflict,
                    cmd.conflict.force,
                    rom_converto_lib::util::ConflictPolicy::Error,
                );
                let run = batch::BatchRun {
                    progress: &progress,
                    total_progress: &total_progress,
                    input_dir: &cmd.input,
                    policy,
                    output_dir: output_dir.as_deref(),
                    output_template: cmd.out.output_template.as_deref(),
                    max_depth: cmd.batch.max_depth,
                    dry_run,
                    skip_space_check,
                    report_path: report.as_deref(),
                    cancel: &cancel,
                };
                batch::nds_crypt(&run, true).await?
            } else {
                ensure_input_exists(&cmd.input)?;
                let resolved = rom_converto_lib::util::resolve_input(&cmd.input, &["nds"])?;
                let input = resolved.path();
                let policy = resolve_policy(
                    cmd.conflict.on_conflict,
                    cmd.conflict.force,
                    rom_converto_lib::util::ConflictPolicy::Error,
                );
                let Some(output) = resolve_single_output(
                    SingleOutput {
                        operation: "encrypt",
                        cli_input: &cmd.input,
                        input,
                        explicit: cmd.output_flag.or(cmd.output),
                        derived: nds_derive_encrypted_path(resolved.output_basis()),
                        output_dir: output_dir.as_deref(),
                        output_template: cmd.out.output_template.as_deref(),
                        output_ext: "nds",
                        keys_path: None,
                        policy,
                        verify: crate::util::OutputVerify::None,
                        media: None,
                        missing_keys: None,
                        report: report.as_deref(),
                        dry_run,
                        cancel: cancel.clone(),
                    },
                    &progress,
                )
                .await?
                else {
                    return Ok(());
                };
                if !skip_space_check {
                    let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                    batch::space_preflight_for_size(file_len(input), check_dir)?;
                }
                let in_path = input.to_path_buf();
                let out_path = output.clone();
                let started = Instant::now();
                match encrypt_nds_rom(&progress, in_path, output, true, cancel.clone()).await {
                    Ok(()) => {}
                    Err(
                        e @ (NdsError::AlreadyEncrypted
                        | NdsError::AlreadyDecrypted
                        | NdsError::NoSecureArea
                        | NdsError::TooSmall),
                    ) => {
                        log::info!("Skipped, {e}: {}", cmd.input.display());
                        skipped_single(&cmd.input, "encrypt", e, report.as_deref())?;
                        return Ok(());
                    }
                    Err(e) => return Err(e.into()),
                }
                finish_single(
                    &cmd.input,
                    &out_path,
                    TallyDirection::Convert,
                    "encrypt",
                    started,
                    report.as_deref(),
                )?;
            }
        }
        NdsCommands::Decrypt(cmd) => {
            let output_dir = cmd.out.output_dir.clone();
            let report = cmd.batch.report.clone();
            if cmd.recursive {
                require_dir(&cmd.input)?;
                let policy = resolve_policy(
                    cmd.conflict.on_conflict,
                    cmd.conflict.force,
                    rom_converto_lib::util::ConflictPolicy::Error,
                );
                let run = batch::BatchRun {
                    progress: &progress,
                    total_progress: &total_progress,
                    input_dir: &cmd.input,
                    policy,
                    output_dir: output_dir.as_deref(),
                    output_template: cmd.out.output_template.as_deref(),
                    max_depth: cmd.batch.max_depth,
                    dry_run,
                    skip_space_check,
                    report_path: report.as_deref(),
                    cancel: &cancel,
                };
                batch::nds_crypt(&run, false).await?
            } else {
                ensure_input_exists(&cmd.input)?;
                let resolved = rom_converto_lib::util::resolve_input(&cmd.input, &["nds"])?;
                let input = resolved.path();
                let policy = resolve_policy(
                    cmd.conflict.on_conflict,
                    cmd.conflict.force,
                    rom_converto_lib::util::ConflictPolicy::Error,
                );
                let Some(output) = resolve_single_output(
                    SingleOutput {
                        operation: "decrypt",
                        cli_input: &cmd.input,
                        input,
                        explicit: cmd.output_flag.or(cmd.output),
                        derived: nds_derive_decrypted_path(resolved.output_basis()),
                        output_dir: output_dir.as_deref(),
                        output_template: cmd.out.output_template.as_deref(),
                        output_ext: "nds",
                        keys_path: None,
                        policy,
                        verify: crate::util::OutputVerify::None,
                        media: None,
                        missing_keys: None,
                        report: report.as_deref(),
                        dry_run,
                        cancel: cancel.clone(),
                    },
                    &progress,
                )
                .await?
                else {
                    return Ok(());
                };
                if !skip_space_check {
                    let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                    batch::space_preflight_for_size(file_len(input), check_dir)?;
                }
                let in_path = input.to_path_buf();
                let out_path = output.clone();
                let started = Instant::now();
                match decrypt_nds_rom(&progress, in_path, output, true, cancel.clone()).await {
                    Ok(()) => {}
                    Err(
                        e @ (NdsError::AlreadyEncrypted
                        | NdsError::AlreadyDecrypted
                        | NdsError::NoSecureArea
                        | NdsError::TooSmall),
                    ) => {
                        log::info!("Skipped, {e}: {}", cmd.input.display());
                        skipped_single(&cmd.input, "decrypt", e, report.as_deref())?;
                        return Ok(());
                    }
                    Err(e) => return Err(e.into()),
                }
                finish_single(
                    &cmd.input,
                    &out_path,
                    TallyDirection::Convert,
                    "decrypt",
                    started,
                    report.as_deref(),
                )?;
            }
        }
        NdsCommands::Info(cmd) => {
            if cmd.keys.is_some() {
                anyhow::bail!("--keys is only supported by nx, wup, and ps3 info");
            }
            let input = require_info_input(&cmd.input)?;
            ensure_input_exists(input)?;
            let resolved = rom_converto_lib::util::resolve_input(input, &["nds", "dsi"])?;
            let info = rom_converto_lib::nintendo::nds::info::read_info(resolved.path())?;
            if let Some(dir) = &cmd.save_icon {
                save_nds_icon(&info, dir)?;
            }
            info_print::print(&rom_converto_lib::info::InfoResult::Nds(info), cmd.json)?;
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
        cmd: NdsCommands,
    }

    #[test]
    fn parses_decrypt() {
        let h = Harness::parse_from(["bin", "decrypt", "game.nds"]);
        let NdsCommands::Decrypt(c) = h.cmd else {
            panic!("expected Decrypt");
        };
        assert_eq!(c.input, PathBuf::from("game.nds"));
        assert_eq!(c.output, None);
    }

    #[test]
    fn parses_encrypt_force() {
        let h = Harness::parse_from(["bin", "encrypt", "game.nds", "-f"]);
        let NdsCommands::Encrypt(c) = h.cmd else {
            panic!("expected Encrypt");
        };
        assert!(c.conflict.force);
        assert!(c.conflict.on_conflict.is_none());
    }

    #[test]
    fn parses_decrypt_recursive() {
        let h = Harness::parse_from(["bin", "decrypt", "roms", "-R"]);
        let NdsCommands::Decrypt(c) = h.cmd else {
            panic!("expected Decrypt");
        };
        assert!(c.recursive);
    }

    #[test]
    fn decrypt_output_flag_overrides_positional() {
        let h = Harness::parse_from(["bin", "decrypt", "game.nds", "-o", "out.nds"]);
        let NdsCommands::Decrypt(c) = h.cmd else {
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
