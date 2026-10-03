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
    // The global flags are defined on the top-level command; pushing this
    // command's own display orders past theirs keeps the globals grouped at
    // the top of --help instead of interleaved with the --single options.
    next_display_order = 1000,
    after_long_help = "EXAMPLES:\n  Preview:      rom-converto organize ./library --output-dir ./sorted --dry-run\n  Sort with reports:\n                rom-converto organize ./library --output-dir ./sorted --report run.json\n  One per game: rom-converto organize ./library --output-dir ./sorted --dat --single --prefer-region USA,EUR,JPN --clean\n"
)]
pub struct OrganizeCommand {
    /// Library folder to organize
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Write the organized library into this directory. Required unless the config file or a preset supplies one. Created if missing
    #[arg(long = "output-dir", value_name = "DIR")]
    pub output_dir: Option<PathBuf>,

    /// Output path template applied per file. Tokens: {title}, {titleId}, {region}
    /// (prefers the DAT region when matched), {console}, {serial}, {ext}, {basename},
    /// {language}, {type}, {dat}, {game}, {input_dir}, and frontend directory tokens
    /// {adam}, {batocera}, {crossmix}, {es}, {funkeyos}, {minui}, {mister}, {miyoocfw},
    /// {onion}, {pocket}, {retrodeck}, {rocknix}, {romm}, {spruce}, {twmenu}.
    /// Defaults to {console}/{basename}.{ext}. {title}, {titleId}, and {serial}
    /// fall back to the input basename; the other tokens resolve to an empty
    /// string when their value is unknown. Joined under --output-dir
    #[arg(long = "output-template", value_name = "TEMPLATE")]
    pub output_template: Option<String>,

    /// Rename files to their No-Intro/Redump names using the Playmatch DAT service (online). Bare flag or =true enables; =false disables, overriding the config
    #[arg(long, require_equals = true, default_missing_value = "true", num_args = 0..=1)]
    pub dat: Option<bool>,

    /// Delete each source after it was organized successfully. Multi-entry archives, patch-only runs, and, with --verify-after, conversions whose format has no output check keep their sources. Under --on-conflict overwrite-invalid, an existing zip, copy, or hardlink that verifies valid counts as organized and its source is deleted. Bare flag or =true enables; =false disables, overriding the config
    #[arg(long = "move", require_equals = true, default_missing_value = "true", num_args = 0..=1)]
    pub move_source: Option<bool>,

    /// Write .m3u playlists for multi-disc sets in the output folders. Bare flag or =true enables; =false disables, overriding the config
    #[arg(long, require_equals = true, default_missing_value = "true", num_args = 0..=1)]
    pub playlists: Option<bool>,

    /// Maximum directory depth. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N")]
    pub max_depth: Option<usize>,

    #[command(flatten)]
    pub conflict: ConflictArgs,

    /// Write a run report to FILE. Format inferred from the extension: .csv, .json, .html or .htm. Unknown extensions default to JSON. The file is overwritten directly
    #[arg(long = "report", value_name = "FILE")]
    pub report: Option<PathBuf>,

    /// Path to `prod.keys`. Defaults to `$HOME/.switch/prod.keys` on Linux/macOS or `%USERPROFILE%/.switch/prod.keys` on Windows, then the binary's own directory
    #[arg(long = "keys", value_name = "PRODKEYS")]
    pub keys: Option<PathBuf>,

    /// Compress an encrypted ROM anyway, even though it barely compresses
    #[arg(long = "allow-encrypted", default_value_t = false)]
    pub allow_encrypted: bool,

    /// Playmatch API base URL for this run. Defaults to the public instance
    #[arg(long = "api-base", value_name = "URL")]
    pub api_base: Option<String>,

    /// Skip inputs whose path relative to INPUT matches any of these globs. Repeatable; `*` stops at `/`, so a recursive match needs `**/`. An excluded file is never cleaned or moved. A cue sheet with its bins, or a split .wud set, is dropped whole when any member matches
    #[arg(long, value_name = "GLOB", action = clap::ArgAction::Append)]
    pub input_exclude: Option<Vec<String>>,

