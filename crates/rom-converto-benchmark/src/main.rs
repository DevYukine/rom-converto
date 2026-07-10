//! Benchmarks rom-converto against reference conversion tools per format
//! family: nsz for Switch, chdman for CHD, DolphinTool for GameCube and Wii
//! disc images, and z3ds_compressor for 3DS. Each run drives both tools on
//! the same input and prints a Markdown results table matching the layout
//! of the committed `benchmark/*.md` files.

mod bench;
mod report;
mod runner;
mod stats;
mod tool;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bench::BenchCtx;
use runner::RunConfig;

const ENV_NSP: &str = "ROMCONVERTO_BENCH_NX_NSP";
const ENV_XCI: &str = "ROMCONVERTO_BENCH_NX_XCI";
const ENV_KEYS: &str = "ROMCONVERTO_BENCH_KEYS";
const ENV_RVL_ISO: &str = "ROMCONVERTO_BENCH_RVL_ISO";
const ENV_DOL_ISO: &str = "ROMCONVERTO_BENCH_DOL_ISO";
const ENV_CUE: &str = "ROMCONVERTO_BENCH_CD_CUE";
const ENV_3DS: &str = "ROMCONVERTO_BENCH_CTR_3DS";
const ENV_CIA: &str = "ROMCONVERTO_BENCH_CTR_CIA";

/// Reproducible benchmark harness comparing rom-converto against external reference tools
#[derive(Parser)]
#[command(name = "rom-converto-benchmark", version, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// rom-converto binary to benchmark. Defaults to ROMCONVERTO_BIN, then a
    /// sibling of this tool, PATH, or ./target/{release,debug}
    #[arg(long, global = true, value_name = "PATH")]
    rom_converto_bin: Option<PathBuf>,

    /// Interleaved runs per operation. Run 1 is the warm-up and is excluded
    #[arg(long, global = true, default_value_t = 10, value_name = "N")]
    iterations: usize,

    /// Cooldown between every individual tool invocation, in seconds
    #[arg(long, global = true, default_value_t = 3, value_name = "SECS")]
    cooldown_secs: u64,

    /// Keep temporary output directories instead of deleting them
    #[arg(long, global = true, default_value_t = false)]
    keep_temp: bool,

    /// Time rom-converto alone, without the reference tool. Use on hosts
    /// where the counterpart CLI is not available
    #[arg(long, global = true, default_value_t = false)]
    rom_converto_only: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Switch NSP/XCI (NSZ/XCZ) versus nsz
    Switch(SwitchArgs),
    /// Wii disc image (RVZ) versus DolphinTool
    Wii(DiscArgs),
    /// GameCube disc image (RVZ) versus DolphinTool
    Gamecube(DiscArgs),
    /// 3DS ROM (Z3DS) versus z3ds_compressor
    #[command(name = "ctr", visible_alias = "3ds")]
    Ctr(CtrArgs),
    /// Disc image (CHD) versus chdman
    Chd(ChdArgs),
    /// Run every platform whose inputs are set via ROMCONVERTO_BENCH_* env vars
    All,
}

#[derive(Args)]
struct SwitchArgs {
    /// Decrypted/normal NSP input (env ROMCONVERTO_BENCH_NX_NSP)
    #[arg(long, value_name = "NSP")]
    nsp: Option<PathBuf>,
    /// XCI input (env ROMCONVERTO_BENCH_NX_XCI)
    #[arg(long, value_name = "XCI")]
    xci: Option<PathBuf>,
    /// prod.keys passed to rom-converto (env ROMCONVERTO_BENCH_KEYS)
    #[arg(long, value_name = "PRODKEYS")]
    keys: Option<PathBuf>,
}

#[derive(Args)]
struct DiscArgs {
    /// Disc image input (.iso). Wii env ROMCONVERTO_BENCH_RVL_ISO, GameCube
    /// env ROMCONVERTO_BENCH_DOL_ISO
    #[arg(long, value_name = "ISO")]
    iso: Option<PathBuf>,
    /// Comma-separated zstd levels to benchmark
    #[arg(long, default_value = "5,22", value_name = "L1,L2")]
    levels: String,
}

#[derive(Args)]
struct ChdArgs {
    /// .cue input with a sibling .bin (env ROMCONVERTO_BENCH_CD_CUE)
    #[arg(long, value_name = "CUE")]
    cue: Option<PathBuf>,
}

