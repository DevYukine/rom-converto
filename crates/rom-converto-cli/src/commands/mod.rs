use crate::commands::chd::ChdCommands;
use crate::commands::completions::ShellCompletionsCommand;
use crate::commands::cso::CsoCommands;
use crate::commands::ctr::CtrCommands;
use crate::commands::cue::CueCommands;
use crate::commands::dat::DatCommands;
use crate::commands::dol::DolCommands;
use crate::commands::hash::HashCommand;
use crate::commands::info_command::InfoCommand;
use crate::commands::misc::{CapabilitiesCommand, SelfUpdateCommand};
use crate::commands::ntr::NtrCommands;
use crate::commands::nx::NxCommands;
use crate::commands::playlist::PlaylistCommand;
use crate::commands::ps3::Ps3Commands;
use crate::commands::psp::PspCommands;
use crate::commands::rvl::RvlCommands;
use crate::commands::vita::VitaCommands;
use crate::commands::wup::WupCommands;
use crate::commands::xbox::XboxCommands;
use crate::commands::xenon::XenonCommands;
use clap::{Args, Parser, Subcommand};
use rom_converto_lib::util::ConflictPolicy;
use std::path::PathBuf;

pub mod chd;
pub mod completions;
pub mod cso;
pub mod ctr;
pub mod cue;
pub mod dat;
pub mod dol;
pub mod hash;
pub mod info_command;
pub mod misc;
pub mod ntr;
pub mod nx;
pub mod playlist;
pub mod ps3;
pub mod psp;
pub mod rvl;
pub mod support;
pub mod vita;
pub mod wup;
pub mod xbox;
pub mod xenon;

/// Encrypt, decrypt, compress, convert, and verify ROMs and disc images
#[derive(Parser, Debug)]
#[command(
	name = env!("CARGO_BIN_NAME"),
	author,                   // pulls env!("CARGO_PKG_AUTHORS")
	version = env!("ROM_CONVERTO_DISPLAY_VERSION"),
	about,                    // doc-comment or Cargo.toml description
	long_about = "Encrypt, decrypt, compress, convert, and verify ROMs and disc images\n\nEach top-level command is a console/format family (ctr, dol, rvl, wup, nx, chd, cso, cue, ps3, psp, vita, ntr, xbox, xenon); each has operations like compress, decompress, verify and info. Output is auto-derived from the input unless you pass an explicit OUTPUT, -o/--output, or --output-dir. Pass -R/--recursive to process every matching file in a directory.",
	help_template = "\
{before-help}{name} {version}\n\
{about-with-newline}\n\
{usage-heading}\n    {usage}\n\n\
{all-args}\n\n\
Made with ❤ by {author}
"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Suppress progress and info output; only warnings and errors
    #[arg(short = 'q', long, global = true)]
    pub quiet: bool,

    /// Increase verbosity (-v debug, -vv trace, -vvv trace + dependencies)
    #[arg(short = 'v', long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Skip the check for a newer release
    #[arg(long = "no-update-check", global = true)]
    pub no_update_check: bool,

    /// Path to a config file; overrides the search order
    #[arg(long = "config", global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Apply a named preset from the config file
    #[arg(long = "preset", global = true, value_name = "NAME")]
    pub preset: Option<String>,

    /// Preview what would happen without writing any output
    #[arg(long = "dry-run", global = true)]
    pub dry_run: bool,

    /// Write a full-detail trace log to FILE regardless of console verbosity
    #[arg(long = "debug-log", global = true, value_name = "FILE")]
    pub debug_log: Option<PathBuf>,

    /// Skip the free-space preflight before writing output
    #[arg(long = "skip-space-check", global = true)]
    pub skip_space_check: bool,

    /// Ignore the persistent hash and verify cache for this run
    #[arg(long = "no-cache", global = true, conflicts_with = "rebuild_cache")]
    pub no_cache: bool,

    /// Discard the persistent cache and rebuild it from this run
    #[arg(long = "rebuild-cache", global = true)]
    pub rebuild_cache: bool,
}

