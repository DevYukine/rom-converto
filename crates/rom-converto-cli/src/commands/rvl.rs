use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands specific to RVL (Wii) disc images.
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum RvlCommands {
    Compress(CompressDiscCommand),
    Decompress(DecompressDiscCommand),
}

/// Compress a Wii disc image to RVZ.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(long_about = "Compress a Wii disc image to Dolphin's RVZ format.\n\n\
Supported input: .iso / .wbfs (read as raw so far).\nOutput defaults to the same path with the extension replaced by .rvz.")]
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

/// Decompress an RVZ Wii disc image back to ISO.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Decompress an RVZ file back to a raw Wii disc image.\n\nOutput defaults to the input path with extension replaced by .iso."
)]
pub struct DecompressDiscCommand {
    /// Input RVZ file path.
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output ISO path.
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,
}
