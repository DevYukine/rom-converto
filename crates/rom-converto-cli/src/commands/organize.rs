use crate::commands::ConflictArgs;
use clap::Parser;
use std::path::PathBuf;
use std::time::Instant;

use crate::batch;
use crate::commands::support::{DispatchCtx, require_dir};
use crate::config::policy_fallback;
use crate::util::{CliProgress, policy_name, resolve_policy};
use anyhow::Result;
use rom_converto_lib::runner::models::{RunOptions, RunRequest};
use rom_converto_lib::runner::run_request;

/// Sort a ROM library into per-console folders and compress every file into its best format
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  Preview:      rom-converto organize ./library --output-dir ./sorted --dry-run\n  Sort with reports:\n                rom-converto organize ./library --output-dir ./sorted --report run.json\n"
)]
pub struct OrganizeCommand {
    /// Library folder to organize
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Write the organized library into this directory. Required unless the config file or a preset supplies one. Created if missing
    #[arg(long = "output-dir", value_name = "DIR")]
    pub output_dir: Option<PathBuf>,

    /// Output path template applied per file. Tokens: {title}, {titleId}, {region},
    /// {console}, {serial}, {ext}, {basename}. Defaults to {console}/{basename}.{ext}.
    /// Joined under --output-dir
    #[arg(long = "output-template", value_name = "TEMPLATE")]
    pub output_template: Option<String>,

    /// Rename files to their No-Intro/Redump names using the Playmatch DAT service (online)
    #[arg(long, default_value_t = false)]
    pub dat: bool,

    /// Delete each source after it was organized successfully
    #[arg(long = "move", default_value_t = false)]
    pub move_source: bool,

    /// Write .m3u playlists for multi-disc sets in the output folders
    #[arg(long, default_value_t = false)]
    pub playlists: bool,

    /// Maximum directory depth. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N")]
    pub max_depth: Option<usize>,

    #[command(flatten)]
    pub conflict: ConflictArgs,

    /// Write a run report to FILE. Format inferred from the extension: .csv, .json, .html or .htm. Unknown extensions default to JSON. The file is overwritten directly
    #[arg(long = "report", value_name = "FILE")]
    pub report: Option<PathBuf>,

    /// Path to `prod.keys` for Switch NSP/XCI compression. Defaults to `$HOME/.switch/prod.keys` on Linux/macOS or `%USERPROFILE%/.switch/prod.keys` on Windows, then the binary's own directory
    #[arg(long = "keys", value_name = "FILE")]
    pub keys: Option<PathBuf>,

    /// Compress an encrypted ROM anyway, even though it barely compresses
    #[arg(long = "allow-encrypted", default_value_t = false)]
    pub allow_encrypted: bool,

    /// Playmatch API base URL for this run. Defaults to the public instance
    #[arg(long = "api-base", value_name = "URL")]
    pub api_base: Option<String>,
}