#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum Commands {
    #[command(subcommand)]
    Ctr(CtrCommands),

    #[command(subcommand)]
    Dol(DolCommands),

    #[command(subcommand)]
    Rvl(RvlCommands),

    #[command(subcommand)]
    Wup(WupCommands),

    #[command(subcommand)]
    Nx(NxCommands),

    #[command(subcommand)]
    Xbox(XboxCommands),

    #[command(subcommand)]
    Xenon(XenonCommands),

    #[command(subcommand)]
    Ps3(Ps3Commands),

    #[command(subcommand)]
    Psp(PspCommands),

    #[command(subcommand)]
    Vita(VitaCommands),

    #[command(subcommand, alias = "nds")]
    Ntr(NtrCommands),

    #[command(subcommand)]
    Chd(ChdCommands),

    #[command(subcommand)]
    Cso(CsoCommands),

    #[command(subcommand)]
    Cue(CueCommands),

    #[command(subcommand)]
    Dat(DatCommands),

    Capabilities(CapabilitiesCommand),

    Hash(HashCommand),

    /// Auto-detect the console of a ROM or disc image and print its metadata
    Info(InfoCommand),

    Playlist(PlaylistCommand),

    SelfUpdate(SelfUpdateCommand),

    ShellCompletions(ShellCompletionsCommand),
}

/// The `--output-dir` / `--output-template` pair shared by every command that
/// derives a single output path from its input.
#[derive(Args, Debug, Clone, Eq, PartialEq)]
pub struct OutputArgs {
    /// Write output into this directory using the derived filename. Created if missing. Works with --recursive
    #[arg(long = "output-dir", value_name = "DIR", conflicts_with_all = ["output", "output_flag"])]
    pub output_dir: Option<PathBuf>,

    /// Output path template applied per file. Tokens: {title}, {titleId}, {region},
    /// {console}, {serial}, {ext}, {basename}. Resolves against extracted metadata;
    /// missing tokens fall back to the input basename. Joined under --output-dir
    #[arg(long = "output-template", value_name = "TEMPLATE", conflicts_with_all = ["output", "output_flag"])]
    pub output_template: Option<String>,
}

/// The `--on-conflict` / `--force` pair. `on_conflict` stays optional so an
/// unset flag falls through to the config or preset value.
#[derive(Args, Debug, Clone, Eq, PartialEq)]
pub struct ConflictArgs {
    /// What to do when an output already exists: error, overwrite, skip, or rename to a numbered sibling (defaults to the config value, else error)
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

/// The `--max-depth` / `--report` pair shared by the recursive commands.
/// `--recursive` itself stays per-command since its help text names the
/// extensions that command walks.
#[derive(Args, Debug, Clone, Eq, PartialEq)]
pub struct BatchArgs {
    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

