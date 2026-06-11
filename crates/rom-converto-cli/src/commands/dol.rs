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
    /// Input disc image path (.iso, .gcm, or .rvz).
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Deep verification: decode the whole disc and compute a whole-disc SHA-1.
    #[arg(long, default_value_t = false)]
    pub full: bool,
}

/// Compress a GameCube disc image to RVZ.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Compress a GameCube disc image to Dolphin's RVZ format.\n\n\
Supported input: .iso / .gcm.\nOutput defaults to the same path with the extension replaced by .rvz."
)]
pub struct CompressDiscCommand {
    /// Input disc image path (.iso or .gcm).
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

/// Decompress an RVZ GameCube disc image back to ISO.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Decompress an RVZ file back to a raw GameCube disc image.\n\nOutput defaults to the input path with extension replaced by .iso."
)]
pub struct DecompressDiscCommand {
    /// Input RVZ file path.
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output ISO path.
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,
}
