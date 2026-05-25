use clap::Parser;
use clap_complete::Shell;
use std::path::PathBuf;

/// Generate shell completion scripts for the rom-converto CLI.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Generate a shell completion script for the rom-converto CLI.\n\n\
                  Writes to stdout by default. Pass --out-dir to write a canonical\n\
                  per-shell file under that directory (created if missing) and\n\
                  print the resulting path instead.\n\n\
                  Examples:\n  \
                  Bash:       rom-converto shell-completions bash >> ~/.bashrc.d/rom-converto\n  \
                  Zsh:        rom-converto shell-completions zsh > \"${fpath[1]}/_rom-converto\"\n  \
                  Fish:       rom-converto shell-completions fish > ~/.config/fish/completions/rom-converto.fish\n  \
                  PowerShell: rom-converto shell-completions powershell >> $PROFILE\n  \
                  Elvish:     rom-converto shell-completions elvish > ~/.elvish/lib/rom-converto.elv"
)]
pub struct ShellCompletionsCommand {
    /// Target shell. Accepts bash, zsh, fish, powershell, elvish.
    #[arg(value_name = "SHELL", value_enum)]
    pub shell: Shell,

    /// Write the completion script into DIR using the canonical filename
    /// for that shell, instead of writing to stdout. Prints the path on
    /// success.
    #[arg(long, short = 'o', value_name = "DIR")]
    pub out_dir: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bash_minimal() {
        let cmd = ShellCompletionsCommand::parse_from(["bin", "bash"]);
        assert_eq!(cmd.shell, Shell::Bash);
        assert!(cmd.out_dir.is_none());
    }

    #[test]
    fn parses_zsh_with_out_dir() {
        let cmd = ShellCompletionsCommand::parse_from(["bin", "zsh", "--out-dir", "/tmp/c"]);
        assert_eq!(cmd.shell, Shell::Zsh);
        assert_eq!(cmd.out_dir, Some(PathBuf::from("/tmp/c")));
    }

    #[test]
    fn parses_out_dir_short_flag() {
        let cmd = ShellCompletionsCommand::parse_from(["bin", "fish", "-o", "/tmp/c"]);
        assert_eq!(cmd.shell, Shell::Fish);
        assert_eq!(cmd.out_dir, Some(PathBuf::from("/tmp/c")));
    }

    #[test]
    fn parses_powershell() {
        let cmd = ShellCompletionsCommand::parse_from(["bin", "powershell"]);
        assert_eq!(cmd.shell, Shell::PowerShell);
    }

    #[test]
    fn parses_elvish() {
        let cmd = ShellCompletionsCommand::parse_from(["bin", "elvish"]);
        assert_eq!(cmd.shell, Shell::Elvish);
    }

    #[test]
    fn rejects_unknown_shell() {
        let result = ShellCompletionsCommand::try_parse_from(["bin", "nushell"]);
        assert!(result.is_err());
    }
}
