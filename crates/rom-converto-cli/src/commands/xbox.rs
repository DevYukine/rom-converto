use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::commands::support::{ALL_IMAGE_EXTS, DispatchCtx, require_info_input, save_xbox_icon};
use crate::util::{ensure_input_exists, resolve_policy};
use crate::{batch, info_print};
use anyhow::Result;
use rom_converto_lib::runner::models::RunOptions;
use rom_converto_lib::util::ConflictPolicy;

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

/// Runs one `xbox` subcommand.
pub async fn run(command: XboxCommands, ctx: DispatchCtx<'_>) -> Result<()> {
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
        XboxCommands::Convert(cmd) => {
            ensure_input_exists(&cmd.input)?;
            let mut options = RunOptions::from(batch::Common {
                recursive: false,
                output_dir: None,
                output_template: None,
                max_depth: None,
                report: None,
                policy: resolve_policy(None, cmd.force, ConflictPolicy::Error),
                skip_space_check,
            });
            options.media_patch = Some(!cmd.no_media_patch);
            batch::run(
                &run,
                "xbox.convert",
                cmd.input,
                cmd.output_flag.or(cmd.output),
                options,
            )
            .await?;
        }
        XboxCommands::Extract(cmd) => {
            ensure_input_exists(&cmd.input)?;
            let options = RunOptions::from(batch::Common {
                recursive: false,
                output_dir: None,
                output_template: None,
                max_depth: None,
                report: None,
                policy: ConflictPolicy::Error,
                skip_space_check,
            });
            batch::run(
                &run,
                "xbox.extract",
                cmd.input,
                Some(cmd.output_dir),
                options,
            )
            .await?;
        }
        XboxCommands::Info(cmd) => {
            if cmd.keys.is_some() {
                anyhow::bail!("--keys is only supported by nx and wup info");
            }
            let input = require_info_input(&cmd.input)?;
            ensure_input_exists(input)?;
            let resolved = rom_converto_lib::util::resolve_input(input, ALL_IMAGE_EXTS)?;
            let info = rom_converto_lib::microsoft::xbox::read_info(resolved.path())?;
            if let Some(dir) = &cmd.save_icon {
                save_xbox_icon(&info, dir)?;
            }
            info_print::print(&rom_converto_lib::info::InfoResult::Xbox(info), cmd.json)?;
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
