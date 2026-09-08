use crate::commands::info_command::InfoCommand;
use crate::commands::{ConflictArgs, ConflictPolicyArg};
use clap::{Parser, Subcommand};
use rom_converto_lib::util::CancelToken;
use std::path::PathBuf;

use crate::commands::support::{
    ALL_IMAGE_EXTS, DispatchCtx, require_dir, require_info_input, save_wup_image,
};
use crate::util::{ensure_input_exists, resolve_policy};
use crate::{batch, config, info_print};
use anyhow::Result;
use rom_converto_lib::nintendo::wup::verify_wup_async;
use rom_converto_lib::runner::models::{RunOptions, WupTitleInputOption};
use rom_converto_lib::util::ConflictPolicy;

/// Commands specific to Wii U (WUP) formats
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum WupCommands {
    Compress(CompressWupCommand),
    Decrypt(DecryptWupCommand),
    Verify(VerifyWupCommand),
    Info(InfoCommand),
}

/// Verify Wii U content integrity by recomputing each content's SHA-1 against the TMD content hashes
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Verify Wii U content integrity by recomputing each content's SHA-1 against the TMD content hashes\n\n\
For NUS directories and .wud / .wux discs, every raw-mode content is decrypted and its SHA-1 compared against the TMD hash. Hashed-mode content is reported as skipped (its TMD hash covers the hash tree, not the content). .wua and loadiine inputs are already decrypted and carry no TMD, so they get a structural readability check only.\n\n\
Disc images resolve their 16-byte master key automatically: --key, a sibling <input>.key or game.key, the built-in key database matched by filename, then an automatic probe of the built-in database.",
    after_long_help = "EXAMPLES:\n  NUS directory: rom-converto wup verify ./title_dir\n  Disc with key: rom-converto wup verify --key game.key game.wud\n  Whole folder:  rom-converto wup verify -R ./titles\n"
)]
pub struct VerifyWupCommand {
    /// Disc master key file (.wud / .wux only). Resolved automatically if omitted: sibling `<input>.key` or `game.key`, then the built-in key database by filename, then a probe of the built-in database
    #[arg(long = "key", value_name = "KEYFILE")]
    pub key: Option<PathBuf>,

    /// Input: NUS directory, loadiine directory, .wua, or .wud / .wux disc, or a parent directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Verify every .wud / .wux disc in the INPUT directory and its subdirectories; NUS title directories are detected among the immediate children of INPUT only
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,
}

/// Decrypt a NUS-format Wii U title directory into a loadiine-style `meta/code/content` tree that Cemu can load directly
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  NUS directory: rom-converto wup decrypt -o ./title_out ./title_nus\n"
)]
pub struct DecryptWupCommand {
    /// Output directory. Created if missing
    #[arg(short, long, value_name = "OUTPUT")]
    pub output: PathBuf,

    /// Input NUS directory (canonical `title.tmd` + `.app` or community `tmd.<N>` + numbered content files)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// What to do when an output already exists: error, overwrite, skip, or rename to a numbered sibling
    #[arg(long = "on-conflict", value_enum, default_value_t = ConflictPolicyArg::Error)]
    pub on_conflict: ConflictPolicyArg,

    /// Alias for --on-conflict overwrite
    #[arg(
        long,
        short = 'f',
        default_value_t = false,
        conflicts_with = "on_conflict"
    )]
    pub force: bool,
}

/// Compress one or more Wii U titles into a Cemu-compatible .wua archive
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Compress one or more Wii U titles into a Cemu-compatible .wua archive\n\n\
                  Each input is auto-detected as one of:\n  \
                  - loadiine directory: already-decrypted `meta/`, `code/`, `content/`\n  \
                  - NUS directory: `title.tmd`, `title.tik`, `*.app` (auto-decrypted)\n  \
                  - disc image: `.wud` or `.wux` file (requires per-disc key)\n\n\
                  Disc images resolve their 16-byte master key automatically. Keys are\n  \
                  resolved in order:\n  \
                  1. `--key` flag, paired positionally with disc inputs\n  \
                  2. sibling `<input>.key` file, or `game.key` in the same directory\n  \
                  3. built-in key database, matched by filename\n  \
                  4. built-in key database, probed against the disc\n\n\
                  Multiple titles (base + update + DLC) can be bundled into a single\n\
                  archive by passing each input as a separate positional argument.",
    after_long_help = "EXAMPLES:\n  Single title:    rom-converto wup compress -o game.wua ./title_base\n  Disc with key:   rom-converto wup compress -o game.wua --key game.key game.wud\n  Bundle titles:   rom-converto wup compress -o game.wua ./title_base ./title_update ./title_dlc\n"
)]
pub struct CompressWupCommand {
    /// Output .wua file path
    #[arg(short, long, value_name = "OUTPUT")]
    pub output: PathBuf,

