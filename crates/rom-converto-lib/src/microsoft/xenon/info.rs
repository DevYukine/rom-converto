//! Read a ZArchive's tree summary without decoding any block payload.

use std::io::BufReader;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::microsoft::xex::read_xex_info;
use crate::microsoft::zar::ZarReader;

use super::error::XenonResult;

/// Summary of a ZArchive's tree contents and root `default.xex` metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZarInfo {
    pub file_count: u64,
    pub dir_count: u64,
    pub logical_size: u64,
    pub compressed_size: u64,
    pub block_count: u64,
    pub has_default_xex: bool,
    /// Xbox 360 title metadata from a root `default.xex`, when present.
    pub xex: Option<crate::microsoft::xex::XexInfo>,
}

/// Reads a ZArchive's file/directory counts, sizes, and root
/// `default.xex` metadata, without decoding any block payload.
///
/// # Errors
/// Returns an error if the archive's footer or structure is invalid.
pub fn read_info(path: &Path) -> XenonResult<ZarInfo> {
    let compressed_size = std::fs::metadata(path)?.len();
    let mut reader = ZarReader::open(BufReader::with_capacity(
        1 << 20,
        std::fs::File::open(path)?,
    ))?;

    let mut file_count = 0u64;
    let mut dir_count = 0u64;
    let mut logical_size = 0u64;
    let mut has_default_xex = false;
    for entry in reader.entries()? {
        if entry.is_file {
            file_count += 1;
            logical_size += entry.size;
            if entry.path.eq_ignore_ascii_case("default.xex") {
                has_default_xex = true;
            }
        } else {
            dir_count += 1;
        }
    }

    let xex = has_default_xex
        .then(|| read_default_xex(&mut reader))
        .flatten();

    Ok(ZarInfo {
        file_count,
        dir_count,
        logical_size,
        compressed_size,
        block_count: reader.block_count(),
        has_default_xex,
        xex,
    })
}

/// Best effort: read the root `default.xex` out of the archive and parse
/// its metadata. Any lookup, read, or parse failure yields `None`.
fn read_default_xex<R: std::io::Read + std::io::Seek>(
    reader: &mut ZarReader<R>,
) -> Option<crate::microsoft::xex::XexInfo> {
    let index = reader.lookup("default.xex").ok()?;
    let mut bytes = Vec::new();
    reader.read_file(index, &mut bytes).ok()?;
    read_xex_info(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::microsoft::zar::ZarWriter;

    #[test]
    fn reads_tree_summary_and_detects_root_default_xex() {
        let mut buf = Vec::new();
        let mut writer = ZarWriter::new(&mut buf, 1).unwrap();
        writer.start_file("default.xex").unwrap();
        writer.append_data(b"xex-bytes").unwrap();
        writer.make_dir("data", true).unwrap();
        writer.start_file("data/save.bin").unwrap();
        writer.append_data(b"save").unwrap();
        writer.finish().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.zar");
        std::fs::write(&path, &buf).unwrap();

        let info = read_info(&path).unwrap();
        assert_eq!(info.file_count, 2);
        assert_eq!(info.dir_count, 1);
        assert_eq!(
            info.logical_size,
            "xex-bytes".len() as u64 + "save".len() as u64
        );
        assert_eq!(info.compressed_size, buf.len() as u64);
        assert!(info.has_default_xex);
        // "xex-bytes" is not a valid XEX2, so the found-and-read plumbing
        // runs but the parse degrades to None.
        assert!(info.xex.is_none());
    }

    #[test]
    fn no_default_xex_yields_none_xex_and_valid_stats() {
        let mut buf = Vec::new();
        let mut writer = ZarWriter::new(&mut buf, 1).unwrap();
        writer.start_file("readme.txt").unwrap();
        writer.append_data(b"hi").unwrap();
        writer.finish().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.zar");
        std::fs::write(&path, &buf).unwrap();

        let info = read_info(&path).unwrap();
        assert_eq!(info.file_count, 1);
        assert!(!info.has_default_xex);
        assert!(info.xex.is_none());
    }
}
