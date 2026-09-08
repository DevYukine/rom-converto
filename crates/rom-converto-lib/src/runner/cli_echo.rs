//! Metadata that lets a frontend render the equivalent CLI invocation for a
//! [`RunRequest`](super::models::RunRequest).
//!
//! Paths and flag names are derived from the runner's own operation and
//! option names; [`OVERRIDES`] carries only the handful of cases where the
//! CLI spells something differently or has no equivalent at all. The
//! per-operation flag lists in [`PATH_FLAGS`] are checked against clap by
//! `cli_echo_derivations_parse` in the CLI crate, so drift fails that test.

use serde::Serialize;
use std::collections::BTreeMap;

use super::ops::operation_names;

/// How a [`CliFlag`] carries its value on the command line.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "cli_echo.ts"))]
pub enum FlagKind {
    /// Present or absent, no value.
    Bool,
    /// A single value in the next argument.
    Value,
    /// A comma-separated list in the next argument.
    List,
    /// Not a flag: the values are extra positional arguments.
    Positional,
}

/// The CLI spelling of one [`RunOptions`](super::models::RunOptions) field.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "cli_echo.ts"))]
pub struct CliFlag {
    pub flag: String,
    /// Global flags are parsed before the subcommand, so they are emitted
    /// ahead of the operation path.
    pub global: bool,
    pub kind: FlagKind,
}

/// Everything a frontend needs to echo a request as a CLI invocation.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "cli_echo.ts"))]
pub struct CliEchoManifest {
    /// Operation name to its subcommand path.
    pub ops: BTreeMap<String, Vec<String>>,
    /// Option field name to its flag, for every field the CLI exposes.
    pub flags: BTreeMap<String, CliFlag>,
    /// Operation name to the non-global option fields its subcommand
    /// accepts. Global flags apply everywhere and are left out.
    pub op_flags: BTreeMap<String, Vec<String>>,
    /// Operation name to where its subcommand takes an output path.
    pub output: BTreeMap<String, OutputKind>,
}

/// Where a subcommand takes its output path.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "cli_echo.ts"))]
pub enum OutputKind {
    /// As a positional argument after the input.
    Positional,
    /// Only through `--output-dir`.
    OutputDir,
    /// Only through `--output`.
    OutputFlag,
    /// The subcommand writes no output to a caller-chosen destination.
    None,
}

/// A derivation the CLI does not follow.
enum Override {
    /// The subcommand path differs from the operation name.
    Path(&'static [&'static str]),
    /// Parsed before the subcommand.
    Global,
    /// The CLI spells the flag differently.
    Flag(&'static str),
    /// No CLI equivalent.
    Dropped,
}

const OVERRIDES: &[(&str, Override)] = &[
    ("config", Override::Global),
    ("preset", Override::Global),
    ("dry_run", Override::Global),
    ("skip_space_check", Override::Global),
    ("extensions", Override::Flag("--ext")),
    ("deep_verify", Override::Flag("--deep")),
    ("verify_after", Override::Dropped),
    // `ctr verify` spells its TMD content-hash pass --full.
    ("content_hashes", Override::Flag("--full")),
    // Legacy alias the runner folds into output_dir before dispatch.
    ("output_dir_cia", Override::Dropped),
    // The CLI only spells the inverse, --no-media-patch.
    ("media_patch", Override::Dropped),
    // RVZ conversions are reached through the Wii/GameCube family.
    ("rvz.compress", Override::Path(&["rvl", "compress"])),
    ("rvz.decompress", Override::Path(&["rvl", "decompress"])),
    ("rvz.migrate", Override::Path(&["rvl", "migrate"])),
    ("playlist.write", Override::Path(&["playlist"])),
    ("info.read", Override::Path(&["info"])),
];

