use crate::commands::ConflictPolicyArg;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

use crate::batch;
use crate::commands::support::DispatchCtx;
use crate::util::{ensure_input_exists, resolve_policy};
use anyhow::Result;
use rom_converto_lib::runner::models::RunOptions;
use rom_converto_lib::util::ConflictPolicy;

/// Commands for PlayStation (PS1) disc images
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum PsxCommands {
    ToPbp(ToPbpCommand),
}

/// How each block of disc data is written into the EBOOT
#[derive(ValueEnum, Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockEncodingArg {
    /// Compressed, smaller
    Deflate,
    /// Uncompressed, the same size as the disc image
    Stored,
}

impl BlockEncodingArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::Deflate => "deflate",
            Self::Stored => "stored",
        }
    }
}

/// Convert a PS1 disc image to a PSP EBOOT.PBP
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Convert a PS1 disc image to a PSP EBOOT.PBP\n\n\
Builds the EBOOT.PBP the PSP's built-in PS1 emulator runs on custom firmware, from a .cue \
sheet with raw 2352-byte sectors, a CD .chd, or an .m3u playlist of either. Writes one game \
folder per EBOOT into OUTPUT_DIR (default <INPUT>_psp next to the input); copy those folders \
into PSP/GAME on the Memory Stick. The disc ID comes from the disc's SYSTEM.CNF.\n\n\
A playlist becomes one EBOOT with disc switching in the HOME menu when this build carries the \
multi-disc table and the discs fit in 4 GiB. Otherwise each disc becomes its own EBOOT with \
disc 1's ID, so all discs share one memory card: save before a disc change, quit, and start \
the next disc's EBOOT.",
    after_long_help = "EXAMPLES:\n  rom-converto psx to-pbp game.cue\n  \
rom-converto psx to-pbp game.chd /Volumes/PSP/PSP/GAME --on-conflict overwrite\n  \
rom-converto psx to-pbp game.m3u --title \"My Game\"\n"
)]
pub struct ToPbpCommand {
    /// Input disc image (.cue, .chd or .m3u)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,
    /// Folder to write game folders into, defaults to <INPUT>_psp
    #[arg(value_name = "OUTPUT_DIR")]
    pub output_dir: Option<PathBuf>,

    /// How to write each block of disc data
    #[arg(long, value_enum, default_value_t = BlockEncodingArg::Deflate)]
    pub compression: BlockEncodingArg,

    /// Title shown on the XMB, defaults to the input file name
    #[arg(long, value_name = "TITLE")]
    pub title: Option<String>,

    /// Disc ID such as SLUS-00001, for a disc whose SYSTEM.CNF names none
    #[arg(long = "disc-id", value_name = "ID")]
    pub disc_id: Option<String>,

    /// PNG or JPEG shown while the game starts, in place of the built-in one; fitted to 480x272
    #[arg(long, value_name = "IMAGE")]
    pub splash: Option<PathBuf>,

    /// PNG or JPEG the XMB shows as the game's icon, such as its cover; fitted to 144x80
    #[arg(long, value_name = "IMAGE")]
    pub icon0: Option<PathBuf>,

    /// PNG or JPEG the XMB shows behind the game; fitted to 480x272
    #[arg(long, value_name = "IMAGE")]
    pub pic1: Option<PathBuf>,

    /// What to do when the output folder is not empty: error, overwrite, or skip
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

/// Runs one `psx` subcommand.
pub async fn run(command: PsxCommands, ctx: DispatchCtx<'_>) -> Result<()> {
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
        PsxCommands::ToPbp(cmd) => {
            ensure_input_exists(&cmd.input)?;
            let mut options = RunOptions::from(batch::Common {
                recursive: false,
                output_dir: None,
                output_template: None,
                max_depth: None,
                report: cmd.report,
                policy: resolve_policy(Some(cmd.on_conflict), cmd.force, ConflictPolicy::Error),
                skip_space_check,
            });
            options.compression = Some(cmd.compression.as_str().to_string());
            options.title = cmd.title;
            options.disc_id = cmd.disc_id;
            options.splash = cmd.splash;
            options.icon0 = cmd.icon0;
            options.pic1 = cmd.pic1;
            batch::run(&run, "psx.to_pbp", cmd.input, cmd.output_dir, options).await?;
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
        cmd: PsxCommands,
    }

    fn to_pbp(args: &[&str]) -> ToPbpCommand {
        let h = Harness::parse_from(["bin", "to-pbp"].iter().chain(args));
        let PsxCommands::ToPbp(c) = h.cmd;
        c
    }

    #[test]
    fn to_pbp_defaults() {
        let c = to_pbp(&["game.cue"]);
        assert_eq!(c.input, PathBuf::from("game.cue"));
        assert_eq!(c.output_dir, None);
        assert_eq!(c.compression, BlockEncodingArg::Deflate);
        assert_eq!(c.on_conflict, ConflictPolicyArg::Error);
        assert_eq!((c.title, c.disc_id, c.splash), (None, None, None));
    }

    #[test]
    fn to_pbp_parses_every_option() {
        let c = to_pbp(&[
            "game.m3u",
            "out",
            "--compression",
            "stored",
            "--title",
            "My Game",
            "--disc-id",
            "SLUS-00001",
            "--splash",
            "boot.png",
            "--icon0",
            "icon.png",
            "--pic1",
            "back.png",
            "--report",
            "run.json",
        ]);
        assert_eq!(c.output_dir, Some(PathBuf::from("out")));
        assert_eq!(c.compression, BlockEncodingArg::Stored);
        assert_eq!(c.title.as_deref(), Some("My Game"));
        assert_eq!(c.disc_id.as_deref(), Some("SLUS-00001"));
        assert_eq!(c.splash, Some(PathBuf::from("boot.png")));
        assert_eq!(c.icon0, Some(PathBuf::from("icon.png")));
        assert_eq!(c.pic1, Some(PathBuf::from("back.png")));
        assert_eq!(c.report, Some(PathBuf::from("run.json")));
    }

    #[test]
    fn to_pbp_rejects_an_unknown_encoding() {
        assert!(
            Harness::try_parse_from(["bin", "to-pbp", "game.cue", "--compression", "zstd"])
                .is_err()
        );
    }

    #[test]
    fn to_pbp_force_and_on_conflict_conflict() {
        assert!(
            Harness::try_parse_from(["bin", "to-pbp", "game.cue", "-f", "--on-conflict", "skip"])
                .is_err()
        );
    }
}
