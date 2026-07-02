use crate::commands::ConflictPolicyArg;
use crate::commands::info_command::InfoCommand;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands specific to CTR (3DS) formats
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum CtrCommands {
    CdnToCia(CdnToCiaCommand),
    GenerateCdnTicket(GenerateCdnTicketCommand),
    Decrypt(DecryptCommand),
    Compress(CompressRomCommand),
    Decompress(DecompressRomCommand),
    Verify(VerifyCommand),
    Convert(ConvertCommand),
    Info(InfoCommand),
}

/// Convert CDN content to CIA format
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Convert CDN content to CIA format\n\nNote: By default the output CIA file is encrypted, if you want to decrypt it after conversion, use the --decrypt flag\nYou can also use the --compress flag to compress the CIA into Z3DS format (.zcia) after conversion, this requires the CIA to be decrypted first",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto ctr cdn-to-cia ./cdn-content\n  Explicit output: rom-converto ctr cdn-to-cia ./cdn-content game.cia\n  Whole folder:    rom-converto ctr cdn-to-cia -R ./cdn-dumps --output-dir ./cia\n"
)]
pub struct CdnToCiaCommand {
    /// Path to the CDN content directory
    #[arg(value_name = "CDN_DIR")]
    pub cdn_dir: PathBuf,

    /// Output CIA file path, defaults to the folder name with .cia extension
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output CIA file path, defaults to the folder name with .cia extension
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

    /// Clean up after conversion by removing the original CDN files
    #[arg(long, short = 'C', default_value = "false")]
    pub cleanup: bool,

    /// Recursively iterate through all directories in the CDN_DIR directory and convert each to a CIA file
    #[arg(long, short = 'R', default_value = "false")]
    pub recursive: bool,

    /// Ensure that a Ticket file exists in the CDN_DIR directory, generating one if it does not
    #[arg(long, short = 'T', default_value = "false")]
    pub ensure_ticket_exists: bool,

    /// Decrypt the CIA file after conversion, useful for emulators like Azahar
    #[arg(long, short = 'D', default_value = "false")]
    pub decrypt: bool,

    /// Compress the CIA file into Z3DS format (.zcia) after conversion, requires the CIA to be decrypted
    #[arg(long, short = 'Z', default_value = "false")]
    pub compress: bool,

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

/// Generate a Ticket file from CDN content
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Generate a Ticket file from CDN content\n\nThis Ticket file is not official from Nintendo: it has non-important data like the console ID set to null. A CIA file built with this ticket will not work on a stock 3DS, but works fine on emulators or a 3DS with custom firmware.",
    after_long_help = "EXAMPLES:\n  Default name: rom-converto ctr generate-cdn-ticket ./cdn-content\n  Custom name:  rom-converto ctr generate-cdn-ticket ./cdn-content my-ticket.tik\n"
)]
pub struct GenerateCdnTicketCommand {
    /// Path to the CDN content directory
    #[arg(value_name = "CDN_DIR")]
    pub cdn_dir: PathBuf,

    /// Output Ticket file path
    #[arg(value_name = "OUTPUT", default_value = "ticket.tik")]
    pub output: PathBuf,
}

/// Decrypt an encrypted 3DS ROM file
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Decrypt an encrypted 3DS ROM file\n\nSupported input formats: .cia, .3ds, .cci, .cxi\nThe format is auto-detected from the file contents.\n\nIf OUTPUT is omitted the decrypted file is written next to the input as <name>.decrypted.<ext>.\n\nUse --recursive/-R to point INPUT at a directory and decrypt every matching file in it and its subdirectories; pass --max-depth N to limit the descent depth (1 = top level only). In batch mode OUTPUT is ignored and each decrypted file is written next to its source as <name>.decrypted.<ext>.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto ctr decrypt game.cia\n  Explicit output: rom-converto ctr decrypt game.3ds game.decrypted.3ds\n  Whole folder:    rom-converto ctr decrypt -R ./roms --output-dir ./decrypted\n"
)]
pub struct DecryptCommand {
    /// Input ROM file path, or a directory when --recursive is set (.cia, .3ds, .cci, or .cxi)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output decrypted file path, defaults to <name>.decrypted.<ext> next to the input (ignored with --recursive)
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output decrypted file path, defaults to <name>.decrypted.<ext> next to the input (ignored with --recursive)
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

