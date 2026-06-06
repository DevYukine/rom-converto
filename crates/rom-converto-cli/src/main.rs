use crate::commands::chd::ChdCommands;
use crate::commands::completions::ShellCompletionsCommand;
use crate::commands::ctr::CtrCommands;
use crate::commands::dol::DolCommands;
use crate::commands::nx::NxCommands;
use crate::commands::rvl::RvlCommands;
use crate::commands::wup::WupCommands;
use crate::commands::{Cli, Commands, SelfUpdateCommand};
use crate::github::api::GithubApi;
use crate::updater::{check_for_new_version_and_notify, cleanup_old_executable, self_update};
use crate::util::IndicatifProgress;
use anyhow::Result;
use clap::{CommandFactory, Parser};
use clap_complete::{generate, generate_to};
use indicatif::MultiProgress;
use indicatif_log_bridge::LogWrapper;
use rom_converto_lib::chd::{convert_to_chd, extract_from_chd, verify_chd};
use rom_converto_lib::nintendo::ctr::convert::{
    convert_rom, convert_rom_batch, derive_converted_path,
};
use rom_converto_lib::nintendo::ctr::verify::{
    CtrVerifyOptions, CtrVerifyResult, verify_ctr, verify_ctr_batch,
};
use rom_converto_lib::nintendo::ctr::z3ds::{
    compress_rom, compress_rom_batch, decompress_rom, decompress_rom_batch, derive_compressed_path,
    derive_decompressed_path,
};
use rom_converto_lib::nintendo::ctr::{
    CdnToCiaOptions, convert_cdn_to_cia, decrypt_rom, decrypt_rom_batch, derive_decrypted_path,
    generate_ticket_from_cdn,
};
use rom_converto_lib::nintendo::nx::{
    NczMode, NxCompressOptions, compress_container_async, decompress_container_async,
    derive_compressed_path as nx_derive_compressed_path,
    derive_decompressed_path as nx_derive_decompressed_path, detect_container, load_keyset,
    verify_container_async,
};
use rom_converto_lib::nintendo::rvz::{
    RvzCompressOptions, compress_disc, decompress_disc, decompress_disc_to_wbfs, derive_disc_path,
    derive_rvz_path,
};
use rom_converto_lib::nintendo::wup::{
    TitleInput, WupCompressOptions, compress_titles_async, decrypt_nus_title_async,
};
use std::mem::discriminant;

mod commands;
mod github;
mod info_print;
mod updater;
mod util;

pub mod built_info {
    // The file has been placed there by the build script.
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

/// Decompress targets a WBFS container when the resolved output path
/// carries a `.wbfs` extension; otherwise it writes a raw disc image.
fn wants_wbfs_output(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|s| s.eq_ignore_ascii_case("wbfs"))
        .unwrap_or(false)
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();

    let cli = Cli::parse();

    // Must run before logger init; otherwise log lines leak into stdout
    // and corrupt the generated completion script.
    if let Commands::ShellCompletions(cmd) = &cli.command {
        return run_shell_completions(cmd);
    }

    let logger = env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .parse_default_env()
        .build();

    let level = logger.filter();
    let pb = MultiProgress::new();

    LogWrapper::new(pb.clone(), logger).try_init()?;
    log::set_max_level(level);

    cleanup_old_executable().await?;

    let mut github = GithubApi::new()?;

    if discriminant(&cli.command) != discriminant(&Commands::SelfUpdate(SelfUpdateCommand {})) {
        // Non-fatal: network outages or GitHub rate limits shouldn't
        // prevent the user from running conversions offline.
        if let Err(e) = check_for_new_version_and_notify(&mut github).await {
            log::debug!("update check skipped: {e}");
        }
    }

