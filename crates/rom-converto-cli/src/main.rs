use crate::commands::chd::ChdCommands;
use crate::commands::ctr::CtrCommands;
use crate::commands::dol::DolCommands;
use crate::commands::rvl::RvlCommands;
use crate::commands::wup::WupCommands;
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
use rom_converto_lib::nintendo::rvz::{
    RvzCompressOptions, compress_disc, decompress_disc, derive_disc_path, derive_rvz_path,
};
use rom_converto_lib::nintendo::wup::{
    TitleInput, WupCompressOptions, compress_titles_async, decrypt_nus_title_async,
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
        // Non-fatal: network outages or GitHub rate limits shouldn't
        // prevent the user from running conversions offline.
        if let Err(e) = check_for_new_version_and_notify(&mut github).await {
            log::debug!("update check skipped: {e}");
        }
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
                    .clone()
                    .unwrap_or_else(|| derive_compressed_path(&cmd.input));
                compress_rom(&cmd.input, &output, cmd.level, &progress).await?
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
        Commands::Dol(inner) => match inner {
            DolCommands::Compress(cmd) => {
                let output = cmd.output.unwrap_or_else(|| derive_rvz_path(&cmd.input));
                let opts = RvzCompressOptions {
                    compression_level: cmd
                        .level
                        .unwrap_or(RvzCompressOptions::default().compression_level),
                    chunk_size: cmd
                        .chunk_size
                        .unwrap_or(RvzCompressOptions::default().chunk_size),
                    ..RvzCompressOptions::default()
                };
                compress_disc(&cmd.input, &output, opts, &progress).await?
            }
            DolCommands::Decompress(cmd) => {
                let output = cmd.output.unwrap_or_else(|| derive_disc_path(&cmd.input));
                decompress_disc(&cmd.input, &output, &progress).await?
            }
        },
        Commands::Rvl(inner) => match inner {
            RvlCommands::Compress(cmd) => {
                let output = cmd.output.unwrap_or_else(|| derive_rvz_path(&cmd.input));
                let opts = RvzCompressOptions {
                    compression_level: cmd
                        .level
                        .unwrap_or(RvzCompressOptions::default().compression_level),
                    chunk_size: cmd
                        .chunk_size
                        .unwrap_or(RvzCompressOptions::default().chunk_size),
                    ..RvzCompressOptions::default()
                };
                compress_disc(&cmd.input, &output, opts, &progress).await?
            }
            RvlCommands::Decompress(cmd) => {
                let output = cmd.output.unwrap_or_else(|| derive_disc_path(&cmd.input));
                decompress_disc(&cmd.input, &output, &progress).await?
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
        Commands::Wup(inner) => match inner {
            WupCommands::Compress(cmd) => {
                let opts = WupCompressOptions {
                    zstd_level: cmd
                        .level
                        .unwrap_or(WupCompressOptions::default().zstd_level),
                };
                // Pair --key values with disc inputs in positional
                // order. Non-disc inputs skip past their key slot.
                let mut key_iter = cmd.keys.into_iter();
                let titles: Vec<TitleInput> = cmd
                    .inputs
                    .into_iter()
                    .map(|p| {
                        let is_disc = p
                            .extension()
                            .and_then(|s| s.to_str())
                            .map(|s| s.eq_ignore_ascii_case("wud") || s.eq_ignore_ascii_case("wux"))
                            .unwrap_or(false)
                            && p.is_file();
                        let mut t = TitleInput::auto(p);
                        if is_disc {
                            t.key_path = key_iter.next();
                        }
                        t
                    })
                    .collect();
                compress_titles_async(titles, cmd.output, opts, &progress).await?
            }
            WupCommands::Decrypt(cmd) => {
                decrypt_nus_title_async(cmd.input, cmd.output, &progress).await?
            }
        },
        Commands::SelfUpdate(_) => self_update(&mut github).await?,
    }

    Ok(())
}
