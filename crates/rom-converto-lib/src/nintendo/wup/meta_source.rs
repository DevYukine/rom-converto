//! Trait abstracting "fetch a virtual-path file from this title".
//!
//! Loadiine on-disk, NUS on-disk, and Cemu `.wua` ZArchive inputs all
//! expose the same `meta/...` and `code/...` namespace; the info path
//! reads the same handful of files from each. This abstraction lets
//! the `read_loadiine` path stay source-agnostic so the WUA reader can
//! stream files straight from the ZArchive without first extracting
//! them to a tempdir.

use anyhow::{Result, anyhow};
use std::path::PathBuf;

use crate::nintendo::wup::wua::ZArchiveReader;

pub trait MetaSource {
    fn read(&mut self, virtual_path: &str) -> Result<Option<Vec<u8>>>;
    fn exists(&mut self, virtual_path: &str) -> Result<bool> {
        Ok(self.read(virtual_path)?.is_some())
    }
}

pub struct DirSource {
    pub root: PathBuf,
}

impl DirSource {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl MetaSource for DirSource {
    fn read(&mut self, virtual_path: &str) -> Result<Option<Vec<u8>>> {
        let path = self.root.join(virtual_path);
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(std::fs::read(&path)?))
    }
}

pub struct WuaSource<'a> {
    pub reader: &'a mut ZArchiveReader,
    pub title_prefix: String,
}

impl<'a> WuaSource<'a> {
    pub fn new(reader: &'a mut ZArchiveReader, title_dir: &str) -> Self {
        let prefix = if title_dir.ends_with('/') {
            title_dir.to_string()
        } else {
            format!("{}/", title_dir)
        };
        Self {
            reader,
            title_prefix: prefix,
        }
    }
}

impl<'a> MetaSource for WuaSource<'a> {
    fn read(&mut self, virtual_path: &str) -> Result<Option<Vec<u8>>> {
        let full = format!("{}{}", self.title_prefix, virtual_path);
        if !self.reader.has_file(&full) {
            return Ok(None);
        }
        self.reader
            .read_file(&full)
            .map(Some)
            .map_err(|e| anyhow!("wua: read {}: {}", full, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn dir_source_returns_none_for_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut src = DirSource::new(tmp.path().to_path_buf());
        assert!(src.read("meta/iconTex.tga").unwrap().is_none());
        assert!(!src.exists("meta/iconTex.tga").unwrap());
    }

    #[test]
    fn dir_source_reads_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let meta_dir = tmp.path().join("meta");
        fs::create_dir_all(&meta_dir).unwrap();
        fs::write(meta_dir.join("meta.xml"), b"<app/>").unwrap();
        let mut src = DirSource::new(tmp.path().to_path_buf());
        assert!(src.exists("meta/meta.xml").unwrap());
        assert_eq!(src.read("meta/meta.xml").unwrap().unwrap(), b"<app/>");
    }
}
