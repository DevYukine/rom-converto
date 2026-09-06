use crate::commands::ConflictArgs;
use clap::Parser;
use std::path::PathBuf;

use crate::commands::support::{DispatchCtx, require_dir};
use crate::dry_run;
use crate::util::{WriteDecision, resolve_output, resolve_policy};
use anyhow::Result;
use rom_converto_lib::playlist::{PlaylistMode, PlaylistOptions, plan_playlists};
use rom_converto_lib::util::{CancelToken, Tally, mixed_playlist_extensions};
use std::path::Path;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum PlaylistModeArg {
    Multiple,
    Always,
}

/// Scan a directory for multi-disc game sets and write one .m3u per game
///
/// Grouping is filename-based only, matching the Redump "(Disc N)" /
/// "(Disc N of M)" and TOSEC "Disc N of M" conventions. No DAT lookup is done.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  Whole folder:    rom-converto playlist ./roms\n  Single-disc too: rom-converto playlist ./roms --playlist-mode always\n  Custom exts:     rom-converto playlist ./roms --ext cue,chd\n"
)]
pub struct PlaylistCommand {
    /// Directory to scan for disc image files
    #[arg(value_name = "DIR")]
    pub input: PathBuf,

    /// Write .m3u files into this directory instead of beside the disc files
    #[arg(long = "output-dir", value_name = "DIR")]
    pub output_dir: Option<PathBuf>,

    /// Whether to also write an .m3u for single-disc games
    #[arg(long = "playlist-mode", value_enum, default_value_t = PlaylistModeArg::Multiple)]
    pub playlist_mode: PlaylistModeArg,

    /// Comma-separated disc image extensions to scan
    #[arg(
        long = "ext",
        value_name = "EXTS",
        default_value = "cue,chd,iso,cso,zso"
    )]
    pub extensions: String,

    /// Maximum directory depth. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N")]
    pub max_depth: Option<usize>,

    #[command(flatten)]
    pub conflict: ConflictArgs,
}

/// Runs one `playlist` subcommand.
pub async fn run(cmd: PlaylistCommand, ctx: DispatchCtx<'_>) -> Result<()> {
    let DispatchCtx { dry_run, .. } = ctx;
    require_dir(&cmd.input)?;

    let exts: Vec<String> = cmd
        .extensions
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let ext_refs: Vec<&str> = exts.iter().map(String::as_str).collect();

    let mode = match cmd.playlist_mode {
        PlaylistModeArg::Multiple => PlaylistMode::Multiple,
        PlaylistModeArg::Always => PlaylistMode::Always,
    };

    let plans = plan_playlists(
        &PlaylistOptions {
            scan_dir: &cmd.input,
            output_dir: cmd.output_dir.as_deref(),
            extensions: &ext_refs,
            mode,
            max_depth: cmd.max_depth,
        },
        &CancelToken::new(),
    )?;

    // An .m3u has no integrity check, so overwrite-invalid degrades to skip.
    let policy = resolve_policy(
        cmd.conflict.on_conflict,
        cmd.conflict.force,
        rom_converto_lib::util::ConflictPolicy::Error,
    );

    if !dry_run && let Some(dir) = cmd.output_dir.as_deref() {
        std::fs::create_dir_all(dir)?;
    }

    let mut tally = Tally::new();
    let started = Instant::now();

    for plan in &plans {
        if plan.has_duplicate_numbers {
            log::warn!(
                "Duplicate disc numbers in set {}, including all entries",
                plan.base_title
            );
        }
        let entry_exts = plan
            .contents
            .lines()
            .filter_map(|line| Path::new(line).extension())
            .filter_map(|ext| ext.to_str());
        if let Some(mixed) = mixed_playlist_extensions(entry_exts) {
            log::warn!(
                "Mixed track formats ({mixed}) in set {}; emulators expect every disc \
                     in a playlist to use the same format",
                plan.base_title
            );
        }
        let decision = resolve_output(&plan.m3u_path, policy)?;
        if dry_run {
            dry_run::log_plan("write", &cmd.input, &plan.m3u_path, &decision, None, None);
            for line in plan.contents.lines() {
                log::info!("    {line}");
            }
            dry_run::record(&mut tally, &cmd.input, &decision);
            continue;
        }
        match decision {
            WriteDecision::Write(path) => {
                std::fs::write(&path, &plan.contents)?;
                log::info!("Wrote {} ({} discs)", path.display(), plan.disc_count);
                tally.record_ok(0, 0, std::time::Duration::ZERO);
            }
            WriteDecision::Skip => {
                log::info!("Skipped existing {}", plan.m3u_path.display());
                tally.record_skipped();
            }
        }
    }

    if dry_run {
        dry_run::finish(&tally, &[], None)?;
    } else {
        log::info!("{}", Tally::count_summary(tally.count(), started.elapsed()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Parser, Debug)]
    struct Harness {
        #[command(subcommand)]
        cmd: Wrapper,
    }

    #[derive(clap::Subcommand, Debug)]
    enum Wrapper {
        Playlist(PlaylistCommand),
    }

    fn parse(args: &[&str]) -> PlaylistCommand {
        let h = Harness::parse_from(args);
        let Wrapper::Playlist(c) = h.cmd;
        c
    }

    #[test]
    fn defaults() {
        let c = parse(&["bin", "playlist", "roms"]);
        assert_eq!(c.playlist_mode, PlaylistModeArg::Multiple);
        assert_eq!(c.extensions, "cue,chd,iso,cso,zso");
        assert_eq!(c.max_depth, None);
        assert_eq!(c.conflict.on_conflict, None);
        assert!(!c.conflict.force);
        assert_eq!(c.output_dir, None);
    }

    #[test]
    fn parses_mode_always() {
        let c = parse(&["bin", "playlist", "roms", "--playlist-mode", "always"]);
        assert_eq!(c.playlist_mode, PlaylistModeArg::Always);
    }

    #[test]
    fn parses_custom_ext_and_depth() {
        let c = parse(&[
            "bin",
            "playlist",
            "roms",
            "--ext",
            "cue,chd",
            "--max-depth",
            "2",
        ]);
        assert_eq!(c.extensions, "cue,chd");
        assert_eq!(c.max_depth, Some(2));
    }

    #[test]
    fn parses_output_dir_and_force() {
        let c = parse(&["bin", "playlist", "roms", "--output-dir", "out", "-f"]);
        assert_eq!(c.output_dir, Some(PathBuf::from("out")));
        assert!(c.conflict.force);
    }

    #[test]
    fn force_conflicts_with_on_conflict() {
        let result =
            Harness::try_parse_from(["bin", "playlist", "roms", "-f", "--on-conflict", "skip"]);
        assert!(result.is_err());
    }
}
