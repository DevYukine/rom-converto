use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands specific to Wii U (WUP) formats.
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum WupCommands {
    Compress(CompressWupCommand),
    Decrypt(DecryptWupCommand),
}

/// Decrypt a NUS-format Wii U title directory into a loadiine-style
/// `meta/code/content` tree that Cemu can load directly.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct DecryptWupCommand {
    /// Output directory. Created if missing.
    #[arg(short, long, value_name = "OUTPUT")]
    pub output: PathBuf,

    /// Input NUS directory (canonical `title.tmd` + `.app` or
    /// community `tmd.<N>` + numbered content files).
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,
}

/// Compress one or more Wii U titles into a Cemu-compatible .wua
/// archive. Each input is auto-detected as a loadiine directory, a
/// NUS directory, or a disc image file (.wud / .wux). Encrypted
/// inputs are decrypted on the fly: NUS titles use the built-in
/// common key, disc images use a per-disc master key supplied via
/// `--key` or a sibling `.key` file.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Compress one or more Wii U titles into a Cemu-compatible .wua archive\n\n\
                  Each input can be:\n  \
                  - loadiine directory: already-decrypted `meta/`, `code/`, `content/`\n  \
                  - NUS directory: `title.tmd`, `title.tik`, `*.app` (auto-decrypted)\n  \
                  - disc image: `.wud` or `.wux` file (requires per-disc key)\n\n\
                  Disc images need a 16-byte master key. Keys are resolved in order:\n  \
                  1. `--key` flag, paired positionally with disc inputs\n  \
                  2. sibling `<input>.key` file\n  \
                  3. `game.key` in the same directory as the disc\n\n\
                  Multiple titles (base + update + DLC) can be bundled into a single\n\
                  archive by passing each input as a separate positional argument."
)]
pub struct CompressWupCommand {
    /// Output .wua file path.
    #[arg(short, long, value_name = "OUTPUT")]
    pub output: PathBuf,

    /// Zstd compression level (0 = Cemu default of 6, 22 = max
    /// ratio). Higher levels produce smaller output at the cost of
    /// compression time.
    #[arg(
        short = 'l',
        long = "level",
        value_name = "LEVEL",
        value_parser = clap::value_parser!(i32).range(0..=22)
    )]
    pub level: Option<i32>,

    /// Disc master key file path(s). Applies only to disc image
    /// inputs. When supplied multiple times, keys are paired with
    /// disc inputs in the order they appear on the command line; the
    /// Nth `--key` applies to the Nth disc input. Non-disc inputs
    /// silently skip past their positional slot. Omit entirely to
    /// let the loader auto-discover `<input>.key` or `game.key` next
    /// to each disc.
    #[arg(long = "key", value_name = "KEYFILE")]
    pub keys: Vec<PathBuf>,

    /// One or more title inputs to bundle into the archive. Each is
    /// auto-detected as a loadiine directory, a NUS directory, or a
    /// disc image file.
    #[arg(required = true, num_args = 1.., value_name = "INPUT")]
    pub inputs: Vec<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Harness {
        #[command(subcommand)]
        cmd: WupCommands,
    }

    #[test]
    fn parses_single_disc_with_key() {
        let h = Harness::parse_from([
            "bin", "compress", "-o", "out.wua", "--key", "game.key", "game.wud",
        ]);
        let WupCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.output, PathBuf::from("out.wua"));
        assert_eq!(c.keys, vec![PathBuf::from("game.key")]);
        assert_eq!(c.inputs, vec![PathBuf::from("game.wud")]);
    }

    #[test]
    fn parses_mixed_inputs_without_keys() {
        let h = Harness::parse_from([
            "bin",
            "compress",
            "-o",
            "out.wua",
            "title_base/",
            "title_update/",
        ]);
        let WupCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.inputs.len(), 2);
        assert!(c.keys.is_empty());
    }

    #[test]
    fn parses_two_disc_inputs_with_two_keys() {
        let h = Harness::parse_from([
            "bin", "compress", "-o", "out.wua", "--key", "a.key", "--key", "b.key", "a.wud",
            "b.wux",
        ]);
        let WupCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.keys, vec![PathBuf::from("a.key"), PathBuf::from("b.key")]);
        assert_eq!(c.inputs.len(), 2);
    }

    #[test]
    fn parses_decrypt() {
        let h = Harness::parse_from(["bin", "decrypt", "-o", "out_dir", "input_dir"]);
        let WupCommands::Decrypt(c) = h.cmd else {
            panic!("expected Decrypt");
        };
        assert_eq!(c.output, PathBuf::from("out_dir"));
        assert_eq!(c.input, PathBuf::from("input_dir"));
    }

    #[test]
    fn rejects_missing_input() {
        let result = Harness::try_parse_from(["bin", "compress", "-o", "out.wua"]);
        assert!(result.is_err());
    }
}