    /// Keep only inputs whose game name matches this regex (the DAT game name with --dat, else the filename stem). Repeatable; `/pattern/flags` syntax accepted; an empty value (--filter-regex=) overrides the config with an empty list
    #[arg(long, value_name = "REGEX", action = clap::ArgAction::Append)]
    pub filter_regex: Option<Vec<String>>,

    /// Drop inputs whose game name matches this regex (the DAT game name with --dat, else the filename stem). Repeatable; `/pattern/flags` syntax accepted; an empty value (--filter-regex-exclude=) overrides the config with an empty list
    #[arg(long, value_name = "REGEX", action = clap::ArgAction::Append)]
    pub filter_regex_exclude: Option<Vec<String>>,

    /// Keep only games with at least one of these languages (2-letter codes, e.g. EN,FR); an empty value (--filter-language=) overrides the config with an empty list
    #[arg(long, value_name = "CODES", value_delimiter = ',')]
    pub filter_language: Option<Vec<String>>,

    /// Keep only games in at least one of these regions (e.g. USA,EUR,JPN,WORLD); an empty value (--filter-region=) overrides the config with an empty list
    #[arg(long, value_name = "CODES", value_delimiter = ',')]
    pub filter_region: Option<Vec<String>>,

    /// Drop games tagged with any of these kinds (bios, unlicensed, debug, demo, beta, sample, prototype, program, aftermarket, homebrew, alpha, bootleg, cracked, fixed, hacked, overdump, pendingdump, pirated, trained, translated, bad, unverified, device). `unverified` matches names that lack the classic `[!]` marker, so it drops every No-Intro or Redump named game; `device` never matches anything; an empty value (--no-type=) overrides the config with an empty list
    #[arg(long, value_name = "KINDS", value_delimiter = ',')]
    pub no_type: Option<Vec<String>>,

    /// Keep only games tagged with at least one of these kinds (see --no-type for the list); an empty value (--only-type=) overrides the config with an empty list
    #[arg(long, value_name = "KINDS", value_delimiter = ',')]
    pub only_type: Option<Vec<String>>,

    /// Keep only retail games, dropping bios/device/alpha/bad/beta/bootleg/cracked/debug/demo/fixed/hacked/homebrew/overdump/pendingdump/pirated/program/prototype/sample/trained/translated/aftermarket releases; unlicensed stays retail. Bare flag or =true enables; =false disables, overriding the config
    #[arg(long, require_equals = true, default_missing_value = "true", num_args = 0..=1)]
    pub only_retail: Option<bool>,

    /// Keep only the best release of each parent/clone group. Requires --dat. Bare flag or =true enables; =false disables, overriding the config
    #[arg(long, require_equals = true, default_missing_value = "true", num_args = 0..=1)]
    pub single: Option<bool>,

    /// With --single: when choosing the best release in a parent/clone group, prefer the clone whose DAT game name matches this regex. Highest-priority preference. Repeatable; an empty value (--prefer-game-regex=) overrides the config with an empty list
    #[arg(long, value_name = "REGEX", action = clap::ArgAction::Append)]
    pub prefer_game_regex: Option<Vec<String>>,

    /// With --single: prefer the dump whose name carries the classic `[!]` verified-dump marker (a name check, not hash verification). Bare flag or =true enables; =false disables, overriding the config
    #[arg(long, require_equals = true, default_missing_value = "true", num_args = 0..=1)]
    pub prefer_verified: Option<bool>,

    /// With --single: prefer a dump whose name lacks the classic `[b]` bad-dump marker. Bare flag or =true enables; =false disables, overriding the config
    #[arg(long, require_equals = true, default_missing_value = "true", num_args = 0..=1)]
    pub prefer_good: Option<bool>,

    /// With --single: prefer these languages, in priority order (2-letter codes); an empty value (--prefer-language=) overrides the config with an empty list
    #[arg(long, value_name = "CODES", value_delimiter = ',')]
    pub prefer_language: Option<Vec<String>>,

    /// With --single: prefer these regions, in priority order; an empty value (--prefer-region=) overrides the config with an empty list
    #[arg(long, value_name = "CODES", value_delimiter = ',')]
    pub prefer_region: Option<Vec<String>>,

