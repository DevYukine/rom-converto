use crate::commands::info_command::InfoCommand;
use crate::commands::{BatchArgs, ConflictArgs, ConflictPolicyArg, OutputArgs};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::commands::support::{
    ALL_IMAGE_EXTS, DispatchCtx, require_dir, require_info_input, require_input, save_nx_icon,
};
use crate::util::{ensure_input_exists, resolve_policy};
use crate::{batch, config, info_print};
use anyhow::Result;
use rom_converto_lib::nintendo::nx::{load_keyset, verify_container_async};
use rom_converto_lib::runner::models::{RunOptions, WupTitleInputOption};
use rom_converto_lib::util::{CancelToken, ConflictPolicy, FileStatus};

/// Commands specific to Nintendo Switch (NX) NSP/XCI containers
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum NxCommands {
    Compress(NxCompressCommand),
    Decompress(NxDecompressCommand),
    Verify(NxVerifyCommand),
    Merge(NxMergeCommand),
    Split(NxSplitCommand),
    Info(InfoCommand),
}

/// Compress an NSP into NSZ or an XCI into XCZ
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Compress an NSP into NSZ or an XCI into XCZ\n\nNCAs inside the container are decrypted, zstd-compressed, and packaged with the already-derived per-section keys cached in NCZSECTN.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto nx compress game.nsp\n  Explicit output: rom-converto nx compress game.xci game.xcz\n  Whole folder:    rom-converto nx compress -R ./roms --output-dir ./nsz\n"
)]
pub struct NxCompressCommand {
    /// Path to `prod.keys`. Defaults to `$HOME/.switch/prod.keys` on Linux/macOS or `%USERPROFILE%/.switch/prod.keys` on Windows, then the binary's own directory
    #[arg(long = "keys", value_name = "PRODKEYS")]
    pub keys: Option<PathBuf>,

    /// Input NSP or XCI, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output path. Defaults to the input path with the extension switched (.nsp -> .nsz, .xci -> .xcz)
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output path. Defaults to the input path with the extension switched (.nsp -> .nsz, .xci -> .xcz)
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    #[command(flatten)]
    pub out: OutputArgs,

    /// Zstd compression level. nsz default is 18; the maximum 22 needs over 1 GiB of RAM during decompression on the Switch
    #[arg(
        short = 'l',
        long = "level",
        value_name = "LEVEL",
        value_parser = clap::value_parser!(i32).range(1..=22)
    )]
    pub level: Option<i32>,

    /// Compression mode. `solid` writes one zstd frame per NCA (smaller output, default for NSP). `block` writes independent zstd frames per fixed-size block (random read friendly, default for XCI)
    #[arg(long = "mode", value_parser = ["solid", "block"])]
    pub mode: Option<String>,

    /// Block-mode block size, expressed as a power of two (`exp` in `1 << exp` bytes). nsz default is 20 (1 MiB). Range 14..=32
    #[arg(long = "block-size-exp", value_parser = clap::value_parser!(u8).range(14..=32))]
    pub block_size_exp: Option<u8>,

    #[command(flatten)]
    pub conflict: ConflictArgs,

    /// Compress every .nsp and .xci found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    #[command(flatten)]
    pub batch: BatchArgs,
}

/// Decompress an NSZ back to NSP or an XCZ back to XCI
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto nx decompress game.nsz\n  Explicit output: rom-converto nx decompress game.xcz game.xci\n  Whole folder:    rom-converto nx decompress -R ./nsz --output-dir ./roms\n"
)]
pub struct NxDecompressCommand {
    /// Path to `prod.keys`. Defaults to `$HOME/.switch/prod.keys` on Linux/macOS or `%USERPROFILE%/.switch/prod.keys` on Windows, then the binary's own directory
    #[arg(long = "keys", value_name = "PRODKEYS")]
    pub keys: Option<PathBuf>,

    /// Input NSZ or XCZ, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output path. Defaults to the input path with the extension switched (.nsz -> .nsp, .xcz -> .xci)
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output path. Defaults to the input path with the extension switched (.nsz -> .nsp, .xcz -> .xci)
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

    /// Decompress every .nsz and .xcz found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    #[command(flatten)]
    pub batch: BatchArgs,
}

