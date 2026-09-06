use crate::commands::ConflictPolicyArg;
use crate::commands::cso::CsoFormatArg;
use clap::{Parser, Subcommand};
use rom_converto_lib::util::CancelToken;
use std::path::PathBuf;

use crate::commands::support::{DispatchCtx, require_dir};
use crate::util::{
    WriteDecision, ensure_input_exists, file_len, log_skipped, resolve_output, resolve_policy,
};
use crate::{batch, dry_run};
use anyhow::Result;
use rom_converto_lib::cso::CsoFormat;
use rom_converto_lib::cue::merge::merge_bin;
use rom_converto_lib::cue::to_iso::cue_to_iso;
use rom_converto_lib::pipeline::cue_to_cso;
use std::path::Path;

/// Commands for CUE/BIN disc images
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum CueCommands {
    Merge(MergeCommand),
    ToIso(ToIsoCommand),
    ToCso(ToCsoCommand),
}

/// Merge a multi-bin .cue disc image into a single .bin and .cue pair
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  Merge tracks: rom-converto cue merge game.cue merged.cue\n"
)]
pub struct MergeCommand {
    /// Input .cue file referencing multiple .bin files
    #[arg(value_name = "INPUT_CUE")]
    pub input_cue: PathBuf,

    /// Output .cue file path, the merged .bin is named after it
    #[arg(value_name = "OUTPUT_CUE")]
    pub output_cue: PathBuf,

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

/// Convert a .cue/.bin disc image's data track to a plain .iso
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Convert a .cue/.bin disc image's data track to a plain .iso\n\nExtracts the first track (which must be a MODE1/MODE2 data track) to 2048-byte ISO sectors. Any audio tracks are skipped.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto cue to-iso game.cue\n  Explicit output: rom-converto cue to-iso game.cue game.iso\n  Whole folder:    rom-converto cue to-iso -R ./roms --output-dir ./iso\n"
)]
pub struct ToIsoCommand {
    /// Input .cue file, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output .iso path, defaults to the input with extension replaced by .iso
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Write output into this directory using the derived filename. Created if missing. Works with --recursive
    #[arg(long = "output-dir", value_name = "DIR", conflicts_with = "output")]
    pub output_dir: Option<PathBuf>,

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

    /// Convert every .cue file found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,
}

/// Convert a .cue/.bin disc image's data track straight to a .cso or .zso
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Convert a .cue/.bin disc image's data track straight to a .cso or .zso\n\nExtracts the data track to a temporary ISO, then compresses it, always removing the temporary ISO afterward.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto cue to-cso game.cue\n  Explicit output: rom-converto cue to-cso game.cue game.cso --format cso\n  Whole folder:    rom-converto cue to-cso -R ./roms --output-dir ./cso\n"
)]
pub struct ToCsoCommand {
    /// Input .cue file, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output path, defaults to the input with the format's extension
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Write output into this directory using the derived filename. Created if missing. Works with --recursive
    #[arg(long = "output-dir", value_name = "DIR", conflicts_with = "output")]
    pub output_dir: Option<PathBuf>,

    /// Output container format
    #[arg(long, value_enum, default_value_t = CsoFormatArg::Zso)]
    pub format: CsoFormatArg,

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

    /// Convert every .cue file found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,
}