#[derive(Args)]
struct CtrArgs {
    /// Decrypted .3ds input (env ROMCONVERTO_BENCH_CTR_3DS)
    #[arg(long = "three-ds", value_name = "3DS")]
    three_ds: Option<PathBuf>,
    /// Decrypted .cia input (env ROMCONVERTO_BENCH_CTR_CIA)
    #[arg(long, value_name = "CIA")]
    cia: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.iterations == 0 {
        bail!("--iterations must be at least 1");
    }

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let cancel = Arc::clone(&cancel);
        ctrlc::set_handler(move || cancel.store(true, Ordering::SeqCst))
            .context("failed to install Ctrl-C handler")?;
    }

    let rom_converto = tool::resolve_rom_converto(cli.rom_converto_bin.as_deref())?;
    let rom_converto_dir = rom_converto
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let ctx = BenchCtx {
        rom_converto,
        rom_converto_dir,
        config: RunConfig {
            iterations: cli.iterations,
            cooldown: Duration::from_secs(cli.cooldown_secs),
            cancel,
        },
        keep_temp: cli.keep_temp,
        rom_converto_only: cli.rom_converto_only,
    };

    println!("Benchmarking {}", ctx.rom_converto.display());
    let mode = if ctx.rom_converto_only {
        "rom-converto only"
    } else {
        "head-to-head"
    };
    println!(
        "{} runs per operation, {}s cooldown, run 1 excluded, {mode}",
        cli.iterations, cli.cooldown_secs
    );

    match cli.command {
        Command::Switch(a) => bench::switch::run(
            &ctx,
            or_env(a.nsp, ENV_NSP),
            or_env(a.xci, ENV_XCI),
            or_env(a.keys, ENV_KEYS),
        )?,
        Command::Wii(a) => bench::disc::run(
            &ctx,
            "rvl",
            "Wii",
            or_env(a.iso, ENV_RVL_ISO),
            &parse_levels(&a.levels)?,
        )?,
        Command::Gamecube(a) => bench::disc::run(
            &ctx,
            "dol",
            "GameCube",
            or_env(a.iso, ENV_DOL_ISO),
            &parse_levels(&a.levels)?,
        )?,
        Command::Chd(a) => bench::chd::run(&ctx, or_env(a.cue, ENV_CUE))?,
        Command::Ctr(a) => {
            bench::ctr::run(&ctx, or_env(a.three_ds, ENV_3DS), or_env(a.cia, ENV_CIA))?
        }
        Command::All => run_all(&ctx)?,
    }
    Ok(())
}

fn run_all(ctx: &BenchCtx) -> Result<()> {
    let mut attempted = 0;

    let nsp = env_path(ENV_NSP);
    let xci = env_path(ENV_XCI);
    if nsp.is_some() || xci.is_some() {
        attempted += 1;
        report_failure(
            "Switch",
            bench::switch::run(ctx, nsp, xci, env_path(ENV_KEYS)),
        );
    } else {
        println!("Skipping Switch: set {ENV_NSP} and/or {ENV_XCI}");
    }

    if let Some(iso) = env_path(ENV_RVL_ISO) {
        attempted += 1;
        report_failure(
            "Wii",
            bench::disc::run(ctx, "rvl", "Wii", Some(iso), &[5, 22]),
        );
    } else {
        println!("Skipping Wii: set {ENV_RVL_ISO}");
    }

    if let Some(iso) = env_path(ENV_DOL_ISO) {
        attempted += 1;
        report_failure(
            "GameCube",
            bench::disc::run(ctx, "dol", "GameCube", Some(iso), &[5, 22]),
        );
    } else {
        println!("Skipping GameCube: set {ENV_DOL_ISO}");
    }

    if let Some(cue) = env_path(ENV_CUE) {
        attempted += 1;
        report_failure("CHD", bench::chd::run(ctx, Some(cue)));
    } else {
        println!("Skipping CHD: set {ENV_CUE}");
    }

    let tds = env_path(ENV_3DS);
    let cia = env_path(ENV_CIA);
    if tds.is_some() || cia.is_some() {
        attempted += 1;
        report_failure("3DS", bench::ctr::run(ctx, tds, cia));
    } else {
        println!("Skipping 3DS: set {ENV_3DS} and/or {ENV_CIA}");
    }

    if attempted == 0 {
        bail!(
            "no platform inputs configured; set the ROMCONVERTO_BENCH_* variables for at least one platform"
        );
    }
    Ok(())
}

fn report_failure(name: &str, result: Result<()>) {
    if let Err(e) = result {
        println!("{name} benchmark failed: {e:#}");
    }
}

fn or_env(flag: Option<PathBuf>, env: &str) -> Option<PathBuf> {
    flag.or_else(|| env_path(env))
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

fn parse_levels(spec: &str) -> Result<Vec<i32>> {
    let mut levels = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        levels.push(
            part.parse::<i32>()
                .with_context(|| format!("invalid level '{part}' in --levels"))?,
        );
    }
    if levels.is_empty() {
        bail!("--levels did not contain any levels");
    }
    Ok(levels)
}
