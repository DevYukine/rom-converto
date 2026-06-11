use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// Commands for CSO/ZSO compressed ISO images (PSP, PS2)
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

/// Compresses an ISO to a CSO or ZSO container.
///
/// Pick the format for the target device: CSO for PSP (hardware and
/// PPSSPP), ZSO for PS2 via Open PS2 Loader. Emulator setups are
/// usually better served by `chd compress`.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct CompressCommand {
    /// Input ISO path, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output path, defaults to the input with the format's extension
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output container format
    #[arg(long, value_enum, default_value_t = CsoFormatArg::Cso)]
    pub format: CsoFormatArg,

    /// Block size in bytes, a power of two. Defaults to 2048, or
    /// 16384 for inputs of 2 GiB and beyond (matching maxcso)
    #[arg(long, value_name = "BYTES")]
    pub block_size: Option<u32>,

    /// Force overwrite of the output file if it already exists
    #[arg(long, short = 'f', default_value_t = false)]
    pub force: bool,

    /// Compress every .iso found in the INPUT directory
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,
}

/// Decompresses a CSO or ZSO container back to a plain ISO.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct DecompressCommand {
    /// Input .cso or .zso path
    pub input: PathBuf,

    /// Output ISO path, defaults to the input with extension replaced by .iso
    pub output: Option<PathBuf>,

    /// Force overwrite of the output file if it already exists
    #[arg(long, short = 'f', default_value_t = false)]
    pub force: bool,
}

/// Verifies the integrity of a CSO or ZSO container.
///
/// The formats embed no checksums, so the standard pass validates
/// the index structure; --full additionally decodes every block.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct VerifyCommand {
    /// Input .cso or .zso path
    pub input: PathBuf,

    /// Decode every block instead of only checking the index
    #[arg(long)]
    pub full: bool,
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
}
