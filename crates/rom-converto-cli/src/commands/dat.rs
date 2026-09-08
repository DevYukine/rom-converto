use crate::commands::{BatchArgs, ConflictPolicyArg};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::commands::support::{DispatchCtx, require_dir};
use crate::util::{CliProgress, ensure_input_exists, policy_name, resolve_policy};
use anyhow::Result;
use log::info;
use rom_converto_lib::runner::models::{
    DatMatchData, RunData, RunOptions, RunRequest, RunResponse, RunRow,
};
use rom_converto_lib::runner::run_request;
use rom_converto_lib::util::FileStatus;

/// Identify, verify and rename ROMs against the Playmatch database
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum DatCommands {
    Verify(DatVerifyCommand),
    Scan(DatScanCommand),
    Rename(DatRenameCommand),
    Identify(DatIdentifyCommand),
    Fixdat(DatFixdatCommand),
}

/// Verify a ROM's decoded content hashes against the Playmatch database
///
/// Container formats (chd, rvz, wbfs, cso, zso, z3ds, gcz, wia, nkit) are hashed on their
/// decoded inner stream, so the verdict matches the original ROM or disc
/// image regardless of compression. Multi-track discs check every track.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto dat verify game.chd\n  Whole directory: rom-converto dat verify -R ./roms --report verify.json\n  Extra digests:   rom-converto dat verify game.rvz --algo crc32,sha1,sha256\n"
)]
pub struct DatVerifyCommand {
    /// Input file path, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Comma-separated digests to compute: crc32, sha1, md5, sha256
    #[arg(long, value_name = "ALGOS", default_value = "crc32,sha1")]
    pub algo: String,

    /// Verify every file in INPUT, descending into subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    #[command(flatten)]
    pub batch: BatchArgs,

    /// Playmatch API base URL (defaults to the public instance)
    #[arg(long = "api-base", value_name = "URL")]
    pub api_base: Option<String>,

    /// Minimum checksum tier always computed before consulting Playmatch: crc32, md5, sha1, sha256 (default: crc32)
    #[arg(long = "input-checksum-min", value_name = "ALGO")]
    pub input_checksum_min: Option<String>,

    /// Maximum checksum tier escalation may reach when the floor tier alone does not verify: crc32, md5, sha1, sha256 (default: sha256)
    #[arg(long = "input-checksum-max", value_name = "ALGO")]
    pub input_checksum_max: Option<String>,

    /// Trust a zip's own CRC32 for an eligible cartridge image instead of extracting and hashing it. Falls back automatically when the archive checksum alone does not verify
    #[arg(long, default_value_t = false)]
    pub quick: bool,
}

#[cfg(test)]
mod verify_tests {
    use super::*;

    #[derive(Parser, Debug)]
    struct Harness {
        #[command(subcommand)]
        cmd: Wrapper,
    }

    #[derive(clap::Subcommand, Debug)]
    enum Wrapper {
        Verify(DatVerifyCommand),
    }

    fn parse(args: &[&str]) -> DatVerifyCommand {
        let h = Harness::parse_from(args);
        let Wrapper::Verify(c) = h.cmd;
        c
    }

    #[test]
    fn defaults() {
        let c = parse(&["bin", "verify", "game.chd"]);
        assert_eq!(c.algo, "crc32,sha1");
        assert!(!c.recursive);
        assert_eq!(c.batch.max_depth, None);
        assert_eq!(c.batch.report, None);
        assert_eq!(c.api_base, None);
        assert_eq!(c.input_checksum_min, None);
        assert_eq!(c.input_checksum_max, None);
        assert!(!c.quick);
    }

    #[test]
    fn parses_quick_flag() {
        let c = parse(&["bin", "verify", "f", "--quick"]);
        assert!(c.quick);
    }

    #[test]
    fn parses_checksum_bounds() {
        let c = parse(&[
            "bin",
            "verify",
            "f",
            "--input-checksum-min",
            "sha1",
            "--input-checksum-max",
            "md5",
        ]);
        assert_eq!(c.input_checksum_min.as_deref(), Some("sha1"));
        assert_eq!(c.input_checksum_max.as_deref(), Some("md5"));
    }

