use crate::commands::info_command::InfoCommand;
use crate::commands::{BatchArgs, ConflictArgs, OutputArgs};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::commands::support::{
    ALL_IMAGE_EXTS, DispatchCtx, finish_single, migrate_dry_run, print_rvz_structure, require_dir,
    require_info_input, resolve_migrate_opts, save_rvl_image, verify_gate, wants_wbfs_output,
};
use crate::util::{
    SingleOutput, WriteDecision, ensure_input_exists, file_len, log_skipped, resolve_output,
    resolve_policy, resolve_single_output,
};
use crate::{batch, config, info_print};
use anyhow::Result;
use rom_converto_lib::nintendo::legacy_input::{
    ALL_MIGRATE_FORMATS, MigrateOptions, migrate_disc, migrate_disc_batch,
};
use rom_converto_lib::nintendo::rvl::verify::{RvlVerifyOptions, verify_rvl};
use rom_converto_lib::nintendo::rvz::{
    RvzCompressOptions, compress_disc, decompress_disc, decompress_disc_to_wbfs, derive_disc_path,
    derive_rvz_path,
};
use rom_converto_lib::util::{CancelToken, TallyDirection, oversized_rvz_chunk};
use std::path::Path;
use std::time::Instant;

/// Commands specific to RVL (Wii) disc images
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum RvlCommands {
    Compress(CompressDiscCommand),
    Migrate(MigrateDiscCommand),
    Decompress(DecompressDiscCommand),
    Verify(VerifyDiscCommand),
    Info(InfoCommand),
}

/// Migrate a legacy Wii disc image (WIA, GCZ, or NKit) to RVZ
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Migrate a legacy Wii disc image (WIA, GCZ, or NKit) to RVZ\n\n\
Supported input: .wia (all compression methods including bzip2/LZMA/LZMA2), .gcz, .nkit.iso, and \
.nkit.gcz, detected by content so renamed files work. The container is integrity-checked first \
(WIA SHA-1 chain, GCZ block checksums, NKit whole-file CRC32), then the original disc is \
reconstructed on the fly (Wii hash tree rebuild and re-encryption included) and compressed to RVZ.\n\n\
Output defaults to the input path with the extension replaced by .rvz.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto rvl migrate game.wia\n  Explicit output: rom-converto rvl migrate game.gcz game.rvz\n  Whole directory: rom-converto rvl migrate -R ./roms\n"
)]
pub struct MigrateDiscCommand {
    /// Input disc image path (.wia, .gcz, .nkit.iso, .nkit.gcz), or a directory with --recursive
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

    /// Decode every WIA group during verification instead of only the
    /// SHA-1 header chain
    #[arg(long, default_value_t = false)]
    pub deep: bool,

    /// Overwrite the output file if it already exists
    #[arg(long, short = 'f', default_value_t = false)]
    pub force: bool,

    /// Migrate every WIA, GCZ and NKit image found in the INPUT directory (detected by content)
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,
}

/// Verify a Wii disc image
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Verify a Wii disc image\n\n\
Fast mode (default) checks the RVZ container's stored SHA-1 hashes (file header, disc struct, partition table). It is a no-op for plain .iso / .wbfs input, which carries no container hashes.\n\n\
--full decrypts every partition cluster and recomputes the H0/H1/H2 hash tree, comparing it to the on-disc hash regions to detect tampering or bit rot. This decrypts and hashes the entire disc and can be slow.\n\n\
Legacy Wii containers (WIA, GCZ, NKit) are decoded on the fly and checked the same way.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto rvl verify game.iso\n  Legacy input:    rom-converto rvl verify game.wia\n  Full check:      rom-converto rvl verify game.rvz --full\n  Whole directory: rom-converto rvl verify -R ./roms\n"
)]
pub struct VerifyDiscCommand {
    /// Input disc image path (.iso, .wbfs, .rvz, .wia, .gcz, .nkit.iso, .nkit.gcz), or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Deep verification: recompute the Wii H0/H1/H2 partition hash tree
    #[arg(long, default_value_t = false)]
    pub full: bool,

    /// Verify every .iso, .wbfs and .rvz found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,
}

/// Compress a Wii disc image to RVZ
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Compress a Wii disc image to RVZ\n\n\
Supported input: .iso, or .wbfs (single file or split .wbf1.. parts), streamed directly.\nOutput defaults to the same path with the extension replaced by .rvz.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto rvl compress game.iso\n  Explicit output: rom-converto rvl compress game.wbfs game.rvz\n  Whole directory: rom-converto rvl compress -R ./roms --output-dir ./rvz\n"
)]
pub struct CompressDiscCommand {
    /// Input disc image path (.iso or .wbfs), or a directory with --recursive
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