/// Verify hash integrity of every NCA in a Switch container
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  Single file:  rom-converto nx verify game.nsp\n  Whole folder: rom-converto nx verify -R ./roms\n"
)]
pub struct NxVerifyCommand {
    /// Path to `prod.keys`. Defaults to `$HOME/.switch/prod.keys` on Linux/macOS or `%USERPROFILE%/.switch/prod.keys` on Windows, then the binary's own directory
    #[arg(long = "keys", value_name = "PRODKEYS")]
    pub keys: Option<PathBuf>,

    /// Input container (NSP / NSZ / XCI / XCZ), or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Verify every .nsp, .xci, .nsz and .xcz found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,
}

/// Merge a base container with its update and DLC into a single super NSP or XCI
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Merge a base container with its update and DLC into a single super NSP or XCI\n\nThe highest-version content metadata wins per title; NCAs shared between inputs are deduplicated. `--format nsp` (default) accepts a mix of NSP and XCI inputs; `--format xci` requires every input to already be an XCI.",
    after_long_help = "EXAMPLES:\n  Super NSP: rom-converto nx merge base.nsp update.nsp dlc.nsp -o super.nsp\n  Super XCI: rom-converto nx merge base.xci update.xci --format xci -o super.xci\n"
)]
pub struct NxMergeCommand {
    /// Path to `prod.keys`. Defaults to `$HOME/.switch/prod.keys` on Linux/macOS or `%USERPROFILE%/.switch/prod.keys` on Windows, then the binary's own directory
    #[arg(long = "keys", value_name = "PRODKEYS")]
    pub keys: Option<PathBuf>,

    /// Base container plus its update and DLC containers, in any order. NSZ/XCZ inputs must be decompressed first
    #[arg(required = true, num_args = 1.., value_name = "INPUT")]
    pub inputs: Vec<PathBuf>,

    /// Output path. Defaults to `<first input's name> (Merged).<nsp|xci>` next to the first input
    #[arg(short = 'o', long = "output", value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output container format. `nsp` (default) accepts a mix of NSP and XCI inputs; `xci` requires every input to already be an XCI
    #[arg(long = "format", value_parser = ["nsp", "xci"])]
    pub format: Option<String>,

    #[command(flatten)]
    pub conflict: ConflictArgs,
}

/// Split a super NSP or XCI into one NSP per title
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  rom-converto nx split super.nsp\n  rom-converto nx split super.xci --output-dir ./split\n"
)]
pub struct NxSplitCommand {
    /// Path to `prod.keys`. Defaults to `$HOME/.switch/prod.keys` on Linux/macOS or `%USERPROFILE%/.switch/prod.keys` on Windows, then the binary's own directory
    #[arg(long = "keys", value_name = "PRODKEYS")]
    pub keys: Option<PathBuf>,

    /// Input NSP or XCI containing multiple titles. NSZ/XCZ inputs must be decompressed first
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Directory to write the per-title `.nsp` outputs into. Created if missing. Defaults to `<input's name>_split` next to the input
    #[arg(long = "output-dir", value_name = "DIR")]
    pub output_dir: Option<PathBuf>,

    /// What to do when the output directory already exists: error, overwrite, or skip. `rename` is rejected for directory outputs
    #[arg(long = "on-conflict", value_enum)]
    pub on_conflict: Option<ConflictPolicyArg>,

    /// Alias for --on-conflict overwrite
    #[arg(
        long,
        short = 'f',
        default_value_t = false,
        conflicts_with = "on_conflict"
    )]
    pub force: bool,
}

