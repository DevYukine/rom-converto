use crate::commands::ConflictPolicyArg;
use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands specific to Nintendo Switch (NX) NSP/XCI containers.
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum NxCommands {
    Compress(NxCompressCommand),
    Decompress(NxDecompressCommand),
    Verify(NxVerifyCommand),
    Info(InfoCommand),
}

/// Compress an NSP into NSZ or an XCI into XCZ. NCAs inside the
/// container are decrypted, zstd-compressed, and packaged with the
/// already-derived per-section keys cached in NCZSECTN.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto nx compress game.nsp\n  Explicit output: rom-converto nx compress game.xci game.xcz\n  Whole folder:    rom-converto nx compress -R ./roms --output-dir ./nsz\n"
)]
pub struct NxCompressCommand {
    /// Path to `prod.keys`. Defaults to `$HOME/.switch/prod.keys` on
    /// Linux/macOS or `%USERPROFILE%/.switch/prod.keys` on Windows,
    /// then the binary's own directory.
    #[arg(long = "keys", value_name = "PRODKEYS")]
    pub keys: Option<PathBuf>,

    /// Input NSP or XCI, or a directory with --recursive.
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output path. Defaults to the input path with the extension
    /// switched (.nsp -> .nsz, .xci -> .xcz).
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output path. Defaults to the input path with the extension
    /// switched (.nsp -> .nsz, .xci -> .xcz).
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    /// Write output into this directory using the derived filename. Created if missing. Works with --recursive.
    #[arg(long = "output-dir", value_name = "DIR", conflicts_with_all = ["output", "output_flag"])]
    pub output_dir: Option<PathBuf>,

    /// Output path template applied per file. Tokens: {title}, {titleId}, {region},
    /// {console}, {serial}, {ext}, {basename}. Resolves against extracted metadata;
    /// missing tokens fall back to the input basename. Joined under --output-dir.
    #[arg(long = "output-template", value_name = "TEMPLATE", conflicts_with_all = ["output", "output_flag"])]
    pub output_template: Option<String>,

    /// Zstd compression level. nsz default is 18; the maximum 22 needs
    /// over 1 GiB of RAM during decompression on the Switch.
    #[arg(
        short = 'l',
        long = "level",
        value_name = "LEVEL",
        value_parser = clap::value_parser!(i32).range(1..=22)
    )]
    pub level: Option<i32>,

    /// Compression mode. `solid` writes one zstd frame per NCA (smaller
    /// output, default for NSP). `block` writes independent zstd frames
    /// per fixed-size block (random read friendly, default for XCI).
    #[arg(long = "mode", value_parser = ["solid", "block"])]
    pub mode: Option<String>,

    /// Block-mode block size, expressed as a power of two (`exp` in
    /// `1 << exp` bytes). nsz default is 20 (1 MiB). Range 14..=32.
    #[arg(long = "block-size-exp", value_parser = clap::value_parser!(u8).range(14..=32))]
    pub block_size_exp: Option<u8>,

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

    /// Compress every .nsp and .xci found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited.
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

    /// Write a run report to FILE. Format inferred from the extension: .csv, .json, .html or .htm. Unknown extensions default to JSON. The file is overwritten directly.
    #[arg(long = "report", value_name = "FILE")]
    pub report: Option<PathBuf>,
}

/// Decompress an NSZ back to NSP or an XCZ back to XCI.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct NxDecompressCommand {
    /// Path to `prod.keys`. Defaults to `$HOME/.switch/prod.keys` on
    /// Linux/macOS or `%USERPROFILE%/.switch/prod.keys` on Windows,
    /// then the binary's own directory.
    #[arg(long = "keys", value_name = "PRODKEYS")]
    pub keys: Option<PathBuf>,

    /// Input NSZ or XCZ, or a directory with --recursive.
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output path. Defaults to the input path with the extension
    /// switched (.nsz -> .nsp, .xcz -> .xci).
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output path. Defaults to the input path with the extension
    /// switched (.nsz -> .nsp, .xcz -> .xci).
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    /// Write output into this directory using the derived filename. Created if missing. Works with --recursive.
    #[arg(long = "output-dir", value_name = "DIR", conflicts_with_all = ["output", "output_flag"])]
    pub output_dir: Option<PathBuf>,

    /// Output path template applied per file. Tokens: {title}, {titleId}, {region},
    /// {console}, {serial}, {ext}, {basename}. Resolves against extracted metadata;
    /// missing tokens fall back to the input basename. Joined under --output-dir.
    #[arg(long = "output-template", value_name = "TEMPLATE", conflicts_with_all = ["output", "output_flag"])]
    pub output_template: Option<String>,

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

    /// Decompress every .nsz and .xcz found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited.
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

    /// Write a run report to FILE. Format inferred from the extension: .csv, .json, .html or .htm. Unknown extensions default to JSON. The file is overwritten directly.
    #[arg(long = "report", value_name = "FILE")]
    pub report: Option<PathBuf>,
}