    #[test]
    fn parses_recursive_depth_and_algo() {
        let c = parse(&[
            "bin",
            "verify",
            "roms",
            "-R",
            "--max-depth",
            "2",
            "--algo",
            "sha256",
        ]);
        assert!(c.recursive);
        assert_eq!(c.batch.max_depth, Some(2));
        assert_eq!(c.algo, "sha256");
    }

    #[test]
    fn max_depth_requires_recursive() {
        let result = Harness::try_parse_from(["bin", "verify", "roms", "--max-depth", "2"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_report_and_api_base() {
        let c = parse(&[
            "bin",
            "verify",
            "f",
            "--report",
            "out.json",
            "--api-base",
            "https://example.test/api/v2",
        ]);
        assert_eq!(c.batch.report, Some(PathBuf::from("out.json")));
        assert_eq!(c.api_base.as_deref(), Some("https://example.test/api/v2"));
    }
}

/// Batch-identify a library and summarize matched, misnamed and unknown files
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  Scan directory: rom-converto dat scan ./roms\n  With report:    rom-converto dat scan ./roms --report scan.csv\n  Limit depth:    rom-converto dat scan ./roms --max-depth 2\n"
)]
pub struct DatScanCommand {
    /// Directory to scan
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Comma-separated digests to compute per file: crc32, sha1, md5, sha256.
    /// Size plus CRC32 identifies almost everything; raise this only when a
    /// match needs a stronger digest
    #[arg(long, value_name = "ALGOS", default_value = "crc32")]
    pub algo: String,

    /// Maximum directory depth. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N")]
    pub max_depth: Option<usize>,

    /// Write a run report to FILE. Format inferred from the extension: .csv, .json, .html or .htm. Unknown extensions default to JSON. The file is overwritten directly
    #[arg(long = "report", value_name = "FILE")]
    pub report: Option<PathBuf>,

    /// Playmatch API base URL (defaults to the public instance)
    #[arg(long = "api-base", value_name = "URL")]
    pub api_base: Option<String>,

    /// Trust a zip's own CRC32 for an eligible cartridge image instead of extracting and hashing it. Falls back automatically when the archive checksum alone does not verify
    #[arg(long, default_value_t = false)]
    pub quick: bool,
}

#[cfg(test)]
mod scan_tests {
    use super::*;

    #[derive(Parser, Debug)]
    struct Harness {
        #[command(subcommand)]
        cmd: Wrapper,
    }

    #[derive(clap::Subcommand, Debug)]
    enum Wrapper {
        Scan(DatScanCommand),
    }

    fn parse(args: &[&str]) -> DatScanCommand {
        let h = Harness::parse_from(args);
        let Wrapper::Scan(c) = h.cmd;
        c
    }

    #[test]
    fn defaults() {
        let c = parse(&["bin", "scan", "roms"]);
        assert_eq!(c.max_depth, None);
        assert_eq!(c.report, None);
        assert_eq!(c.api_base, None);
        assert!(!c.quick);
    }

    #[test]
    fn parses_quick_flag() {
        let c = parse(&["bin", "scan", "roms", "--quick"]);
        assert!(c.quick);
    }

    #[test]
    fn max_depth_does_not_require_recursive_flag() {
        // scan has no --recursive flag; --max-depth is bare, like playlist.
        let c = parse(&["bin", "scan", "roms", "--max-depth", "2"]);
        assert_eq!(c.max_depth, Some(2));
    }

    #[test]
    fn parses_report_and_api_base() {
        let c = parse(&[
            "bin",
            "scan",
            "roms",
            "--report",
            "scan.csv",
            "--api-base",
            "https://example.test/api/v2",
        ]);
        assert_eq!(c.report, Some(PathBuf::from("scan.csv")));
        assert_eq!(c.api_base.as_deref(), Some("https://example.test/api/v2"));
    }
}

/// Rename ROMs to their canonical database names
///
/// Hash-verified matches only.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  Preview:       rom-converto dat rename ./roms --dry-run\n  Rename all:    rom-converto dat rename ./roms\n  One file:      rom-converto dat rename game.chd\n"
)]
pub struct DatRenameCommand {
    /// Input file path, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Rename every file in INPUT, descending into subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

