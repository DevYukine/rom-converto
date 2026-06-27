use crate::commands::ConflictPolicyArg;
use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// Commands for CSO/ZSO compressed ISO images (PSP, PS2).
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum CsoCommands {
    Compress(CompressCommand),
    Decompress(DecompressCommand),
    Verify(VerifyCommand),
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

/// Compress an ISO to a CSO or ZSO container.
///
/// Pick the format for the target device: CSO for PSP (hardware and
/// PPSSPP), ZSO for PS2 via Open PS2 Loader. Emulator setups are
/// usually better served by `chd compress`.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
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

    /// Write output into this directory using the derived filename. Created if missing. Works with --recursive.
    #[arg(long = "output-dir", value_name = "DIR", conflicts_with_all = ["output", "output_flag"])]
    pub output_dir: Option<PathBuf>,

    /// Output container format
    #[arg(long, value_enum, default_value_t = CsoFormatArg::Cso)]
    pub format: CsoFormatArg,

    /// Block size in bytes, a power of two. Defaults to 2048, or
    /// 16384 for inputs of 2 GiB and beyond (matching maxcso)
    #[arg(long, value_name = "BYTES")]
    pub block_size: Option<u32>,

    /// What to do when an output already exists: error, overwrite, skip, or rename to a numbered sibling
    #[arg(long = "on-conflict", value_enum, default_value_t = ConflictPolicyArg::Error)]
    pub on_conflict: ConflictPolicyArg,

    /// Alias for --on-conflict overwrite
    #[arg(long, short = 'f', default_value_t = false, conflicts_with = "on_conflict")]
    pub force: bool,

    /// Compress every .iso found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited.
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,
}

/// Decompress a CSO or ZSO container back to a plain ISO.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct DecompressCommand {
    /// Input .cso or .zso path, or a directory with --recursive
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

    /// Write output into this directory using the derived filename. Created if missing. Works with --recursive.
    #[arg(long = "output-dir", value_name = "DIR", conflicts_with_all = ["output", "output_flag"])]
    pub output_dir: Option<PathBuf>,

    /// What to do when an output already exists: error, overwrite, skip, or rename to a numbered sibling
    #[arg(long = "on-conflict", value_enum, default_value_t = ConflictPolicyArg::Error)]
    pub on_conflict: ConflictPolicyArg,

    /// Alias for --on-conflict overwrite
    #[arg(long, short = 'f', default_value_t = false, conflicts_with = "on_conflict")]
    pub force: bool,

    /// Decompress every .cso and .zso found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited.
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,
}

/// Verify the integrity of a CSO or ZSO container.
///
/// The formats embed no checksums, so the standard pass validates
/// the index structure; --full additionally decodes every block.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct VerifyCommand {
    /// Input .cso or .zso path, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Decode every block instead of only checking the index
    #[arg(long)]
    pub full: bool,

    /// Verify every .cso and .zso found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited.
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
}