/// Every option field the CLI can carry, with the shape of its value.
const FIELDS: &[(&str, FlagKind)] = &[
    ("config", FlagKind::Value),
    ("preset", FlagKind::Value),
    ("dry_run", FlagKind::Bool),
    ("skip_space_check", FlagKind::Bool),
    ("on_conflict", FlagKind::Value),
    ("recursive", FlagKind::Bool),
    ("output_dir", FlagKind::Value),
    ("output_template", FlagKind::Value),
    ("max_depth", FlagKind::Value),
    ("report", FlagKind::Value),
    ("format", FlagKind::Value),
    ("block_size", FlagKind::Value),
    ("hunk_size", FlagKind::Value),
    ("codecs", FlagKind::List),
    ("mode", FlagKind::Value),
    ("parent", FlagKind::Value),
    ("full", FlagKind::Bool),
    ("fix", FlagKind::Bool),
    ("level", FlagKind::Value),
    ("chunk_size", FlagKind::Value),
    ("skip_verify", FlagKind::Bool),
    ("deep", FlagKind::Bool),
    ("deep_verify", FlagKind::Bool),
    ("algo", FlagKind::Value),
    ("allow_encrypted", FlagKind::Bool),
    ("content_hashes", FlagKind::Bool),
    ("compress", FlagKind::Bool),
    ("cleanup", FlagKind::Bool),
    ("ensure_ticket_exists", FlagKind::Bool),
    ("decrypt", FlagKind::Bool),
    ("output_dir_cia", FlagKind::Value),
    ("keys", FlagKind::Value),
    ("block_size_exp", FlagKind::Value),
    ("key", FlagKind::Value),
    ("extensions", FlagKind::Value),
    ("playlist_mode", FlagKind::Value),
    ("api_base", FlagKind::Value),
    ("input_checksum_min", FlagKind::Value),
    ("input_checksum_max", FlagKind::Value),
    ("inputs", FlagKind::Positional),
    ("platform", FlagKind::Value),
    ("dat_id", FlagKind::Value),
    ("dat_name", FlagKind::Value),
    ("subset", FlagKind::Value),
    ("verify_after", FlagKind::Bool),
    ("quick", FlagKind::Bool),
    ("skip_probe", FlagKind::Bool),
    ("media_patch", FlagKind::Bool),
    ("title", FlagKind::Value),
];

