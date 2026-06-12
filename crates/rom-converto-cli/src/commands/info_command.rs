use clap::Parser;
use std::path::PathBuf;

/// Shared `info` subcommand reused by every per-console parent command.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct InfoCommand {
    /// File or directory to inspect.
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Emit JSON instead of pretty text.
    #[arg(long, default_value_t = false)]
    pub json: bool,

    /// Save the embedded icon as `<title_id>.png` under DIR.
    #[arg(long, value_name = "DIR")]
    pub save_icon: Option<PathBuf>,

    /// Path to prod.keys for Switch, or a disc master key file for Wii U .wud/.wux info. Other consoles do not use it.
    #[arg(long, value_name = "FILE")]
    pub keys: Option<PathBuf>,
}
