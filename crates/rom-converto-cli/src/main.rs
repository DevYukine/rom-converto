//! Clap-based CLI over rom-converto-lib. Each subcommand maps one to one
//! onto a library conversion, verification, or info function; this crate
//! adds argument parsing, progress reporting, batch/dry-run orchestration,
//! and config file resolution around those calls.

use crate::commands::{Cli, Commands};
use crate::github::api::GithubApi;
use crate::updater::{check_for_new_version_and_notify, cleanup_old_executable, self_update};
use crate::util::{IndicatifProgress, TotalProgress};
use anyhow::{Context, Result};
use clap::Parser;
use indicatif::MultiProgress;
use indicatif_log_bridge::LogWrapper;
use rom_converto_lib::runner::is_cancelled_error;
use std::io::IsTerminal;

use crate::commands::support::DispatchCtx;

mod batch;
mod commands;
mod config;
mod dry_run;
mod github;
mod info_print;
mod logging;
mod updater;
mod util;
// Mirrors the inline logic in build.rs; kept here so it is unit-testable.
mod version;

pub mod built_info {
    // The file has been placed there by the build script.
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();

    let cli = Cli::parse_from(std::env::args_os().map(crate::util::expand_tilde_arg));

    // Must run before logger init; otherwise log lines leak into stdout
    // and corrupt the generated completion script.
    if let Commands::ShellCompletions(cmd) = &cli.command {
        return crate::commands::completions::run(cmd);
    }
    let (project_level, global_level) = logging::resolve_log_levels(cli.quiet, cli.verbose);

    let debug_file = match cli.debug_log.as_deref() {
        Some(path) => {
            let f = std::fs::File::create(path)
                .with_context(|| format!("cannot open debug log: {}", path.display()))?;
            Some(std::io::BufWriter::new(f))
        }
        None => None,
    };

    let mut builder = env_logger::builder();
    builder
        .filter_level(global_level)
        .filter_module("rom_converto", project_level)
        .filter_module("rom_converto_lib", project_level)
        .format_timestamp(None);
    if cli.verbose == 0 && !cli.quiet {
        builder.format_target(false);
        // At default verbosity ordinary info lines are user-facing
        // summaries, so they print as plain text; warnings and errors
        // keep a label so they still stand out.
        builder.format(|buf, record| {
            use std::io::Write;
            match record.level() {
                log::Level::Info => writeln!(buf, "{}", record.args()),
                level => writeln!(buf, "[{level}] {}", record.args()),
            }
        });
    }
    let console_logger = builder.parse_default_env().build();

    let pb = MultiProgress::new();

    let max_level = if debug_file.is_some() {
        log::LevelFilter::Trace
    } else {
        console_logger.filter()
    };

    match debug_file {
        Some(file) => {
            let dual = logging::DualLogger::new(console_logger, file);
            LogWrapper::new(pb.clone(), dual).try_init()?;
        }
        None => {
            LogWrapper::new(pb.clone(), console_logger).try_init()?;
        }
    }
    log::set_max_level(max_level);

    // Non-fatal: a locked or read-only leftover from a previous self-update
    // must not stop the freshly installed binary from running.
    if let Err(e) = cleanup_old_executable().await {
        log::debug!("Could not remove old executable: {e}");
    }

    if should_check_for_updates(cli.no_update_check) {
        // Non-fatal: network outages or GitHub rate limits shouldn't
        // prevent the user from running conversions offline.
        if let Err(e) = check_for_new_version_and_notify(&mut GithubApi::new()?).await {
            log::debug!("Update check skipped: {e}");
        }
    }

    let progress = IndicatifProgress::new(pb.clone());
    let total_progress = TotalProgress::new(pb);

    let user_config = rom_converto_lib::config::load_config(cli.config.as_deref())?;
    let preset = rom_converto_lib::config::resolve_preset(&user_config, cli.preset.as_deref())?;
    let effective = config::resolve(&user_config, preset.as_ref());
    let dry_run = cli.dry_run;
    let skip_space_check = cli.skip_space_check;
    let cache = std::sync::Arc::new(rom_converto_lib::util::HashCache::load(
        cli.no_cache,
        cli.rebuild_cache,
    ));

    let cancel = rom_converto_lib::util::CancelToken::new();
    {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                cancel.cancel();
            }
        });
    }

    let dispatch = dispatch_command(
        cli.command,
        DispatchCtx {
            progress,
            total_progress,
            effective: &effective,
            config: cli.config,
            preset: cli.preset,
            dry_run,
            skip_space_check,
            cancel: cancel.clone(),
            cache: &cache,
        },
    )
    .await;

    cache.save();
    log::logger().flush();

    if let Err(err) = dispatch {
        if cancel.is_cancelled() && is_cancelled_error(&err) {
            eprintln!("Cancelled");
            std::process::exit(130);
        }
        return Err(err);
    }
    Ok(())
}

fn should_check_for_updates(no_update_check: bool) -> bool {
    if no_update_check {
        return false;
    }
    for var in ["ROM_CONVERTO_NO_UPDATE_CHECK", "NO_UPDATE_NOTIFIER", "CI"] {
        if std::env::var(var).is_ok() {
            return false;
        }
    }
    std::io::stderr().is_terminal()
}

async fn dispatch_command(command: Commands, ctx: DispatchCtx<'_>) -> Result<()> {
    use crate::commands as c;
    match command {
        Commands::Ctr(inner) => c::ctr::run(inner, ctx).await,
        Commands::Dol(inner) => c::dol::run(inner, ctx).await,
        Commands::Rvl(inner) => c::rvl::run(inner, ctx).await,
        Commands::Wup(inner) => c::wup::run(inner, ctx).await,
        Commands::Nx(inner) => c::nx::run(inner, ctx).await,
        Commands::Xbox(inner) => c::xbox::run(inner, ctx).await,
        Commands::Xenon(inner) => c::xenon::run(inner, ctx).await,
        Commands::Ps3(inner) => c::ps3::run(inner, ctx).await,
        Commands::Psp(inner) => c::psp::run(inner, ctx).await,
        Commands::Vita(inner) => c::vita::run(inner, ctx).await,
        Commands::Ntr(inner) => c::ntr::run(inner, ctx).await,
        Commands::Chd(inner) => c::chd::run(inner, ctx).await,
        Commands::Cso(inner) => c::cso::run(inner, ctx).await,
        Commands::Cue(inner) => c::cue::run(inner, ctx).await,
        Commands::Dat(inner) => c::dat::run(inner, ctx).await,
        Commands::Organize(cmd) => c::organize::run(cmd, ctx).await,
        Commands::Capabilities(cmd) => c::misc::run(cmd).await,
        Commands::Hash(cmd) => c::hash::run(cmd, ctx).await,
        Commands::Info(cmd) => c::info_command::run(cmd, ctx).await,
        Commands::Playlist(cmd) => c::playlist::run(cmd, ctx).await,
        Commands::SelfUpdate(_) => self_update(&mut GithubApi::new()?).await,
        Commands::ShellCompletions(cmd) => c::completions::run(&cmd),
    }
}
