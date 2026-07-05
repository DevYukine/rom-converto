use crate::commands::ConflictPolicyArg;
use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// Commands for CSO/ZSO compressed ISO images (PSP, PS2)
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum CsoCommands {
    Compress(CompressCommand),
    Decompress(DecompressCommand),
    Verify(VerifyCommand),
    ToChd(ToChdCommand),
    Info(InfoCommand),
}

#[derive(ValueEnum, Debug, Clone, Copy, Eq, PartialEq, Default)]
pub enum CsoFormatArg {
    /// CISO v1 (deflate): real PSP hardware with CFW and PPSSPP
    #[default]
    Cso,
    /// ZISO (LZ4): Open PS2 Loader on real PS2 hardware, ARK-4 on PSP
    Zso,
}

/// Compress an ISO to a CSO or ZSO container
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Compress an ISO to a CSO or ZSO container\n\nPick the format for the target device: CSO for PSP (hardware and PPSSPP), ZSO for PS2 via Open PS2 Loader. Emulator setups are usually better served by `chd compress`.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto cso compress game.iso\n  Explicit output: rom-converto cso compress game.iso game.zso --format zso\n  Whole folder:    rom-converto cso compress -R ./roms --format cso --output-dir ./cso\n"
)]
pub struct CompressCommand {
    /// Input ISO path, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output path, defaults to the input with the format's extension
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output path, defaults to the input with the format's extension
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

    /// Output container format
    #[arg(long, value_enum, default_value_t = CsoFormatArg::Cso)]
    pub format: CsoFormatArg,

    /// Block size in bytes, a power of two. Defaults to 2048, or 16384 for inputs of 2 GiB and beyond (matching maxcso)
    #[arg(long, value_name = "BYTES")]
    pub block_size: Option<u32>,

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

    /// Compress every .iso found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

    /// Write a run report to FILE. Format inferred from the extension: .csv, .json, .html or .htm. Unknown extensions default to JSON. The file is overwritten directly
    #[arg(long = "report", value_name = "FILE")]
    pub report: Option<PathBuf>,
}

/// Decompress a CSO, ZSO, or DAX container back to a plain ISO
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto cso decompress game.cso\n  Explicit output: rom-converto cso decompress game.zso game.iso\n  Whole folder:    rom-converto cso decompress -R ./cso --output-dir ./roms\n"
)]
pub struct DecompressCommand {
    /// Input .cso, .zso, or .dax path, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output ISO path, defaults to the input with extension replaced by .iso (ignored with --recursive)
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output ISO path, defaults to the input with extension replaced by .iso (ignored with --recursive)
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

    /// Decompress every .cso, .zso, and .dax found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

    /// Write a run report to FILE. Format inferred from the extension: .csv, .json, .html or .htm. Unknown extensions default to JSON. The file is overwritten directly
    #[arg(long = "report", value_name = "FILE")]
    pub report: Option<PathBuf>,
}

/// Compress a CSO, ZSO, or DAX straight to a CHD, through a temporary ISO
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Compress a CSO, ZSO, or DAX straight to a CHD, through a temporary ISO\n\nDecodes the container to a temporary ISO, then runs the same disc-to-CHD writer `chd compress` uses (so any embedded GAME/NAME tags match a direct build), and always deletes the temporary ISO afterward.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto cso to-chd game.cso\n  Explicit output: rom-converto cso to-chd game.zso game.chd\n  Whole folder:    rom-converto cso to-chd -R ./cso --output-dir ./chd\n"
)]
pub struct ToChdCommand {
    /// Input .cso, .zso, or .dax path, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output CHD path, defaults to the input path with extension replaced by .chd
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output CHD path, defaults to the input path with extension replaced by .chd
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

    /// Force DVD mode for the intermediate ISO
    #[arg(long, conflicts_with = "cd")]
    pub dvd: bool,

    /// Force CD mode for the intermediate ISO
    #[arg(long, conflicts_with = "dvd")]
    pub cd: bool,

    /// DVD hunk size in bytes, a multiple of 2048. Defaults to 4096, or 2048 for detected PSP images (PPSSPP reads 2048-byte blocks)
    #[arg(long, value_name = "BYTES")]
    pub hunk_size: Option<u32>,

    /// Add zstd to the DVD codec set: slightly better ratio, but some older players and cores do not support zstd-compressed CHD
    #[arg(long)]
    pub zstd: bool,

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

    /// Convert every .cso, .zso, and .dax found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

    /// Write a run report to FILE. Format inferred from the extension: .csv, .json, .html or .htm. Unknown extensions default to JSON. The file is overwritten directly
    #[arg(long = "report", value_name = "FILE")]
    pub report: Option<PathBuf>,
}