    /// Write a run report to FILE. Format inferred from the extension: .csv, .json, .html or .htm. Unknown extensions default to JSON. The file is overwritten directly
    #[arg(long = "report", value_name = "FILE")]
    pub report: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ConflictPolicyArg {
    Error,
    Overwrite,
    Skip,
    Rename,
    OverwriteInvalid,
}

impl From<ConflictPolicyArg> for ConflictPolicy {
    fn from(arg: ConflictPolicyArg) -> Self {
        match arg {
            ConflictPolicyArg::Error => ConflictPolicy::Error,
            ConflictPolicyArg::Overwrite => ConflictPolicy::Overwrite,
            ConflictPolicyArg::Skip => ConflictPolicy::Skip,
            ConflictPolicyArg::Rename => ConflictPolicy::Rename,
            ConflictPolicyArg::OverwriteInvalid => ConflictPolicy::OverwriteInvalid,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_flags_default_off() {
        let cli = Cli::try_parse_from(["bin", "hash", "game.iso"]).unwrap();
        assert!(!cli.no_cache);
        assert!(!cli.rebuild_cache);
    }

    #[test]
    fn cache_flags_parse() {
        let cli = Cli::try_parse_from(["bin", "--no-cache", "hash", "game.iso"]).unwrap();
        assert!(cli.no_cache);
        let cli = Cli::try_parse_from(["bin", "hash", "game.iso", "--rebuild-cache"]).unwrap();
        assert!(cli.rebuild_cache);
    }

    #[test]
    fn no_cache_and_rebuild_cache_conflict() {
        let result =
            Cli::try_parse_from(["bin", "--no-cache", "--rebuild-cache", "hash", "game.iso"]);
        assert!(result.is_err());
    }

    /// Arguments a subcommand requires beyond the input before it parses at
    /// all. Where a required argument conflicts with an option under test,
    /// several alternatives are listed and a flag counts as accepted when
    /// any of them takes it.
    const REQUIRED_EXTRA: &[(&str, &[&[&str]])] = &[
        ("chd extract", &[&["--output-dir", "out"]]),
        ("cue merge", &[&["out"]]),
        ("psp extract", &[&["out"]]),
        ("vita extract", &[&["out"]]),
        ("xbox extract", &[&["out"]]),
        ("xenon extract", &[&["out"]]),
        ("wup compress", &[&["--output", "out"]]),
        ("wup decrypt", &[&["--output", "out"]]),
        (
            "dat fixdat",
            &[
                &["--output", "out", "--platform", "p"],
                &["--output", "out", "--dat-id", "d"],
            ],
        ),
    ];

    fn leaf(path: &[String]) -> clap::Command {
        let mut cmd = <Cli as clap::CommandFactory>::command();
        for name in path {
            let next = cmd
                .get_subcommands()
                .find(|s| s.get_name() == name)
                .cloned()
                .unwrap_or_else(|| panic!("no subcommand for {path:?}"));
            cmd = next;
        }
        cmd
    }

    /// A value the subcommand's parser accepts for `flag`.
    fn sample(cmd: &clap::Command, flag: &str) -> String {
        let long = flag.trim_start_matches("--");
        let values = cmd
            .get_arguments()
            .find(|a| a.get_long() == Some(long))
            .and_then(|a| a.get_value_parser().possible_values())
            .and_then(|mut v| v.next());
        match values {
            Some(value) => value.get_name().to_string(),
            None => match flag {
                "--codecs" => "lzma,zlib".to_string(),
                "--block-size-exp" => "14".to_string(),
                _ => "1".to_string(),
            },
        }
    }

    fn parses(bases: &[Vec<String>], extra: &[String]) -> bool {
        bases
            .iter()
            .any(|base| Cli::try_parse_from(base.iter().chain(extra)).is_ok())
    }

    /// Every derived subcommand path and flag in the CLI-echo manifest is
    /// something clap actually accepts, and the manifest's per-operation
    /// flag lists match what clap accepts for that subcommand.
    #[test]
    fn cli_echo_derivations_parse() {
        use rom_converto_lib::runner::cli_echo::{FlagKind, OutputKind, manifest};

        let manifest = manifest();
        let globals: Vec<String> = manifest
            .flags
            .values()
            .filter(|f| f.global)
            .flat_map(|f| match f.kind {
                FlagKind::Bool => vec![f.flag.clone()],
                _ => vec![f.flag.clone(), "1".to_string()],
            })
            .collect();

        let mut computed: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        let mut outputs: std::collections::BTreeMap<String, OutputKind> = Default::default();
        let mut drift = Vec::new();
        let mut output_drift = Vec::new();
        for (op, path) in &manifest.ops {
            let cmd = leaf(path);
            let key = path.join(" ");
            let mut base = vec!["bin".to_string()];
            base.extend(globals.iter().cloned());
            base.extend(path.iter().cloned());
            base.push("input".to_string());
            let extras = REQUIRED_EXTRA
                .iter()
                .find(|(p, _)| *p == key)
                .map(|(_, e)| *e)
                .unwrap_or(&[&[]]);
            let bases: Vec<Vec<String>> = extras
                .iter()
                .map(|extra| {
                    let mut base = base.clone();
                    base.extend(extra.iter().map(|s| (*s).to_string()));
                    base
                })
                .collect();
            assert!(
                parses(&bases, &[]),
                "{op}: base invocation does not parse: {bases:?}"
            );

            // A second positional is the output; failing that, --output-dir
            // then --output.
            let output = if cmd.get_positionals().count() > 1 {
                assert!(
                    Cli::try_parse_from(
                        ["bin"]
                            .iter()
                            .map(|s| (*s).to_string())
                            .chain(path.iter().cloned())
                            .chain(["input".to_string(), "out".to_string()])
                    )
                    .is_ok(),
                    "{op}: positional output does not parse"
                );
                OutputKind::Positional
            } else if cmd.get_opts().any(|a| a.get_long() == Some("output-dir")) {
                OutputKind::OutputDir
            } else if cmd.get_opts().any(|a| a.get_long() == Some("output")) {
                let output_flag = "--output".to_string();
                assert!(
                    bases.iter().any(|b| b.contains(&output_flag))
                        || parses(&bases, &[output_flag, "out".to_string()]),
                    "{op}: --output does not parse"
                );
                OutputKind::OutputFlag
            } else {
                OutputKind::None
            };
            if output != manifest.output[op] {
                output_drift.push(op.clone());
            }
            outputs.insert(key.clone(), output);

            let recursive = parses(&bases, &["--recursive".to_string()]);
            let mut accepted = Vec::new();
            for (field, flag) in &manifest.flags {
                if flag.global || flag.kind == FlagKind::Positional {
                    continue;
                }
                let mut tokens = match flag.kind {
                    FlagKind::Bool => vec![flag.flag.clone()],
                    _ => vec![flag.flag.clone(), sample(&cmd, &flag.flag)],
                };
                // --max-depth is only accepted alongside --recursive.
                if field == "max_depth" && recursive {
                    tokens.insert(0, "--recursive".to_string());
                }
                // Two option fields can spell the same flag (--full); only
                // the one the table lists, and so the runner reads for this
                // subcommand, belongs in the echo.
                if !manifest.op_flags[op].contains(field)
                    && manifest.flags.iter().any(|(other, f)| {
                        other != field
                            && f.flag == flag.flag
                            && manifest.op_flags[op].contains(other)
                    })
                {
                    continue;
                }
                // A flag a base already had to supply cannot be repeated,
                // but the subcommand plainly accepts it.
                if bases.iter().any(|b| b.contains(&flag.flag)) || parses(&bases, &tokens) {
                    accepted.push(field.clone());
                }
            }
            if accepted != manifest.op_flags[op] {
                drift.push(op.clone());
            }
            computed.insert(key, accepted);
        }

        let table: String = computed
            .iter()
            .map(|(path, fields)| {
                let fields: Vec<_> = fields.iter().map(|f| format!("{f:?}")).collect();
                format!("    ({path:?}, &[{}]),\n", fields.join(", "))
            })
            .collect();
        assert!(
            drift.is_empty(),
            "PATH_FLAGS in runner/cli_echo.rs is stale for {drift:?}; expected:\n{table}"
        );

        let table: String = outputs
            .iter()
            .filter(|(_, kind)| **kind != OutputKind::Positional)
            .map(|(path, kind)| format!("    ({path:?}, OutputKind::{kind:?}),\n"))
            .collect();
        assert!(
            output_drift.is_empty(),
            "PATH_OUTPUT in runner/cli_echo.rs is stale for {output_drift:?}; expected:\n{table}"
        );
    }
}