    /// What to do when an output already exists: error, overwrite, skip, or rename to a numbered sibling
    #[arg(long = "on-conflict", value_enum, value_name = "MODE")]
    pub on_conflict: Option<ConflictPolicyArg>,

    /// Alias for --on-conflict overwrite
    #[arg(short = 'f', long = "force", conflicts_with = "on_conflict")]
    pub force: bool,

    /// Write a run report to FILE. Format inferred from the extension: .csv, .json, .html or .htm. Unknown extensions default to JSON. The file is overwritten directly
    #[arg(long = "report", value_name = "FILE")]
    pub report: Option<PathBuf>,

    /// Playmatch API base URL (defaults to the public instance)
    #[arg(long = "api-base", value_name = "URL")]
    pub api_base: Option<String>,
}

#[cfg(test)]
mod rename_tests {
    use super::*;

    #[derive(Parser, Debug)]
    struct Harness {
        #[command(subcommand)]
        cmd: Wrapper,
    }

    #[derive(clap::Subcommand, Debug)]
    enum Wrapper {
        Rename(DatRenameCommand),
    }

    fn parse(args: &[&str]) -> DatRenameCommand {
        let h = Harness::parse_from(args);
        let Wrapper::Rename(c) = h.cmd;
        c
    }

    #[test]
    fn defaults() {
        let c = parse(&["bin", "rename", "roms"]);
        assert!(!c.recursive);
        assert_eq!(c.max_depth, None);
        assert_eq!(c.on_conflict, None);
        assert!(!c.force);
        assert_eq!(c.report, None);
        assert_eq!(c.api_base, None);
    }

    #[test]
    fn max_depth_requires_recursive() {
        let result = Harness::try_parse_from(["bin", "rename", "roms", "--max-depth", "2"]);
        assert!(result.is_err());
    }

    #[test]
    fn force_conflicts_with_on_conflict() {
        let result =
            Harness::try_parse_from(["bin", "rename", "roms", "-f", "--on-conflict", "skip"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_on_conflict_and_force() {
        let c = parse(&["bin", "rename", "roms", "--on-conflict", "overwrite"]);
        assert_eq!(c.on_conflict, Some(ConflictPolicyArg::Overwrite));

        let c = parse(&["bin", "rename", "roms", "-f"]);
        assert!(c.force);
    }
}

/// Look up a single file and print everything the database knows about it
///
/// Unlike verify, a filename-and-size match is shown as a weak match rather
/// than rejected, so near-misses are still informative.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  Identify:      rom-converto dat identify game.chd\n  All digests:   rom-converto dat identify game.iso --algo crc32,sha1,md5,sha256\n"
)]
pub struct DatIdentifyCommand {
    /// Input file path
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Comma-separated digests to compute: crc32, sha1, md5, sha256
    #[arg(long, value_name = "ALGOS", default_value = "crc32,sha1")]
    pub algo: String,

    /// Playmatch API base URL (defaults to the public instance)
    #[arg(long = "api-base", value_name = "URL")]
    pub api_base: Option<String>,

    /// Minimum checksum tier always computed before consulting Playmatch: crc32, md5, sha1, sha256 (default: crc32)
    #[arg(long = "input-checksum-min", value_name = "ALGO")]
    pub input_checksum_min: Option<String>,

    /// Maximum checksum tier escalation may reach when the floor tier alone does not verify: crc32, md5, sha1, sha256 (default: sha256)
    #[arg(long = "input-checksum-max", value_name = "ALGO")]
    pub input_checksum_max: Option<String>,
}

#[cfg(test)]
mod identify_tests {
    use super::*;

