use crate::commands::ctr::CtrCommands;
use clap::{Parser, Subcommand};

pub mod ctr;

/// CLI for en/decrypting, compressing and converting ROMs.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    #[command(subcommand)]
    Ctr(CtrCommands),
}
