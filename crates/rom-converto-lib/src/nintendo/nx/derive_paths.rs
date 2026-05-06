//! Default-output filename helpers shared by CLI and GUI.

use std::path::{Path, PathBuf};

pub fn derive_compressed_path(input: &Path) -> PathBuf {
    let new_ext = match input.extension().and_then(|s| s.to_str()) {
        Some(e) if e.eq_ignore_ascii_case("xci") => "xcz",
        _ => "nsz",
    };
    input.with_extension(new_ext)
}

pub fn derive_decompressed_path(input: &Path) -> PathBuf {
    let new_ext = match input.extension().and_then(|s| s.to_str()) {
        Some(e) if e.eq_ignore_ascii_case("xcz") => "xci",
        _ => "nsp",
    };
    input.with_extension(new_ext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nsp_to_nsz() {
        assert_eq!(
            derive_compressed_path(Path::new("/games/Foo.nsp")),
            PathBuf::from("/games/Foo.nsz")
        );
    }

    #[test]
    fn xci_to_xcz() {
        assert_eq!(
            derive_compressed_path(Path::new("Foo.xci")),
            PathBuf::from("Foo.xcz")
        );
    }

    #[test]
    fn nsz_to_nsp() {
        assert_eq!(
            derive_decompressed_path(Path::new("Foo.nsz")),
            PathBuf::from("Foo.nsp")
        );
    }

    #[test]
    fn xcz_to_xci() {
        assert_eq!(
            derive_decompressed_path(Path::new("Foo.xcz")),
            PathBuf::from("Foo.xci")
        );
    }
}
