use crate::commands::ctr::CtrCommands;
use crate::commands::{Cli, Commands};
use crate::nintendo::ctr::{convert_cdn_to_cia, decrypt_cia, generate_ticket_from_cdn};
use anyhow::Result;
use clap::Parser;

mod commands;
mod nintendo;

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();
    env_logger::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Ctr(inner) => match inner {
            CtrCommands::CdnToCia(cmd) => convert_cdn_to_cia(cmd).await?,
            CtrCommands::GenerateCdnTicket(cmd) => {
                generate_ticket_from_cdn(&cmd.cdn_dir, &cmd.output).await?
            }
            CtrCommands::DecryptCia(cmd) => decrypt_cia(&cmd.input, &cmd.output).await?,
        },
    }

    Ok(())
}
