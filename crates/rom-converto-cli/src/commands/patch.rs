use crate::batch;
use crate::commands::support::DispatchCtx;
use crate::commands::{ConflictArgs, OutputArgs};
use crate::util::{ensure_input_exists, resolve_policy};
use anyhow::Result;
use clap::{Parser, Subcommand};
use rom_converto_lib::runner::models::RunOptions;
use rom_converto_lib::util::ConflictPolicy;
use std::path::PathBuf;

/// Commands for applying ROM patches
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum PatchCommands {
    Apply(ApplyCommand),
}

/// Apply one patch file to one ROM
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Apply one patch file to one ROM and write a patched copy\n\n\
Supports .aps, .bps, .ebp, .ips, .ips32, .ppf, .rup, .ups, .vcdiff and .xdelta patches, \
detected by extension and magic. BPS, UPS and RUP verify the source checksum before writing; APS and PPF 2.0 check the source size.\n\n\
Output defaults to the patch file's name with the ROM's extension, beside the ROM. \
When that name equals the ROM's file name (ignoring case), output defaults to <patch> (patched).<ext> instead. Archives are not opened.",
    after_long_help = "EXAMPLES:\n  rom-converto patch apply game.sfc hack.ips\n  rom-converto patch apply game.gba hack.ups --output-dir ./patched\n  rom-converto patch apply game.sfc hack.bps -f\n"
)]
pub struct ApplyCommand {
    /// Input ROM to patch
    #[arg(value_name = "ROM")]
    pub input: PathBuf,

    /// Patch file to apply
    #[arg(value_name = "PATCH")]
    pub patch: PathBuf,

    /// Output path, defaults to the patch file's name with the ROM's extension, beside the ROM
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Alias for the positional OUTPUT argument
    #[arg(
        short = 'o',
        long = "output",
        value_name = "PATH",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    #[command(flatten)]
    pub out: OutputArgs,

    #[command(flatten)]
    pub conflict: ConflictArgs,
}

/// Runs one `patch` subcommand.
pub async fn run(command: PatchCommands, ctx: DispatchCtx<'_>) -> Result<()> {
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
        PatchCommands::Apply(cmd) => {
            ensure_input_exists(&cmd.input)?;
            let mut options = RunOptions::from(batch::Common {
                recursive: false,
                output_dir: cmd.out.output_dir,
                output_template: cmd.out.output_template,
                max_depth: None,
                report: None,
                policy: resolve_policy(
                    cmd.conflict.on_conflict,
                    cmd.conflict.force,
                    ConflictPolicy::Error,
                ),
                skip_space_check,
            });
            options.patch_file = Some(cmd.patch);
            batch::run(
                &run,
                "patch.apply",
                cmd.input,
                cmd.output_flag.or(cmd.output),
                options,
            )
            .await?;
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
        cmd: PatchCommands,
    }

    #[test]
    fn parses_apply() {
        let h = Harness::parse_from(["bin", "apply", "rom.sfc", "hack.ips"]);
        let PatchCommands::Apply(c) = h.cmd;
        assert_eq!(c.input, PathBuf::from("rom.sfc"));
        assert_eq!(c.patch, PathBuf::from("hack.ips"));
        assert_eq!(c.output, None);
    }

    #[test]
    fn parses_apply_output() {
        let h = Harness::parse_from(["bin", "apply", "rom.sfc", "hack.ips", "out.sfc"]);
        let PatchCommands::Apply(c) = h.cmd;
        assert_eq!(c.output, Some(PathBuf::from("out.sfc")));
    }

    #[test]
    fn apply_output_flag_conflicts_with_positional() {
        let result =
            Harness::try_parse_from(["bin", "apply", "rom.sfc", "hack.ips", "out.sfc", "-o", "x"]);
        assert!(result.is_err());
    }

    #[test]
    fn apply_output_dir_conflicts_with_positional() {
        let result = Harness::try_parse_from([
            "bin",
            "apply",
            "rom.sfc",
            "hack.ips",
            "out.sfc",
            "--output-dir",
            "d",
        ]);
        assert!(result.is_err());
    }
}
