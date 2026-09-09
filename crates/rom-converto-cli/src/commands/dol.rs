use crate::commands::info_command::InfoCommand;
use crate::commands::{BatchArgs, ConflictArgs, OutputArgs};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::commands::support::{
    ALL_IMAGE_EXTS, DispatchCtx, print_rvz_structure, require_dir, require_info_input,
    require_input, resolve_migrate_opts, save_dol_banner, verify_gate,
};
use crate::util::{ensure_input_exists, ok_str, resolve_policy};
use crate::{batch, config, info_print};
use anyhow::Result;
use rom_converto_lib::nintendo::disc::legacy::{
    DOL_MIGRATE_FORMATS, detect_legacy_format, ensure_format_allowed,
};
use rom_converto_lib::nintendo::disc::rvz::RvzCompressOptions;
use rom_converto_lib::nintendo::dol::verify::{DolVerifyOptions, verify_dol};
use rom_converto_lib::runner::models::RunOptions;
use rom_converto_lib::util::{CancelToken, oversized_rvz_chunk};

/// Commands specific to DOL (GameCube) disc images
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum DolCommands {
    Compress(CompressDiscCommand),
    Migrate(MigrateDiscCommand),
    Decompress(DecompressDiscCommand),
    Verify(VerifyDiscCommand),
    Info(InfoCommand),
}

/// Migrate a legacy GameCube disc image (GCZ or NKit) to RVZ
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Migrate a legacy GameCube disc image (GCZ or NKit) to RVZ\n\n\
Supported input: .gcz, .nkit.iso, and .nkit.gcz, detected by content so renamed files work. \
The container is integrity-checked first (GCZ block checksums, NKit whole-file CRC32), then the \
original disc is reconstructed on the fly (NKit junk regeneration included) and compressed to RVZ. \
NKit restorations are additionally verified against the embedded source CRC32 while converting.\n\n\
Output defaults to the input path with the extension replaced by .rvz.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto dol migrate game.gcz\n  Explicit output: rom-converto dol migrate game.nkit.iso game.rvz\n  Whole directory: rom-converto dol migrate -R ./roms\n"
)]
pub struct MigrateDiscCommand {
    /// Input disc image path (.gcz, .nkit.iso, .nkit.gcz), or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output RVZ path, defaults to the input path with extension replaced by .rvz (ignored with --recursive)
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output RVZ path, defaults to the input path with extension replaced by .rvz (ignored with --recursive)
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    /// Zstandard compression level (signed, negative levels allowed).
    /// Defaults to 22 (archive quality)
    #[arg(long, short = 'l')]
    pub level: Option<i32>,

    /// RVZ chunk size in bytes. Must be a power of two between 32 KiB
    /// and 2 MiB. Defaults to 128 KiB
    #[arg(long)]
    pub chunk_size: Option<u32>,

    /// Skip the pre-conversion integrity pass
    #[arg(long, default_value_t = false)]
    pub skip_verify: bool,

    /// Overwrite the output file if it already exists
    #[arg(long, short = 'f', default_value_t = false)]
    pub force: bool,

    /// Migrate every GCZ and NKit image found in the INPUT directory (detected by content)
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,
}

/// Verify a GameCube disc image
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Verify a GameCube disc image\n\n\
Fast mode (default) checks the RVZ container's stored SHA-1 hashes (file header, disc struct, partition table). It is a no-op for plain .iso / .gcm input, which carries no integrity data.\n\n\
--full decodes the whole disc, validates the FST geometry, and computes a whole-disc SHA-1. GameCube discs carry no built-in integrity hashes, so that digest is informational (for external DAT/Redump matching), never a pass/fail.\n\n\
Legacy GameCube containers (GCZ, NKit) are decoded on the fly and checked the same way. A WIA holds a Wii disc image, so it is rejected with a pointer to rvl verify.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto dol verify game.iso\n  Legacy input:    rom-converto dol verify game.gcz\n  Full check:      rom-converto dol verify game.rvz --full\n  Whole directory: rom-converto dol verify -R ./roms\n"
)]
pub struct VerifyDiscCommand {
    /// Input disc image path (.iso, .gcm, .rvz, .gcz, .nkit.iso, .nkit.gcz), or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Deep verification: decode the whole disc and compute a whole-disc SHA-1
    #[arg(long, default_value_t = false)]
    pub full: bool,