/// Verify the integrity of a CSO, ZSO, or DAX container
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Verify the integrity of a CSO, ZSO, or DAX container\n\nThe formats embed no checksums, so the standard pass validates the container structure; --full additionally decodes every block.",
    after_long_help = "EXAMPLES:\n  Single file:  rom-converto cso verify game.cso\n  Whole folder: rom-converto cso verify -R ./roms --full\n"
)]
pub struct VerifyCommand {
    /// Input .cso, .zso, or .dax path, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Decode every block instead of only checking the index
    #[arg(long)]
    pub full: bool,

    /// Verify every .cso, .zso, and .dax found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Parser, Debug)]
    struct Harness {
        #[command(subcommand)]
        cmd: CsoCommands,
    }

    #[test]
    fn parses_compress_with_format() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--format", "zso", "-R"]);
        let CsoCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.format, CsoFormatArg::Zso);
        assert!(c.recursive);
        assert_eq!(c.output, None);
    }

    #[test]
    fn parses_verify_full() {
        let h = Harness::parse_from(["bin", "verify", "game.cso", "--full"]);
        let CsoCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert!(c.full);
    }

    #[test]
    fn parses_verify_recursive() {
        let h = Harness::parse_from(["bin", "verify", "roms", "-R"]);
        let CsoCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert!(c.recursive);
    }

    #[test]
    fn parses_decompress_recursive() {
        let h = Harness::parse_from(["bin", "decompress", "roms", "-R"]);
        let CsoCommands::Decompress(c) = h.cmd else {
            panic!("expected Decompress");
        };
        assert!(c.recursive);
    }

    #[test]
    fn decompress_output_flag_overrides_positional() {
        let h = Harness::parse_from(["bin", "decompress", "game.cso", "-o", "out.iso"]);
        let CsoCommands::Decompress(c) = h.cmd else {
            panic!("expected Decompress");
        };
        assert_eq!(c.output, None);
        assert_eq!(c.output_flag, Some(PathBuf::from("out.iso")));
    }

    #[test]
    fn decompress_output_flag_conflicts_with_positional() {
        let result =
            Harness::try_parse_from(["bin", "decompress", "game.cso", "pos.iso", "-o", "flag.iso"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_compress_output_dir() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--output-dir", "out"]);
        let CsoCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.output_dir, Some(PathBuf::from("out")));
        assert_eq!(c.output, None);
    }

    #[test]
    fn parses_compress_report_flag() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--report", "out.json"]);
        let CsoCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.report, Some(PathBuf::from("out.json")));
    }

    #[test]
    fn parses_decompress_report_flag() {
        let h = Harness::parse_from(["bin", "decompress", "game.cso", "--report", "out.csv"]);
        let CsoCommands::Decompress(c) = h.cmd else {
            panic!("expected Decompress");
        };
        assert_eq!(c.report, Some(PathBuf::from("out.csv")));
    }

    #[test]
    fn decompress_output_dir_conflicts_with_positional() {
        let result = Harness::try_parse_from([
            "bin",
            "decompress",
            "game.cso",
            "pos.iso",
            "--output-dir",
            "out",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn on_conflict_absent_is_none() {
        let h = Harness::parse_from(["bin", "compress", "game.iso"]);
        let CsoCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.on_conflict.is_none());
    }

    #[test]
    fn parses_to_chd_defaults() {
        let h = Harness::parse_from(["bin", "to-chd", "game.cso"]);
        let CsoCommands::ToChd(c) = h.cmd else {
            panic!("expected ToChd");
        };
        assert_eq!(c.input, PathBuf::from("game.cso"));
        assert_eq!(c.output, None);
        assert!(!c.dvd && !c.cd && !c.zstd && !c.force && !c.recursive);
        assert_eq!(c.hunk_size, None);
    }

    #[test]
    fn parses_to_chd_dvd_flags() {
        let h = Harness::parse_from([
            "bin",
            "to-chd",
            "game.zso",
            "game.chd",
            "--dvd",
            "--hunk-size",
            "2048",
            "--zstd",
            "-R",
        ]);
        let CsoCommands::ToChd(c) = h.cmd else {
            panic!("expected ToChd");
        };
        assert_eq!(c.output, Some(PathBuf::from("game.chd")));
        assert!(c.dvd && c.zstd && c.recursive);
        assert_eq!(c.hunk_size, Some(2048));
    }

    #[test]
    fn to_chd_cd_conflicts_with_dvd() {
        let result = Harness::try_parse_from(["bin", "to-chd", "game.cso", "--cd", "--dvd"]);
        assert!(result.is_err());
    }

    #[test]
    fn to_chd_output_dir_conflicts_with_positional() {
        let result = Harness::try_parse_from([
            "bin",
            "to-chd",
            "game.cso",
            "pos.chd",
            "--output-dir",
            "out",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn to_chd_max_depth_requires_recursive() {
        let result = Harness::try_parse_from(["bin", "to-chd", "dir", "--max-depth", "2"]);
        assert!(result.is_err());
    }
}
