//! Home-directory tilde expansion for paths read from config or CLI args.

use std::path::{Path, PathBuf};

/// Expands a leading `~` to the user's home directory. Only `~` on its own
/// or `~/...` / `~\...` are expanded; `~user` forms and paths without a
/// leading `~` are returned unchanged, as is any path when the home
/// directory can't be determined.
pub fn expand_tilde(path: &Path) -> PathBuf {
    expand_tilde_with(path, dirs::home_dir().as_deref())
}

fn expand_tilde_with(path: &Path, home: Option<&Path>) -> PathBuf {
    let Some(home) = home else {
        return path.to_path_buf();
    };
    let Some(s) = path.to_str() else {
        return path.to_path_buf();
    };

    if s == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        return home.join(rest);
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::expand_tilde_with;
    use std::path::{Path, PathBuf};

    fn home() -> PathBuf {
        PathBuf::from("/home/user")
    }

    #[test]
    fn bare_tilde_expands_to_home() {
        assert_eq!(expand_tilde_with(Path::new("~"), Some(&home())), home());
    }

    #[test]
    fn tilde_slash_expands() {
        assert_eq!(
            expand_tilde_with(Path::new("~/x/y"), Some(&home())),
            home().join("x/y")
        );
    }

    #[test]
    fn tilde_backslash_expands_on_all_platforms() {
        assert_eq!(
            expand_tilde_with(Path::new("~\\x"), Some(&home())),
            home().join("x")
        );
    }

    #[test]
    fn tilde_user_form_unchanged() {
        let path = Path::new("~user/x");
        assert_eq!(expand_tilde_with(path, Some(&home())), path.to_path_buf());
    }

    #[test]
    fn absolute_path_unchanged() {
        let path = Path::new("/abs/x");
        assert_eq!(expand_tilde_with(path, Some(&home())), path.to_path_buf());
    }

    #[test]
    fn relative_path_unchanged() {
        let path = Path::new("relative/x");
        assert_eq!(expand_tilde_with(path, Some(&home())), path.to_path_buf());
    }

    #[test]
    fn mixed_separator_output_path_expands() {
        // Issue #19 shape: a `~/dir` prefix joined with a backslash file name.
        assert_eq!(
            expand_tilde_with(Path::new("~/roms/output\\game.chd"), Some(&home())),
            home().join("roms/output\\game.chd")
        );
    }

    #[test]
    fn no_home_returns_unchanged() {
        let path = Path::new("~/x");
        assert_eq!(expand_tilde_with(path, None), path.to_path_buf());
    }
}