    /// Compress every .iso and .wbfs found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    #[command(flatten)]
    pub batch: BatchArgs,
}

/// Decompress an RVZ Wii disc image back to ISO or WBFS
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Decompress an RVZ Wii disc image back to ISO or WBFS\n\nThe output format follows the output file extension: a .wbfs path writes a scrubbed WBFS container streamed directly from the RVZ, anything else writes a raw .iso. Defaults to .iso.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto rvl decompress game.rvz\n  Explicit output: rom-converto rvl decompress game.rvz game.wbfs\n  Whole directory: rom-converto rvl decompress -R ./rvz --output-dir ./roms\n"
)]
pub struct DecompressDiscCommand {
    /// Input RVZ file path, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output path. A .wbfs extension writes a WBFS container; otherwise a raw .iso (ignored with --recursive)
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output path. A .wbfs extension writes a WBFS container; otherwise a raw .iso (ignored with --recursive)
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

/// Runs one `rvl` subcommand.
pub async fn run(command: RvlCommands, ctx: DispatchCtx<'_>) -> Result<()> {
    let DispatchCtx {
        progress,
        total_progress,
        effective,
        dry_run,
        skip_space_check,
        cancel,
        cache,
        ..
    } = ctx;
    match command {
        RvlCommands::Compress(cmd) => {
            let eff = &effective.rvl;
            let opts = RvzCompressOptions {
                compression_level: cmd
                    .level
                    .or(eff.level)
                    .unwrap_or(RvzCompressOptions::default().compression_level),
                chunk_size: cmd
                    .chunk_size
                    .or(eff.chunk_size)
                    .unwrap_or(RvzCompressOptions::default().chunk_size),
                ..RvzCompressOptions::default()
            };
            if let Some(msg) = oversized_rvz_chunk(opts.chunk_size) {
                log::warn!("{msg}");
            }
            let output_dir = cmd
                .out
                .output_dir
                .clone()
                .or_else(|| eff.output_dir.clone());
            let report = cmd.batch.report.clone().or_else(|| eff.report.clone());
            let fallback = config::policy_fallback(&eff.on_conflict)?;
            if cmd.recursive {
                require_dir(&cmd.input)?;
                let run = batch::BatchRun {
                    progress: &progress,
                    total_progress: &total_progress,
                    input_dir: &cmd.input,
                    policy: resolve_policy(cmd.conflict.on_conflict, cmd.conflict.force, fallback),
                    output_dir: output_dir.as_deref(),
                    output_template: cmd.out.output_template.as_deref(),
                    max_depth: cmd.batch.max_depth,
                    dry_run,
                    skip_space_check,
                    report_path: report.as_deref(),
                    cancel: &cancel,
                };
                batch::rvz_compress(&run, &["iso", "wbfs"], opts, cache).await?
            } else {
                ensure_input_exists(&cmd.input)?;
                let resolved = rom_converto_lib::util::resolve_input(
                    &cmd.input,
                    &["iso", "wbfs", "gcz", "wia"],
                )?;
                let input = resolved.path();
                let policy = resolve_policy(cmd.conflict.on_conflict, cmd.conflict.force, fallback);
                let Some(output) = resolve_single_output(
                    SingleOutput {
                        operation: "compress",
                        cli_input: &cmd.input,
                        input,
                        explicit: cmd.output_flag.or(cmd.output),
                        derived: derive_rvz_path(resolved.output_basis()),
                        output_dir: output_dir.as_deref(),
                        output_template: cmd.out.output_template.as_deref(),
                        output_ext: "rvz",
                        keys_path: None,
                        policy,
                        verify: crate::util::OutputVerify::Rvz,
                        media: Some("RVZ"),
                        missing_keys: None,
                        report: report.as_deref(),
                        dry_run,
                        cancel: cancel.clone(),
                    },
                    &progress,
                )
                .await?
                else {
                    return Ok(());
                };
                if !skip_space_check {
                    let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                    batch::space_preflight_for_size(file_len(input), check_dir)?;
                }
                let started = Instant::now();
                compress_disc(input, &output, opts, &progress, cancel.clone()).await?;
                finish_single(
                    &cmd.input,
                    &output,
                    TallyDirection::Compress,
                    "compress",
                    started,
                    report.as_deref(),
                )?;
            }
        }
        RvlCommands::Migrate(cmd) => {
            let opts = resolve_migrate_opts(cmd.level, cmd.chunk_size, &effective.rvl);
            if let Some(msg) = oversized_rvz_chunk(opts.chunk_size) {
                log::warn!("{msg}");
            }
            let migrate_opts = MigrateOptions {
                skip_verify: cmd.skip_verify,
                deep_verify: cmd.deep,
            };
            if dry_run {
                return migrate_dry_run(
                    &cmd.input,
                    cmd.output_flag.or(cmd.output),
                    cmd.recursive,
                    cmd.force,
                    ALL_MIGRATE_FORMATS,
                );
            }
            if cmd.recursive {
                require_dir(&cmd.input)?;
                migrate_disc_batch(
                    &cmd.input,
                    opts,
                    migrate_opts,
                    ALL_MIGRATE_FORMATS,
                    cmd.force,
                    &progress,
                    cancel.clone(),
                )
                .await?;
            } else {
                let output = cmd
                    .output_flag
                    .or(cmd.output)
                    .unwrap_or_else(|| derive_rvz_path(&cmd.input));
                let policy = resolve_policy(
                    None,
                    cmd.force,
                    rom_converto_lib::util::ConflictPolicy::Error,
                );
                let output = match resolve_output(&output, policy)? {
                    WriteDecision::Skip => {
                        log_skipped(&output);
                        return Ok(());
                    }
                    WriteDecision::Write(p) => p,
                };
                migrate_disc(
                    &cmd.input,
                    &output,
                    opts,
                    migrate_opts,
                    ALL_MIGRATE_FORMATS,
                    &progress,
                    cancel.clone(),
                )
                .await?
            }
        }
        RvlCommands::Decompress(cmd) => {
            let eff = &effective.rvl;
            let output_dir = cmd
                .out
                .output_dir
                .clone()
                .or_else(|| eff.output_dir.clone());
            let report = cmd.batch.report.clone().or_else(|| eff.report.clone());
            let fallback = config::policy_fallback(&eff.on_conflict)?;
            if cmd.recursive {
                require_dir(&cmd.input)?;
                let run = batch::BatchRun {
                    progress: &progress,
                    total_progress: &total_progress,
                    input_dir: &cmd.input,
                    policy: resolve_policy(cmd.conflict.on_conflict, cmd.conflict.force, fallback),
                    output_dir: output_dir.as_deref(),
                    output_template: cmd.out.output_template.as_deref(),
                    max_depth: cmd.batch.max_depth,
                    dry_run,
                    skip_space_check,
                    report_path: report.as_deref(),
                    cancel: &cancel,
                };
                batch::rvz_decompress(&run).await?
            } else {
                ensure_input_exists(&cmd.input)?;
                let resolved = rom_converto_lib::util::resolve_input(&cmd.input, &["rvz"])?;
                let input = resolved.path();
                let policy = resolve_policy(cmd.conflict.on_conflict, cmd.conflict.force, fallback);
                let Some(output) = resolve_single_output(
                    SingleOutput {
                        operation: "decompress",
                        cli_input: &cmd.input,
                        input,
                        explicit: cmd.output_flag.or(cmd.output),
                        derived: derive_disc_path(resolved.output_basis()),
                        output_dir: output_dir.as_deref(),
                        output_template: cmd.out.output_template.as_deref(),
                        output_ext: "iso",
                        keys_path: None,
                        policy,
                        verify: crate::util::OutputVerify::None,
                        media: None,
                        missing_keys: None,
                        report: report.as_deref(),
                        dry_run,
                        cancel: cancel.clone(),
                    },
                    &progress,
                )
                .await?
                else {
                    return Ok(());
                };
                if !skip_space_check {
                    let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                    batch::space_preflight_for_size(file_len(input), check_dir)?;
                }
                let started = Instant::now();
                if wants_wbfs_output(&output) {
                    decompress_disc_to_wbfs(input, &output, &progress, cancel.clone()).await?
                } else {
                    decompress_disc(input, &output, &progress, cancel.clone()).await?
                }
                finish_single(
                    &cmd.input,
                    &output,
                    TallyDirection::Decompress,
                    "decompress",
                    started,
                    report.as_deref(),
                )?;
            }
        }
        RvlCommands::Verify(cmd) => {
            if cmd.recursive {
                require_dir(&cmd.input)?;
                batch::rvl_verify(
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
                    &["iso", "wbfs", "gcz", "wia", "rvz"],
                )?;
                let input = resolved.path();
                verify_gate(input, ALL_MIGRATE_FORMATS)?;
                let opts = RvlVerifyOptions { full: cmd.full };
                let result = verify_rvl(input, &opts, &progress, &CancelToken::new())?;
                log::info!("Game ID: {}", result.game_id);
                print_rvz_structure(result.rvz_structure.as_ref());
                if result.rvz_structure.is_none() && !cmd.full {
                    log::info!(
                        "No RVZ container hashes to check; pass --full to verify the partition hash tree"
                    );
                }
                for p in &result.partitions {
                    log::info!(
                        "  Partition @0x{:X} ({}): {} ({} clusters, {} mismatched)",
                        p.offset,
                        p.kind,
                        if p.ok { "OK" } else { "FAIL" },
                        p.clusters_checked,
                        p.mismatched_clusters
                    );
                    if p.scrubbed_clusters > 0 {
                        log::info!(
                            "    {} scrubbed clusters skipped (zero-filled by the dump tool)",
                            p.scrubbed_clusters
                        );
                    }
                    if let Some(note) = &p.note {
                        log::info!("    {note}");
                    }
                    if !p.sample_bad_clusters.is_empty() {
                        log::info!("    bad clusters: {:?}", p.sample_bad_clusters);
                    }
                }
                log::info!("Overall: {}", if result.ok { "OK" } else { "FAIL" });
                if !result.ok {
                    anyhow::bail!("verification failed");
                }
            }
        }
        RvlCommands::Info(cmd) => {
            if cmd.keys.is_some() {
                anyhow::bail!("--keys is only supported by nx, wup, and ps3 info");
            }
            let input = require_info_input(&cmd.input)?;
            ensure_input_exists(input)?;
            let resolved = rom_converto_lib::util::resolve_input(input, ALL_IMAGE_EXTS)?;
            let info = rom_converto_lib::nintendo::rvl::info::read_info(resolved.path())?;
            if let Some(dir) = &cmd.save_icon {
                save_rvl_image(&info, dir)?;
            }
            info_print::print(&rom_converto_lib::info::InfoResult::Rvl(info), cmd.json)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Parser, Debug)]
    struct Harness {
        #[command(subcommand)]
        cmd: RvlCommands,
    }

    #[test]
    fn parses_compress_recursive() {
        let h = Harness::parse_from(["bin", "compress", "roms", "-R"]);
        let RvlCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.recursive);
        assert_eq!(c.output, None);
    }

