use crate::commands::ConflictPolicyArg;
use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands specific to Wii U (WUP) formats
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum WupCommands {
    Compress(CompressWupCommand),
    Decrypt(DecryptWupCommand),
    Verify(VerifyWupCommand),
    Info(InfoCommand),
}

/// Verify Wii U content integrity by recomputing each content's SHA-1 against the TMD content hashes
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Verify Wii U content integrity by recomputing each content's SHA-1 against the TMD content hashes\n\n\
For NUS directories and .wud / .wux discs, every raw-mode content is decrypted and its SHA-1 compared against the TMD hash. Hashed-mode content is reported as skipped (its TMD hash covers the hash tree, not the content). .wua and loadiine inputs are already decrypted and carry no TMD, so they get a structural readability check only.\n\n\
Disc images need the 16-byte master key, resolved from --key, a sibling <input>.key, or game.key next to the disc.",
    after_long_help = "EXAMPLES:\n  NUS directory: rom-converto wup verify ./title_dir\n  Disc with key: rom-converto wup verify --key game.key game.wud\n  Whole folder:  rom-converto wup verify -R ./titles\n"
)]
pub struct VerifyWupCommand {
    /// Disc master key file (.wud / .wux only). Auto-discovers `<input>.key` or `game.key` when omitted
    #[arg(long = "key", value_name = "KEYFILE")]
    pub key: Option<PathBuf>,

    /// Input: NUS directory, loadiine directory, .wua, or .wud / .wux disc, or a parent directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Verify every .wud / .wux disc in the INPUT directory and its subdirectories; NUS title directories are detected among the immediate children of INPUT only
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,
}

/// Decrypt a NUS-format Wii U title directory into a loadiine-style `meta/code/content` tree that Cemu can load directly
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  NUS directory: rom-converto wup decrypt -o ./title_out ./title_nus\n"
)]
pub struct DecryptWupCommand {
    /// Output directory. Created if missing
    #[arg(short, long, value_name = "OUTPUT")]
    pub output: PathBuf,

    /// Input NUS directory (canonical `title.tmd` + `.app` or community `tmd.<N>` + numbered content files)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// What to do when an output already exists: error, overwrite, skip, or rename to a numbered sibling
    #[arg(long = "on-conflict", value_enum, default_value_t = ConflictPolicyArg::Error)]
    pub on_conflict: ConflictPolicyArg,

    /// Alias for --on-conflict overwrite
    #[arg(
        long,
        short = 'f',
        default_value_t = false,
        conflicts_with = "on_conflict"
    )]
    pub force: bool,
}

/// Compress one or more Wii U titles into a Cemu-compatible .wua archive
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Compress one or more Wii U titles into a Cemu-compatible .wua archive\n\n\
                  Each input is auto-detected as one of:\n  \
                  - loadiine directory: already-decrypted `meta/`, `code/`, `content/`\n  \
                  - NUS directory: `title.tmd`, `title.tik`, `*.app` (auto-decrypted)\n  \
                  - disc image: `.wud` or `.wux` file (requires per-disc key)\n\n\
                  Disc images need a 16-byte master key. Keys are resolved in order:\n  \
                  1. `--key` flag, paired positionally with disc inputs\n  \
                  2. sibling `<input>.key` file\n  \
                  3. `game.key` in the same directory as the disc\n\n\
                  Multiple titles (base + update + DLC) can be bundled into a single\n\
                  archive by passing each input as a separate positional argument.",
    after_long_help = "EXAMPLES:\n  Single title:    rom-converto wup compress -o game.wua ./title_base\n  Disc with key:   rom-converto wup compress -o game.wua --key game.key game.wud\n  Bundle titles:   rom-converto wup compress -o game.wua ./title_base ./title_update ./title_dlc\n"
)]
pub struct CompressWupCommand {
    /// Output .wua file path
    #[arg(short, long, value_name = "OUTPUT")]
    pub output: PathBuf,

    /// Zstd compression level (0 = Cemu default of 6, 22 = max ratio). Higher levels produce smaller output at the cost of compression time
    #[arg(
        short = 'l',
        long = "level",
        value_name = "LEVEL",
        value_parser = clap::value_parser!(i32).range(0..=22)
    )]
    pub level: Option<i32>,

    /// Disc master key file path(s). Applies only to disc image inputs. When supplied multiple times, keys are paired with disc inputs in the order they appear on the command line; the Nth `--key` applies to the Nth disc input. Non-disc inputs silently skip past their positional slot. Omit entirely to let the loader auto-discover `<input>.key` or `game.key` next to each disc
    #[arg(long = "key", value_name = "KEYFILE")]
    pub key: Vec<PathBuf>,

    /// One or more title inputs to bundle into the archive. Each is auto-detected as a loadiine directory, a NUS directory, or a disc image file
    #[arg(required = true, num_args = 1.., value_name = "INPUT")]
    pub inputs: Vec<PathBuf>,

    /// What to do when an output already exists: error, overwrite, skip, or rename to a numbered sibling
    #[arg(long = "on-conflict", value_enum)]
    pub on_conflict: Option<ConflictPolicyArg>,

    /// Alias for --on-conflict overwrite
    #[arg(
        long,
        short = 'f',
        default_value_t = false,
        conflicts_with = "on_conflict"
    )]
    pub force: bool,
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
        assert_eq!(c.key, vec![PathBuf::from("game.key")]);
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
        assert!(c.key.is_empty());
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
        assert_eq!(c.key, vec![PathBuf::from("a.key"), PathBuf::from("b.key")]);
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

    #[test]
    fn parses_verify_with_key() {
        let h = Harness::parse_from(["bin", "verify", "--key", "game.key", "game.wud"]);
        let WupCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert_eq!(c.key, Some(PathBuf::from("game.key")));
        assert_eq!(c.input, PathBuf::from("game.wud"));
    }

    #[test]
    fn parses_verify_without_key() {
        let h = Harness::parse_from(["bin", "verify", "title_dir"]);
        let WupCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert!(c.key.is_none());
        assert_eq!(c.input, PathBuf::from("title_dir"));
    }

    #[test]
    fn parses_verify_recursive() {
        let h = Harness::parse_from(["bin", "verify", "-R", "roms"]);
        let WupCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert!(c.recursive);
        assert_eq!(c.input, PathBuf::from("roms"));
    }

    #[test]
    fn parses_compress_force() {
        let h = Harness::parse_from(["bin", "compress", "-o", "out.wua", "-f", "title_dir/"]);
        let WupCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.force);
    }

    #[test]
    fn compress_on_conflict_absent_is_none() {
        let h = Harness::parse_from(["bin", "compress", "-o", "out.wua", "title_dir/"]);
        let WupCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.on_conflict.is_none());
    }
}