/// Runs the `organize` command.
pub async fn run(cmd: OrganizeCommand, ctx: DispatchCtx<'_>) -> Result<()> {
    let DispatchCtx {
        progress,
        total_progress,
        effective,
        config,
        preset,
        dry_run,
        skip_space_check,
        cancel,
        cache,
    } = ctx;
    require_dir(&cmd.input)?;
    let organize = &effective.organize;
    let output_dir = match cmd.output_dir.or_else(|| organize.output_dir.clone()) {
        Some(dir) => dir,
        None => anyhow::bail!("organize needs --output-dir"),
    };
    let options = RunOptions {
        // The runner walks the library root itself; `recursive` stays unset
        // because the organize operation does not take the flag.
        output_dir: Some(output_dir),
        output_template: cmd
            .output_template
            .or_else(|| organize.output_template.clone()),
        max_depth: cmd.max_depth,
        report: cmd.report.or_else(|| organize.report.clone()),
        on_conflict: Some(
            policy_name(resolve_policy(
                cmd.conflict.on_conflict,
                cmd.conflict.force,
                policy_fallback(&organize.on_conflict)?,
            ))
            .to_string(),
        ),
        keys: cmd.keys,
        allow_encrypted: Some(cmd.allow_encrypted),
        api_base: cmd.api_base.or_else(|| effective.dat.api_base.clone()),
        dat: Some(cmd.dat || organize.dat == Some(true)),
        move_source: Some(cmd.move_source || organize.move_source == Some(true)),
        playlists: Some(cmd.playlists || organize.playlists == Some(true)),
        skip_space_check: Some(skip_space_check),
        ..Default::default()
    };
    let mut req = RunRequest {
        schema: None,
        operation: "organize".to_string(),
        input: Some(cmd.input),
        output: None,
        config,
        preset,
        options,
        dry_run,
        ctx: Default::default(),
    };
    req.ctx.hash_cache = Some(cache.clone());
    let reporter = CliProgress {
        file: &progress,
        total: &total_progress,
        print_row: if dry_run {
            batch::print_plan_row
        } else {
            batch::print_row
        },
        count_units: true,
    };
    let started = Instant::now();
    let response = run_request(req, &reporter, cancel).await?;
    total_progress.finish_bar();
    // Organize is a batch operation even though it takes no --recursive
    // flag, so it always closes with the batch tally and failed-file bail.
    if let Some(totals) = &response.totals {
        batch::finish(totals, batch::direction("organize"), dry_run, started)?;
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
        cmd: Wrapper,
    }

    #[derive(clap::Subcommand, Debug)]
    enum Wrapper {
        Organize(OrganizeCommand),
    }

    fn parse(args: &[&str]) -> OrganizeCommand {
        let h = Harness::parse_from(args);
        let Wrapper::Organize(c) = h.cmd;
        c
    }

    #[test]
    fn defaults() {
        let c = parse(&["bin", "organize", "./library"]);
        assert_eq!(c.input, PathBuf::from("./library"));
        assert_eq!(c.output_dir, None);
        assert_eq!(c.output_template, None);
        assert!(!c.dat);
        assert!(!c.move_source);
        assert!(!c.playlists);
        assert_eq!(c.max_depth, None);
        assert_eq!(c.conflict.on_conflict, None);
        assert!(!c.conflict.force);
        assert_eq!(c.report, None);
        assert_eq!(c.keys, None);
        assert!(!c.allow_encrypted);
        assert_eq!(c.api_base, None);
    }

    #[test]
    fn parses_every_flag() {
        let c = parse(&[
            "bin",
            "organize",
            "./library",
            "--output-dir",
            "./sorted",
            "--output-template",
            "{console}/{basename}.{ext}",
            "--dat",
            "--move",
            "--playlists",
            "--max-depth",
            "2",
            "--on-conflict",
            "rename",
            "--report",
            "run.json",
            "--keys",
            "prod.keys",
            "--allow-encrypted",
            "--api-base",
            "https://example.test/api/v2",
        ]);
        assert_eq!(c.output_dir, Some(PathBuf::from("./sorted")));
        assert_eq!(
            c.output_template.as_deref(),
            Some("{console}/{basename}.{ext}")
        );
        assert!(c.dat);
        assert!(c.move_source);
        assert!(c.playlists);
        assert_eq!(c.max_depth, Some(2));
        assert_eq!(c.conflict.on_conflict, Some(ConflictPolicyArg::Rename));
        assert_eq!(c.report, Some(PathBuf::from("run.json")));
        assert_eq!(c.keys, Some(PathBuf::from("prod.keys")));
        assert!(c.allow_encrypted);
        assert_eq!(c.api_base.as_deref(), Some("https://example.test/api/v2"));
    }

    #[test]
    fn move_flag_maps_to_move_source() {
        let c = parse(&["bin", "organize", "./library", "--move"]);
        assert!(c.move_source);
    }

    #[test]
    fn force_conflicts_with_on_conflict() {
        let result = Harness::try_parse_from([
            "bin",
            "organize",
            "./library",
            "-f",
            "--on-conflict",
            "skip",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_force_alone() {
        let c = parse(&["bin", "organize", "./library", "-f"]);
        assert!(c.conflict.force);
        assert_eq!(c.conflict.on_conflict, None);
    }
}
