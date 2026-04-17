//! High-level TMD loading helpers.
//!
//! Thin wrappers over [`crate::nintendo::wup::models::tmd::WupTmd`]
//! for the NUS pipeline: read a `title.tmd` file and return the
//! parsed metadata + content list.

use std::path::Path;

use crate::nintendo::wup::error::WupResult;
use crate::nintendo::wup::models::WupTmd;

/// Parse an in-memory TMD blob.
pub fn parse_tmd_bytes(bytes: &[u8]) -> WupResult<WupTmd> {
    WupTmd::parse(bytes)
}

/// Convenience: read a TMD file from disk and run [`parse_tmd_bytes`]
/// on its contents.
pub fn read_tmd_file(path: &Path) -> WupResult<WupTmd> {
    let bytes = std::fs::read(path)?;
    parse_tmd_bytes(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::wup::models::tmd::{
        TmdContentEntry, TmdContentFlags, WUP_TMD_CONTENT_ENTRY_SIZE, WUP_TMD_HEADER_SIZE,
    };

    fn make_tmd_blob(title_id: u64, title_version: u16, contents: &[TmdContentEntry]) -> Vec<u8> {
        let mut bytes =
            vec![0u8; WUP_TMD_HEADER_SIZE + contents.len() * WUP_TMD_CONTENT_ENTRY_SIZE];
        bytes[0..4].copy_from_slice(&0x0001_0004u32.to_be_bytes());
        bytes[0x180] = 1;
        bytes[0x18C..0x194].copy_from_slice(&title_id.to_be_bytes());
        bytes[0x1DC..0x1DE].copy_from_slice(&title_version.to_be_bytes());
        bytes[0x1DE..0x1E0].copy_from_slice(&(contents.len() as u16).to_be_bytes());
        for (i, entry) in contents.iter().enumerate() {
            let start = WUP_TMD_HEADER_SIZE + i * WUP_TMD_CONTENT_ENTRY_SIZE;
            bytes[start..start + 4].copy_from_slice(&entry.content_id.to_be_bytes());
            bytes[start + 4..start + 6].copy_from_slice(&entry.index.to_be_bytes());
            bytes[start + 6..start + 8].copy_from_slice(&entry.flags.bits().to_be_bytes());
            bytes[start + 8..start + 16].copy_from_slice(&entry.size.to_be_bytes());
            bytes[start + 16..start + 48].copy_from_slice(&entry.hash);
        }
        bytes
    }

    #[test]
    fn parse_tmd_bytes_returns_content_list() {
        let contents = vec![
            TmdContentEntry {
                content_id: 0,
                index: 0,
                flags: TmdContentFlags::ENCRYPTED,
                size: 0x4000,
                hash: [0u8; 32],
            },
            TmdContentEntry {
                content_id: 1,
                index: 1,
                flags: TmdContentFlags::ENCRYPTED | TmdContentFlags::HASHED,
                size: 0x1000_0000,
                hash: [0u8; 32],
            },
        ];
        let bytes = make_tmd_blob(0x0005_000E_1010_2000, 32, &contents);
        let tmd = parse_tmd_bytes(&bytes).unwrap();
        assert_eq!(tmd.title_id, 0x0005_000E_1010_2000);
        assert_eq!(tmd.title_version, 32);
        assert_eq!(tmd.contents.len(), 2);
        assert!(tmd.contents[1].is_hashed());
    }

    #[test]
    fn read_from_path_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("title.tmd");
        let bytes = make_tmd_blob(0x0005_000E_1010_2000, 42, &[]);
        std::fs::write(&path, &bytes).unwrap();
        let tmd = read_tmd_file(&path).unwrap();
        assert_eq!(tmd.title_version, 42);
        assert!(tmd.contents.is_empty());
    }
}
