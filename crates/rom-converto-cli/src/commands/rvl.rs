use crate::commands::ConflictPolicyArg;
use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands specific to RVL (Wii) disc images
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum RvlCommands {
    Compress(CompressDiscCommand),
    Migrate(MigrateDiscCommand),
    Decompress(DecompressDiscCommand),
    Verify(VerifyDiscCommand),
    Info(InfoCommand),
}

/// Migrate a legacy Wii disc image (WIA, GCZ, or NKit) to RVZ
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Migrate a legacy Wii disc image (WIA, GCZ, or NKit) to RVZ\n\n\
Supported input: .wia (all compression methods including bzip2/LZMA/LZMA2), .gcz, .nkit.iso, and \
.nkit.gcz, detected by content so renamed files work. The container is integrity-checked first \
(WIA SHA-1 chain, GCZ block checksums, NKit whole-file CRC32), then the original disc is \
reconstructed on the fly (Wii hash tree rebuild and re-encryption included) and compressed to RVZ.\n\n\
Output defaults to the input path with the extension replaced by .rvz.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto rvl migrate game.wia\n  Explicit output: rom-converto rvl migrate game.gcz game.rvz\n  Whole directory: rom-converto rvl migrate -R ./roms\n"
)]
pub struct MigrateDiscCommand {
    /// Input disc image path (.wia, .gcz, .nkit.iso, .nkit.gcz), or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output RVZ path, defaults to the input path with extension replaced by .rvz (ignored with --recursive)
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output RVZ path, defaults to the input path with extension replaced by .rvz (ignored with --recursive)
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    /// Zstandard compression level (signed, negative levels allowed).
    /// Defaults to 22 (archive quality)
    #[arg(long, short = 'l')]
    pub level: Option<i32>,

    /// RVZ chunk size in bytes. Must be a power of two between 32 KiB
    /// and 2 MiB. Defaults to 128 KiB
    #[arg(long)]
    pub chunk_size: Option<u32>,

    /// Skip the pre-conversion integrity pass
    #[arg(long, default_value_t = false)]
    pub skip_verify: bool,

    /// Decode every WIA group during verification instead of only the
    /// SHA-1 header chain
    #[arg(long, default_value_t = false)]
    pub deep: bool,

    /// Overwrite the output file if it already exists
    #[arg(long, short = 'f', default_value_t = false)]
    pub force: bool,

    /// Migrate every WIA, GCZ and NKit image found in the INPUT directory (detected by content)
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,
}

/// Verify a Wii disc image
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Verify a Wii disc image\n\n\
Fast mode (default) checks the RVZ container's stored SHA-1 hashes (file header, disc struct, partition table). It is a no-op for plain .iso / .wbfs input, which carries no container hashes.\n\n\
--full decrypts every partition cluster and recomputes the H0/H1/H2 hash tree, comparing it to the on-disc hash regions to detect tampering or bit rot. This decrypts and hashes the entire disc and can be slow.\n\n\
Legacy Wii containers (WIA, GCZ, NKit) are decoded on the fly and checked the same way.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto rvl verify game.iso\n  Legacy input:    rom-converto rvl verify game.wia\n  Full check:      rom-converto rvl verify game.rvz --full\n  Whole directory: rom-converto rvl verify -R ./roms\n"
)]
pub struct VerifyDiscCommand {
    /// Input disc image path (.iso, .wbfs, .rvz, .wia, .gcz, .nkit.iso, .nkit.gcz), or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Deep verification: recompute the Wii H0/H1/H2 partition hash tree
    #[arg(long, default_value_t = false)]
    pub full: bool,

    /// Verify every .iso, .wbfs and .rvz found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,
}

/// Compress a Wii disc image to RVZ
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Compress a Wii disc image to RVZ\n\n\
Supported input: .iso, or .wbfs (single file or split .wbf1.. parts), streamed directly.\nOutput defaults to the same path with the extension replaced by .rvz.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto rvl compress game.iso\n  Explicit output: rom-converto rvl compress game.wbfs game.rvz\n  Whole directory: rom-converto rvl compress -R ./roms --output-dir ./rvz\n"
)]
pub struct CompressDiscCommand {
    /// Input disc image path (.iso or .wbfs), or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output RVZ path, defaults to the input path with extension replaced by .rvz (ignored with --recursive)
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output RVZ path, defaults to the input path with extension replaced by .rvz (ignored with --recursive)
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    /// Write output into this directory using the derived filename. Created if missing. Works with --recursive
    #[arg(long = "output-dir", value_name = "DIR", conflicts_with_all = ["output", "output_flag"])]
    pub output_dir: Option<PathBuf>,

    /// Output path template applied per file. Tokens: {title}, {titleId}, {region},
    /// {console}, {serial}, {ext}, {basename}. Resolves against extracted metadata;
    /// missing tokens fall back to the input basename. Joined under --output-dir
    #[arg(long = "output-template", value_name = "TEMPLATE", conflicts_with_all = ["output", "output_flag"])]
    pub output_template: Option<String>,