    #[derive(Parser, Debug)]
    struct Harness {
        #[command(subcommand)]
        cmd: Wrapper,
    }

    #[derive(clap::Subcommand, Debug)]
    enum Wrapper {
        Identify(DatIdentifyCommand),
    }

    fn parse(args: &[&str]) -> DatIdentifyCommand {
        let h = Harness::parse_from(args);
        let Wrapper::Identify(c) = h.cmd;
        c
    }

    #[test]
    fn defaults() {
        let c = parse(&["bin", "identify", "game.chd"]);
        assert_eq!(c.algo, "crc32,sha1");
        assert_eq!(c.api_base, None);
        assert_eq!(c.input_checksum_min, None);
        assert_eq!(c.input_checksum_max, None);
    }

    #[test]
    fn parses_checksum_bounds() {
        let c = parse(&[
            "bin",
            "identify",
            "game.chd",
            "--input-checksum-min",
            "md5",
            "--input-checksum-max",
            "sha1",
        ]);
        assert_eq!(c.input_checksum_min.as_deref(), Some("md5"));
        assert_eq!(c.input_checksum_max.as_deref(), Some("sha1"));
    }

    #[test]
    fn parses_algo_and_api_base() {
        let c = parse(&[
            "bin",
            "identify",
            "game.iso",
            "--algo",
            "crc32,sha1,md5,sha256",
            "--api-base",
            "https://example.test/api/v2",
        ]);
        assert_eq!(c.algo, "crc32,sha1,md5,sha256");
        assert_eq!(c.api_base.as_deref(), Some("https://example.test/api/v2"));
    }
}

/// Build a Logiqx fixdat of the database entries missing from a local library
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  By platform:   rom-converto dat fixdat ./roms --platform \"PlayStation\" -o missing.dat\n  Exact DAT:     rom-converto dat fixdat ./roms --dat-id 5c1e... -o missing.dat\n  Narrow it:     rom-converto dat fixdat ./roms --platform \"PlayStation\" --dat-name \"...\" -o missing.dat\n"
)]
pub struct DatFixdatCommand {
    /// Directory containing the local library
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output fixdat path (Logiqx XML)
    #[arg(short = 'o', long = "output", value_name = "FILE")]
    pub output: PathBuf,

    /// Platform name to resolve via the database (such as a console name)
    #[arg(
        long,
        value_name = "NAME",
        required_unless_present = "dat_id",
        conflicts_with = "dat_id"
    )]
    pub platform: Option<String>,

    /// Exact DAT file id (uuid); skips platform/name resolution
    #[arg(long = "dat-id", value_name = "UUID")]
    pub dat_id: Option<String>,

    /// Filter candidate DATs by name substring
    #[arg(
        long = "dat-name",
        value_name = "NAME",
        requires = "platform",
        conflicts_with = "dat_id"
    )]
    pub dat_name: Option<String>,

    /// Filter candidate DATs by subset
    #[arg(
        long,
        value_name = "SUBSET",
        requires = "platform",
        conflicts_with = "dat_id"
    )]
    pub subset: Option<String>,

    /// Maximum directory depth. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N")]
    pub max_depth: Option<usize>,

    /// What to do when an output already exists: error, overwrite, skip, or rename to a numbered sibling
    #[arg(long = "on-conflict", value_enum, value_name = "MODE")]
    pub on_conflict: Option<ConflictPolicyArg>,

    /// Alias for --on-conflict overwrite
    #[arg(short = 'f', long = "force", conflicts_with = "on_conflict")]
    pub force: bool,

    /// Playmatch API base URL (defaults to the public instance)
    #[arg(long = "api-base", value_name = "URL")]
    pub api_base: Option<String>,
}

#[cfg(test)]
mod fixdat_tests {
    use super::*;

    #[derive(Parser, Debug)]
    struct Harness {
        #[command(subcommand)]
        cmd: Wrapper,
    }

    #[derive(clap::Subcommand, Debug)]
    enum Wrapper {
        Fixdat(DatFixdatCommand),
    }

