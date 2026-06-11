use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands specific to RVL (Wii) disc images.
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum RvlCommands {
    Compress(CompressDiscCommand),
    Decompress(DecompressDiscCommand),
    Verify(VerifyDiscCommand),
    Info(InfoCommand),
}

/// Verify a Wii disc image.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(long_about = "Verify a Wii disc image.\n\n\
Fast mode (default) checks the RVZ container's stored SHA-1 hashes (file header, disc struct, partition table). It is a no-op for plain .iso / .wbfs input, which carries no container hashes.\n\n\
--full decrypts every partition cluster and recomputes the H0/H1/H2 hash tree, comparing it to the on-disc hash regions to detect tampering or bit rot. This decrypts and hashes the entire disc and can be slow.")]
pub struct VerifyDiscCommand {
    /// Input disc image path (.iso, .wbfs, or .rvz).
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Deep verification: recompute the Wii H0/H1/H2 partition hash tree.
    #[arg(long, default_value_t = false)]
    pub full: bool,
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
