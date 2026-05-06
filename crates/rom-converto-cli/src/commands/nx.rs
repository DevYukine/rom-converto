use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands specific to Nintendo Switch (NX) NSP/XCI containers.
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum NxCommands {
    Compress(NxCompressCommand),
    Decompress(NxDecompressCommand),
    Verify(NxVerifyCommand),
}

/// Compress an NSP into NSZ or an XCI into XCZ. NCAs inside the
/// container are decrypted, zstd-compressed, and packaged with the
/// already-derived per-section keys cached in NCZSECTN.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct NxCompressCommand {
    /// Path to `prod.keys`. Defaults to `$HOME/.switch/prod.keys` on
    /// Linux/macOS or `%USERPROFILE%/.switch/prod.keys` on Windows,
    /// then the binary's own directory.
    #[arg(long = "keys", value_name = "PRODKEYS")]
    pub keys: Option<PathBuf>,

    /// Output path. Defaults to the input path with the extension
    /// switched (.nsp -> .nsz, .xci -> .xcz).
    #[arg(short, long, value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Zstd compression level. nsz default is 18; the maximum 22 needs
    /// over 1 GiB of RAM during decompression on the Switch.
    #[arg(
        short = 'l',
        long = "level",
        value_name = "LEVEL",
        value_parser = clap::value_parser!(i32).range(1..=22)
    )]
    pub level: Option<i32>,

    /// Compression mode. `solid` writes one zstd frame per NCA (smaller
    /// output, default for NSP). `block` writes independent zstd frames
    /// per fixed-size block (random read friendly, default for XCI).
    #[arg(long = "mode", value_parser = ["solid", "block"])]
    pub mode: Option<String>,

    /// Block-mode block size, expressed as a power of two (`exp` in
    /// `1 << exp` bytes). nsz default is 20 (1 MiB). Range 14..=32.
    #[arg(long = "block-size-exp", value_parser = clap::value_parser!(u8).range(14..=32))]
    pub block_size_exp: Option<u8>,

    /// Input NSP or XCI.
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,
}

/// Decompress an NSZ back to NSP or an XCZ back to XCI.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct NxDecompressCommand {
    #[arg(long = "keys", value_name = "PRODKEYS")]
    pub keys: Option<PathBuf>,

    #[arg(short, long, value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Input NSZ or XCZ.
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,
}

/// Verify hash integrity of every NCA in a Switch container.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct NxVerifyCommand {
    #[arg(long = "keys", value_name = "PRODKEYS")]
    pub keys: Option<PathBuf>,

    /// Input container (NSP / NSZ / XCI / XCZ).
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Harness {
        #[command(subcommand)]
        cmd: NxCommands,
    }

    #[test]
    fn parses_compress_minimal() {
        let h = Harness::parse_from(["bin", "compress", "game.nsp"]);
        let NxCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.input, PathBuf::from("game.nsp"));
        assert!(c.output.is_none());
        assert!(c.level.is_none());
    }

    #[test]
    fn parses_compress_with_keys_level_mode() {
        let h = Harness::parse_from([
            "bin",
            "compress",
            "--keys",
            "/k/prod.keys",
            "-l",
            "18",
            "--mode",
            "block",
            "--block-size-exp",
            "20",
            "-o",
            "out.nsz",
            "game.nsp",
        ]);
        let NxCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.keys, Some(PathBuf::from("/k/prod.keys")));
        assert_eq!(c.level, Some(18));
        assert_eq!(c.mode.as_deref(), Some("block"));
        assert_eq!(c.block_size_exp, Some(20));
        assert_eq!(c.output, Some(PathBuf::from("out.nsz")));
    }

    #[test]
    fn parses_decompress() {
        let h = Harness::parse_from(["bin", "decompress", "g.nsz"]);
        let NxCommands::Decompress(c) = h.cmd else {
            panic!("expected Decompress");
        };
        assert_eq!(c.input, PathBuf::from("g.nsz"));
    }

    #[test]
    fn parses_verify() {
        let h = Harness::parse_from(["bin", "verify", "--keys", "k", "g.nsz"]);
        let NxCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert_eq!(c.keys, Some(PathBuf::from("k")));
        assert_eq!(c.input, PathBuf::from("g.nsz"));
    }
}
