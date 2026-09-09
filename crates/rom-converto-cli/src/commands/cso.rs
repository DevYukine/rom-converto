use crate::commands::chd::{ChdCodecList, parse_chd_codecs};
use crate::commands::info_command::InfoCommand;
use crate::commands::{BatchArgs, ConflictArgs, OutputArgs};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

use crate::commands::support::{
    ALL_IMAGE_EXTS, DispatchCtx, disc_mode_name, maybe_log_dvd_codec_tip, require_dir,
    require_info_input, require_input, resolve_chd_codecs, resolved_dvd_mode,
};
use crate::util::{ensure_input_exists, resolve_policy};
use crate::{batch, config, info_print};
use anyhow::Result;
use rom_converto_lib::cso::verify_cso;
use rom_converto_lib::disc::chd::DiscMode;
use rom_converto_lib::runner::models::RunOptions;
use rom_converto_lib::util::CancelToken;

/// Commands for CSO/ZSO compressed ISO images (PSP, PS2)
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum CsoCommands {
    Compress(CompressCommand),
    Decompress(DecompressCommand),
    Verify(VerifyCommand),
    ToChd(ToChdCommand),
    Info(InfoCommand),
}

#[derive(ValueEnum, Debug, Clone, Copy, Eq, PartialEq, Default)]
pub enum CsoFormatArg {
    /// CISO v1 (deflate): real PSP hardware with CFW and PPSSPP
    #[default]
    Cso,
    /// ZISO (LZ4): Open PS2 Loader on real PS2 hardware, ARK-4 on PSP
    Zso,
}

/// Compress an ISO to a CSO or ZSO container
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Compress an ISO to a CSO or ZSO container\n\nPick the format for the target device: CSO for PSP (hardware and PPSSPP), ZSO for PS2 via Open PS2 Loader. Emulator setups are usually better served by `chd compress`.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto cso compress game.iso\n  Explicit output: rom-converto cso compress game.iso game.zso --format zso\n  Whole folder:    rom-converto cso compress -R ./roms --format cso --output-dir ./cso\n"
)]
pub struct CompressCommand {
    /// Input ISO path, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output path, defaults to the input with the format's extension
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output path, defaults to the input with the format's extension
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    #[command(flatten)]
    pub out: OutputArgs,

    /// Output container format
    #[arg(long, value_enum, default_value_t = CsoFormatArg::Cso)]
    pub format: CsoFormatArg,

    /// Block size in bytes, a power of two. Defaults to 2048, or 16384 for inputs of 2 GiB and beyond (matching maxcso)
    #[arg(long, value_name = "BYTES")]
    pub block_size: Option<u32>,

    #[command(flatten)]
    pub conflict: ConflictArgs,

    /// Compress every .iso found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    #[command(flatten)]
    pub batch: BatchArgs,
}

/// Decompress a CSO, ZSO, or DAX container back to a plain ISO
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto cso decompress game.cso\n  Explicit output: rom-converto cso decompress game.zso game.iso\n  Whole folder:    rom-converto cso decompress -R ./cso --output-dir ./roms\n"
)]
pub struct DecompressCommand {
    /// Input .cso, .zso, or .dax path, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output ISO path, defaults to the input with extension replaced by .iso (ignored with --recursive)
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output ISO path, defaults to the input with extension replaced by .iso (ignored with --recursive)
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

    /// Decompress every .cso, .zso, and .dax found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    #[command(flatten)]
    pub batch: BatchArgs,
}

/// Compress a CSO, ZSO, or DAX straight to a CHD, through a temporary ISO
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Compress a CSO, ZSO, or DAX straight to a CHD, through a temporary ISO\n\nDecodes the container to a temporary ISO, then runs the same disc-to-CHD writer `chd compress` uses (so any embedded GAME/NAME tags match a direct build), and always deletes the temporary ISO afterward.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto cso to-chd game.cso\n  Explicit output: rom-converto cso to-chd game.zso game.chd\n  Whole folder:    rom-converto cso to-chd -R ./cso --output-dir ./chd\n"
)]
pub struct ToChdCommand {
    /// Input .cso, .zso, or .dax path, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output CHD path, defaults to the input path with extension replaced by .chd
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output CHD path, defaults to the input path with extension replaced by .chd
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    #[command(flatten)]
    pub out: OutputArgs,