    /// With --single: prefer the older revision, the newer revision, or either (`any`). `any` is the explicit no-preference value, overriding the config
    #[arg(long, value_parser = ["older", "newer", "any"])]
    pub prefer_revision: Option<String>,

    /// With --single: prefer a retail release (see --only-retail). Bare flag or =true enables; =false disables, overriding the config
    #[arg(long, require_equals = true, default_missing_value = "true", num_args = 0..=1)]
    pub prefer_retail: Option<bool>,

    /// With --single: prefer the parent release over a clone. Bare flag or =true enables; =false disables, overriding the config
    #[arg(long, require_equals = true, default_missing_value = "true", num_args = 0..=1)]
    pub prefer_parent: Option<bool>,

    /// Prefer the input whose own file name matches this regex; breaks ties between inputs for the same game or the same output path, after the already-placed and format preferences, including without --dat. Repeatable, first match wins; an empty value (--prefer-filename-regex=) overrides the config with an empty list
    #[arg(long, value_name = "REGEX", action = clap::ArgAction::Append)]
    pub prefer_filename_regex: Option<Vec<String>>,

    /// Group output files under a subdirectory named after the first letter(s) of the filename. Bare flag or =true enables; =false disables, overriding the config
    #[arg(long, require_equals = true, default_missing_value = "true", num_args = 0..=1)]
    pub dir_letter: Option<bool>,

    /// Number of leading characters to use for the letter subdirectory. Defaults to 1. Ignored unless --dir-letter is on (flag or config)
    #[arg(long, value_name = "N", value_parser = clap::builder::RangedI64ValueParser::<usize>::new().range(1..=26))]
    pub dir_letter_count: Option<usize>,

    /// Cap the number of games per letter subdirectory; larger groups split into numbered chunks. Ignored unless --dir-letter is on (flag or config)
    #[arg(long, value_name = "N", value_parser = clap::builder::RangedI64ValueParser::<usize>::new().range(1..))]
    pub dir_letter_limit: Option<usize>,

    /// Merge adjacent under-full letter subdirectories into ranges (e.g. A-C). Requires --dir-letter-limit (flag or config). Ignored unless --dir-letter is on (flag or config). Bare flag or =true enables; =false disables, overriding the config
    #[arg(long, require_equals = true, default_missing_value = "true", num_args = 0..=1)]
    pub dir_letter_group: Option<bool>,

    /// Place each multi-disc set in its own folder named after the game. With --playlists the .m3u is written inside that folder. Bare flag or =true enables; =false disables, overriding the config
    #[arg(long, require_equals = true, default_missing_value = "true", num_args = 0..=1)]
    pub multi_disc_dirs: Option<bool>,

    /// Zip archive format: torrentzip (default) or rvzstd
    #[arg(long, value_parser = ["torrentzip", "rvzstd"])]
    pub zip_format: Option<String>,

    /// Copy instead of zipping files whose planned .zip output path under --output-dir matches this glob; `*` stops at `/`, so a recursive match needs `**/`. An empty value (--zip-exclude=) overrides the config with no exclusion
    #[arg(long, value_name = "GLOB")]
    pub zip_exclude: Option<String>,

    /// Use a hardlink, symlink, or reflink instead of copying files that are already in their best format. Links are placed for plain, unpatched sources; archive members, patched variants and header-stripped/padded payloads are copied instead (the row detail says so). Symlink cannot be combined with --move (flag or config). Hardlinks and reflinks need the source and the output on the same filesystem, and reflinks also need a filesystem with copy-on-write clones (for example APFS, Btrfs, XFS, ReFS); elsewhere the item fails
    #[arg(long, value_parser = ["hardlink", "symlink", "reflink"])]
    pub link_mode: Option<String>,

    /// Make symlinks created by --link-mode symlink relative instead of absolute. On Windows, a target on another drive stays absolute. Bare flag or =true enables; =false disables, overriding the config
    #[arg(long, require_equals = true, default_missing_value = "true", num_args = 0..=1)]
    pub symlink_relative: Option<bool>,

