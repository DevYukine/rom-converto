use crate::commands::ctr::CtrCommands;
use clap::{Parser, Subcommand};

pub mod ctr;

/// CLI for en/decrypting, compressing and converting ROMs.
#[derive(Parser, Debug)]
#[command(
	author,                   // pulls env!("CARGO_PKG_AUTHORS")
	version,                  // pulls env!("CARGO_PKG_VERSION")
	about,                    // doc-comment or Cargo.toml description
	help_template = "\
{before-help}{name} {version}\n\
{about-with-newline}\n\
{usage-heading}\n    {usage}\n\n\
{all-args}\n\n\
Made with ❤ by {author}
"
)]
#[command(propagate_version = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum Commands {
    #[command(subcommand)]
    Ctr(CtrCommands),

    SelfUpdate(SelfUpdateCommand),
}

/// Command to check for a new version of the CLI and updates it if available
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct SelfUpdateCommand {}