    /// Force DVD mode for the intermediate ISO
    #[arg(long, conflicts_with = "cd")]
    pub dvd: bool,

    /// Force CD mode for the intermediate ISO
    #[arg(long, conflicts_with = "dvd")]
    pub cd: bool,

    /// DVD hunk size in bytes, a multiple of 2048. Defaults to 4096, or 2048 for detected PSP images (PPSSPP reads 2048-byte blocks)
    #[arg(long, value_name = "BYTES")]
    pub hunk_size: Option<u32>,

    /// Codec list for the CHD header's compressor slots: comma-separated chdman-style names, at most 4 of zlib, zstd, lzma, huff, flac, cdzl, cdzs, cdlz, cdfl. Defaults to cdlz,cdzl,cdfl for CD-mode and lzma,zlib,huff,flac for DVD-mode (chdman parity)
    #[arg(short = 'c', long = "codecs", value_name = "LIST", value_parser = parse_chd_codecs)]
    pub codecs: Option<ChdCodecList>,

    /// Compression level in 1..=22. zstd uses the level directly; zlib and lzma cap at 9. Unset uses per-codec defaults (zstd 19, lzma 8, zlib 9)
    #[arg(
        short = 'l',
        long = "level",
        value_name = "LEVEL",
        value_parser = clap::value_parser!(i32).range(1..=22)
    )]
    pub level: Option<i32>,

    #[command(flatten)]
    pub conflict: ConflictArgs,

    /// Convert every .cso, .zso, and .dax found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    #[command(flatten)]
    pub batch: BatchArgs,
}

/// Verify the integrity of a CSO, ZSO, or DAX container
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Verify the integrity of a CSO, ZSO, or DAX container\n\nThe formats embed no checksums, so the standard pass validates the container structure; --full additionally decodes every block.",
    after_long_help = "EXAMPLES:\n  Single file:  rom-converto cso verify game.cso\n  Whole folder: rom-converto cso verify -R ./roms --full\n"
)]
pub struct VerifyCommand {
    /// Input .cso, .zso, or .dax path, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Decode every block instead of only checking the index
    #[arg(long)]
    pub full: bool,

    /// Verify every .cso, .zso, and .dax found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,
}

/// Runs one `cso` subcommand.
pub async fn run(command: CsoCommands, ctx: DispatchCtx<'_>) -> Result<()> {
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
        CsoCommands::Compress(cmd) => {
            let eff = &effective.cso;
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
            options.format = Some(cso_format_name(cmd.format).to_string());
            options.block_size = cmd.block_size.or(eff.block_size);
            batch::run(
                &run,
                "cso.compress",
                cmd.input,
                cmd.output_flag.or(cmd.output),
                options,
            )
            .await?;
        }
        CsoCommands::Decompress(cmd) => {
            let eff = &effective.cso;
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
                "cso.decompress",
                cmd.input,
                cmd.output_flag.or(cmd.output),
                options,
            )
            .await?;
        }
        CsoCommands::Verify(cmd) => {
            if cmd.recursive {
                require_dir(&cmd.input)?;
                batch::cso_verify(
                    &progress,
                    &total_progress,
                    &cmd.input,
                    cmd.full,
                    cmd.max_depth,
                )
                .await?
            } else {
                ensure_input_exists(&cmd.input)?;
                let resolved =
                    rom_converto_lib::util::resolve_input(&cmd.input, &["cso", "zso", "dax"])?;
                verify_cso(
                    &progress,
                    resolved.path().to_path_buf(),
                    cmd.full,
                    CancelToken::new(),
                )
                .await?
            }
        }
        CsoCommands::ToChd(cmd) => {
            let eff = &effective.chd;
            require_input(&cmd.input, cmd.recursive)?;
            let codecs = resolve_chd_codecs(cmd.codecs.clone(), &eff.codecs)?;
            let mode = if cmd.dvd {
                Some(DiscMode::Dvd)
            } else if cmd.cd {
                Some(DiscMode::Cd)
            } else {
                None
            };
            if !cmd.recursive {
                maybe_log_dvd_codec_tip(resolved_dvd_mode(mode, &cmd.input), codecs.is_some());
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
            options.hunk_size = cmd.hunk_size.or(eff.hunk_size);
            options.codecs = codecs.map(|c| c.iter().map(ToString::to_string).collect());
            options.level = cmd.level.or(eff.level);
            options.mode = mode.map(|m| disc_mode_name(m).to_string());
            batch::run(
                &run,
                "cso.to_chd",
                cmd.input,
                cmd.output_flag.or(cmd.output),
                options,
            )
            .await?;
        }
        CsoCommands::Info(cmd) => {
            if cmd.keys.is_some() {
                anyhow::bail!("--keys is only supported by nx, wup, and ps3 info");
            }
            if cmd.save_icon.is_some() {
                anyhow::bail!(
                    "--save-icon is not supported for cso: the format has no embedded artwork"
                );
            }
            let input = require_info_input(&cmd.input)?;
            ensure_input_exists(input)?;
            let resolved = rom_converto_lib::util::resolve_input(input, ALL_IMAGE_EXTS)?;
            let info = rom_converto_lib::cso::info::read_info(resolved.path())?;
            info_print::print(&rom_converto_lib::info::InfoResult::Cso(info), cmd.json)?;
        }
    }
    Ok(())
}