    /// Strip ROM headers before compressing or copying, so the archived file matches its No-Intro/Redump hash. Bare flag or =all strips every detected header; --remove-headers=nes,fds limits which (nes, fds, a78, lnx→lyx, smc/sfc→sfc); --remove-headers=none strips nothing, overriding the config; an empty value (=) is rejected. With --dat, a file that matches the DAT only once its header is removed is written headerless whatever this flag says, including --remove-headers=none; a match in a "(Headered)" DAT keeps the header
    #[arg(long, value_name = "EXTS", num_args = 0..=1, require_equals = true, value_delimiter = ',')]
    pub remove_headers: Option<Vec<String>>,

    /// Re-pad trimmed GBA/NDS dumps back to their full size. Bare flag or =true enables; =false disables, overriding the config
    #[arg(long, require_equals = true, default_missing_value = "true", num_args = 0..=1)]
    pub trim_add_padding: Option<bool>,

    /// Apply a patch (.aps, .bps, .ebp, .ips, .ips32, .ppf, .rup, .ups, or .vcdiff/.xdelta (VCDIFF)) from this file or directory, matched to inputs by CRC32. BPS and UPS carry the source CRC32; every other format pairs through the CRC32 in the patch file name: [XXXXXXXX], (XXXXXXXX), 0xXXXXXXXX, or a bare 8-hex run containing a letter A-F. The APS declared size and the .rup source and target MD5 are checked when the patch is applied. Repeatable
    #[arg(long, value_name = "PATH", action = clap::ArgAction::Append)]
    pub patch: Option<Vec<PathBuf>>,

    /// Only output games that received a patch, dropping unpatched inputs. Requires --patch
    #[arg(long, default_value_t = false, requires = "patch")]
    pub patch_only: bool,

    /// Delete files in the output folders this run wrote to that this run did not produce. Never deletes files under INPUT. Bare flag or =true enables; =false disables, overriding the config
    #[arg(long, require_equals = true, default_missing_value = "true", num_args = 0..=1)]
    pub clean: Option<bool>,

    /// Never delete files matching this glob, relative to --output-dir (absolute globs also work); `*` stops at `/`, so a recursive match needs `**/`; matching ignores letter case; an empty value (--clean-exclude=) overrides the config with an empty list
    #[arg(long, value_name = "GLOB", action = clap::ArgAction::Append)]
    pub clean_exclude: Option<Vec<String>>,

    /// Move files that --clean would delete into this directory instead of deleting them, flat with a " (n)" suffix on collision
    #[arg(long, value_name = "DIR")]
    pub clean_backup: Option<PathBuf>,

    /// With --move, when to delete emptied source folders: never, auto (default; only folders this run emptied), or always (every empty folder under INPUT, including ones that were already empty)
    #[arg(long, value_parser = ["never", "auto", "always"])]
    pub move_delete_dirs: Option<String>,

    /// Re-verify each written zip, copy, or converted output after writing it (size and CRC32 for zips and copies plus TorrentZip or RVZSTD structure for zips; the format's own verify for conversions). A zip, copy, or conversion whose post-write verification fails fails its row, and conversions whose format has no output check (3DS, Wii U, Xbox, Xbox 360, and PS3) fail as unverified and keep their source
    #[arg(long, default_value_t = false)]
    pub verify_after: bool,
}

/// `--flag=` (an empty value) is the explicit empty list: it overrides a
/// config list where omitting the flag would take the config value. Empty
/// entries in mixed spellings are dropped.
fn explicit_empty(values: Option<Vec<String>>) -> Option<Vec<String>> {
    values.map(|entries| {
        entries
            .into_iter()
            .filter(|entry| !entry.is_empty())
            .collect()
    })
}