/// Runs one `nx` subcommand.
pub async fn run(command: NxCommands, ctx: DispatchCtx<'_>) -> Result<()> {
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
        NxCommands::Compress(cmd) => {
            let eff = &effective.nx;
            require_input(&cmd.input, cmd.recursive)?;
            let block_size_exp = cmd.block_size_exp.or(eff.block_size_exp);
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
            options.keys = cmd.keys;
            options.level = cmd.level.or(eff.level);
            // A bare --block-size-exp still selects block mode, as it did
            // before the runner picked the mode apart from the exponent.
            options.mode = cmd
                .mode
                .or_else(|| eff.mode.clone())
                .or_else(|| block_size_exp.map(|_| "block".to_string()));
            options.block_size_exp = block_size_exp.map(u32::from);
            batch::run(
                &run,
                "nx.compress",
                cmd.input,
                cmd.output_flag.or(cmd.output),
                options,
            )
            .await?;
        }
        NxCommands::Decompress(cmd) => {
            let eff = &effective.nx;
            require_input(&cmd.input, cmd.recursive)?;
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
            options.keys = cmd.keys;
            batch::run(
                &run,
                "nx.decompress",
                cmd.input,
                cmd.output_flag.or(cmd.output),
                options,
            )
            .await?;
        }
        NxCommands::Verify(cmd) => {
            let keys = load_keyset(cmd.keys.as_deref())?;
            if cmd.recursive {
                require_dir(&cmd.input)?;
                batch::nx_verify(&progress, &total_progress, &cmd.input, keys, cmd.max_depth)
                    .await?;
                return Ok(());
            }
            ensure_input_exists(&cmd.input)?;
            let resolved = rom_converto_lib::util::resolve_input(
                &cmd.input,
                &["nsp", "xci", "nca", "nsz", "xcz", "ncz"],
            )?;
            let result = verify_container_async(
                resolved.path().to_path_buf(),
                keys,
                &progress,
                CancelToken::new(),
            )
            .await?;
            log::info!("Container kind: {}", result.kind);
            log::info!("Overall: {}", if result.ok { "OK" } else { "FAIL" });
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
            if !result.ok {
                anyhow::bail!("verification failed");
            }
        }
        NxCommands::Merge(cmd) => {
            let eff = &effective.nx;
            for input in &cmd.inputs {
                ensure_input_exists(input)?;
            }
            let mut options = RunOptions::from(batch::Common {
                recursive: false,
                output_dir: eff.output_dir.clone(),
                output_template: None,
                max_depth: None,
                report: None,
                policy: resolve_policy(
                    cmd.conflict.on_conflict,
                    cmd.conflict.force,
                    ConflictPolicy::Error,
                ),
                skip_space_check,
            });
            options.keys = cmd.keys;
            options.format = Some(cmd.format.unwrap_or_else(|| "nsp".to_string()));
            options.inputs = Some(
                cmd.inputs
                    .iter()
                    .cloned()
                    .map(WupTitleInputOption::Path)
                    .collect(),
            );
            let first = cmd.inputs[0].clone();
            let response = batch::run(&run, "nx.merge", first, cmd.output, options).await?;
            if let Some(record) = response.records.first()
                && record.status == FileStatus::Ok
            {
                log::info!("wrote {}", record.output_path);
            }
        }
        NxCommands::Split(cmd) => {
            let eff = &effective.nx;
            ensure_input_exists(&cmd.input)?;
            let mut options = RunOptions::from(batch::Common {
                recursive: false,
                output_dir: cmd.output_dir.or_else(|| eff.output_dir.clone()),
                output_template: None,
                max_depth: None,
                report: None,
                policy: resolve_policy(cmd.on_conflict, cmd.force, ConflictPolicy::Error),
                skip_space_check,
            });
            options.keys = cmd.keys;
            let response = batch::run(&run, "nx.split", cmd.input, None, options).await?;
            if response.records.iter().any(|r| r.status == FileStatus::Ok) {
                log::info!("{}", response.message);
            }
        }
        NxCommands::Info(cmd) => {
            let input = require_info_input(&cmd.input)?;
            ensure_input_exists(input)?;
            let resolved = rom_converto_lib::util::resolve_input(input, ALL_IMAGE_EXTS)?;
            let info = rom_converto_lib::nintendo::nx::info::read_info(
                resolved.path(),
                cmd.keys.as_deref(),
            )?;
            if let Some(dir) = &cmd.save_icon {
                save_nx_icon(&info, dir)?;
            }
            info_print::print(&rom_converto_lib::info::InfoResult::Nx(info), cmd.json)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Harness {
        #[command(subcommand)]
        cmd: NxCommands,
    }

    #[test]
    fn parses_compress_minimal() {
        let h = Harness::parse_from(["bin", "compress", "game.nsp"]);
        let NxCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.input, PathBuf::from("game.nsp"));
        assert!(c.output.is_none());
        assert!(c.level.is_none());
    }

    #[test]
    fn parses_compress_with_keys_level_mode() {
        let h = Harness::parse_from([
            "bin",
            "compress",
            "--keys",
            "/k/prod.keys",
            "-l",
            "18",
            "--mode",
            "block",
            "--block-size-exp",
            "20",
            "-o",
            "out.nsz",
            "game.nsp",
        ]);
        let NxCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.keys, Some(PathBuf::from("/k/prod.keys")));
        assert_eq!(c.level, Some(18));
        assert_eq!(c.mode.as_deref(), Some("block"));
        assert_eq!(c.block_size_exp, Some(20));
        assert_eq!(c.output_flag, Some(PathBuf::from("out.nsz")));
    }

    #[test]
    fn parses_compress_positional_output() {
        let h = Harness::parse_from(["bin", "compress", "game.nsp", "out.nsz"]);
        let NxCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.input, PathBuf::from("game.nsp"));
        assert_eq!(c.output, Some(PathBuf::from("out.nsz")));
        assert!(c.output_flag.is_none());
    }

    #[test]
    fn output_flag_conflicts_with_positional_output() {
        let result =
            Harness::try_parse_from(["bin", "compress", "game.nsp", "out.nsz", "-o", "other.nsz"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_decompress() {
        let h = Harness::parse_from(["bin", "decompress", "g.nsz"]);
        let NxCommands::Decompress(c) = h.cmd else {
            panic!("expected Decompress");
        };
        assert_eq!(c.input, PathBuf::from("g.nsz"));
        assert!(!c.conflict.force);
    }

    #[test]
    fn parses_compress_force() {
        let h = Harness::parse_from(["bin", "compress", "-f", "game.nsp"]);
        let NxCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.conflict.force);
    }

    #[test]
    fn parses_verify() {
        let h = Harness::parse_from(["bin", "verify", "--keys", "k", "g.nsz"]);
        let NxCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert_eq!(c.keys, Some(PathBuf::from("k")));
        assert_eq!(c.input, PathBuf::from("g.nsz"));
    }

    #[test]
    fn parses_compress_recursive() {
        let h = Harness::parse_from(["bin", "compress", "-R", "roms"]);
        let NxCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.recursive);
        assert!(c.output.is_none());
    }

    #[test]
    fn parses_verify_recursive() {
        let h = Harness::parse_from(["bin", "verify", "-R", "roms"]);
        let NxCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert!(c.recursive);
    }

    #[test]
    fn parses_compress_output_dir() {
        let h = Harness::parse_from(["bin", "compress", "--output-dir", "out", "game.nsp"]);
        let NxCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.out.output_dir, Some(PathBuf::from("out")));
        assert!(c.output.is_none());
    }

    #[test]
    fn output_dir_conflicts_with_output() {
        let result = Harness::try_parse_from([
            "bin",
            "compress",
            "-o",
            "out.nsz",
            "--output-dir",
            "out",
            "game.nsp",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_compress_report_flag() {
        let h = Harness::parse_from(["bin", "compress", "game.nsp", "--report", "out.json"]);
        let NxCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.batch.report, Some(PathBuf::from("out.json")));
    }

    #[test]
    fn parses_decompress_report_flag() {
        let h = Harness::parse_from(["bin", "decompress", "game.nsz", "--report", "out.csv"]);
        let NxCommands::Decompress(c) = h.cmd else {
            panic!("expected Decompress");
        };
        assert_eq!(c.batch.report, Some(PathBuf::from("out.csv")));
    }

    #[test]
    fn on_conflict_absent_is_none() {
        let h = Harness::parse_from(["bin", "compress", "game.nsp"]);
        let NxCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.conflict.on_conflict.is_none());
    }

    #[test]
    fn parses_merge_multiple_inputs() {
        let h = Harness::parse_from(["bin", "merge", "a.nsp", "b.nsp", "c.nsp", "-o", "out.nsp"]);
        let NxCommands::Merge(c) = h.cmd else {
            panic!("expected Merge");
        };
        assert_eq!(
            c.inputs,
            vec![
                PathBuf::from("a.nsp"),
                PathBuf::from("b.nsp"),
                PathBuf::from("c.nsp"),
            ]
        );
        assert_eq!(c.output, Some(PathBuf::from("out.nsp")));
        assert!(c.format.is_none());
    }

    #[test]
    fn parses_merge_format_xci() {
        let h = Harness::parse_from(["bin", "merge", "base.xci", "upd.xci", "--format", "xci"]);
        let NxCommands::Merge(c) = h.cmd else {
            panic!("expected Merge");
        };
        assert_eq!(c.format.as_deref(), Some("xci"));
    }

    #[test]
    fn parses_split_output_dir() {
        let h = Harness::parse_from(["bin", "split", "super.nsp", "--output-dir", "out"]);
        let NxCommands::Split(c) = h.cmd else {
            panic!("expected Split");
        };
        assert_eq!(c.input, PathBuf::from("super.nsp"));
        assert_eq!(c.output_dir, Some(PathBuf::from("out")));
    }
}