pub(crate) fn cso_format_name(format: CsoFormatArg) -> &'static str {
    match format {
        CsoFormatArg::Cso => "cso",
        CsoFormatArg::Zso => "zso",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rom_converto_lib::disc::chd::ChdCodec;

    #[derive(Parser, Debug)]
    struct Harness {
        #[command(subcommand)]
        cmd: CsoCommands,
    }

    #[test]
    fn parses_compress_with_format() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--format", "zso", "-R"]);
        let CsoCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.format, CsoFormatArg::Zso);
        assert!(c.recursive);
        assert_eq!(c.output, None);
    }

    #[test]
    fn parses_verify_full() {
        let h = Harness::parse_from(["bin", "verify", "game.cso", "--full"]);
        let CsoCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert!(c.full);
    }

    #[test]
    fn parses_verify_recursive() {
        let h = Harness::parse_from(["bin", "verify", "roms", "-R"]);
        let CsoCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert!(c.recursive);
    }

    #[test]
    fn parses_decompress_recursive() {
        let h = Harness::parse_from(["bin", "decompress", "roms", "-R"]);
        let CsoCommands::Decompress(c) = h.cmd else {
            panic!("expected Decompress");
        };
        assert!(c.recursive);
    }

    #[test]
    fn decompress_output_flag_overrides_positional() {
        let h = Harness::parse_from(["bin", "decompress", "game.cso", "-o", "out.iso"]);
        let CsoCommands::Decompress(c) = h.cmd else {
            panic!("expected Decompress");
        };
        assert_eq!(c.output, None);
        assert_eq!(c.output_flag, Some(PathBuf::from("out.iso")));
    }

    #[test]
    fn decompress_output_flag_conflicts_with_positional() {
        let result =
            Harness::try_parse_from(["bin", "decompress", "game.cso", "pos.iso", "-o", "flag.iso"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_compress_output_dir() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--output-dir", "out"]);
        let CsoCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.out.output_dir, Some(PathBuf::from("out")));
        assert_eq!(c.output, None);
    }

    #[test]
    fn parses_compress_report_flag() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--report", "out.json"]);
        let CsoCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.batch.report, Some(PathBuf::from("out.json")));
    }

    #[test]
    fn parses_decompress_report_flag() {
        let h = Harness::parse_from(["bin", "decompress", "game.cso", "--report", "out.csv"]);
        let CsoCommands::Decompress(c) = h.cmd else {
            panic!("expected Decompress");
        };
        assert_eq!(c.batch.report, Some(PathBuf::from("out.csv")));
    }

    #[test]
    fn decompress_output_dir_conflicts_with_positional() {
        let result = Harness::try_parse_from([
            "bin",
            "decompress",
            "game.cso",
            "pos.iso",
            "--output-dir",
            "out",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn on_conflict_absent_is_none() {
        let h = Harness::parse_from(["bin", "compress", "game.iso"]);
        let CsoCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.conflict.on_conflict.is_none());
    }

    #[test]
    fn parses_to_chd_defaults() {
        let h = Harness::parse_from(["bin", "to-chd", "game.cso"]);
        let CsoCommands::ToChd(c) = h.cmd else {
            panic!("expected ToChd");
        };
        assert_eq!(c.input, PathBuf::from("game.cso"));
        assert_eq!(c.output, None);
        assert!(!c.dvd && !c.cd && !c.conflict.force && !c.recursive);
        assert_eq!(c.hunk_size, None);
        assert_eq!(c.codecs, None);
        assert_eq!(c.level, None);
    }

    #[test]
    fn parses_to_chd_dvd_flags() {
        let h = Harness::parse_from([
            "bin",
            "to-chd",
            "game.zso",
            "game.chd",
            "--dvd",
            "--hunk-size",
            "2048",
            "-R",
        ]);
        let CsoCommands::ToChd(c) = h.cmd else {
            panic!("expected ToChd");
        };
        assert_eq!(c.output, Some(PathBuf::from("game.chd")));
        assert!(c.dvd && c.recursive);
        assert_eq!(c.hunk_size, Some(2048));
    }

    #[test]
    fn to_chd_parses_codecs_and_level() {
        let h = Harness::parse_from([
            "bin",
            "to-chd",
            "game.cso",
            "--codecs",
            "zstd,lzma,zlib,flac",
            "--level",
            "9",
        ]);
        let CsoCommands::ToChd(c) = h.cmd else {
            panic!("expected ToChd");
        };
        assert_eq!(
            c.codecs,
            Some(vec![
                ChdCodec::Zstd,
                ChdCodec::Lzma,
                ChdCodec::Zlib,
                ChdCodec::Flac
            ])
        );
        assert_eq!(c.level, Some(9));
    }

    #[test]
    fn to_chd_rejects_unknown_codec_name() {
        let result = Harness::try_parse_from(["bin", "to-chd", "game.cso", "--codecs", "bogus"]);
        assert!(result.is_err());
    }

    #[test]
    fn to_chd_rejects_too_many_codecs() {
        let result = Harness::try_parse_from([
            "bin",
            "to-chd",
            "game.cso",
            "--codecs",
            "zlib,zstd,lzma,huff,flac",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn to_chd_rejects_level_out_of_range() {
        let result = Harness::try_parse_from(["bin", "to-chd", "game.cso", "--level", "0"]);
        assert!(result.is_err());
    }

    #[test]
    fn to_chd_cd_codec_with_dvd_rejected_by_lib_validation() {
        let h = Harness::parse_from([
            "bin",
            "to-chd",
            "game.cso",
            "--dvd",
            "--codecs",
            "cdlz,cdzl,cdfl",
        ]);
        let CsoCommands::ToChd(c) = h.cmd else {
            panic!("expected ToChd");
        };
        let codecs = c.codecs.expect("codecs parsed");
        assert!(rom_converto_lib::disc::chd::validate_codecs(&codecs, true).is_err());
    }

    #[test]
    fn to_chd_cd_conflicts_with_dvd() {
        let result = Harness::try_parse_from(["bin", "to-chd", "game.cso", "--cd", "--dvd"]);
        assert!(result.is_err());
    }

    #[test]
    fn to_chd_output_dir_conflicts_with_positional() {
        let result = Harness::try_parse_from([
            "bin",
            "to-chd",
            "game.cso",
            "pos.chd",
            "--output-dir",
            "out",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn to_chd_max_depth_requires_recursive() {
        let result = Harness::try_parse_from(["bin", "to-chd", "dir", "--max-depth", "2"]);
        assert!(result.is_err());
    }
}