impl OrganizeCommand {
    /// The organize options exactly as spelled on the command line: an
    /// explicit value always reaches the runner, so the config fill cannot
    /// apply behind the flag the user gave.
    fn spelled_options(&self) -> RunOptions {
        RunOptions {
            dat: self.dat,
            move_source: self.move_source,
            playlists: self.playlists,
            allow_encrypted: self.allow_encrypted.then_some(true),
            input_exclude: explicit_empty(self.input_exclude.clone()),
            filter_regex: explicit_empty(self.filter_regex.clone()),
            filter_regex_exclude: explicit_empty(self.filter_regex_exclude.clone()),
            filter_language: explicit_empty(self.filter_language.clone()),
            filter_region: explicit_empty(self.filter_region.clone()),
            no_type: explicit_empty(self.no_type.clone()),
            only_type: explicit_empty(self.only_type.clone()),
            only_retail: self.only_retail,
            single: self.single,
            prefer_game_regex: explicit_empty(self.prefer_game_regex.clone()),
            prefer_verified: self.prefer_verified,
            prefer_good: self.prefer_good,
            prefer_language: explicit_empty(self.prefer_language.clone()),
            prefer_region: explicit_empty(self.prefer_region.clone()),
            prefer_revision: self.prefer_revision.clone(),
            prefer_retail: self.prefer_retail,
            prefer_parent: self.prefer_parent,
            prefer_filename_regex: explicit_empty(self.prefer_filename_regex.clone()),
            dir_letter: self.dir_letter,
            dir_letter_count: self.dir_letter_count,
            dir_letter_limit: self.dir_letter_limit,
            dir_letter_group: self.dir_letter_group,
            multi_disc_dirs: self.multi_disc_dirs,
            zip_format: self.zip_format.clone(),
            zip_exclude: self.zip_exclude.clone(),
            link_mode: self.link_mode.clone(),
            symlink_relative: self.symlink_relative,
            remove_headers: self.remove_headers.clone(),
            trim_add_padding: self.trim_add_padding,
            patch: self.patch.clone(),
            patch_only: self.patch_only.then_some(true),
            clean: self.clean,
            clean_exclude: explicit_empty(self.clean_exclude.clone()),
            clean_backup: self.clean_backup.clone(),
            move_delete_dirs: self.move_delete_dirs.clone(),
            verify_after: self.verify_after.then_some(true),
            ..Default::default()
        }
    }
}