/// Non-global option fields each subcommand accepts, keyed by the joined
/// subcommand path so aliases and shared paths share one entry.
const PATH_FLAGS: &[(&str, &[&str])] = &[
    (
        "chd compress",
        &[
            "codecs",
            "hunk_size",
            "level",
            "max_depth",
            "on_conflict",
            "output_dir",
            "output_template",
            "recursive",
            "report",
        ],
    ),
    (
        "chd extract",
        &[
            "max_depth",
            "on_conflict",
            "output_dir",
            "output_template",
            "parent",
            "recursive",
            "report",
        ],
    ),
    (
        "chd migrate",
        &[
            "codecs",
            "hunk_size",
            "level",
            "max_depth",
            "on_conflict",
            "output_dir",
            "output_template",
            "recursive",
            "report",
        ],
    ),
    (
        "chd to-cso",
        &[
            "block_size",
            "format",
            "max_depth",
            "on_conflict",
            "output_dir",
            "output_template",
            "recursive",
            "report",
        ],
    ),
    ("chd verify", &["fix", "max_depth", "parent", "recursive"]),
    (
        "cso compress",
        &[
            "block_size",
            "format",
            "max_depth",
            "on_conflict",
            "output_dir",
            "output_template",
            "recursive",
            "report",
        ],
    ),
    (
        "cso decompress",
        &[
            "max_depth",
            "on_conflict",
            "output_dir",
            "output_template",
            "recursive",
            "report",
        ],
    ),
    (
        "cso to-chd",
        &[
            "codecs",
            "hunk_size",
            "level",
            "max_depth",
            "on_conflict",
            "output_dir",
            "output_template",
            "recursive",
            "report",
        ],
    ),
    (
        "cso verify",
        &["content_hashes", "full", "max_depth", "recursive"],
    ),
    (
        "ctr cdn-to-cia",
        &[
            "cleanup",
            "compress",
            "decrypt",
            "ensure_ticket_exists",
            "on_conflict",
            "output_dir",
            "recursive",
        ],
    ),
    (
        "ctr compress",
        &[
            "allow_encrypted",
            "level",
            "max_depth",
            "on_conflict",
            "output_dir",
            "output_template",
            "recursive",
        ],
    ),
    (
        "ctr convert",
        &[
            "max_depth",
            "on_conflict",
            "output_dir",
            "output_template",
            "recursive",
        ],
    ),
    (
        "ctr decompress",
        &[
            "max_depth",
            "on_conflict",
            "output_dir",
            "output_template",
            "recursive",
        ],
    ),
    (
        "ctr decrypt",
        &[
            "max_depth",
            "on_conflict",
            "output_dir",
            "output_template",
            "recursive",
        ],
    ),
    (
        "ctr encrypt",
        &[
            "max_depth",
            "on_conflict",
            "output_dir",
            "output_template",
            "recursive",
        ],
    ),
    ("ctr generate-cdn-ticket", &[]),
    // The runner reads `content_hashes` here; `full` is the same `--full`
    // flag and would echo it twice.
    ("ctr verify", &["content_hashes", "max_depth", "recursive"]),
    ("cue merge", &["on_conflict"]),
    (
        "cue to-cso",
        &[
            "format",
            "max_depth",
            "on_conflict",
            "output_dir",
            "recursive",
        ],
    ),
    (
        "cue to-iso",
        &["max_depth", "on_conflict", "output_dir", "recursive"],
    ),
    (
        "dat fixdat",
        &[
            "api_base",
            "dat_id",
            "dat_name",
            "max_depth",
            "on_conflict",
            "platform",
            "subset",
        ],
    ),
    (
        "dat identify",
        &[
            "algo",
            "api_base",
            "input_checksum_max",
            "input_checksum_min",
        ],
    ),
    (
        "dat rename",
        &[
            "api_base",
            "max_depth",
            "on_conflict",
            "recursive",
            "report",
        ],
    ),
    (
        "dat scan",
        &["algo", "api_base", "max_depth", "quick", "report"],
    ),
    (
        "dat verify",
        &[
            "algo",
            "api_base",
            "input_checksum_max",
            "input_checksum_min",
            "max_depth",
            "quick",
            "recursive",
            "report",
        ],
    ),
    (
        "dol compress",
        &[
            "chunk_size",
            "level",
            "max_depth",
            "on_conflict",
            "output_dir",
            "output_template",
            "recursive",
            "report",
        ],
    ),
    (
        "dol decompress",
        &[
            "max_depth",
            "on_conflict",
            "output_dir",
            "output_template",
            "recursive",
            "report",
        ],
    ),
    (
        "dol migrate",
        &["chunk_size", "level", "recursive", "skip_verify"],
    ),
    (
        "dol verify",
        &["content_hashes", "full", "max_depth", "recursive"],
    ),
    ("hash", &["algo", "max_depth", "recursive", "report"]),
    ("info", &["keys"]),
    (
        "nds decrypt",
        &[
            "max_depth",
            "on_conflict",
            "output_dir",
            "output_template",
            "recursive",
            "report",
        ],
    ),
    (
        "nds encrypt",
        &[
            "max_depth",
            "on_conflict",
            "output_dir",
            "output_template",
            "recursive",
            "report",
        ],
    ),
    (
        "nx compress",
        &[
            "block_size_exp",
            "keys",
            "level",
            "max_depth",
            "mode",
            "on_conflict",
            "output_dir",
            "output_template",
            "recursive",
            "report",
        ],
    ),
    (
        "nx decompress",
        &[
            "keys",
            "max_depth",
            "on_conflict",
            "output_dir",
            "output_template",
            "recursive",
            "report",
        ],
    ),
    ("nx merge", &["format", "keys", "on_conflict"]),
    ("nx split", &["keys", "on_conflict", "output_dir"]),
    ("nx verify", &["keys", "max_depth", "recursive"]),
    (
        "playlist",
        &[
            "extensions",
            "max_depth",
            "on_conflict",
            "output_dir",
            "playlist_mode",
        ],
    ),
    (
        "ps3 decrypt",
        &[
            "key",
            "max_depth",
            "on_conflict",
            "output_dir",
            "output_template",
            "recursive",
            "report",
            "skip_probe",
        ],
    ),
    ("psp extract", &[]),
    ("psp to-iso", &["on_conflict", "output_template", "report"]),
    (
        "rvl compress",
        &[
            "chunk_size",
            "level",
            "max_depth",
            "on_conflict",
            "output_dir",
            "output_template",
            "recursive",
            "report",
        ],
    ),
    (
        "rvl decompress",
        &[
            "max_depth",
            "on_conflict",
            "output_dir",
            "output_template",
            "recursive",
            "report",
        ],
    ),
    (
        "rvl migrate",
        &[
            "chunk_size",
            "deep",
            "deep_verify",
            "level",
            "recursive",
            "skip_verify",
        ],
    ),
    (
        "rvl verify",
        &["content_hashes", "full", "max_depth", "recursive"],
    ),
    ("vita extract", &[]),
    ("wup compress", &["key", "level", "on_conflict"]),
    ("wup decrypt", &["on_conflict"]),
    ("wup verify", &["key", "max_depth", "recursive"]),
    ("xbox convert", &[]),
    ("xbox extract", &[]),
    ("xenon compress", &[]),
    ("xenon convert", &["on_conflict", "title"]),
    ("xenon extract", &[]),
    ("xenon verify", &[]),
];

