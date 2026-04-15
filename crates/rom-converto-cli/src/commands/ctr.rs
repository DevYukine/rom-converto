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
}

/// Convert CDN content to CIA format
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Convert CDN content to CIA format\n\nNote: By default the output CIA file is encrypted, if you want to decrypt it after conversion, use the --decrypt flag\nYou can also use the --compress flag to compress the CIA into Z3DS format (.zcia) after conversion, this requires the CIA to be decrypted first"
)]
pub struct CdnToCiaCommand {
    /// Path to the CDN content directory
    #[arg(value_name = "CDN_DIR")]
    pub cdn_dir: PathBuf,

    /// Output CIA file path, defaults to the folder name with .cia extension
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Whether to clean up the CDN directory after conversion
    #[arg(
        value_name = "CLEANUP",
        long,
        short = 'C',
        help = "cleans up after conversion by removing the original CDN files",
        default_value = "false"
    )]
    pub cleanup: bool,

    #[arg(
        value_name = "RECURSIVE",
        long,
        short = 'R',
        help = "recursively iterates through all dictionaries in the CDN_DIR directory and convert each to a CIA file",
        default_value = "false"
    )]
    pub recursive: bool,

    #[arg(
        value_name = "ENSURE_TICKET_EXISTS",
        long,
        short = 'T',
        help = "ensures that a Ticket file exists in the CDN_DIR directory, if not it will generate one",
        default_value = "false"
    )]
    pub ensure_ticket_exists: bool,

    #[arg(
        value_name = "DECRYPT",
        long,
        short = 'D',
        help = "decrypts the CIA file after conversion, useful for emulators like Azahar",
        default_value = "false"
    )]
    pub decrypt: bool,

    #[arg(
        value_name = "COMPRESS",
        long,
        short = 'Z',
        help = "compresses the CIA file into Z3DS format (.zcia) after conversion, requires the CIA to be decrypted",
        default_value = "false"
    )]
    pub compress: bool,
}

/// Generate a Ticket file from CDN content
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Generate a Ticket file from CDN content\n\nNote: that this Ticket file is not official from Nintendo\nInstead it has non-important data like Console ID set to null, a CIA file build with this ticket will not work on a Stock 3DS but fine on emulators or a 3DS with custom firmware"
)]
pub struct GenerateCdnTicketCommand {
    /// Path to the CDN content directory
    #[arg(value_name = "CDN_DIR")]
    pub cdn_dir: PathBuf,

    /// Output Ticket file path
    #[arg(value_name = "OUTPUT", default_value = "ticket.tik")]
    pub output: PathBuf,
}

/// Decrypts an encrypted 3DS ROM file
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Decrypt an encrypted 3DS ROM file\n\nSupported input formats: .cia, .3ds, .cci, .cxi\nThe format is auto-detected from the file contents.\nA new decrypted file is written to the output path."
)]
pub struct DecryptCommand {
    /// Input ROM file path (.cia, .3ds, .cci, or .cxi)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output decrypted file path
    #[arg(value_name = "OUTPUT")]
    pub output: PathBuf,
}

/// Compresses a decrypted 3DS ROM to the Z3DS format
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Compress a decrypted 3DS ROM to the Z3DS format\n\nSupported input formats: .cia, .cci, .3ds, .cxi, .3dsx\nOutput extensions: .zcia, .zcci, .zcxi, .z3dsx\n\nNote: only decrypted ROMs can be compressed, since encrypted ROMs have near-zero compression ratios."
)]
pub struct CompressRomCommand {
    /// Input ROM file path (.cia, .cci, .3ds, .cxi, or .3dsx)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output file path, defaults to the input path with the extension prefixed by "z"
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Zstd compression level (0 = library default, 22 = maximum
    /// ratio). Higher levels produce smaller output at the cost of
    /// compression time. Leave unset to use the library default.
    #[arg(short = 'l', long = "level", value_name = "LEVEL", value_parser = clap::value_parser!(i32).range(0..=22))]
    pub level: Option<i32>,
}

/// Decompresses a Z3DS file back to the original ROM format
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Decompress a Z3DS file back to the original ROM format\n\nSupported input formats: .zcia, .zcci, .zcxi, .z3dsx\nOutput extensions: .cia, .cci, .cxi, .3dsx"
)]
pub struct DecompressRomCommand {
    /// Input Z3DS file path (.zcia, .zcci, .zcxi, or .z3dsx)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output file path, defaults to the input path with the "z" prefix removed from the extension
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,
}

/// Verify CTR ROM file integrity and legitimacy
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Verify a CTR ROM file's integrity by checking hashes and signatures\n\nSupported formats: .cia, .3ds, .cci, .cxi, .zcia, .zcci, .zcxi\n\nFor .cia files, classifies as:\n  - Legit: Both ticket and TMD signatures verify through Nintendo's cert chain\n  - Piratelegit: TMD signature verifies but ticket is forged\n  - Standard: Neither signature verifies\n\nFor .3ds/.cci files, verifies NCCH partition hashes (ExeFS, RomFS, ExHeader)\nCompressed Z3DS files are decompressed automatically before verification"
)]
pub struct VerifyCommand {
    /// Input ROM file path (.cia, .3ds, .cci, .cxi, .zcia, .zcci, .zcxi)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Also verify content hashes against TMD (CIA only, slower)
    #[arg(long, default_value_t = false)]
    pub verify_content: bool,
}
