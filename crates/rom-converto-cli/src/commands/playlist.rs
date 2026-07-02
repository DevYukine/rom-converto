use crate::commands::ConflictPolicyArg;
use clap::Parser;
use std::path::PathBuf;

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

    /// What to do when an output already exists: error, overwrite, skip, or rename to a numbered sibling
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
        assert_eq!(c.on_conflict, None);
        assert!(!c.force);
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
        assert!(c.force);
    }

    #[test]
    fn force_conflicts_with_on_conflict() {
        let result =
            Harness::try_parse_from(["bin", "playlist", "roms", "-f", "--on-conflict", "skip"]);
        assert!(result.is_err());
    }
}