/// Subcommands that do not take a positional output, keyed by the joined
/// subcommand path.
const PATH_OUTPUT: &[(&str, OutputKind)] = &[
    ("chd verify", OutputKind::None),
    ("cso verify", OutputKind::None),
    ("ctr verify", OutputKind::None),
    ("dat fixdat", OutputKind::OutputFlag),
    ("dat identify", OutputKind::None),
    ("dat rename", OutputKind::None),
    ("dat scan", OutputKind::None),
    ("dat verify", OutputKind::None),
    ("dol verify", OutputKind::None),
    ("hash", OutputKind::None),
    ("info", OutputKind::None),
    ("nx merge", OutputKind::OutputFlag),
    ("nx split", OutputKind::OutputDir),
    ("nx verify", OutputKind::None),
    ("playlist", OutputKind::OutputDir),
    ("rvl verify", OutputKind::None),
    ("wup compress", OutputKind::OutputFlag),
    ("wup decrypt", OutputKind::OutputFlag),
    ("wup verify", OutputKind::None),
    ("xenon verify", OutputKind::None),
];

fn find_override(key: &str) -> Option<&'static Override> {
    OVERRIDES.iter().find(|(k, _)| *k == key).map(|(_, o)| o)
}

/// The subcommand path for an operation: the name split on `.`, with `_`
/// turned into `-`, unless [`OVERRIDES`] says otherwise.
pub fn cli_path(op: &str) -> Vec<String> {
    if let Some(Override::Path(path)) = find_override(op) {
        return path.iter().map(|s| (*s).to_string()).collect();
    }
    op.split('.').map(|seg| seg.replace('_', "-")).collect()
}

/// The CLI flag for an option field, or `None` when the CLI has no
/// equivalent.
pub fn cli_flag(field: &str) -> Option<CliFlag> {
    let (_, kind) = FIELDS.iter().find(|(f, _)| *f == field)?;
    let ovr = find_override(field);
    if matches!(ovr, Some(Override::Dropped)) {
        return None;
    }
    Some(CliFlag {
        flag: match ovr {
            Some(Override::Flag(flag)) => (*flag).to_string(),
            _ => format!("--{}", field.replace('_', "-")),
        },
        global: matches!(ovr, Some(Override::Global)),
        kind: *kind,
    })
}

/// Where a subcommand takes its output path. Positional unless
/// [`PATH_OUTPUT`] says otherwise.
fn output_kind(path: &str) -> OutputKind {
    PATH_OUTPUT
        .iter()
        .find(|(p, _)| *p == path)
        .map_or(OutputKind::Positional, |(_, kind)| *kind)
}

/// The full CLI-echo metadata for every runner operation.
pub fn manifest() -> CliEchoManifest {
    let mut ops = BTreeMap::new();
    let mut op_flags = BTreeMap::new();
    let mut output = BTreeMap::new();
    for op in operation_names() {
        let path = cli_path(op);
        let key = path.join(" ");
        let fields = PATH_FLAGS
            .iter()
            .find(|(p, _)| *p == key)
            .map(|(_, f)| f.iter().map(|s| (*s).to_string()).collect())
            .unwrap_or_default();
        op_flags.insert((*op).to_string(), fields);
        output.insert((*op).to_string(), output_kind(&key));
        ops.insert((*op).to_string(), path);
    }
    let flags = FIELDS
        .iter()
        .filter_map(|(field, _)| cli_flag(field).map(|f| ((*field).to_string(), f)))
        .collect();
    CliEchoManifest {
        ops,
        flags,
        op_flags,
        output,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_split_on_dots_and_kebab_segments() {
        assert_eq!(cli_path("chd.compress"), ["chd", "compress"]);
        assert_eq!(cli_path("cue.to_iso"), ["cue", "to-iso"]);
        assert_eq!(cli_path("ctr.cdn_to_cia"), ["ctr", "cdn-to-cia"]);
        assert_eq!(cli_path("playlist.write"), ["playlist"]);
        assert_eq!(cli_path("hash"), ["hash"]);
    }

    #[test]
    fn flags_kebab_with_overrides() {
        let on_conflict = cli_flag("on_conflict").unwrap();
        assert_eq!(on_conflict.flag, "--on-conflict");
        assert!(!on_conflict.global);
        assert_eq!(on_conflict.kind, FlagKind::Value);
        assert!(cli_flag("dry_run").unwrap().global);
        assert_eq!(cli_flag("extensions").unwrap().flag, "--ext");
        assert!(cli_flag("verify_after").is_none());
        assert!(cli_flag("nonexistent").is_none());
    }

    #[test]
    fn manifest_covers_every_operation() {
        let manifest = manifest();
        assert_eq!(manifest.ops.len(), operation_names().len());
        assert_eq!(manifest.op_flags.len(), manifest.ops.len());
        assert!(manifest.flags.contains_key("recursive"));
    }
}