    let progress = IndicatifProgress::new(pb.clone());
    let total_progress = IndicatifProgress::new(pb);

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
                convert_cdn_to_cia(opts, &progress, &total_progress).await?
            }
            CtrCommands::GenerateCdnTicket(cmd) => {
                generate_ticket_from_cdn(&cmd.cdn_dir, &cmd.output).await?
            }
            CtrCommands::Decrypt(cmd) => {
                if cmd.recursive {
                    if !cmd.input.is_dir() {
                        anyhow::bail!(
                            "INPUT must be a directory when --recursive is set: {}",
                            cmd.input.display()
                        );
                    }
                    decrypt_rom_batch(&cmd.input, &progress, &total_progress).await?
                } else {
                    let output = cmd
                        .output
                        .unwrap_or_else(|| derive_decrypted_path(&cmd.input));
                    decrypt_rom(&cmd.input, &output, &progress).await?
                }
            }
            CtrCommands::Compress(cmd) => {
                if cmd.recursive {
                    if !cmd.input.is_dir() {
                        anyhow::bail!(
                            "INPUT must be a directory when --recursive is set: {}",
                            cmd.input.display()
                        );
                    }
                    compress_rom_batch(&cmd.input, cmd.level, &progress, &total_progress).await?
                } else {
                    let output = cmd
                        .output
                        .clone()
                        .unwrap_or_else(|| derive_compressed_path(&cmd.input));
                    compress_rom(&cmd.input, &output, cmd.level, &progress).await?
                }
            }
            CtrCommands::Decompress(cmd) => {
                if cmd.recursive {
                    if !cmd.input.is_dir() {
                        anyhow::bail!(
                            "INPUT must be a directory when --recursive is set: {}",
                            cmd.input.display()
                        );
                    }
                    decompress_rom_batch(&cmd.input, &progress, &total_progress).await?
                } else {
                    let output = cmd
                        .output
                        .unwrap_or_else(|| derive_decompressed_path(&cmd.input));
                    decompress_rom(&cmd.input, &output, &progress).await?
                }
            }
            CtrCommands::Convert(cmd) => {
                if cmd.recursive {
                    if !cmd.input.is_dir() {
                        anyhow::bail!(
                            "INPUT must be a directory when --recursive is set: {}",
                            cmd.input.display()
                        );
                    }
                    convert_rom_batch(&cmd.input, &progress, &total_progress).await?
                } else {
                    let output = cmd
                        .output
                        .clone()
                        .unwrap_or_else(|| derive_converted_path(&cmd.input));
                    convert_rom(&cmd.input, &output, &progress).await?
                }
            }
            CtrCommands::Verify(cmd) => {
                let opts = CtrVerifyOptions {
                    verify_content_hashes: cmd.verify_content,
                };
                if cmd.recursive {
                    if !cmd.input.is_dir() {
                        anyhow::bail!(
                            "INPUT must be a directory when --recursive is set: {}",
                            cmd.input.display()
                        );
                    }
                    let summary =
                        verify_ctr_batch(&cmd.input, &opts, &progress, &total_progress).await?;
                    log::info!(
                        "Verified {} files: {} OK, {} failed",
                        summary.total,
                        summary.ok,
                        summary.failed
                    );
                } else {
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
            }
            CtrCommands::Info(cmd) => {
                let info = rom_converto_lib::nintendo::ctr::info::read_info(&cmd.input)?;
                if let Some(dir) = &cmd.save_icon {
                    save_ctr_icon(&info, dir)?;
                }
                info_print::print(&rom_converto_lib::info::InfoResult::Ctr(info), cmd.json)?;
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
                if wants_wbfs_output(&output) {
                    decompress_disc_to_wbfs(&cmd.input, &output, &progress).await?
                } else {
                    decompress_disc(&cmd.input, &output, &progress).await?
                }
            }
            DolCommands::Info(cmd) => {
                let info = rom_converto_lib::nintendo::dol::info::read_info(&cmd.input)?;
                if let Some(dir) = &cmd.save_icon {
                    save_dol_banner(&info, dir)?;
                }
                info_print::print(&rom_converto_lib::info::InfoResult::Dol(info), cmd.json)?;
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
                if wants_wbfs_output(&output) {
                    decompress_disc_to_wbfs(&cmd.input, &output, &progress).await?
                } else {
                    decompress_disc(&cmd.input, &output, &progress).await?
                }
            }
            RvlCommands::Info(cmd) => {
                let info = rom_converto_lib::nintendo::rvl::info::read_info(&cmd.input)?;
                info_print::print(&rom_converto_lib::info::InfoResult::Rvl(info), cmd.json)?;
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
            ChdCommands::Info(cmd) => {
                let info = rom_converto_lib::chd::info::read_info(&cmd.input)?;
                info_print::print(&rom_converto_lib::info::InfoResult::Chd(info), cmd.json)?;
            }
        },
        Commands::Nx(inner) => match inner {
            NxCommands::Compress(cmd) => {
                let keys = load_keyset(cmd.keys.as_deref())?;
                let kind = detect_container(&cmd.input)?;
                let mut opts = NxCompressOptions::for_kind(kind);
                if let Some(level) = cmd.level {
                    opts.level = level;
                }
                if let Some(mode) = cmd.mode.as_deref() {
                    opts.mode = match mode {
                        "solid" => NczMode::Solid,
                        "block" => NczMode::Block {
                            size_exp: cmd.block_size_exp.unwrap_or(20),
                        },
                        _ => unreachable!("clap value_parser already validated"),
                    };
                } else if let Some(exp) = cmd.block_size_exp {
                    opts.mode = NczMode::Block { size_exp: exp };
                }
                let output = cmd
                    .output
                    .clone()
                    .unwrap_or_else(|| nx_derive_compressed_path(&cmd.input));
                compress_container_async(cmd.input, output, opts, keys, &progress).await?
            }
            NxCommands::Decompress(cmd) => {
                let keys = load_keyset(cmd.keys.as_deref())?;
                let output = cmd
                    .output
                    .clone()
                    .unwrap_or_else(|| nx_derive_decompressed_path(&cmd.input));
                decompress_container_async(cmd.input, output, keys, &progress).await?
            }
            NxCommands::Verify(cmd) => {
                let keys = load_keyset(cmd.keys.as_deref())?;
                let result = verify_container_async(cmd.input, keys, &progress).await?;
                log::info!("Container kind: {}", result.kind);
                log::info!("Overall: {}", if result.ok { "OK" } else { "MISMATCHES" });
                for v in &result.ncas {
                    let prefix = match &v.partition {
                        Some(p) => format!("[{p}] "),
                        None => String::new(),
                    };
                    log::info!(
                        "  {prefix}{}: {} (sections mismatched: {})",
                        v.name,
                        if v.ok { "OK" } else { "FAIL" },
                        v.mismatched_sections
                    );
                }
            }
            NxCommands::Info(cmd) => {
                let info = rom_converto_lib::nintendo::nx::info::read_info(
                    &cmd.input,
                    cmd.keys.as_deref(),
                )?;
                if let Some(dir) = &cmd.save_icon {
                    save_nx_icon(&info, dir)?;
                }
                info_print::print(&rom_converto_lib::info::InfoResult::Nx(info), cmd.json)?;
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
            WupCommands::Info(cmd) => {
                let info = rom_converto_lib::nintendo::wup::info::read_info(&cmd.input)?;
                info_print::print(&rom_converto_lib::info::InfoResult::Wup(info), cmd.json)?;
            }
        },
        Commands::SelfUpdate(_) => self_update(&mut github).await?,
        Commands::ShellCompletions(_) => unreachable!("handled before logger init"),
    }

    Ok(())
}

