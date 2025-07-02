use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Commands specific to CTR (3DS) formats
#[derive(Subcommand, Debug)]
pub enum CtrCommands {
    CdnToCia(CdnToCiaCommand),
    GenerateCdnTicket(GenerateCdnTicketCommand),
    DecryptCia(DecryptCiaCommand),
}

/// Convert CDN content to CIA format
#[derive(Parser, Debug)]
#[command(
    long_about = "Convert CDN content to CIA format\n\nNote: By default the output CIA file is encrypted, if you want to decrypt it after conversion, use the --decrypt flag"
)]
#[derive(Clone)]
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
}

/// Generate a Ticket file from CDN content
#[derive(Parser, Debug)]
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

/// Decrypts a CIA file
#[derive(Parser, Debug)]
pub struct DecryptCiaCommand {
    /// Input CIA file path
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output decrypted CIA file path
    #[arg(value_name = "OUTPUT")]
    pub output: PathBuf,
}
