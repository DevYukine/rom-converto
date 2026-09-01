use crate::commands::ConflictPolicyArg;
use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands for PlayStation 3 disc images
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum Ps3Commands {
    Decrypt(DecryptPs3Command),
    Info(InfoCommand),
}

/// Decrypt a PS3 ISO into a plain ISO
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Decrypt a PS3 ISO into a plain ISO\n\nThe disc alternates plain and encrypted sector regions; encrypted regions are AES-128-CBC decrypted with the per-disc data key. Output covers the region-table's sector span; trailing padding past it is not copied.\n\nThe data key is resolved from --key, else a sibling <input>.dkey.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto ps3 decrypt game.iso\n  Explicit key:    rom-converto ps3 decrypt --key game.dkey game.iso game.dec.iso\n  Whole folder:    rom-converto ps3 decrypt -R ./roms --output-dir ./decrypted\n"
)]
pub struct DecryptPs3Command {
    /// Input encrypted PS3 ISO path, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output ISO path, defaults to the input with `.decrypted` inserted before the extension
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Alias for the positional OUTPUT argument
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    /// Write output into this directory using the derived filename. Created if missing. Works with --recursive
    #[arg(long = "output-dir", value_name = "DIR", conflicts_with_all = ["output", "output_flag"])]
    pub output_dir: Option<PathBuf>,

    /// Output path template applied per file. Tokens: {title}, {titleId}, {region},
    /// {console}, {serial}, {ext}, {basename}. Resolves against extracted metadata;
    /// missing tokens fall back to the input basename. Joined under --output-dir
    #[arg(long = "output-template", value_name = "TEMPLATE", conflicts_with_all = ["output", "output_flag"])]
    pub output_template: Option<String>,

    /// Disc data key file (.dkey). Auto-discovers a sibling `<input>.dkey` when omitted.
    /// Can't be combined with --recursive: one key can't be right for every disc in the
    /// batch. Place a per-disc <name>.dkey file next to each ISO instead
    #[arg(long = "key", value_name = "FILE", conflicts_with = "recursive")]
    pub key: Option<PathBuf>,

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

    /// Decrypt every .iso found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

    /// Write a run report to FILE. Format inferred from the extension: .csv, .json, .html or .htm. Unknown extensions default to JSON. The file is overwritten directly
    #[arg(long = "report", value_name = "FILE")]
    pub report: Option<PathBuf>,

    /// Skip the encryption and key verification probe (use if a correct key is rejected)
    #[arg(long = "skip-probe", default_value_t = false)]
    pub skip_probe: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Parser, Debug)]
    struct Harness {
        #[command(subcommand)]
        cmd: Ps3Commands,
    }

    #[test]
    fn parses_decrypt_with_key() {
        let h = Harness::parse_from(["bin", "decrypt", "game.iso", "--key", "k.dkey"]);
        let Ps3Commands::Decrypt(c) = h.cmd else {
            panic!("expected Decrypt");
        };
        assert_eq!(c.input, PathBuf::from("game.iso"));
        assert_eq!(c.key, Some(PathBuf::from("k.dkey")));
        assert_eq!(c.output, None);
    }

    #[test]
    fn parses_decrypt_force() {
        let h = Harness::parse_from(["bin", "decrypt", "game.iso", "-f"]);
        let Ps3Commands::Decrypt(c) = h.cmd else {
            panic!("expected Decrypt");
        };
        assert!(c.force);
        assert!(c.on_conflict.is_none());
    }

    #[test]
    fn parses_decrypt_skip_probe() {
        let h = Harness::parse_from(["bin", "decrypt", "game.iso", "--skip-probe"]);
        let Ps3Commands::Decrypt(c) = h.cmd else {
            panic!("expected Decrypt");
        };
        assert!(c.skip_probe);
    }

    #[test]
    fn parses_decrypt_recursive() {
        let h = Harness::parse_from(["bin", "decrypt", "roms", "-R"]);
        let Ps3Commands::Decrypt(c) = h.cmd else {
            panic!("expected Decrypt");
        };
        assert!(c.recursive);
    }

    #[test]
    fn decrypt_output_flag_overrides_positional() {
        let h = Harness::parse_from(["bin", "decrypt", "game.iso", "-o", "out.iso"]);
        let Ps3Commands::Decrypt(c) = h.cmd else {
            panic!("expected Decrypt");
        };
        assert_eq!(c.output, None);
        assert_eq!(c.output_flag, Some(PathBuf::from("out.iso")));
    }

    #[test]
    fn decrypt_output_flag_conflicts_with_positional() {
        let result =
            Harness::try_parse_from(["bin", "decrypt", "game.iso", "pos.iso", "-o", "flag.iso"]);
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_output_dir_conflicts_with_positional() {
        let result = Harness::try_parse_from([
            "bin",
            "decrypt",
            "game.iso",
            "pos.iso",
            "--output-dir",
            "out",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn max_depth_requires_recursive() {
        let result = Harness::try_parse_from(["bin", "decrypt", "roms", "--max-depth", "2"]);
        assert!(result.is_err());
    }

    #[test]
    fn key_conflicts_with_recursive() {
        let result = Harness::try_parse_from(["bin", "decrypt", "roms", "-R", "--key", "k.dkey"]);
        assert!(result.is_err());
    }
}