    #[test]
    fn parses_verify_recursive() {
        let h = Harness::parse_from(["bin", "verify", "roms", "-R"]);
        let RvlCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert!(c.recursive);
    }

    #[test]
    fn parses_migrate_force_and_output_flag() {
        let h = Harness::parse_from(["bin", "migrate", "game.wia", "-o", "out.rvz", "-f"]);
        let RvlCommands::Migrate(c) = h.cmd else {
            panic!("expected Migrate");
        };
        assert!(c.force);
        assert_eq!(c.output, None);
        assert_eq!(c.output_flag, Some(PathBuf::from("out.rvz")));
    }

    #[test]
    fn migrate_output_flag_conflicts_with_positional() {
        let result =
            Harness::try_parse_from(["bin", "migrate", "game.wia", "pos.rvz", "-o", "flag.rvz"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_migrate_deep() {
        let h = Harness::parse_from(["bin", "migrate", "game.wia", "--deep"]);
        let RvlCommands::Migrate(c) = h.cmd else {
            panic!("expected Migrate");
        };
        assert!(c.deep);
    }

    #[test]
    fn parses_compress_output_dir() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--output-dir", "out"]);
        let RvlCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.out.output_dir, Some(PathBuf::from("out")));
        assert_eq!(c.output, None);
    }

    #[test]
    fn parses_compress_report_flag() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--report", "out.csv"]);
        let RvlCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.batch.report, Some(PathBuf::from("out.csv")));
    }

    #[test]
    fn parses_decompress_report_flag() {
        let h = Harness::parse_from(["bin", "decompress", "game.rvz", "--report", "out.json"]);
        let RvlCommands::Decompress(c) = h.cmd else {
            panic!("expected Decompress");
        };
        assert_eq!(c.batch.report, Some(PathBuf::from("out.json")));
    }

    #[test]
    fn on_conflict_absent_is_none() {
        let h = Harness::parse_from(["bin", "compress", "game.iso"]);
        let RvlCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.conflict.on_conflict.is_none());
    }
}