fn save_dol_banner(info: &rom_converto_lib::info::DolInfo, dir: &std::path::Path) -> Result<()> {
    let Some(img) = &info.banner_image else {
        log::warn!("no GameCube banner decoded; nothing to save");
        return Ok(());
    };
    std::fs::create_dir_all(dir)?;
    let stem = if info.game_id.is_empty() {
        "gamecube-banner".to_string()
    } else {
        info.game_id.clone()
    };
    let path = dir.join(format!("{stem}.png"));
    std::fs::write(&path, &img.png_bytes)?;
    log::info!("wrote {}", path.display());
    Ok(())
}

fn save_ctr_icon(info: &rom_converto_lib::info::CtrInfo, dir: &std::path::Path) -> Result<()> {
    let Some(img) = &info.icon else {
        log::warn!("no SMDH icon decoded; nothing to save");
        return Ok(());
    };
    std::fs::create_dir_all(dir)?;
    let stem = if info.title_id.is_empty() {
        "ctr-icon".to_string()
    } else {
        info.title_id.clone()
    };
    let path = dir.join(format!("{stem}.png"));
    std::fs::write(&path, &img.png_bytes)?;
    log::info!("wrote {}", path.display());
    Ok(())
}

fn save_nx_icon(info: &rom_converto_lib::info::NxInfo, dir: &std::path::Path) -> Result<()> {
    let Some(full) = &info.full else {
        log::warn!("no control NCA payload available; nothing to save");
        return Ok(());
    };
    let Some(ctrl) = &full.control else {
        log::warn!("no NACP/icon decoded; nothing to save");
        return Ok(());
    };
    let Some(img) = &ctrl.icon else {
        log::warn!("control NACP loaded but no icon present; nothing to save");
        return Ok(());
    };
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{:016X}.png", full.application_title_id));
    std::fs::write(&path, &img.png_bytes)?;
    log::info!("wrote {}", path.display());
    Ok(())
}

fn run_shell_completions(cmd: &ShellCompletionsCommand) -> Result<()> {
    // The package is rom-converto-cli but the installed binary is
    // rom-converto; completions must key off the binary name the user
    // actually types. CARGO_BIN_NAME tracks the [[bin]] name even if
    // the crate is renamed.
    let bin = env!("CARGO_BIN_NAME");
    let mut clap_cmd = Cli::command().name(bin).bin_name(bin);

    match &cmd.out_dir {
        Some(dir) => {
            std::fs::create_dir_all(dir)?;
            let path = generate_to(cmd.shell, &mut clap_cmd, bin, dir)?;
            println!("{}", path.display());
        }
        None => {
            generate(cmd.shell, &mut clap_cmd, bin, &mut std::io::stdout().lock());
        }
    }
    Ok(())
}
