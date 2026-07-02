use clap::Parser;
use std::path::PathBuf;

/// Print metadata about a ROM or disc image: title, region, hashes and embedded artwork
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  Single file: rom-converto ctr info game.cia\n  Save icon:   rom-converto ctr info game.cia --save-icon ./icons\n  As JSON:     rom-converto ctr info game.cia --json\n"
)]
pub struct InfoCommand {
    /// File or directory to inspect
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Emit JSON instead of pretty text
    #[arg(long, default_value_t = false)]
    pub json: bool,

    /// Save the embedded icon as `<title_id>.png` under DIR
    #[arg(long, value_name = "DIR")]
    pub save_icon: Option<PathBuf>,

    /// Path to prod.keys for Switch, or a disc master key file for Wii U .wud/.wux info. Other consoles do not use it
    #[arg(long, value_name = "FILE")]
    pub keys: Option<PathBuf>,
}
