use crate::commands::ConflictPolicyArg;
use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands specific to DOL (GameCube) disc images.
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum DolCommands {
    Compress(CompressDiscCommand),
    Decompress(DecompressDiscCommand),
    Verify(VerifyDiscCommand),
    Info(InfoCommand),
}

/// Verify a GameCube disc image.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(long_about = "Verify a GameCube disc image.\n\n\
Fast mode (default) checks the RVZ container's stored SHA-1 hashes (file header, disc struct, partition table). It is a no-op for plain .iso / .gcm input, which carries no integrity data.\n\n\
--full decodes the whole disc, validates the FST geometry, and computes a whole-disc SHA-1. GameCube discs carry no built-in integrity hashes, so that digest is informational (for external DAT/Redump matching), never a pass/fail.")]
pub struct VerifyDiscCommand {
    /// Input disc image path (.iso, .gcm, or .rvz), or a directory with --recursive.
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Deep verification: decode the whole disc and compute a whole-disc SHA-1.
    #[arg(long, default_value_t = false)]
    pub full: bool,

    /// Verify every .iso, .gcm and .rvz found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited.
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,
}

/// Compress a GameCube disc image to RVZ.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Compress a GameCube disc image to Dolphin's RVZ format.\n\n\
Supported input: .iso / .gcm.\nOutput defaults to the same path with the extension replaced by .rvz.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto dol compress game.iso\n  Explicit output: rom-converto dol compress game.gcm game.rvz\n  Whole folder:    rom-converto dol compress -R ./roms --output-dir ./rvz\n"
)]
pub struct CompressDiscCommand {
    /// Input disc image path (.iso or .gcm), or a directory with --recursive.
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output RVZ path, defaults to the input path with extension replaced by .rvz (ignored with --recursive).
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output RVZ path, defaults to the input path with extension replaced by .rvz (ignored with --recursive).
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

    /// Zstandard compression level (signed, negative levels allowed).
    /// Defaults to 22 (archive quality). Lower values trade ratio for
    /// speed; Dolphin's documented suggestion is 5.
    #[arg(long, short = 'l', value_parser = clap::value_parser!(i32).range(-22..=22))]
    pub level: Option<i32>,

    /// Chunk size in bytes. Must be a power of two between 32 KiB and
    /// 2 MiB. Defaults to 128 KiB (matches Dolphin's RVZ default).
    #[arg(long)]
    pub chunk_size: Option<u32>,

    /// What to do when an output already exists: error, overwrite, skip, or rename to a numbered sibling
    #[arg(long = "on-conflict", value_enum)]
    pub on_conflict: Option<ConflictPolicyArg>,

    /// Alias for --on-conflict overwrite
    #[arg(long, short = 'f', default_value_t = false, conflicts_with = "on_conflict")]
    pub force: bool,

    /// Compress every .iso and .gcm found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited.
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

    /// Write a run report to FILE. Format inferred from the extension: .csv, .json, .html or .htm. Unknown extensions default to JSON. The file is overwritten directly.
    #[arg(long = "report", value_name = "FILE")]
    pub report: Option<PathBuf>,
}

/// Decompress an RVZ GameCube disc image back to ISO.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Decompress an RVZ file back to a raw GameCube disc image.\n\nOutput defaults to the input path with extension replaced by .iso."
)]
pub struct DecompressDiscCommand {
    /// Input RVZ file path, or a directory with --recursive.
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output ISO path (ignored with --recursive).
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output ISO path (ignored with --recursive).
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
    #[arg(long, short = 'f', default_value_t = false, conflicts_with = "on_conflict")]
    pub force: bool,

    /// Decompress every .rvz found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited.
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

    /// Write a run report to FILE. Format inferred from the extension: .csv, .json, .html or .htm. Unknown extensions default to JSON. The file is overwritten directly.
    #[arg(long = "report", value_name = "FILE")]
    pub report: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Parser, Debug)]
    struct Harness {
        #[command(subcommand)]
        cmd: DolCommands,
    }

    #[test]
    fn parses_compress_recursive() {
        let h = Harness::parse_from(["bin", "compress", "roms", "-R"]);
        let DolCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.recursive);
        assert_eq!(c.output, None);
    }

    #[test]
    fn parses_verify_recursive() {
        let h = Harness::parse_from(["bin", "verify", "roms", "-R"]);
        let DolCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert!(c.recursive);
    }

    #[test]
    fn parses_compress_output_dir() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--output-dir", "out"]);
        let DolCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.output_dir, Some(PathBuf::from("out")));
        assert_eq!(c.output, None);
    }

    #[test]
    fn parses_compress_report_flag() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--report", "out.html"]);
        let DolCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.report, Some(PathBuf::from("out.html")));
    }

    #[test]
    fn parses_decompress_report_flag() {
        let h = Harness::parse_from(["bin", "decompress", "game.rvz", "--report", "out.json"]);
        let DolCommands::Decompress(c) = h.cmd else {
            panic!("expected Decompress");
        };
        assert_eq!(c.report, Some(PathBuf::from("out.json")));
    }

    #[test]
    fn on_conflict_absent_is_none() {
        let h = Harness::parse_from(["bin", "compress", "game.iso"]);
        let DolCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.on_conflict.is_none());
    }

    #[test]
    fn parses_compress_output_template() {
        let h = Harness::parse_from([
            "bin",
            "compress",
            "game.iso",
            "--output-template",
            "{console}/{title}.{ext}",
        ]);
        let DolCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(
            c.output_template,
            Some("{console}/{title}.{ext}".to_string())
        );
    }

    #[test]
    fn output_template_conflicts_with_explicit_output() {
        let r = Harness::try_parse_from([
            "bin",
            "compress",
            "game.iso",
            "out.rvz",
            "--output-template",
            "{title}.{ext}",
        ]);
        assert!(r.is_err());
    }

    #[test]
    fn parses_on_conflict_skip() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--on-conflict", "skip"]);
        let DolCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.on_conflict, Some(ConflictPolicyArg::Skip));
    }
}
