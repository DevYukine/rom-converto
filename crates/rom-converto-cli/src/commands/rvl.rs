use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands specific to RVL (Wii) disc images.
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum RvlCommands {
    Compress(CompressDiscCommand),
    Migrate(MigrateDiscCommand),
    Decompress(DecompressDiscCommand),
    Info(InfoCommand),
}

/// Migrate a legacy Wii image (WIA, GCZ, or NKit) to RVZ.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(long_about = "Migrate a legacy Wii image to Dolphin's RVZ format.\n\n\
Supported input: .wia (all compression methods including bzip2/LZMA/LZMA2), .gcz, .nkit.iso, and \
.nkit.gcz, detected by content so renamed files work. The container is integrity-checked first \
(WIA SHA-1 chain, GCZ block checksums, NKit whole-file CRC32), then the original disc is \
reconstructed on the fly (Wii hash tree rebuild and re-encryption included) and compressed to RVZ.\n\n\
Output defaults to the input path with the extension replaced by .rvz.")]
pub struct MigrateDiscCommand {
    /// Input image path (.wia, .gcz, .nkit.iso, .nkit.gcz), or a directory with --recursive.
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output RVZ path, defaults to the input path with extension replaced by .rvz.
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Zstandard compression level (signed, negative levels allowed).
    /// Defaults to 22 (archive quality).
    #[arg(long, short = 'l')]
    pub level: Option<i32>,

    /// RVZ chunk size in bytes. Must be a power of two between 32 KiB
    /// and 2 MiB. Defaults to 128 KiB.
    #[arg(long)]
    pub chunk_size: Option<u32>,

    /// Skip the pre-conversion integrity pass.
    #[arg(long)]
    pub skip_verify: bool,

    /// Decode every WIA group during verification instead of only the
    /// SHA-1 header chain.
    #[arg(long)]
    pub deep: bool,

    /// Migrate every legacy image found in the INPUT directory.
    #[arg(long, short = 'R')]
    pub recursive: bool,
}

/// Compress a Wii disc image to RVZ.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(long_about = "Compress a Wii disc image to Dolphin's RVZ format.\n\n\
Supported input: .iso, or .wbfs (single file or split .wbf1.. parts), streamed directly.\nOutput defaults to the same path with the extension replaced by .rvz.")]
pub struct CompressDiscCommand {
    /// Input disc image path (.iso or .wbfs).
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output RVZ path, defaults to the input path with extension replaced by .rvz.
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Zstandard compression level (signed, negative levels allowed).
    /// Defaults to 22 (archive quality). Lower values trade ratio for
    /// speed; Dolphin's documented suggestion is 5.
    #[arg(long, short = 'l')]
    pub level: Option<i32>,

    /// Chunk size in bytes. Must be a power of two between 32 KiB and
    /// 2 MiB. Defaults to 128 KiB (matches Dolphin's RVZ default).
    #[arg(long)]
    pub chunk_size: Option<u32>,
}

/// Decompress an RVZ Wii disc image back to ISO or WBFS.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Decompress an RVZ file back to a Wii disc image.\n\nThe output format follows the output file extension: a .wbfs path writes a scrubbed WBFS container streamed directly from the RVZ, anything else writes a raw .iso. Defaults to .iso."
)]
pub struct DecompressDiscCommand {
    /// Input RVZ file path.
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output path. A .wbfs extension writes a WBFS container; otherwise a raw .iso.
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,
}