/// The final options: the flags as spelled, with the config fallbacks for
/// the fields the CLI does not carry (or carries only as a blank).
fn effective_options(
    cmd: &OrganizeCommand,
    effective: &crate::config::Effective,
) -> anyhow::Result<RunOptions> {
    let organize = &effective.organize;
    let mut options = cmd.spelled_options();
    options.output_dir = Some(
        cmd.output_dir
            .clone()
            .or_else(|| organize.output_dir.clone())
            .ok_or_else(|| anyhow::anyhow!("organize needs --output-dir"))?,
    );
    options.output_template = cmd
        .output_template
        .clone()
        .or_else(|| organize.output_template.clone());
    options.max_depth = cmd.max_depth;
    options.report = cmd.report.clone().or_else(|| organize.report.clone());
    options.on_conflict = Some(
        policy_name(resolve_policy(
            cmd.conflict.on_conflict,
            cmd.conflict.force,
            policy_fallback(&organize.on_conflict)?,
        ))
        .to_string(),
    );
    options.keys = cmd.keys.clone();
    options.api_base = cmd
        .api_base
        .clone()
        .or_else(|| effective.dat.api_base.clone());
    Ok(options)
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
    let mut options = effective_options(&cmd, effective)?;
    options.skip_space_check = Some(skip_space_check);
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

    /// Splits a rendered echo line into argv, honoring the single quotes,
    /// `'\''` escapes, and backslash escapes the renderer emits.
    fn echo_argv(line: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut current = String::new();
        let mut in_quotes = false;
        let mut chars = line.chars();
        while let Some(ch) = chars.next() {
            match ch {
                '\'' => in_quotes = !in_quotes,
                '\\' if !in_quotes => {
                    // Outside quotes a backslash escapes the next character.
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                }
                c if c.is_whitespace() && !in_quotes => {
                    if !current.is_empty() {
                        words.push(std::mem::take(&mut current));
                    }
                }
                c => current.push(c),
            }
        }
        if !current.is_empty() {
            words.push(current);
        }
        words
    }

    #[test]
    fn remove_headers_bare_flag_means_all() {
        let c = parse(&["bin", "organize", "./library", "--remove-headers"]);
        assert_eq!(c.remove_headers, Some(vec![]));
    }

    #[test]
    fn remove_headers_value_does_not_swallow_the_positional() {
        // The space form leaves the next token to the positional, so a bare
        // flag followed by the input keeps meaning "strip everything".
        let c = parse(&["bin", "organize", "--remove-headers", "./library"]);
        assert_eq!(c.remove_headers, Some(vec![]));
        assert_eq!(c.input, PathBuf::from("./library"));

        let listed = parse(&["bin", "organize", "--remove-headers=nes,fds", "./library"]);
        assert_eq!(
            listed.remove_headers,
            Some(vec!["nes".to_string(), "fds".to_string()])
        );
        assert_eq!(listed.input, PathBuf::from("./library"));
    }

    #[test]
    fn regex_flags_repeat() {
        let c = parse(&[
            "bin",
            "organize",
            "./library",
            "--filter-regex",
            "Mario",
            "--filter-regex",
            "Zelda",
            "--patch",
            "a.ips",
            "--patch",
            "b.bps",
        ]);
        assert_eq!(
            c.filter_regex,
            Some(vec!["Mario".to_string(), "Zelda".to_string()])
        );
        assert_eq!(
            c.patch,
            Some(vec![PathBuf::from("a.ips"), PathBuf::from("b.bps")])
        );
    }

    #[test]
    fn exclude_globs_keep_brace_groups() {
        let c = parse(&[
            "bin",
            "organize",
            "./library",
            "--input-exclude",
            "*.{txt,pdf}",
        ]);
        assert_eq!(c.input_exclude, Some(vec!["*.{txt,pdf}".to_string()]));
    }

    #[test]
    fn bare_command_leaves_config_backed_fields_unset() {
        let c = parse(&["bin", "organize", "./library"]);
        let options = c.spelled_options();
        assert_eq!(options.dat, None);
        assert_eq!(options.move_source, None);
        assert_eq!(options.playlists, None);
        assert_eq!(options.only_retail, None);
        assert_eq!(options.single, None);
        assert_eq!(options.prefer_verified, None);
        assert_eq!(options.prefer_good, None);
        assert_eq!(options.prefer_retail, None);
        assert_eq!(options.prefer_parent, None);
        assert_eq!(options.dir_letter, None);
        assert_eq!(options.dir_letter_group, None);
        assert_eq!(options.multi_disc_dirs, None);
        assert_eq!(options.symlink_relative, None);
        assert_eq!(options.trim_add_padding, None);
        assert_eq!(options.clean, None);
        assert_eq!(options.filter_regex, None);
        assert_eq!(options.clean_exclude, None);

        // The fields the CLI does not carry (or leaves blank) fall back to
        // the config, and an explicit flag wins over the config.
        let mut effective = crate::config::Effective::default();
        effective.organize.output_dir = Some(PathBuf::from("./sorted"));
        effective.organize.output_template = Some("{dat}/{basename}.{ext}".to_string());
        effective.organize.report = Some(PathBuf::from("run.json"));
        effective.organize.on_conflict = Some("skip".to_string());
        effective.dat.api_base = Some("https://example.test/api/v2".to_string());
        let merged = effective_options(&c, &effective).unwrap();
        assert_eq!(merged.output_dir, Some(PathBuf::from("./sorted")));
        assert_eq!(
            merged.output_template.as_deref(),
            Some("{dat}/{basename}.{ext}")
        );
        assert_eq!(merged.report, Some(PathBuf::from("run.json")));
        assert_eq!(merged.on_conflict.as_deref(), Some("skip"));
        assert_eq!(
            merged.api_base.as_deref(),
            Some("https://example.test/api/v2")
        );

        let flagged = parse(&[
            "bin",
            "organize",
            "./library",
            "--output-dir",
            "./mine",
            "--report",
            "mine.json",
            "--on-conflict",
            "rename",
        ]);
        let merged = effective_options(&flagged, &effective).unwrap();
        assert_eq!(merged.output_dir, Some(PathBuf::from("./mine")));
        assert_eq!(merged.report, Some(PathBuf::from("mine.json")));
        assert_eq!(merged.on_conflict.as_deref(), Some("rename"));
    }

    #[test]
    fn gui_echo_fixture_round_trips() {
        use rom_converto_lib::config::OrganizeDefaults;
        use rom_converto_lib::runner::defaults::apply_organize_defaults;

        // The committed fixture (crates/rom-converto-gui/lib/opdefs/
        // organize_echo_fixture.json) is written by organize.test.ts from
        // the live payloadOf() options and the buildCliCommand() echo.
        // Parsing it here keeps the Rust argument surface and the config
        // fill in lockstep with what the GUI actually sends.
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../rom-converto-gui/lib/opdefs/organize_echo_fixture.json"
        ))
        .expect("the echo fixture is valid JSON; run the GUI vitest with UPDATE_ECHO_FIXTURE=1");

        // A config whose toggles all carry `flag` and whose other values
        // differ from the payloads': a field the payload set must win over
        // the fill, and a flag the echo drops shows up as the fill replacing
        // the payload's value. The default and symlink payloads turn every
        // toggle off, so they run against the all-true config. The on
        // payload turns every toggle on, so it runs against the all-false
        // config. output_template and on_conflict are set so the flag-wins
        // precedence is exercised too.
        let hostile = |flag: bool| OrganizeDefaults {
            output_template: Some("{dat}/{basename}.{ext}".to_string()),
            on_conflict: Some("skip".to_string()),
            clean: Some(flag),
            clean_exclude: Some(vec!["stale.txt".to_string()]),
            dat: Some(flag),
            dir_letter: Some(flag),
            dir_letter_count: Some(3),
            dir_letter_group: Some(flag),
            dir_letter_limit: Some(7),
            filter_language: Some(vec!["JA".to_string()]),
            filter_regex: Some(vec!["Japan".to_string()]),
            filter_regex_exclude: Some(vec!["Beta".to_string()]),
            filter_region: Some(vec!["JPN".to_string()]),
            move_source: Some(flag),
            multi_disc_dirs: Some(flag),
            no_type: Some(vec!["demo".to_string()]),
            only_retail: Some(flag),
            only_type: Some(vec!["bios".to_string()]),
            playlists: Some(flag),
            prefer_filename_regex: Some(vec!["Other".to_string()]),
            prefer_game_regex: Some(vec!["Other".to_string()]),
            prefer_good: Some(flag),
            prefer_language: Some(vec!["JA".to_string()]),
            prefer_parent: Some(flag),
            prefer_region: Some(vec!["JPN".to_string()]),
            prefer_revision: Some("newer".to_string()),
            prefer_retail: Some(flag),
            prefer_verified: Some(flag),
            remove_headers: Some(vec!["nes".to_string()]),
            single: Some(flag),
            symlink_relative: Some(flag),
            trim_add_padding: Some(flag),
            zip_exclude: Some("**/*.gba".to_string()),
            zip_format: Some("rvzstd".to_string()),
            link_mode: Some("symlink".to_string()),
            move_delete_dirs: Some("always".to_string()),
            clean_backup: Some("./stale-backup".into()),
            ..Default::default()
        };

        for (entry, flag) in [("default", true), ("on", false), ("symlink", true)] {
            let mut hostile_effective = crate::config::Effective::default();
            hostile_effective.organize = hostile(flag);
            hostile_effective.dat.api_base = Some("https://hostile.example/api".to_string());
            let echo = fixture[entry]["echo"].as_str().expect("echo is a string");
            let payload = fixture[entry]["options"].clone();

            // The echo may lead with global flags (skip_space_check), so the
            // harness splices the subcommand and everything after it.
            let words = echo_argv(echo);
            assert_eq!(words[0], ">");
            assert_eq!(words[1], "rom-converto");
            let subcommand = words
                .iter()
                .position(|word| word == "organize")
                .expect("the echo names the organize subcommand");
            let mut argv = vec!["bin".to_string()];
            argv.extend(words[subcommand..].iter().cloned());
            let harness = Harness::try_parse_from(argv)
                .unwrap_or_else(|e| panic!("{entry}: the echoed command does not parse: {e}"));
            let Wrapper::Organize(c) = harness.cmd;
            let mut effective = effective_options(&c, &hostile_effective)
                .unwrap_or_else(|e| panic!("{entry}: effective options failed: {e}"));
            apply_organize_defaults(&mut effective, None, Some(&hostile_effective.organize));
            // The GUI side: the payload as RunOptions with the hostile
            // config fill applied the same way. Comparing the full
            // serialized options catches both a flag the echo dropped and
            // one the echo carries while the payload leaves at Config
            // default.
            let mut expected: RunOptions = serde_json::from_value(payload)
                .expect("the fixture payload is a valid RunOptions shape");
            apply_organize_defaults(&mut expected, None, Some(&hostile_effective.organize));
            let (serde_json::Value::Object(effective), serde_json::Value::Object(expected)) = (
                serde_json::to_value(&effective).unwrap(),
                serde_json::to_value(&expected).unwrap(),
            ) else {
                panic!("options serialize to an object");
            };
            for (key, expected_value) in &expected {
                // skip_space_check is a global dispatch flag: it travels
                // beside the subcommand, not inside the organize parse, so
                // the two sides cannot carry the same value.
                if key == "skip_space_check" {
                    continue;
                }
                // api_base is a documented CLI/GUI divergence: the echoed
                // CLI command reads [dat] api_base while a GUI organize run
                // uses the public instance (see configuration.md), so the
                // payload's null and the echo's [dat] fill are both correct.
                if key == "api_base" {
                    continue;
                }
                // The CLI-only toggles allow_encrypted, verify_after and
                // patch_only have no config key, so a false in the payload
                // and an unset flag mean the same thing. The same holds for
                // the CLI-only lists input_exclude and patch.
                const CLI_ONLY_FALSE: [&str; 3] = ["allow_encrypted", "verify_after", "patch_only"];
                const CLI_ONLY_EMPTY: [&str; 2] = ["input_exclude", "patch"];
                if (expected_value == &serde_json::Value::Bool(false)
                    && CLI_ONLY_FALSE.contains(&key.as_str())
                    && effective.get(key) == Some(&serde_json::Value::Null))
                    || (expected_value == &serde_json::Value::Array(vec![])
                        && CLI_ONLY_EMPTY.contains(&key.as_str())
                        && effective.get(key) == Some(&serde_json::Value::Null))
                {
                    continue;
                }
                assert_eq!(
                    effective.get(key),
                    Some(expected_value),
                    "{entry}: {key} differs between the GUI payload and the parsed echo"
                );
            }
        }
    }

    #[test]
    fn explicit_empty_drops_blank_entries() {
        assert_eq!(explicit_empty(None), None);
        assert_eq!(explicit_empty(Some(vec![])), Some(vec![]));
        assert_eq!(explicit_empty(Some(vec!["".to_string()])), Some(vec![]));
        assert_eq!(
            explicit_empty(Some(vec!["".to_string(), "Game".to_string()])),
            Some(vec!["Game".to_string()])
        );
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

    #[test]
    fn dir_letter_value_ranges_reject_zero_and_over_26() {
        assert!(
            Harness::try_parse_from([
                "bin",
                "organize",
                "./library",
                "--dir-letter",
                "--dir-letter-count",
                "0"
            ])
            .is_err()
        );
        assert!(
            Harness::try_parse_from([
                "bin",
                "organize",
                "./library",
                "--dir-letter",
                "--dir-letter-count",
                "27"
            ])
            .is_err()
        );
        assert!(
            Harness::try_parse_from([
                "bin",
                "organize",
                "./library",
                "--dir-letter",
                "--dir-letter-limit",
                "0"
            ])
            .is_err()
        );
        let ok = parse(&[
            "bin",
            "organize",
            "./library",
            "--dir-letter",
            "--dir-letter-count",
            "26",
            "--dir-letter-limit",
            "10",
        ]);
        assert_eq!(ok.dir_letter_count, Some(26));
        assert_eq!(ok.dir_letter_limit, Some(10));
    }

    #[test]
    fn patch_only_requires_patch() {
        let result = Harness::try_parse_from(["bin", "organize", "./library", "--patch-only"]);
        assert!(result.is_err());
    }
}
