use crate::commands::chd::ChdCommands;
use crate::commands::ctr::CtrCommands;
use crate::commands::{Cli, Commands, SelfUpdateCommand};
use crate::github::api::GithubApi;
use crate::updater::{check_for_new_version_and_notify, cleanup_old_executable, self_update};
use crate::util::IndicatifProgress;
use anyhow::Result;
use clap::Parser;
use indicatif::MultiProgress;
use indicatif_log_bridge::LogWrapper;
use rom_converto_lib::chd::{convert_to_chd, extract_from_chd, verify_chd};
use rom_converto_lib::nintendo::ctr::verify::{CtrVerifyOptions, CtrVerifyResult, verify_ctr};
use rom_converto_lib::nintendo::ctr::z3ds::{
    compress_rom, decompress_rom, derive_compressed_path, derive_decompressed_path,
};
use rom_converto_lib::nintendo::ctr::{
    CdnToCiaOptions, convert_cdn_to_cia, decrypt_rom, generate_ticket_from_cdn,
};
use std::mem::discriminant;

mod commands;
mod github;
mod updater;
mod util;

pub mod built_info {
    // The file has been placed there by the build script.
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();
    let logger = env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .parse_default_env()
        .build();

    let level = logger.filter();
    let pb = MultiProgress::new();

    LogWrapper::new(pb.clone(), logger).try_init()?;
    log::set_max_level(level);

    cleanup_old_executable().await?;

    let cli = Cli::parse();

    let mut github = GithubApi::new()?;

    if discriminant(&cli.command) != discriminant(&Commands::SelfUpdate(SelfUpdateCommand {})) {
        check_for_new_version_and_notify(&mut github).await?;
    }

    let progress = IndicatifProgress::new(pb);

    match cli.command {
        Commands::Ctr(inner) => match inner {
            CtrCommands::CdnToCia(cmd) => {
                let opts = CdnToCiaOptions {
                    cdn_dir: cmd.cdn_dir,
                    output: cmd.output,
                    cleanup: cmd.cleanup,
                    recursive: cmd.recursive,
                    ensure_ticket_exists: cmd.ensure_ticket_exists,
                    decrypt: cmd.decrypt,
                    compress: cmd.compress,
                };
                convert_cdn_to_cia(opts, &progress, &progress).await?
            }
            CtrCommands::GenerateCdnTicket(cmd) => {
                generate_ticket_from_cdn(&cmd.cdn_dir, &cmd.output).await?
            }
            CtrCommands::Decrypt(cmd) => decrypt_rom(&cmd.input, &cmd.output, &progress).await?,
            CtrCommands::Compress(cmd) => {
                let output = cmd
                    .output
                    .unwrap_or_else(|| derive_compressed_path(&cmd.input));
                compress_rom(&cmd.input, &output, &progress).await?
            }
            CtrCommands::Decompress(cmd) => {
                let output = cmd
                    .output
                    .unwrap_or_else(|| derive_decompressed_path(&cmd.input));
                decompress_rom(&cmd.input, &output, &progress).await?
            }
            CtrCommands::Verify(cmd) => {
                let opts = CtrVerifyOptions {
                    verify_content_hashes: cmd.verify_content,
                };
                let result = verify_ctr(&cmd.input, &opts, &progress).await?;
                match &result {
                    CtrVerifyResult::Cia(cia) => {
                        log::info!("Format: CIA");
                        log::info!("Legitimacy: {}", cia.legitimacy);
                        for line in &cia.details {
                            log::info!("  {line}");
                        }
                    }
                    CtrVerifyResult::Ncsd(ncsd) => {
                        log::info!("Format: NCSD");
                        log::info!("Title ID: {}", ncsd.title_id);
                        for line in &ncsd.details {
                            log::info!("  {line}");
                        }
                        for part in &ncsd.partitions {
                            log::info!(
                                "  Partition {} ({}): {}",
                                part.index,
                                part.name,
                                if part.ncch_magic_valid {
                                    "NCCH OK"
                                } else {
                                    "NCCH INVALID"
                                }
                            );
                            for line in &part.details {
                                log::info!("    {line}");
                            }
                        }
                    }
                }
            }
        },
        Commands::Chd(inner) => match inner {
            ChdCommands::Compress(cmd) => {
                convert_to_chd(&progress, cmd.input_cue, cmd.output, cmd.force).await?
            }
            ChdCommands::Extract(cmd) => {
                extract_from_chd(&progress, cmd.input, cmd.output, cmd.parent).await?
            }
            ChdCommands::Verify(cmd) => {
                verify_chd(&progress, cmd.input, cmd.parent, cmd.fix).await?
            }
        },
        Commands::SelfUpdate(_) => self_update(&mut github).await?,
    }

    Ok(())
}