    /// Verify every .iso, .gcm and .rvz found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,
}

/// Compress a GameCube disc image to RVZ
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Compress a GameCube disc image to RVZ\n\n\
Supported input: .iso / .gcm.\nOutput defaults to the same path with the extension replaced by .rvz.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto dol compress game.iso\n  Explicit output: rom-converto dol compress game.gcm game.rvz\n  Whole directory: rom-converto dol compress -R ./roms --output-dir ./rvz\n"
)]
pub struct CompressDiscCommand {
    /// Input disc image path (.iso or .gcm), or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output RVZ path, defaults to the input path with extension replaced by .rvz (ignored with --recursive)
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output RVZ path, defaults to the input path with extension replaced by .rvz (ignored with --recursive)
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    #[command(flatten)]
    pub out: OutputArgs,

    /// Zstandard compression level (signed, negative levels allowed). Defaults to 22 (archive quality). Lower values trade ratio for speed; Dolphin's documented suggestion is 5
    #[arg(long, short = 'l', value_parser = clap::value_parser!(i32).range(-22..=22))]
    pub level: Option<i32>,

    /// Chunk size in bytes. Must be a power of two between 32 KiB and 2 MiB. Defaults to 128 KiB (matches Dolphin's RVZ default)
    #[arg(long)]
    pub chunk_size: Option<u32>,

    #[command(flatten)]
    pub conflict: ConflictArgs,

    /// Compress every .iso and .gcm found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    #[command(flatten)]
    pub batch: BatchArgs,
}

/// Decompress an RVZ GameCube disc image back to ISO
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Decompress an RVZ GameCube disc image back to ISO\n\nOutput defaults to the input path with extension replaced by .iso.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto dol decompress game.rvz\n  Explicit output: rom-converto dol decompress game.rvz game.iso\n  Whole directory: rom-converto dol decompress -R ./rvz --output-dir ./roms\n"
)]
pub struct DecompressDiscCommand {
    /// Input RVZ file path, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output ISO path (ignored with --recursive)
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output ISO path (ignored with --recursive)
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    #[command(flatten)]
    pub out: OutputArgs,

    #[command(flatten)]
    pub conflict: ConflictArgs,

    /// Decompress every .rvz found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    #[command(flatten)]
    pub batch: BatchArgs,
}