/// Verify hash integrity of every NCA in a Switch container.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct NxVerifyCommand {
    /// Path to `prod.keys`. Defaults to `$HOME/.switch/prod.keys` on
    /// Linux/macOS or `%USERPROFILE%/.switch/prod.keys` on Windows,
    /// then the binary's own directory.
    #[arg(long = "keys", value_name = "PRODKEYS")]
    pub keys: Option<PathBuf>,

    /// Input container (NSP / NSZ / XCI / XCZ), or a directory with --recursive.
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Verify every .nsp, .xci, .nsz and .xcz found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited.
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Harness {
        #[command(subcommand)]
        cmd: NxCommands,
    }

    #[test]
    fn parses_compress_minimal() {
        let h = Harness::parse_from(["bin", "compress", "game.nsp"]);
        let NxCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.input, PathBuf::from("game.nsp"));
        assert!(c.output.is_none());
        assert!(c.level.is_none());
    }

    #[test]
    fn parses_compress_with_keys_level_mode() {
        let h = Harness::parse_from([
            "bin",
            "compress",
            "--keys",
            "/k/prod.keys",
            "-l",
            "18",
            "--mode",
            "block",
            "--block-size-exp",
            "20",
            "-o",
            "out.nsz",
            "game.nsp",
        ]);
        let NxCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.keys, Some(PathBuf::from("/k/prod.keys")));
        assert_eq!(c.level, Some(18));
        assert_eq!(c.mode.as_deref(), Some("block"));
        assert_eq!(c.block_size_exp, Some(20));
        assert_eq!(c.output_flag, Some(PathBuf::from("out.nsz")));
    }

    #[test]
    fn parses_compress_positional_output() {
        let h = Harness::parse_from(["bin", "compress", "game.nsp", "out.nsz"]);
        let NxCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.input, PathBuf::from("game.nsp"));
        assert_eq!(c.output, Some(PathBuf::from("out.nsz")));
        assert!(c.output_flag.is_none());
    }

    #[test]
    fn output_flag_conflicts_with_positional_output() {
        let result =
            Harness::try_parse_from(["bin", "compress", "game.nsp", "out.nsz", "-o", "other.nsz"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_decompress() {
        let h = Harness::parse_from(["bin", "decompress", "g.nsz"]);
        let NxCommands::Decompress(c) = h.cmd else {
            panic!("expected Decompress");
        };
        assert_eq!(c.input, PathBuf::from("g.nsz"));
        assert!(!c.force);
    }

    #[test]
    fn parses_compress_force() {
        let h = Harness::parse_from(["bin", "compress", "-f", "game.nsp"]);
        let NxCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.force);
    }

    #[test]
    fn parses_verify() {
        let h = Harness::parse_from(["bin", "verify", "--keys", "k", "g.nsz"]);
        let NxCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert_eq!(c.keys, Some(PathBuf::from("k")));
        assert_eq!(c.input, PathBuf::from("g.nsz"));
    }

    #[test]
    fn parses_compress_recursive() {
        let h = Harness::parse_from(["bin", "compress", "-R", "roms"]);
        let NxCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.recursive);
        assert!(c.output.is_none());
    }

    #[test]
    fn parses_verify_recursive() {
        let h = Harness::parse_from(["bin", "verify", "-R", "roms"]);
        let NxCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert!(c.recursive);
    }

    #[test]
    fn parses_compress_output_dir() {
        let h = Harness::parse_from(["bin", "compress", "--output-dir", "out", "game.nsp"]);
        let NxCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.output_dir, Some(PathBuf::from("out")));
        assert!(c.output.is_none());
    }

    #[test]
    fn output_dir_conflicts_with_output() {
        let result = Harness::try_parse_from([
            "bin",
            "compress",
            "-o",
            "out.nsz",
            "--output-dir",
            "out",
            "game.nsp",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_compress_report_flag() {
        let h = Harness::parse_from(["bin", "compress", "game.nsp", "--report", "out.json"]);
        let NxCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.report, Some(PathBuf::from("out.json")));
    }

    #[test]
    fn parses_decompress_report_flag() {
        let h = Harness::parse_from(["bin", "decompress", "game.nsz", "--report", "out.csv"]);
        let NxCommands::Decompress(c) = h.cmd else {
            panic!("expected Decompress");
        };
        assert_eq!(c.report, Some(PathBuf::from("out.csv")));
    }

    #[test]
    fn on_conflict_absent_is_none() {
        let h = Harness::parse_from(["bin", "compress", "game.nsp"]);
        let NxCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.on_conflict.is_none());
    }
}
