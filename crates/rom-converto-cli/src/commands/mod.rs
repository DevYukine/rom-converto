use crate::commands::chd::ChdCommands;
use crate::commands::completions::ShellCompletionsCommand;
use crate::commands::cso::CsoCommands;
use crate::commands::ctr::CtrCommands;
use crate::commands::cue::CueCommands;
use crate::commands::dol::DolCommands;
use crate::commands::hash::HashCommand;
use crate::commands::nx::NxCommands;
use crate::commands::playlist::PlaylistCommand;
use crate::commands::rvl::RvlCommands;
use crate::commands::wup::WupCommands;
use clap::{Parser, Subcommand};
use rom_converto_lib::util::ConflictPolicy;
use std::path::PathBuf;

pub mod chd;
pub mod completions;
pub mod cso;
pub mod ctr;
pub mod cue;
pub mod dol;
pub mod hash;
pub mod info_command;
pub mod nx;
pub mod playlist;
pub mod rvl;
pub mod wup;

/// CLI for en/decrypting, compressing and converting ROMs.
#[derive(Parser, Debug)]
#[command(
	name = env!("CARGO_BIN_NAME"),
	author,                   // pulls env!("CARGO_PKG_AUTHORS")
	version = env!("ROM_CONVERTO_DISPLAY_VERSION"),
	about,                    // doc-comment or Cargo.toml description
	long_about = "Convert, compress, decrypt and verify ROMs and disc images across Nintendo and Sony consoles.\n\nEach top-level command is a console/format family (ctr, dol, rvl, wup, nx, chd, cso, cue); each has operations like compress, decompress, verify and info. Output is auto-derived from the input unless you pass an explicit OUTPUT, -o/--output, or --output-dir. Pass -R/--recursive to process every matching file in a directory.",
	help_template = "\
{before-help}{name} {version}\n\
{about-with-newline}\n\
{usage-heading}\n    {usage}\n\n\
{all-args}\n\n\
Made with ❤ by {author}
"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    #[arg(
        short = 'q',
        long,
        global = true,
        help = "Suppress progress and info output; only warnings and errors"
    )]
    pub quiet: bool,

    #[arg(short = 'v', long, global = true, action = clap::ArgAction::Count, help = "Increase verbosity (-v debug, -vv trace, -vvv trace + dependencies)")]
    pub verbose: u8,

    #[arg(
        long = "no-update-check",
        global = true,
        help = "Skip the check for a newer release"
    )]
    pub no_update_check: bool,

    #[arg(
        long = "config",
        global = true,
        value_name = "FILE",
        help = "Path to a config file; overrides the search order"
    )]
    pub config: Option<PathBuf>,

    #[arg(
        long = "preset",
        global = true,
        value_name = "NAME",
        help = "Apply a named preset from the config file"
    )]
    pub preset: Option<String>,

    #[arg(
        long = "dry-run",
        global = true,
        help = "Preview what would happen without writing any output"
    )]
    pub dry_run: bool,

    #[arg(
        long = "debug-log",
        global = true,
        value_name = "FILE",
        help = "Write a full-detail trace log to FILE regardless of console verbosity"
    )]
    pub debug_log: Option<PathBuf>,

    #[arg(
        long = "skip-space-check",
        global = true,
        help = "Skip the free-space preflight before writing output"
    )]
    pub skip_space_check: bool,
}

#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum Commands {
    #[command(subcommand)]
    Ctr(CtrCommands),

    #[command(subcommand)]
    Dol(DolCommands),

    #[command(subcommand)]
    Rvl(RvlCommands),

    #[command(subcommand)]
    Wup(WupCommands),

    #[command(subcommand)]
    Nx(NxCommands),

    #[command(subcommand)]
    Chd(ChdCommands),

    #[command(subcommand)]
    Cso(CsoCommands),

    #[command(subcommand)]
    Cue(CueCommands),

    Hash(HashCommand),

    Playlist(PlaylistCommand),

    SelfUpdate(SelfUpdateCommand),

    ShellCompletions(ShellCompletionsCommand),
}

/// Check for and install a newer version of the CLI.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(long_about = "Check for a newer version of the CLI and install it if one is available.")]
pub struct SelfUpdateCommand {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ConflictPolicyArg {
    Error,
    Overwrite,
    Skip,
    Rename,
    OverwriteInvalid,
}

impl From<ConflictPolicyArg> for ConflictPolicy {
    fn from(arg: ConflictPolicyArg) -> Self {
        match arg {
            ConflictPolicyArg::Error => ConflictPolicy::Error,
            ConflictPolicyArg::Overwrite => ConflictPolicy::Overwrite,
            ConflictPolicyArg::Skip => ConflictPolicy::Skip,
            ConflictPolicyArg::Rename => ConflictPolicy::Rename,
            ConflictPolicyArg::OverwriteInvalid => ConflictPolicy::OverwriteInvalid,
        }
    }
}