/// Runs one `cue` subcommand.
pub async fn run(command: CueCommands, ctx: DispatchCtx<'_>) -> Result<()> {
    let DispatchCtx {
        progress,
        total_progress,
        dry_run,
        skip_space_check,
        cancel,
        cache,
        ..
    } = ctx;
    match command {
        CueCommands::Merge(cmd) => {
            ensure_input_exists(&cmd.input_cue)?;
            let policy = resolve_policy(
                Some(cmd.on_conflict),
                cmd.force,
                rom_converto_lib::util::ConflictPolicy::Error,
            );
            let decision = resolve_output(&cmd.output_cue, policy)?;
            if dry_run {
                let bin = cmd.output_cue.with_extension("bin");
                let note = format!("+ {}", bin.display());
                return dry_run::single(
                    "merge",
                    &cmd.input_cue,
                    &cmd.output_cue,
                    &decision,
                    Some(&note),
                    None,
                    None,
                );
            }
            let output_cue = match decision {
                WriteDecision::Skip => {
                    log_skipped(&cmd.output_cue);
                    return Ok(());
                }
                WriteDecision::Write(p) => p,
            };
            if !skip_space_check {
                let check_dir = output_cue.parent().unwrap_or_else(|| Path::new("."));
                let required = rom_converto_lib::cue::referenced_files_size(&cmd.input_cue)
                    .await
                    .unwrap_or_else(|_| file_len(&cmd.input_cue));
                batch::space_preflight_for_size(required, check_dir)?;
            }
            merge_bin(
                &progress,
                cmd.input_cue,
                output_cue,
                true,
                CancelToken::new(),
            )
            .await?
        }
        CueCommands::ToIso(cmd) => {
            if cmd.recursive {
                require_dir(&cmd.input)?;
                let policy = resolve_policy(
                    Some(cmd.on_conflict),
                    cmd.force,
                    rom_converto_lib::util::ConflictPolicy::Error,
                );
                let run = batch::BatchRun {
                    progress: &progress,
                    total_progress: &total_progress,
                    input_dir: &cmd.input,
                    policy,
                    output_dir: cmd.output_dir.as_deref(),
                    output_template: None,
                    max_depth: cmd.max_depth,
                    dry_run,
                    skip_space_check,
                    report_path: None,
                    cancel: &cancel,
                };
                batch::cue_to_iso(&run).await?
            } else {
                ensure_input_exists(&cmd.input)?;
                let output = match cmd.output.clone() {
                    Some(p) => p,
                    None => {
                        if !dry_run && let Some(dir) = cmd.output_dir.as_deref() {
                            std::fs::create_dir_all(dir)?;
                        }
                        rom_converto_lib::util::place_in_dir(
                            &cmd.input.with_extension("iso"),
                            cmd.output_dir.as_deref(),
                        )
                    }
                };
                let policy = resolve_policy(
                    Some(cmd.on_conflict),
                    cmd.force,
                    rom_converto_lib::util::ConflictPolicy::Error,
                );
                let decision = resolve_output(&output, policy)?;
                if dry_run {
                    return dry_run::single(
                        "to-iso", &cmd.input, &output, &decision, None, None, None,
                    );
                }
                let output = match decision {
                    WriteDecision::Skip => {
                        log_skipped(&output);
                        return Ok(());
                    }
                    WriteDecision::Write(p) => p,
                };
                if !skip_space_check {
                    let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                    let required = rom_converto_lib::cue::referenced_files_size(&cmd.input)
                        .await
                        .unwrap_or_else(|_| file_len(&cmd.input));
                    batch::space_preflight_for_size(required, check_dir)?;
                }
                cue_to_iso(&progress, cmd.input, output, true).await?
            }
        }
        CueCommands::ToCso(cmd) => {
            let format = match cmd.format {
                CsoFormatArg::Cso => CsoFormat::Cso,
                CsoFormatArg::Zso => CsoFormat::Zso,
            };
            if cmd.recursive {
                require_dir(&cmd.input)?;
                let policy = resolve_policy(
                    Some(cmd.on_conflict),
                    cmd.force,
                    rom_converto_lib::util::ConflictPolicy::Error,
                );
                let run = batch::BatchRun {
                    progress: &progress,
                    total_progress: &total_progress,
                    input_dir: &cmd.input,
                    policy,
                    output_dir: cmd.output_dir.as_deref(),
                    output_template: None,
                    max_depth: cmd.max_depth,
                    dry_run,
                    skip_space_check,
                    report_path: None,
                    cancel: &cancel,
                };
                batch::cue_to_cso(&run, format, cache).await?
            } else {
                ensure_input_exists(&cmd.input)?;
                let output = match cmd.output.clone() {
                    Some(p) => p,
                    None => {
                        if !dry_run && let Some(dir) = cmd.output_dir.as_deref() {
                            std::fs::create_dir_all(dir)?;
                        }
                        rom_converto_lib::util::place_in_dir(
                            &cmd.input.with_extension(format.extension()),
                            cmd.output_dir.as_deref(),
                        )
                    }
                };
                let policy = resolve_policy(
                    Some(cmd.on_conflict),
                    cmd.force,
                    rom_converto_lib::util::ConflictPolicy::Error,
                );
                let decision = resolve_output(&output, policy)?;
                if dry_run {
                    return dry_run::single(
                        "to-cso",
                        &cmd.input,
                        &output,
                        &decision,
                        Some(format.name()),
                        None,
                        None,
                    );
                }
                let output = match decision {
                    WriteDecision::Skip => {
                        log_skipped(&output);
                        return Ok(());
                    }
                    WriteDecision::Write(p) => p,
                };
                if !skip_space_check {
                    let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                    let required = rom_converto_lib::cue::referenced_files_size(&cmd.input)
                        .await
                        .unwrap_or_else(|_| file_len(&cmd.input));
                    batch::space_preflight_for_size(required, check_dir)?;
                }
                cue_to_cso(&progress, cmd.input, output, format, true).await?
            }
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
        cmd: CueCommands,
    }

    #[test]
    fn parses_on_conflict_skip() {
        let h = Harness::parse_from(["bin", "merge", "in.cue", "out.cue", "--on-conflict", "skip"]);
        let CueCommands::Merge(c) = h.cmd else {
            panic!("expected Merge");
        };
        assert_eq!(c.on_conflict, ConflictPolicyArg::Skip);
    }

    #[test]
    fn parses_on_conflict_rename() {
        let h = Harness::parse_from([
            "bin",
            "merge",
            "in.cue",
            "out.cue",
            "--on-conflict",
            "rename",
        ]);
        let CueCommands::Merge(c) = h.cmd else {
            panic!("expected Merge");
        };
        assert_eq!(c.on_conflict, ConflictPolicyArg::Rename);
    }

    #[test]
    fn force_still_accepted() {
        let h = Harness::parse_from(["bin", "merge", "in.cue", "out.cue", "-f"]);
        let CueCommands::Merge(c) = h.cmd else {
            panic!("expected Merge");
        };
        assert!(c.force);
        assert_eq!(c.on_conflict, ConflictPolicyArg::Error);
    }

    #[test]
    fn force_and_on_conflict_conflict() {
        let result = Harness::try_parse_from([
            "bin",
            "merge",
            "in.cue",
            "out.cue",
            "-f",
            "--on-conflict",
            "skip",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn defaults_on_conflict_to_error() {
        let h = Harness::parse_from(["bin", "merge", "in.cue", "out.cue"]);
        let CueCommands::Merge(c) = h.cmd else {
            panic!("expected Merge");
        };
        assert_eq!(c.on_conflict, ConflictPolicyArg::Error);
    }

    #[test]
    fn parses_to_iso_defaults() {
        let h = Harness::parse_from(["bin", "to-iso", "game.cue"]);
        let CueCommands::ToIso(c) = h.cmd else {
            panic!("expected ToIso");
        };
        assert_eq!(c.input, PathBuf::from("game.cue"));
        assert_eq!(c.output, None);
        assert_eq!(c.on_conflict, ConflictPolicyArg::Error);
        assert!(!c.force && !c.recursive);
    }

    #[test]
    fn parses_to_iso_recursive_flag() {
        let h = Harness::parse_from(["bin", "to-iso", "-R", "roms"]);
        let CueCommands::ToIso(c) = h.cmd else {
            panic!("expected ToIso");
        };
        assert!(c.recursive);
        assert_eq!(c.input, PathBuf::from("roms"));
    }

    #[test]
    fn parses_to_iso_max_depth_and_output_dir() {
        let h = Harness::parse_from([
            "bin",
            "to-iso",
            "-R",
            "--max-depth",
            "2",
            "roms",
            "--output-dir",
            "out",
        ]);
        let CueCommands::ToIso(c) = h.cmd else {
            panic!("expected ToIso");
        };
        assert_eq!(c.max_depth, Some(2));
        assert_eq!(c.output_dir, Some(PathBuf::from("out")));
        assert_eq!(c.output, None);
    }

    #[test]
    fn to_iso_max_depth_requires_recursive() {
        let result = Harness::try_parse_from(["bin", "to-iso", "--max-depth", "2", "roms"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_to_cso_defaults_to_zso() {
        let h = Harness::parse_from(["bin", "to-cso", "game.cue"]);
        let CueCommands::ToCso(c) = h.cmd else {
            panic!("expected ToCso");
        };
        assert_eq!(c.format, CsoFormatArg::Zso);
        assert_eq!(c.output, None);
    }

    #[test]
    fn parses_to_cso_with_format_and_output() {
        let h = Harness::parse_from(["bin", "to-cso", "game.cue", "game.cso", "--format", "cso"]);
        let CueCommands::ToCso(c) = h.cmd else {
            panic!("expected ToCso");
        };
        assert_eq!(c.format, CsoFormatArg::Cso);
        assert_eq!(c.output, Some(PathBuf::from("game.cso")));
    }

    #[test]
    fn parses_to_cso_recursive_max_depth_and_output_dir() {
        let h = Harness::parse_from([
            "bin",
            "to-cso",
            "-R",
            "--max-depth",
            "3",
            "roms",
            "--output-dir",
            "out",
        ]);
        let CueCommands::ToCso(c) = h.cmd else {
            panic!("expected ToCso");
        };
        assert!(c.recursive);
        assert_eq!(c.max_depth, Some(3));
        assert_eq!(c.output_dir, Some(PathBuf::from("out")));
        assert_eq!(c.output, None);
    }

    #[test]
    fn to_cso_max_depth_requires_recursive() {
        let result = Harness::try_parse_from(["bin", "to-cso", "--max-depth", "2", "roms"]);
        assert!(result.is_err());
    }
}
