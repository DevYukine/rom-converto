use clap::{ArgGroup, Parser};
use std::path::PathBuf;

use crate::commands::support::{
    ALL_IMAGE_EXTS, DispatchCtx, collect_info_batch_files, read_info_paths_file,
    require_info_input, run_info_batch, save_info_icon,
};
use crate::info_print;
use crate::util::ensure_input_exists;
use anyhow::Result;

/// Print metadata about a ROM or disc image: title, region, hashes and embedded artwork
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    group(ArgGroup::new("input_source").required(true).args(["input", "paths_file"])),
    after_long_help = "EXAMPLES:\n  Single file:   rom-converto info game.cia\n  Save icon:     rom-converto info game.cia --save-icon ./icons\n  As JSON:       rom-converto info game.cia --json\n  Batch a dir:   rom-converto info --batch ./roms --json\n  From a list:   rom-converto info --paths-file games.txt --json\n"
)]
pub struct InfoCommand {
    /// File or directory to inspect
    #[arg(value_name = "INPUT")]
    pub input: Option<PathBuf>,

    /// File with one input path per line; blank lines and # comments skipped
    #[arg(long, value_name = "FILE")]
    pub paths_file: Option<PathBuf>,

    /// Treat a directory INPUT as a batch scan instead of a Wii U title directory
    #[arg(long, default_value_t = false)]
    pub batch: bool,

    /// Emit JSON instead of pretty text
    #[arg(long, default_value_t = false)]
    pub json: bool,

    /// Save the embedded icon as `<title_id>.png` under DIR
    #[arg(long, value_name = "DIR")]
    pub save_icon: Option<PathBuf>,

    /// Path to prod.keys for Switch, a disc master key file for Wii U .wud/.wux info, or a .dkey file for PS3 info. Other consoles do not use it
    #[arg(long, value_name = "FILE")]
    pub keys: Option<PathBuf>,
}

/// Runs one `info` subcommand.
pub async fn run(cmd: InfoCommand, ctx: DispatchCtx<'_>) -> Result<()> {
    let DispatchCtx { .. } = ctx;
    let opts = rom_converto_lib::info::InfoOptions {
        keys_path: cmd.keys.clone(),
        parent_path: None,
    };
    let batch_paths = if let Some(paths_file) = &cmd.paths_file {
        Some(read_info_paths_file(paths_file)?)
    } else {
        let input = require_info_input(&cmd.input)?;
        if cmd.batch && input.is_dir() {
            let paths = collect_info_batch_files(input)?;
            if paths.is_empty() {
                anyhow::bail!("no supported files found under {}", input.display());
            }
            Some(paths)
        } else {
            None
        }
    };

    if let Some(paths) = batch_paths {
        run_info_batch(&paths, &opts, cmd.json, cmd.save_icon.as_deref());
    } else {
        let input = require_info_input(&cmd.input)?;
        ensure_input_exists(input)?;
        let resolved = rom_converto_lib::util::resolve_input(input, ALL_IMAGE_EXTS)?;
        let info = rom_converto_lib::info::read_info(resolved.path(), &opts)?;
        if let Some(dir) = &cmd.save_icon {
            save_info_icon(&info, dir)?;
        }
        info_print::print(&info, cmd.json)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Harness {
        #[command(flatten)]
        cmd: InfoCommand,
    }

    fn parse(args: &[&str]) -> Result<InfoCommand, clap::Error> {
        let mut full = vec!["bin"];
        full.extend_from_slice(args);
        Harness::try_parse_from(full).map(|h| h.cmd)
    }

    #[test]
    fn input_alone_parses() {
        let cmd = parse(&["game.cia"]).unwrap();
        assert_eq!(cmd.input, Some(PathBuf::from("game.cia")));
        assert_eq!(cmd.paths_file, None);
        assert!(!cmd.batch);
    }

    #[test]
    fn paths_file_alone_parses() {
        let cmd = parse(&["--paths-file", "games.txt"]).unwrap();
        assert_eq!(cmd.input, None);
        assert_eq!(cmd.paths_file, Some(PathBuf::from("games.txt")));
    }

    #[test]
    fn neither_input_nor_paths_file_is_rejected() {
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn input_and_paths_file_together_is_rejected() {
        assert!(parse(&["game.cia", "--paths-file", "games.txt"]).is_err());
    }

    #[test]
    fn batch_flag_parses() {
        let cmd = parse(&["./roms", "--batch"]).unwrap();
        assert!(cmd.batch);
        assert_eq!(cmd.input, Some(PathBuf::from("./roms")));
    }
}
