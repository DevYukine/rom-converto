use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands specific to DOL (GameCube) disc images.
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum DolCommands {
    Compress(CompressDiscCommand),
    Migrate(MigrateDiscCommand),
    Decompress(DecompressDiscCommand),
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

    /// Decode every group during verification instead of only the
    /// header checks (WIA only; GCZ and NKit checks are already
    /// exhaustive).
    #[arg(long)]
    pub deep: bool,

    /// Migrate every legacy image found in the INPUT directory.
    #[arg(long, short = 'R')]
    pub recursive: bool,
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