    /// Process all matching files in INPUT and its subdirectories
    #[arg(long, short = 'R', default_value = "false")]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

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

/// Compress a decrypted 3DS ROM to the Z3DS format
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Compress a decrypted 3DS ROM to the Z3DS format\n\nSupported input formats: .cia, .cci, .3ds, .cxi, .3dsx\nOutput extensions: .zcia, .zcci, .zcxi, .z3dsx\n\nNote: only decrypted ROMs can be compressed, since encrypted ROMs have near-zero compression ratios.\n\nUse --recursive/-R to point INPUT at a directory and compress every matching file in it and its subdirectories; pass --max-depth N to limit the descent depth (1 = top level only). In batch mode OUTPUT is ignored and each output is written next to its source.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto ctr compress game.cia\n  Explicit output: rom-converto ctr compress game.3ds game.z3ds\n  Whole folder:    rom-converto ctr compress -R ./roms --output-dir ./z3ds\n"
)]
pub struct CompressRomCommand {
    /// Input ROM file path, or a directory when --recursive is set (.cia, .cci, .3ds, .cxi, or .3dsx)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output file path, defaults to the input path with the extension prefixed by "z" (ignored with --recursive)
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output file path, defaults to the input path with the extension prefixed by "z" (ignored with --recursive)
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

    /// Zstd compression level (0 = library default, 22 = maximum ratio). Higher levels produce smaller output at the cost of compression time. Defaults to the library default when unset
    #[arg(short = 'l', long = "level", value_name = "LEVEL", value_parser = clap::value_parser!(i32).range(0..=22))]
    pub level: Option<i32>,

    /// Process all matching files in INPUT and its subdirectories
    #[arg(long, short = 'R', default_value = "false")]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

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

    /// Compress an encrypted ROM anyway, even though it barely compresses. Decrypt first with: rom-converto ctr decrypt <INPUT>
    #[arg(long = "allow-encrypted", default_value_t = false)]
    pub allow_encrypted: bool,
}

/// Decompress a Z3DS file back to the original ROM format
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Decompress a Z3DS file back to the original ROM format\n\nSupported input formats: .zcia, .zcci, .zcxi, .z3dsx\nOutput extensions: .cia, .cci, .cxi, .3dsx\n\nUse --recursive/-R to point INPUT at a directory and decompress every matching file in it and its subdirectories; pass --max-depth N to limit the descent depth (1 = top level only). In batch mode OUTPUT is ignored and each output is written next to its source.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto ctr decompress game.zcia\n  Explicit output: rom-converto ctr decompress game.z3ds game.3ds\n  Whole folder:    rom-converto ctr decompress -R ./z3ds --output-dir ./roms\n"
)]
pub struct DecompressRomCommand {
    /// Input Z3DS file path, or a directory when --recursive is set (.zcia, .zcci, .zcxi, or .z3dsx)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output file path, defaults to the input path with the "z" prefix removed (ignored with --recursive)
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output file path, defaults to the input path with the "z" prefix removed (ignored with --recursive)
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

    /// Process all matching files in INPUT and its subdirectories
    #[arg(long, short = 'R', default_value = "false")]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

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

/// Convert between CIA and CCI/3DS formats
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Convert between CIA and CCI/3DS formats\n\nDirection is auto-detected from the INPUT extension:\n  .cia       -> .3ds (CCI / NCSD)\n  .3ds, .cci -> .cia\n\nCCI/3DS to CIA produces an unsigned CIA with a zero title key, compatible with CFW (Luma3DS) and emulators (Citra/Lime3DS/Azahar). Not installable on stock 3DS.\n\nUse --recursive/-R to point INPUT at a directory and convert every matching file in it and its subdirectories; pass --max-depth N to limit the descent depth (1 = top level only). In batch mode OUTPUT is ignored and each output is written next to its source with the opposite extension.",
    after_long_help = "EXAMPLES:\n  CIA to 3DS:      rom-converto ctr convert game.cia\n  Explicit output: rom-converto ctr convert game.3ds game.cia\n  Whole folder:    rom-converto ctr convert -R ./roms --output-dir ./converted\n"
)]
pub struct ConvertCommand {
    /// Input ROM file path, or a directory when --recursive is set (.cia, .3ds, or .cci)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output file path, defaults to the input path with the converted extension (ignored with --recursive)
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output file path, defaults to the input path with the converted extension (ignored with --recursive)
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

