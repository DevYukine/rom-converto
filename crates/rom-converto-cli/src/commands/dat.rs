use crate::commands::ConflictPolicyArg;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Identify, verify and rename ROMs against the Playmatch database.
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum DatCommands {
    Verify(DatVerifyCommand),
    Scan(DatScanCommand),
    Rename(DatRenameCommand),
    Identify(DatIdentifyCommand),
    Fixdat(DatFixdatCommand),
}

/// Verify a ROM's decoded content hashes against the Playmatch database.
///
/// Container formats (chd, rvz, wbfs, cso, zso, z3ds) are hashed on their
/// decoded inner stream, so the verdict matches the original disc or cart
/// image regardless of compression. Multi-track discs check every track.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  Single file:   rom-converto dat verify game.chd\n  Whole folder:  rom-converto dat verify -R ./roms --report verify.json\n  Extra digests: rom-converto dat verify game.rvz --algo crc32,sha1,sha256\n"
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

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited.
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

    /// Write a run report to FILE. Format inferred from the extension: .csv, .json, .html or .htm. Unknown extensions default to JSON. The file is overwritten directly.
    #[arg(long = "report", value_name = "FILE")]
    pub report: Option<PathBuf>,

    /// Playmatch API base URL (defaults to the public instance)
    #[arg(long = "api-base", value_name = "URL")]
    pub api_base: Option<String>,
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
        assert_eq!(c.max_depth, None);
        assert_eq!(c.report, None);
        assert_eq!(c.api_base, None);
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
        assert_eq!(c.max_depth, Some(2));
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
        assert_eq!(c.report, Some(PathBuf::from("out.json")));
        assert_eq!(c.api_base.as_deref(), Some("https://example.test/api/v2"));
    }
}

/// Batch-identify a library and summarize matched, misnamed and unknown files.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  Scan folder:   rom-converto dat scan ./roms\n  With report:   rom-converto dat scan ./roms --report scan.csv\n  Limit depth:   rom-converto dat scan ./roms --max-depth 2\n"
)]
pub struct DatScanCommand {
    /// Directory to scan
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Maximum directory depth. 1 = top level only. Omit for unlimited.
    #[arg(long = "max-depth", value_name = "N")]
    pub max_depth: Option<usize>,

    /// Write a run report to FILE. Format inferred from the extension: .csv, .json, .html or .htm. Unknown extensions default to JSON. The file is overwritten directly.
    #[arg(long = "report", value_name = "FILE")]
    pub report: Option<PathBuf>,

    /// Playmatch API base URL (defaults to the public instance)
    #[arg(long = "api-base", value_name = "URL")]
    pub api_base: Option<String>,
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

/// Rename ROMs to their canonical database names. Hash-verified matches only.
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

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited.
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

    /// What to do when the target name already exists on disk
    #[arg(long = "on-conflict", value_enum, value_name = "MODE")]
    pub on_conflict: Option<ConflictPolicyArg>,

    /// Shorthand for --on-conflict overwrite
    #[arg(short = 'f', long = "force", conflicts_with = "on_conflict")]
    pub force: bool,

    /// Write a run report to FILE. Format inferred from the extension: .csv, .json, .html or .htm. Unknown extensions default to JSON. The file is overwritten directly.
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

/// Look up a single file and print everything the database knows about it.
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

/// Build a Logiqx fixdat of the database entries missing from a local library.
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

    /// Platform name to resolve via the database (e.g. a console name)
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

    /// Maximum directory depth. 1 = top level only. Omit for unlimited.
    #[arg(long = "max-depth", value_name = "N")]
    pub max_depth: Option<usize>,

    /// What to do when OUTPUT already exists
    #[arg(long = "on-conflict", value_enum, value_name = "MODE")]
    pub on_conflict: Option<ConflictPolicyArg>,

    /// Shorthand for --on-conflict overwrite
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
