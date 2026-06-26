use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};

/// Build the path of a per-content `.ncch` scratch file. The writer
/// ([`parse_ncch`]) and the CIA read-back ([`decrypt_from_encrypted_cia`]) both
/// route through here so the two can never disagree on the name. The
/// `content_id` is rendered as lowercase hex to match the rest of the CTR code
/// (`write_cia` names CDN content files the same way); on a case-sensitive
/// filesystem an uppercase/lowercase split here makes the read-back miss the
/// file with `ENOENT`.
pub(crate) fn ncch_component_path(
    parent: &Path,
    stem: &str,
    label: &str,
    content_id: u32,
) -> PathBuf {
    parent.join(format!("{stem}.{label}.{content_id:08x}.ncch"))
}

/// Resolve the directory and filename stem the `.ncch` scratch files live
/// under. Canonicalizing on both the write and read side keeps them in
/// agreement for relative paths and symlinked input directories.
pub(crate) fn ncch_artifact_dir_and_stem(input: &Path) -> Result<(PathBuf, String)> {
    let file_name = input
        .file_name()
        .ok_or_else(|| anyhow!("input path has no filename"))?
        .to_string_lossy();
    let stem = file_name
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(&file_name)
        .to_string();

    let absolute = input.canonicalize()?;
    let normalized = if cfg!(windows) && absolute.to_string_lossy().starts_with(r"\\?\") {
        PathBuf::from(absolute.to_string_lossy()[4..].replace('\\', "/"))
    } else {
        absolute
    };
    let parent = normalized
        .parent()
        .ok_or_else(|| anyhow!("input path has no parent directory"))?
        .to_path_buf();

    Ok((parent, stem))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_name_uses_lowercase_hex_for_letter_bearing_id() {
        // A content_id with hex digits A-F is exactly the case that broke on
        // Linux: it must render lowercase so the writer and reader agree.
        let path = ncch_component_path(Path::new("/roms"), "game", "1", 0x0000_ABCD);
        assert_eq!(path, Path::new("/roms/game.1.0000abcd.ncch"));
    }

    #[test]
    fn component_name_pads_content_id_to_eight_hex_digits() {
        let path = ncch_component_path(Path::new("/d"), "g", "0", 0x5);
        assert_eq!(path.file_name().unwrap(), "g.0.00000005.ncch");
    }

    #[test]
    fn artifact_stem_drops_only_the_final_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let input = tmp.path().join("My.Game.v1.0.cia");
        std::fs::write(&input, b"x").unwrap();

        let (parent, stem) = ncch_artifact_dir_and_stem(&input).unwrap();
        assert_eq!(stem, "My.Game.v1.0");
        assert_eq!(parent, input.parent().unwrap().canonicalize().unwrap());
    }
}