    fn parse(args: &[&str]) -> DatFixdatCommand {
        let h = Harness::parse_from(args);
        let Wrapper::Fixdat(c) = h.cmd;
        c
    }

    #[test]
    fn requires_platform_or_dat_id() {
        let result = Harness::try_parse_from(["bin", "fixdat", "roms", "-o", "missing.dat"]);
        assert!(result.is_err());
    }

    #[test]
    fn platform_and_dat_id_are_mutually_exclusive() {
        let result = Harness::try_parse_from([
            "bin",
            "fixdat",
            "roms",
            "-o",
            "missing.dat",
            "--platform",
            "PlayStation",
            "--dat-id",
            "abc",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_with_platform() {
        let c = parse(&[
            "bin",
            "fixdat",
            "roms",
            "-o",
            "missing.dat",
            "--platform",
            "PlayStation",
        ]);
        assert_eq!(c.platform.as_deref(), Some("PlayStation"));
        assert_eq!(c.output, PathBuf::from("missing.dat"));
        assert!(!c.force);
    }

    #[test]
    fn parses_with_dat_id() {
        let c = parse(&[
            "bin",
            "fixdat",
            "roms",
            "-o",
            "missing.dat",
            "--dat-id",
            "abc-123",
        ]);
        assert_eq!(c.dat_id.as_deref(), Some("abc-123"));
        assert_eq!(c.platform, None);
    }

    #[test]
    fn dat_name_and_subset_require_platform() {
        let result = Harness::try_parse_from([
            "bin",
            "fixdat",
            "roms",
            "-o",
            "missing.dat",
            "--dat-id",
            "abc",
            "--dat-name",
            "foo",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn force_conflicts_with_on_conflict() {
        let result = Harness::try_parse_from([
            "bin",
            "fixdat",
            "roms",
            "-o",
            "missing.dat",
            "--dat-id",
            "abc",
            "-f",
            "--on-conflict",
            "skip",
        ]);
        assert!(result.is_err());
    }
}

/// Runs one `dat` subcommand.
pub async fn run(command: DatCommands, ctx: DispatchCtx<'_>) -> Result<()> {
    let DispatchCtx {
        progress,
        total_progress,
        effective,
        dry_run,
        cancel,
        cache,
        config,
        preset,
        ..
    } = ctx;
    let dat = &effective.dat;
    let mut req = RunRequest {
        schema: None,
        operation: String::new(),
        input: None,
        output: None,
        config,
        preset,
        options: RunOptions {
            api_base: dat.api_base.clone(),
            report: dat.report.clone(),
            input_checksum_min: dat.input_checksum_min.clone(),
            input_checksum_max: dat.input_checksum_max.clone(),
            ..Default::default()
        },
        dry_run,
        ctx: Default::default(),
    };
    req.ctx.hash_cache = Some(cache.clone());
    let opts = &mut req.options;
    let mut recursive = false;
    match command {
        DatCommands::Verify(cmd) => {
            if cmd.recursive {
                require_dir(&cmd.input)?;
            } else {
                ensure_input_exists(&cmd.input)?;
            }
            recursive = cmd.recursive;
            req.operation = "dat.verify".to_string();
            req.input = Some(cmd.input);
            opts.algo = Some(cmd.algo);
            opts.max_depth = cmd.batch.max_depth;
            opts.quick = Some(cmd.quick);
            fill(&mut opts.api_base, cmd.api_base);
            fill(&mut opts.report, cmd.batch.report);
            fill(&mut opts.input_checksum_min, cmd.input_checksum_min);
            fill(&mut opts.input_checksum_max, cmd.input_checksum_max);
        }
        DatCommands::Scan(cmd) => {
            require_dir(&cmd.input)?;
            req.operation = "dat.scan".to_string();
            req.input = Some(cmd.input);
            opts.algo = Some(cmd.algo);
            opts.max_depth = cmd.max_depth;
            opts.quick = Some(cmd.quick);
            fill(&mut opts.api_base, cmd.api_base);
            fill(&mut opts.report, cmd.report);
        }
        DatCommands::Rename(cmd) => {
            if cmd.recursive {
                require_dir(&cmd.input)?;
            } else {
                ensure_input_exists(&cmd.input)?;
            }
            req.operation = "dat.rename".to_string();
            req.input = Some(cmd.input);
            opts.max_depth = cmd.max_depth;
            opts.on_conflict = Some(
                policy_name(resolve_policy(
                    cmd.on_conflict,
                    cmd.force,
                    rom_converto_lib::util::ConflictPolicy::Error,
                ))
                .to_string(),
            );
            fill(&mut opts.api_base, cmd.api_base);
            fill(&mut opts.report, cmd.report);
        }
        DatCommands::Identify(cmd) => {
            ensure_input_exists(&cmd.input)?;
            req.operation = "dat.identify".to_string();
            req.input = Some(cmd.input);
            opts.algo = Some(cmd.algo);
            fill(&mut opts.api_base, cmd.api_base);
            fill(&mut opts.input_checksum_min, cmd.input_checksum_min);
            fill(&mut opts.input_checksum_max, cmd.input_checksum_max);
        }
        DatCommands::Fixdat(cmd) => {
            require_dir(&cmd.input)?;
            req.operation = "dat.fixdat".to_string();
            req.input = Some(cmd.input);
            req.output = Some(cmd.output);
            opts.platform = cmd.platform;
            opts.dat_id = cmd.dat_id;
            opts.dat_name = cmd.dat_name;
            opts.subset = cmd.subset;
            opts.max_depth = cmd.max_depth;
            opts.on_conflict = Some(
                policy_name(resolve_policy(
                    cmd.on_conflict,
                    cmd.force,
                    rom_converto_lib::util::ConflictPolicy::Error,
                ))
                .to_string(),
            );
            fill(&mut opts.api_base, cmd.api_base);
        }
    }
    let reporter = CliProgress {
        file: &progress,
        total: &total_progress,
        print_row,
        count_units: true,
    };
    let response = run_request(req, &reporter, cancel).await;
    total_progress.finish_bar();
    let response = response.map_err(cli_flag_names)?;
    // A single input the runner could not even digest (an unreadable file, an
    // archive with no image inside) is an error, not a verdict.
    if !recursive
        && let Some(RunData::DatMatch(data)) = &response.data
        && data.verdict == "failed"
    {
        anyhow::bail!(
            "{}",
            data.error.as_deref().unwrap_or("could not read the input")
        );
    }
    if !recursive && let Some(RunData::DatMatch(data)) = &response.data {
        match data.kind {
            "identify" => print_identify(data),
            _ => print_verdict(data),
        }
    }
    print_summary(&response);
    Ok(())
}

fn fill<T>(slot: &mut Option<T>, value: Option<T>) {
    if value.is_some() {
        *slot = value;
    }
}

/// The runner names its narrowing options `dat_name` and `subset`; a CLI user
/// only ever sees them as flags, so the hint is respelled on the way out.
fn cli_flag_names(err: anyhow::Error) -> anyhow::Error {
    let text = err.to_string();
    match text.contains("dat_name or subset") {
        true => anyhow::anyhow!(text.replace("dat_name or subset", "--dat-name or --subset")),
        false => err,
    }
}

/// Streamed rows: verify verdicts in batch form and rename actions. Scan
/// rows only feed the final summary.
fn print_row(row: &RunRow) {
    match row {
        RunRow::DatMatch(data) if data.error.is_some() => {
            info!("{}: {}", data.path.display(), data.verdict);
        }
        RunRow::DatMatch(data) => print_verdict(data),
        RunRow::DatRename(row) => match (row.action, &row.to) {
            ("would_rename", Some(to)) => {
                info!("Would rename {} -> {}", row.from.display(), to.display());
            }
            ("renamed", Some(to)) => info!("{} -> {}", row.from.display(), to.display()),
            _ => {}
        },
        _ => {}
    }
}

fn print_summary(response: &RunResponse) {
    match &response.data {
        Some(RunData::DatVerify(data)) => info!("{} verified, {} hint", data.verified, data.hint),
        Some(RunData::DatScan(data)) => info!(
            "{} matched, {} misnamed, {} hint, {} unknown, {} unsupported, {} failed",
            data.matched, data.misnamed, data.hint, data.unknown, data.unsupported, data.failed
        ),
        Some(RunData::DatRename(data)) => {
            let already = data
                .rows
                .iter()
                .filter(|r| r.action == "already_canonical")
                .count();
            info!(
                "{} renamed, {} already canonical, {} skipped",
                data.renamed,
                already,
                data.skipped + data.failed - already
            );
        }
        Some(RunData::FixdatPlan(data)) => {
            info!(
                "Using DAT {} (version {})",
                data.dat_file.name, data.dat_file.current_version
            );
            info!(
                "Dry run: missing {} of {} games ({} files); would write {}",
                data.missing_count,
                data.total_games,
                data.missing_files,
                data.output.display()
            );
        }
        Some(RunData::FixdatWritten(data)) => {
            info!(
                "Using DAT {} (version {})",
                data.dat_file.name, data.dat_file.current_version
            );
            info!(
                "Missing {} of {} games ({} files); wrote {}",
                data.missing_count,
                data.total_games,
                data.missing_files,
                data.output.display()
            );
        }
        _ => {
            if let Some(record) = response.records.first()
                && record.status == FileStatus::Skipped
            {
                info!("Skipped, output exists: {}", record.output_path);
            }
        }
    }
}

/// One verify verdict line, with a per-track summary when the unit is a
/// track set.
fn print_verdict(data: &DatMatchData) {
    let name = data.path.display();
    match data.verdict.as_str() {
        "verified" => {
            let algo = data
                .match_algo
                .as_deref()
                .map(|a| format!(" [{a}]"))
                .unwrap_or_default();
            let game = data.game_name.as_deref().unwrap_or("");
            let mut extra = Vec::new();
            if let Some(p) = &data.platform {
                extra.push(format!("platform: {p}"));
            }
            if let Some(g) = &data.signature_group {
                extra.push(format!("group: {g}"));
            }
            if let Some(d) = &data.dat_file {
                extra.push(format!("DAT: {d}"));
            }
            if let Some(v) = &data.dat_version {
                extra.push(format!("version: {v}"));
            }
            let suffix = if extra.is_empty() {
                String::new()
            } else {
                format!("  ({})", extra.join(", "))
            };
            info!("{name}: verified{algo} -> {game}{suffix}");
            if let Some(d) = data.track_detail() {
                info!("  {d}");
            }
        }
        "hint" => {
            let game = data.game_name.as_deref().unwrap_or("?");
            info!("{name}: not verified  (name+size hint only: \"{game}\")");
        }
        "unsupported" => info!("{name}: unsupported (decompress the file first)"),
        "failed" => info!("{name}: failed"),
        _ => info!("{name}: no match"),
    }
}

fn print_identify(data: &DatMatchData) {
    match data.verdict.as_str() {
        "verified" => info!(
            "Match: {} (verified)",
            data.match_algo
                .as_deref()
                .unwrap_or_default()
                .to_uppercase()
        ),
        "hint" => info!("Match: name+size (weak)"),
        _ => {
            info!("No match");
            return;
        }
    }
    if let Some(g) = &data.game_name {
        info!(
            "Game:  {g}      platform: {}   group: {}",
            data.platform.as_deref().unwrap_or("?"),
            data.signature_group.as_deref().unwrap_or("?")
        );
    }
    if let Some(i) = data
        .matched
        .as_ref()
        .and_then(|m| m.dat_file_import.as_ref())
    {
        info!("DAT:   version {}", i.version);
    }
    if !data.external_ids.is_empty() {
        let ids: Vec<String> = data
            .external_ids
            .iter()
            .map(|e| format!("{} {}", e.provider, e.id))
            .collect();
        info!("IDs:   {}", ids.join(", "));
    }
}
