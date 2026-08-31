//! Read a ZArchive's tree summary without decoding any block payload.

use std::io::BufReader;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::microsoft::zar::ZarReader;

use super::error::XenonResult;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ZarInfo {
    pub file_count: u64,
    pub dir_count: u64,
    pub logical_size: u64,
    pub compressed_size: u64,
    pub block_count: u64,
    pub has_default_xex: bool,
}

pub fn read_info(path: &Path) -> XenonResult<ZarInfo> {
    let compressed_size = std::fs::metadata(path)?.len();
    let reader = ZarReader::open(BufReader::with_capacity(
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

    Ok(ZarInfo {
        file_count,
        dir_count,
        logical_size,
        compressed_size,
        block_count: reader.block_count(),
        has_default_xex,
    })
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
    }
}