    /// Process all matching files in INPUT and its subdirectories
    #[arg(long, short = 'R', default_value = "false")]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

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

/// Verify CTR ROM file integrity and legitimacy
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Verify a CTR ROM file's integrity by checking hashes and signatures\n\nSupported formats: .cia, .3ds, .cci, .cxi, .zcia, .zcci, .zcxi\n\nFor .cia files, classifies as:\n  - Legit: Both ticket and TMD signatures verify through Nintendo's cert chain\n  - Piratelegit: TMD signature verifies but ticket is forged\n  - Standard: Neither signature verifies\n\nFor .3ds/.cci files, verifies NCCH partition hashes (ExeFS, RomFS, ExHeader)\nCompressed Z3DS files are decompressed automatically before verification\n\nUse --recursive/-R to point INPUT at a directory and verify every matching file in it and its subdirectories; pass --max-depth N to limit the descent depth (1 = top level only). The command prints one line per file and a final tally.",
    after_long_help = "EXAMPLES:\n  Single file:  rom-converto ctr verify game.cia\n  Full check:   rom-converto ctr verify game.cia --full\n  Whole folder: rom-converto ctr verify -R ./roms\n"
)]
pub struct VerifyCommand {
    /// Input ROM file path, or a directory when --recursive is set (.cia, .3ds, .cci, .cxi, .zcia, .zcci, .zcxi)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Also verify content hashes against the TMD (CIA only, slower)
    #[arg(
        long = "full",
        visible_alias = "verify-content",
        default_value_t = false
    )]
    pub verify_content: bool,

    /// Process all matching files in INPUT and its subdirectories
    #[arg(long, short = 'R', default_value = "false")]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Parser, Debug)]
    struct Harness {
        #[command(subcommand)]
        cmd: CtrCommands,
    }

    #[test]
    fn output_flag_overrides_positional() {
        let h = Harness::parse_from(["bin", "decrypt", "game.cia", "-o", "out.cia"]);
        let CtrCommands::Decrypt(c) = h.cmd else {
            panic!("expected Decrypt");
        };
        assert_eq!(c.input, PathBuf::from("game.cia"));
        assert_eq!(c.output, None);
        assert_eq!(c.output_flag, Some(PathBuf::from("out.cia")));
    }

    #[test]
    fn output_flag_conflicts_with_positional() {
        let result =
            Harness::try_parse_from(["bin", "decrypt", "game.cia", "pos.cia", "-o", "flag.cia"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_compress_force() {
        let h = Harness::parse_from(["bin", "compress", "game.cia", "-f"]);
        let CtrCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.force);
    }

    #[test]
    fn parses_compress_output_dir() {
        let h = Harness::parse_from(["bin", "compress", "game.cia", "--output-dir", "out"]);
        let CtrCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.output_dir, Some(PathBuf::from("out")));
        assert_eq!(c.output, None);
    }

    #[test]
    fn parses_on_conflict_skip() {
        let h = Harness::parse_from(["bin", "compress", "game.cia", "--on-conflict", "skip"]);
        let CtrCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.on_conflict, ConflictPolicyArg::Skip);
    }

    #[test]
    fn parses_on_conflict_rename() {
        let h = Harness::parse_from(["bin", "compress", "game.cia", "--on-conflict", "rename"]);
        let CtrCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.on_conflict, ConflictPolicyArg::Rename);
    }

    #[test]
    fn force_still_accepted() {
        let h = Harness::parse_from(["bin", "compress", "game.cia", "-f"]);
        let CtrCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.force);
        assert_eq!(c.on_conflict, ConflictPolicyArg::Error);
    }

    #[test]
    fn force_and_on_conflict_conflict() {
        let result =
            Harness::try_parse_from(["bin", "compress", "game.cia", "-f", "--on-conflict", "skip"]);
        assert!(result.is_err());
    }

    #[test]
    fn defaults_on_conflict_to_error() {
        let h = Harness::parse_from(["bin", "compress", "game.cia"]);
        let CtrCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.on_conflict, ConflictPolicyArg::Error);
    }

    #[test]
    fn parses_allow_encrypted() {
        let h = Harness::parse_from(["bin", "compress", "game.cia", "--allow-encrypted"]);
        let CtrCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.allow_encrypted);
    }

    #[test]
    fn allow_encrypted_defaults_false() {
        let h = Harness::parse_from(["bin", "compress", "game.cia"]);
        let CtrCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(!c.allow_encrypted);
    }

    #[test]
    fn cdn_to_cia_recursive_parses_on_conflict() {
        let h = Harness::parse_from(["bin", "cdn-to-cia", "-R", "./cdn", "--on-conflict", "skip"]);
        let CtrCommands::CdnToCia(c) = h.cmd else {
            panic!("expected CdnToCia");
        };
        assert!(c.recursive);
        assert_eq!(c.on_conflict, ConflictPolicyArg::Skip);
    }

    #[test]
    fn output_dir_conflicts_with_output() {
        let result = Harness::try_parse_from([
            "bin",
            "compress",
            "game.cia",
            "pos.zcia",
            "--output-dir",
            "out",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn verify_full_and_alias() {
        let full = Harness::parse_from(["bin", "verify", "game.cia", "--full"]);
        let CtrCommands::Verify(c) = full.cmd else {
            panic!("expected Verify");
        };
        assert!(c.verify_content);

        let alias = Harness::parse_from(["bin", "verify", "game.cia", "--verify-content"]);
        let CtrCommands::Verify(c) = alias.cmd else {
            panic!("expected Verify");
        };
        assert!(c.verify_content);
    }
}