    /// Zstd compression level (0 = Cemu default of 6, 22 = max ratio). Higher levels produce smaller output at the cost of compression time
    #[arg(
        short = 'l',
        long = "level",
        value_name = "LEVEL",
        value_parser = clap::value_parser!(i32).range(0..=22)
    )]
    pub level: Option<i32>,

    /// Disc master key file path(s). Applies only to disc image inputs. When supplied multiple times, keys are paired with disc inputs in the order they appear on the command line; the Nth `--key` applies to the Nth disc input. Non-disc inputs silently skip past their positional slot. Omit entirely to let the loader resolve automatically: sibling `<input>.key` or `game.key`, then the built-in key database by filename, then a probe of the built-in database
    #[arg(long = "key", value_name = "KEYFILE")]
    pub key: Vec<PathBuf>,

    /// One or more title inputs to bundle into the archive. Each is auto-detected as a loadiine directory, a NUS directory, or a disc image file
    #[arg(required = true, num_args = 1.., value_name = "INPUT")]
    pub inputs: Vec<PathBuf>,

    #[command(flatten)]
    pub conflict: ConflictArgs,
}

/// Runs one `wup` subcommand.
pub async fn run(command: WupCommands, ctx: DispatchCtx<'_>) -> Result<()> {
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
        WupCommands::Compress(cmd) => {
            let eff = &effective.wup;
            let mut options = RunOptions::from(batch::Common {
                recursive: false,
                output_dir: None,
                output_template: None,
                max_depth: None,
                report: None,
                policy: resolve_policy(
                    cmd.conflict.on_conflict,
                    cmd.conflict.force,
                    config::policy_fallback(&eff.on_conflict)?,
                ),
                skip_space_check,
            });
            options.level = cmd.level.or(eff.level);
            // Pair --key values with disc inputs in positional order.
            // Non-disc inputs skip past their key slot.
            let mut key_iter = cmd.key.into_iter();
            options.inputs = Some(
                cmd.inputs
                    .iter()
                    .map(|path| {
                        let is_disc = path
                            .extension()
                            .and_then(|s| s.to_str())
                            .map(|s| s.eq_ignore_ascii_case("wud") || s.eq_ignore_ascii_case("wux"))
                            .unwrap_or(false)
                            && path.is_file();
                        WupTitleInputOption::Object {
                            path: path.clone(),
                            format: None,
                            key: is_disc.then(|| key_iter.next()).flatten(),
                            key_path: None,
                        }
                    })
                    .collect(),
            );
            let input = cmd
                .inputs
                .first()
                .cloned()
                .unwrap_or_else(|| cmd.output.clone());
            batch::run(&run, "wup.compress", input, Some(cmd.output), options).await?;
        }
        WupCommands::Decrypt(cmd) => {
            ensure_input_exists(&cmd.input)?;
            let options = RunOptions::from(batch::Common {
                recursive: false,
                output_dir: None,
                output_template: None,
                max_depth: None,
                report: None,
                policy: resolve_policy(Some(cmd.on_conflict), cmd.force, ConflictPolicy::Error),
                skip_space_check,
            });
            batch::run(&run, "wup.decrypt", cmd.input, Some(cmd.output), options).await?;
        }
        WupCommands::Verify(cmd) => {
            if cmd.recursive {
                require_dir(&cmd.input)?;
                batch::wup_verify(&progress, &total_progress, &cmd.input, cmd.max_depth).await?
            } else {
                ensure_input_exists(&cmd.input)?;
                let resolved = rom_converto_lib::util::resolve_input(&cmd.input, &["wud", "wux"])?;
                let result = verify_wup_async(
                    resolved.path().to_path_buf(),
                    cmd.key,
                    &progress,
                    CancelToken::new(),
                )
                .await?;
                log::info!("Source kind: {}", result.kind);
                log::info!("Overall: {}", if result.ok { "OK" } else { "FAIL" });
                for t in &result.titles {
                    log::info!(
                        "  {}: {} (verified: {}, mismatched: {}, skipped: {})",
                        t.title_id_hex,
                        if t.ok { "OK" } else { "FAIL" },
                        t.verified_content,
                        t.mismatched_content,
                        t.skipped_content
                    );
                }
                if !result.ok {
                    anyhow::bail!("verification failed");
                }
            }
        }
        WupCommands::Info(cmd) => {
            let input = require_info_input(&cmd.input)?;
            ensure_input_exists(input)?;
            let resolved = rom_converto_lib::util::resolve_input(input, ALL_IMAGE_EXTS)?;
            let info = rom_converto_lib::nintendo::wup::info::read_info(
                resolved.path(),
                cmd.keys.as_deref(),
            )?;
            if let Some(dir) = &cmd.save_icon {
                save_wup_image(&info, dir)?;
            }
            info_print::print(&rom_converto_lib::info::InfoResult::Wup(info), cmd.json)?;
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
        cmd: WupCommands,
    }

    #[test]
    fn parses_single_disc_with_key() {
        let h = Harness::parse_from([
            "bin", "compress", "-o", "out.wua", "--key", "game.key", "game.wud",
        ]);
        let WupCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.output, PathBuf::from("out.wua"));
        assert_eq!(c.key, vec![PathBuf::from("game.key")]);
        assert_eq!(c.inputs, vec![PathBuf::from("game.wud")]);
    }

    #[test]
    fn parses_mixed_inputs_without_keys() {
        let h = Harness::parse_from([
            "bin",
            "compress",
            "-o",
            "out.wua",
            "title_base/",
            "title_update/",
        ]);
        let WupCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.inputs.len(), 2);
        assert!(c.key.is_empty());
    }

    #[test]
    fn parses_two_disc_inputs_with_two_keys() {
        let h = Harness::parse_from([
            "bin", "compress", "-o", "out.wua", "--key", "a.key", "--key", "b.key", "a.wud",
            "b.wux",
        ]);
        let WupCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.key, vec![PathBuf::from("a.key"), PathBuf::from("b.key")]);
        assert_eq!(c.inputs.len(), 2);
    }

    #[test]
    fn parses_decrypt() {
        let h = Harness::parse_from(["bin", "decrypt", "-o", "out_dir", "input_dir"]);
        let WupCommands::Decrypt(c) = h.cmd else {
            panic!("expected Decrypt");
        };
        assert_eq!(c.output, PathBuf::from("out_dir"));
        assert_eq!(c.input, PathBuf::from("input_dir"));
    }

    #[test]
    fn rejects_missing_input() {
        let result = Harness::try_parse_from(["bin", "compress", "-o", "out.wua"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_verify_with_key() {
        let h = Harness::parse_from(["bin", "verify", "--key", "game.key", "game.wud"]);
        let WupCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert_eq!(c.key, Some(PathBuf::from("game.key")));
        assert_eq!(c.input, PathBuf::from("game.wud"));
    }

    #[test]
    fn parses_verify_without_key() {
        let h = Harness::parse_from(["bin", "verify", "title_dir"]);
        let WupCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert!(c.key.is_none());
        assert_eq!(c.input, PathBuf::from("title_dir"));
    }

    #[test]
    fn parses_verify_recursive() {
        let h = Harness::parse_from(["bin", "verify", "-R", "roms"]);
        let WupCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert!(c.recursive);
        assert_eq!(c.input, PathBuf::from("roms"));
    }

    #[test]
    fn parses_compress_force() {
        let h = Harness::parse_from(["bin", "compress", "-o", "out.wua", "-f", "title_dir/"]);
        let WupCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.conflict.force);
    }

    #[test]
    fn compress_on_conflict_absent_is_none() {
        let h = Harness::parse_from(["bin", "compress", "-o", "out.wua", "title_dir/"]);
        let WupCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.conflict.on_conflict.is_none());
    }
}
