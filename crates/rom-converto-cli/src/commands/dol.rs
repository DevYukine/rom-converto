use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands specific to DOL (GameCube) disc images.
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum DolCommands {
    Compress(CompressDiscCommand),
    Migrate(MigrateDiscCommand),
    Decompress(DecompressDiscCommand),
    Verify(VerifyDiscCommand),
    Info(InfoCommand),
}

/// Migrate a legacy GameCube image (GCZ or NKit) to RVZ.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Migrate a legacy GameCube image to Dolphin's RVZ format.\n\n\
Supported input: .gcz, .nkit.iso, and .nkit.gcz, detected by content so renamed files work. \
The container is integrity-checked first (GCZ block checksums, NKit whole-file CRC32), then the \
original disc is reconstructed on the fly (NKit junk regeneration included) and compressed to RVZ. \
NKit restorations are additionally verified against the embedded source CRC32 while converting.\n\n\
Output defaults to the input path with the extension replaced by .rvz."
)]
pub struct MigrateDiscCommand {
    /// Input image path (.gcz, .nkit.iso, .nkit.gcz), or a directory with --recursive.
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output RVZ path, defaults to the input path with extension replaced by .rvz.
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

    /// Zstandard compression level (signed, negative levels allowed).
    /// Defaults to 22 (archive quality).
    #[arg(long, short = 'l')]
    pub level: Option<i32>,

    /// RVZ chunk size in bytes. Must be a power of two between 32 KiB
    /// and 2 MiB. Defaults to 128 KiB.
    #[arg(long)]
    pub chunk_size: Option<u32>,

    /// Skip the pre-conversion integrity pass.
    #[arg(long, default_value_t = false)]
    pub skip_verify: bool,

    /// Decode every group during verification instead of only the
    /// header checks (WIA only; GCZ and NKit checks are already
    /// exhaustive).
    #[arg(long, default_value_t = false)]
    pub deep: bool,

    /// Overwrite the output file if it already exists
    #[arg(long, short = 'f', default_value_t = false)]
    pub force: bool,

    /// Migrate every GCZ and NKit image found in the INPUT directory (detected by content)
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,
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

    /// Verify every .iso, .gcm and .rvz found in the INPUT directory
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,
}

/// Compress a GameCube disc image to RVZ.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Compress a GameCube disc image to Dolphin's RVZ format.\n\n\
Supported input: .iso / .gcm.\nOutput defaults to the same path with the extension replaced by .rvz."
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

    /// Zstandard compression level (signed, negative levels allowed).
    /// Defaults to 22 (archive quality). Lower values trade ratio for
    /// speed; Dolphin's documented suggestion is 5.
    #[arg(long, short = 'l')]
    pub level: Option<i32>,

    /// Chunk size in bytes. Must be a power of two between 32 KiB and
    /// 2 MiB. Defaults to 128 KiB (matches Dolphin's RVZ default).
    #[arg(long)]
    pub chunk_size: Option<u32>,

    /// Overwrite the output file if it already exists
    #[arg(long, short = 'f', default_value_t = false)]
    pub force: bool,

    /// Compress every .iso and .gcm found in the INPUT directory
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,
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

    /// Overwrite the output file if it already exists
    #[arg(long, short = 'f', default_value_t = false)]
    pub force: bool,

    /// Decompress every .rvz found in the INPUT directory
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,
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
    fn parses_migrate_force_and_output_flag() {
        let h = Harness::parse_from(["bin", "migrate", "game.gcz", "-o", "out.rvz", "-f"]);
        let DolCommands::Migrate(c) = h.cmd else {
            panic!("expected Migrate");
        };
        assert!(c.force);
        assert_eq!(c.output, None);
        assert_eq!(c.output_flag, Some(PathBuf::from("out.rvz")));
    }

    #[test]
    fn migrate_output_flag_conflicts_with_positional() {
        let result =
            Harness::try_parse_from(["bin", "migrate", "game.gcz", "pos.rvz", "-o", "flag.rvz"]);
        assert!(result.is_err());
    }
}