/// Runs one `dol` subcommand.
pub async fn run(command: DolCommands, ctx: DispatchCtx<'_>) -> Result<()> {
    let DispatchCtx {
        progress,
        total_progress,
        effective,
        dry_run,
        skip_space_check,
        cancel,
        cache,
        config,
        preset,
        ..
    } = ctx;
    let run = batch::BatchRun {
        progress: &progress,
        total_progress: &total_progress,
        cache,
        cancel: &cancel,
        config,
        preset,
        dry_run,
    };
    match command {
        DolCommands::Compress(cmd) => {
            let eff = &effective.dol;
            require_input(&cmd.input, cmd.recursive)?;
            let level = cmd
                .level
                .or(eff.level)
                .unwrap_or(RvzCompressOptions::default().compression_level);
            let chunk_size = cmd
                .chunk_size
                .or(eff.chunk_size)
                .unwrap_or(RvzCompressOptions::default().chunk_size);
            if let Some(msg) = oversized_rvz_chunk(chunk_size) {
                log::warn!("{msg}");
            }
            if !cmd.recursive
                && let Some(fmt) = detect_legacy_format(&cmd.input)?
            {
                ensure_format_allowed(fmt, DOL_MIGRATE_FORMATS)?;
            }
            let mut options = RunOptions::from(batch::Common {
                recursive: cmd.recursive,
                output_dir: cmd.out.output_dir.or_else(|| eff.output_dir.clone()),
                output_template: cmd.out.output_template,
                max_depth: cmd.batch.max_depth,
                report: cmd.batch.report.or_else(|| eff.report.clone()),
                policy: resolve_policy(
                    cmd.conflict.on_conflict,
                    cmd.conflict.force,
                    config::policy_fallback(&eff.on_conflict)?,
                ),
                skip_space_check,
            });
            options.level = Some(level);
            options.chunk_size = Some(chunk_size);
            batch::run(
                &run,
                "dol.compress",
                cmd.input,
                cmd.output_flag.or(cmd.output),
                options,
            )
            .await?;
        }
        DolCommands::Migrate(cmd) => {
            let opts = resolve_migrate_opts(cmd.level, cmd.chunk_size, &effective.dol);
            if let Some(msg) = oversized_rvz_chunk(opts.chunk_size) {
                log::warn!("{msg}");
            }
            require_input(&cmd.input, cmd.recursive)?;
            let mut options = RunOptions::from(batch::Common {
                recursive: cmd.recursive,
                output_dir: None,
                output_template: None,
                max_depth: None,
                report: None,
                policy: resolve_policy(
                    None,
                    cmd.force,
                    rom_converto_lib::util::ConflictPolicy::Error,
                ),
                skip_space_check,
            });
            options.level = Some(opts.compression_level);
            options.chunk_size = Some(opts.chunk_size);
            options.skip_verify = Some(cmd.skip_verify);
            batch::run(
                &run,
                "dol.migrate",
                cmd.input,
                cmd.output_flag.or(cmd.output),
                options,
            )
            .await?;
        }
        DolCommands::Decompress(cmd) => {
            let eff = &effective.dol;
            require_input(&cmd.input, cmd.recursive)?;
            let options = RunOptions::from(batch::Common {
                recursive: cmd.recursive,
                output_dir: cmd.out.output_dir.or_else(|| eff.output_dir.clone()),
                output_template: cmd.out.output_template,
                max_depth: cmd.batch.max_depth,
                report: cmd.batch.report.or_else(|| eff.report.clone()),
                policy: resolve_policy(
                    cmd.conflict.on_conflict,
                    cmd.conflict.force,
                    config::policy_fallback(&eff.on_conflict)?,
                ),
                skip_space_check,
            });
            batch::run(
                &run,
                "dol.decompress",
                cmd.input,
                cmd.output_flag.or(cmd.output),
                options,
            )
            .await?;
        }
        DolCommands::Verify(cmd) => {
            if cmd.recursive {
                require_dir(&cmd.input)?;
                batch::dol_verify(
                    &progress,
                    &total_progress,
                    &cmd.input,
                    cmd.full,
                    cmd.max_depth,
                )
                .await?
            } else {
                ensure_input_exists(&cmd.input)?;
                let resolved = rom_converto_lib::util::resolve_input(
                    &cmd.input,
                    &["iso", "gcm", "gcz", "rvz"],
                )?;
                let input = resolved.path();
                verify_gate(input, DOL_MIGRATE_FORMATS)?;
                let opts = DolVerifyOptions { full: cmd.full };
                let result = verify_dol(input, &opts, &progress, &CancelToken::new())?;
                log::info!("Game ID: {}", result.game_id);
                print_rvz_structure(result.rvz_structure.as_ref());
                if let Some(st) = &result.structural {
                    log::info!("FST within bounds: {}", ok_str(st.fst_within_bounds));
                    for n in &st.notes {
                        log::info!("  {n}");
                    }
                }
                if let Some(d) = &result.disc_sha1 {
                    log::info!("Whole-disc SHA-1: {d}");
                }
                log::info!("Overall: {}", if result.ok { "OK" } else { "FAIL" });
                if !result.ok {
                    anyhow::bail!("verification failed");
                }
            }
        }
        DolCommands::Info(cmd) => {
            if cmd.keys.is_some() {
                anyhow::bail!("--keys is only supported by nx, wup, and ps3 info");
            }
            let input = require_info_input(&cmd.input)?;
            ensure_input_exists(input)?;
            let resolved = rom_converto_lib::util::resolve_input(input, ALL_IMAGE_EXTS)?;
            let info = rom_converto_lib::nintendo::dol::info::read_info(resolved.path())?;
            if let Some(dir) = &cmd.save_icon {
                save_dol_banner(&info, dir)?;
            }
            info_print::print(&rom_converto_lib::info::InfoResult::Dol(info), cmd.json)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::ConflictPolicyArg;

    #[derive(Parser, Debug)]
    struct Harness {
        #[command(subcommand)]
        cmd: DolCommands,
    }

    #[test]
    fn parses_compress_recursive() {
        let h = Harness::parse_from(["bin", "compress", "roms", "-R"]);
        let DolCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.recursive);
        assert_eq!(c.output, None);
    }

    #[test]
    fn parses_verify_recursive() {
        let h = Harness::parse_from(["bin", "verify", "roms", "-R"]);
        let DolCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert!(c.recursive);
    }

    #[test]
    fn parses_migrate_force_and_output_flag() {
        let h = Harness::parse_from(["bin", "migrate", "game.gcz", "-o", "out.rvz", "-f"]);
        let DolCommands::Migrate(c) = h.cmd else {
            panic!("expected Migrate");
        };
        assert!(c.force);
        assert_eq!(c.output, None);
        assert_eq!(c.output_flag, Some(PathBuf::from("out.rvz")));
    }

    #[test]
    fn migrate_output_flag_conflicts_with_positional() {
        let result =
            Harness::try_parse_from(["bin", "migrate", "game.gcz", "pos.rvz", "-o", "flag.rvz"]);
        assert!(result.is_err());
    }

    #[test]
    fn migrate_rejects_deep_flag() {
        let result = Harness::try_parse_from(["bin", "migrate", "game.gcz", "--deep"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_compress_output_dir() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--output-dir", "out"]);
        let DolCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.out.output_dir, Some(PathBuf::from("out")));
        assert_eq!(c.output, None);
    }

    #[test]
    fn parses_compress_report_flag() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--report", "out.html"]);
        let DolCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.batch.report, Some(PathBuf::from("out.html")));
    }

    #[test]
    fn parses_decompress_report_flag() {
        let h = Harness::parse_from(["bin", "decompress", "game.rvz", "--report", "out.json"]);
        let DolCommands::Decompress(c) = h.cmd else {
            panic!("expected Decompress");
        };
        assert_eq!(c.batch.report, Some(PathBuf::from("out.json")));
    }

    #[test]
    fn on_conflict_absent_is_none() {
        let h = Harness::parse_from(["bin", "compress", "game.iso"]);
        let DolCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.conflict.on_conflict.is_none());
    }

    #[test]
    fn parses_compress_output_template() {
        let h = Harness::parse_from([
            "bin",
            "compress",
            "game.iso",
            "--output-template",
            "{console}/{title}.{ext}",
        ]);
        let DolCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(
            c.out.output_template,
            Some("{console}/{title}.{ext}".to_string())
        );
    }

    #[test]
    fn output_template_conflicts_with_explicit_output() {
        let r = Harness::try_parse_from([
            "bin",
            "compress",
            "game.iso",
            "out.rvz",
            "--output-template",
            "{title}.{ext}",
        ]);
        assert!(r.is_err());
    }

    #[test]
    fn parses_on_conflict_skip() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--on-conflict", "skip"]);
        let DolCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.conflict.on_conflict, Some(ConflictPolicyArg::Skip));
    }
}
