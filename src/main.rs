use crate::commands::ctr::CtrCommands;
use crate::commands::{Cli, Commands, SelfUpdateCommand};
use crate::github::api::GithubApi;
use crate::nintendo::ctr::{convert_cdn_to_cia, decrypt_cia, generate_ticket_from_cdn};
use crate::updater::{check_for_new_version_and_notify, cleanup_old_executable, self_update};
use anyhow::Result;
use clap::Parser;
use std::mem::discriminant;

mod commands;
mod github;
mod nintendo;
mod updater;
mod util;

pub mod built_info {
    // The file has been placed there by the build script.
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();
    env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .parse_default_env()
        .init();

    cleanup_old_executable().await?;

    let cli = Cli::parse();

    let mut github = GithubApi::new()?;

    if discriminant(&cli.command) != discriminant(&Commands::SelfUpdate(SelfUpdateCommand {})) {
        check_for_new_version_and_notify(&mut github).await?;
    }

    match cli.command {
        Commands::Ctr(inner) => match inner {
            CtrCommands::CdnToCia(cmd) => convert_cdn_to_cia(cmd).await?,
            CtrCommands::GenerateCdnTicket(cmd) => {
                generate_ticket_from_cdn(&cmd.cdn_dir, &cmd.output).await?
            }
            CtrCommands::DecryptCia(cmd) => decrypt_cia(&cmd.input, &cmd.output).await?,
        },
        Commands::SelfUpdate(_) => self_update(&mut github).await?,
    }

    Ok(())
}