    /// Zstandard compression level (signed, negative levels allowed). Defaults to 22 (archive quality). Lower values trade ratio for speed; Dolphin's documented suggestion is 5
    #[arg(long, short = 'l', value_parser = clap::value_parser!(i32).range(-22..=22))]
    pub level: Option<i32>,

    /// Chunk size in bytes. Must be a power of two between 32 KiB and 2 MiB. Defaults to 128 KiB (matches Dolphin's RVZ default)
    #[arg(long)]
    pub chunk_size: Option<u32>,

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

    /// Compress every .iso and .wbfs found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

    /// Write a run report to FILE. Format inferred from the extension: .csv, .json, .html or .htm. Unknown extensions default to JSON. The file is overwritten directly
    #[arg(long = "report", value_name = "FILE")]
    pub report: Option<PathBuf>,
}

/// Decompress an RVZ Wii disc image back to ISO or WBFS
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Decompress an RVZ Wii disc image back to ISO or WBFS\n\nThe output format follows the output file extension: a .wbfs path writes a scrubbed WBFS container streamed directly from the RVZ, anything else writes a raw .iso. Defaults to .iso.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto rvl decompress game.rvz\n  Explicit output: rom-converto rvl decompress game.rvz game.wbfs\n  Whole directory: rom-converto rvl decompress -R ./rvz --output-dir ./roms\n"
)]
pub struct DecompressDiscCommand {
    /// Input RVZ file path, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output path. A .wbfs extension writes a WBFS container; otherwise a raw .iso (ignored with --recursive)
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output path. A .wbfs extension writes a WBFS container; otherwise a raw .iso (ignored with --recursive)
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    /// Write output into this directory using the derived filename. Created if missing. Works with --recursive
    #[arg(long = "output-dir", value_name = "DIR", conflicts_with_all = ["output", "output_flag"])]
    pub output_dir: Option<PathBuf>,

    /// Output path template applied per file. Tokens: {title}, {titleId}, {region},
    /// {console}, {serial}, {ext}, {basename}. Resolves against extracted metadata;
    /// missing tokens fall back to the input basename. Joined under --output-dir
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

    /// Decompress every .rvz found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

    /// Write a run report to FILE. Format inferred from the extension: .csv, .json, .html or .htm. Unknown extensions default to JSON. The file is overwritten directly
    #[arg(long = "report", value_name = "FILE")]
    pub report: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Parser, Debug)]
    struct Harness {
        #[command(subcommand)]
        cmd: RvlCommands,
    }

    #[test]
    fn parses_compress_recursive() {
        let h = Harness::parse_from(["bin", "compress", "roms", "-R"]);
        let RvlCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.recursive);
        assert_eq!(c.output, None);
    }

    #[test]
    fn parses_verify_recursive() {
        let h = Harness::parse_from(["bin", "verify", "roms", "-R"]);
        let RvlCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert!(c.recursive);
    }

    #[test]
    fn parses_migrate_force_and_output_flag() {
        let h = Harness::parse_from(["bin", "migrate", "game.wia", "-o", "out.rvz", "-f"]);
        let RvlCommands::Migrate(c) = h.cmd else {
            panic!("expected Migrate");
        };
        assert!(c.force);
        assert_eq!(c.output, None);
        assert_eq!(c.output_flag, Some(PathBuf::from("out.rvz")));
    }

    #[test]
    fn migrate_output_flag_conflicts_with_positional() {
        let result =
            Harness::try_parse_from(["bin", "migrate", "game.wia", "pos.rvz", "-o", "flag.rvz"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_migrate_deep() {
        let h = Harness::parse_from(["bin", "migrate", "game.wia", "--deep"]);
        let RvlCommands::Migrate(c) = h.cmd else {
            panic!("expected Migrate");
        };
        assert!(c.deep);
    }

    #[test]
    fn parses_compress_output_dir() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--output-dir", "out"]);
        let RvlCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.output_dir, Some(PathBuf::from("out")));
        assert_eq!(c.output, None);
    }

    #[test]
    fn parses_compress_report_flag() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--report", "out.csv"]);
        let RvlCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.report, Some(PathBuf::from("out.csv")));
    }

    #[test]
    fn parses_decompress_report_flag() {
        let h = Harness::parse_from(["bin", "decompress", "game.rvz", "--report", "out.json"]);
        let RvlCommands::Decompress(c) = h.cmd else {
            panic!("expected Decompress");
        };
        assert_eq!(c.report, Some(PathBuf::from("out.json")));
    }

    #[test]
    fn on_conflict_absent_is_none() {
        let h = Harness::parse_from(["bin", "compress", "game.iso"]);
        let RvlCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.on_conflict.is_none());
    }
}
