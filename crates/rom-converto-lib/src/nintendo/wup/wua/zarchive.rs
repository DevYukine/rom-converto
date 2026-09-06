//! Read-only ZArchive (`.wua`) reader.
//!
//! The format is footer-anchored: the 144-byte [`Footer`] at the tail
//! of the file points at the four index sections plus the compressed
//! data. Data blocks are 64 KiB and zstd-compressed; a sentinel
//! `compressed_size == 64 KiB` means "stored raw" so an
//! incompressible block still fits in one offset-record slot. The
//! structures themselves live in [`crate::zar::format`], shared with
//! the writer and the Xbox 360 `.zar` pipeline.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use crate::nintendo::wup::error::{WupError, WupResult};
use crate::zar::format::{
    COMPRESSED_BLOCK_SIZE, CompressionOffsetRecord, ENTRIES_PER_OFFSET_RECORD,
    FILE_DIRECTORY_ENTRY_SIZE, FOOTER_SIZE, FileDirectoryEntry, Footer, OFFSET_RECORD_SIZE,
    Section, decode_name_len,
};

/// Read-only handle onto one `.wua` file. Holds the parsed index
/// sections (names, file tree, offset records) in memory; file data
/// is decompressed on demand from the still-open backing file.
pub struct ZArchiveReader {
    file: File,
    names: Vec<u8>,
    entries: Vec<FileDirectoryEntry>,
    offset_records: Vec<CompressionOffsetRecord>,
    children_by_dir: HashMap<u32, Vec<u32>>,
    compressed_data_size: u64,
}

impl ZArchiveReader {
    /// Opens `path`, parses its footer and index sections, and
    /// builds a directory-to-children lookup for traversal.
    ///
    /// # Errors
    /// Returns [`WupError::InvalidZArchive`] if the file is too short,
    /// its footer magic/version don't match, its recorded total size
    /// disagrees with the file length, or any index section fails to parse.
    pub fn open(path: &Path) -> WupResult<Self> {
        let mut file = File::open(path)?;
        let total = file.metadata()?.len();
        if total < FOOTER_SIZE as u64 {
            return Err(WupError::InvalidZArchive("file shorter than footer".into()));
        }

        file.seek(SeekFrom::Start(total - FOOTER_SIZE as u64))?;
        let mut footer_buf = [0u8; FOOTER_SIZE];
        file.read_exact(&mut footer_buf)?;
        let footer = Footer::from_bytes(&footer_buf)
            .map_err(|e| WupError::InvalidZArchive(e.to_string()))?;

        if footer.total_size != total {
            return Err(WupError::InvalidZArchive(format!(
                "footer total_size {} does not match file length {}",
                footer.total_size, total
            )));
        }

        let names = read_section(&mut file, footer.names)?;

        let entries_bytes = read_section(&mut file, footer.file_tree)?;
        let (entry_chunks, rest) = entries_bytes.as_chunks::<FILE_DIRECTORY_ENTRY_SIZE>();
        if !rest.is_empty() {
            return Err(WupError::InvalidZArchive(
                "file tree section size not aligned to entry size".into(),
            ));
        }
        let entries: Vec<FileDirectoryEntry> = entry_chunks
            .iter()
            .map(FileDirectoryEntry::from_bytes)
            .collect();

        let record_bytes = read_section(&mut file, footer.offset_records)?;
        let (record_chunks, rest) = record_bytes.as_chunks::<OFFSET_RECORD_SIZE>();
        if !rest.is_empty() {
            return Err(WupError::InvalidZArchive(
                "offset records section not aligned to record size".into(),
            ));
        }
        let offset_records: Vec<CompressionOffsetRecord> = record_chunks
            .iter()
            .map(CompressionOffsetRecord::from_bytes)
            .collect();

        let mut children_by_dir: HashMap<u32, Vec<u32>> = HashMap::new();
        for (idx, entry) in entries.iter().enumerate() {
            if !entry.is_file() {
                let start = entry.node_start_index();
                let count = entry.count();
                let children: Vec<u32> = (start..start + count).collect();
                children_by_dir.insert(idx as u32, children);
            }
        }

        Ok(Self {
            file,
            names,
            entries,
            offset_records,
            children_by_dir,
            compressed_data_size: footer.compressed_data.size,
        })
    }

    /// Names of every entry directly under the archive root.
    pub fn top_level_names(&self) -> Vec<String> {
        let Some(root_children) = self.children_by_dir.get(&0) else {
            return Vec::new();
        };
        root_children
            .iter()
            .filter_map(|&idx| {
                let e = self.entries.get(idx as usize)?;
                self.entry_name(e).ok().map(|s| s.to_string())
            })
            .collect()
    }

    /// Names of the files directly inside `dir_path` (not recursive,
    /// and excludes subdirectories). Returns an empty list if the
    /// path is missing or names a file.
    pub fn list_files_in_dir(&self, dir_path: &str) -> Vec<String> {
        let idx = match self.resolve(dir_path) {
            Some(i) => i,
            None => return Vec::new(),
        };
        if self.entries[idx as usize].is_file() {
            return Vec::new();
        }
        let Some(children) = self.children_by_dir.get(&idx) else {
            return Vec::new();
        };
        children
            .iter()
            .filter_map(|&child_idx| {
                let entry = self.entries.get(child_idx as usize)?;
                if !entry.is_file() {
                    return None;
                }
                self.entry_name(entry).ok().map(|s| s.to_string())
            })
            .collect()
    }

    /// Recursively lists every file under `dir_path`, as paths
    /// relative to `dir_path` joined with `/`. Returns an empty list
    /// if the path is missing or names a file.
    pub fn walk_files(&self, dir_path: &str) -> Vec<String> {
        let Some(root_idx) = self.resolve(dir_path) else {
            return Vec::new();
        };
        if self.entries[root_idx as usize].is_file() {
            return Vec::new();
        }
        let trimmed = dir_path.trim_end_matches('/');
        let mut out = Vec::new();
        let mut stack: Vec<(u32, String)> = vec![(root_idx, trimmed.to_string())];
        while let Some((idx, prefix)) = stack.pop() {
            let Some(children) = self.children_by_dir.get(&idx) else {
                continue;
            };
            for &child_idx in children {
                let entry = match self.entries.get(child_idx as usize) {
                    Some(e) => *e,
                    None => continue,
                };
                let name = match self.entry_name(&entry) {
                    Ok(n) => n.to_string(),
                    Err(_) => continue,
                };
                let full = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{}/{}", prefix, name)
                };
                if entry.is_file() {
                    out.push(full);
                } else {
                    stack.push((child_idx, full));
                }
            }
        }
        out
    }

    /// True if `path` resolves to an existing file entry.
    pub fn has_file(&self, path: &str) -> bool {
        self.resolve(path)
            .map(|idx| self.entries[idx as usize].is_file())
            .unwrap_or(false)
    }

    /// Reads and decompresses the full contents of the file at `path`.
    ///
    /// # Errors
    /// Returns [`WupError::InvalidZArchive`] if `path` does not exist
    /// or names a directory.
    pub fn read_file(&mut self, path: &str) -> WupResult<Vec<u8>> {
        let idx = self
            .resolve(path)
            .ok_or_else(|| WupError::InvalidZArchive(format!("path not found: {}", path)))?;
        let entry = self.entries[idx as usize];
        if !entry.is_file() {
            return Err(WupError::InvalidZArchive(format!(
                "{} is a directory",
                path
            )));
        }
        let file_offset = entry.file_offset();
        let file_size = entry.file_size();
        self.read_at(file_offset, file_size)
    }

    fn resolve(&self, path: &str) -> Option<u32> {
        let mut cursor: u32 = 0;
        for component in path.split('/').filter(|s| !s.is_empty()) {
            let children = self.children_by_dir.get(&cursor)?;
            let mut found = None;
            for &child_idx in children {
                let entry = self.entries.get(child_idx as usize)?;
                if self
                    .entry_name(entry)
                    .ok()
                    .map(|n| n == component)
                    .unwrap_or(false)
                {
                    found = Some(child_idx);
                    break;
                }
            }
            cursor = found?;
        }
        Some(cursor)
    }

    fn entry_name(&self, entry: &FileDirectoryEntry) -> WupResult<&str> {
        let offset = entry.name_offset() as usize;
        let (len, header) = decode_name_len(&self.names, offset)
            .map_err(|e| WupError::InvalidZArchive(e.to_string()))?;
        let start = offset + header;
        if start + len > self.names.len() {
            return Err(WupError::InvalidZArchive("name slice past table".into()));
        }
        std::str::from_utf8(&self.names[start..start + len])
            .map_err(|_| WupError::InvalidZArchive("name is not valid UTF-8".into()))
    }

    fn read_at(&mut self, file_offset: u64, file_size: u64) -> WupResult<Vec<u8>> {
        let mut out = Vec::with_capacity(file_size as usize);
        if file_size == 0 {
            return Ok(out);
        }
        let block_bytes = COMPRESSED_BLOCK_SIZE as u64;
        let mut remaining = file_size;
        let mut absolute = file_offset;
        while remaining > 0 {
            let block_index = absolute / block_bytes;
            let in_block_off = (absolute % block_bytes) as usize;
            let block = self.read_block(block_index)?;
            let take = (block.len() - in_block_off).min(remaining as usize);
            out.extend_from_slice(&block[in_block_off..in_block_off + take]);
            absolute += take as u64;
            remaining -= take as u64;
        }
        Ok(out)
    }

    fn read_block(&mut self, block_index: u64) -> WupResult<Vec<u8>> {
        let record_index = (block_index / ENTRIES_PER_OFFSET_RECORD as u64) as usize;
        let slot = (block_index % ENTRIES_PER_OFFSET_RECORD as u64) as usize;
        let record = self.offset_records.get(record_index).ok_or_else(|| {
            WupError::InvalidZArchive(format!("block {} past offset record table", block_index))
        })?;
        let mut compressed_offset = record.base_offset;
        for s in 0..slot {
            compressed_offset += record.sizes[s] as u64 + 1;
        }
        let block_size = record.sizes[slot] as usize + 1;
        if compressed_offset + block_size as u64 > self.compressed_data_size {
            return Err(WupError::InvalidZArchive(
                "block read past compressed data".into(),
            ));
        }

        self.file.seek(SeekFrom::Start(compressed_offset))?;
        let mut buf = vec![0u8; block_size];
        self.file.read_exact(&mut buf)?;

        if block_size == COMPRESSED_BLOCK_SIZE {
            Ok(buf)
        } else {
            zstd::stream::decode_all(Cursor::new(&buf))
                .map_err(|e| WupError::InvalidZArchive(format!("zstd decode: {}", e)))
        }
    }
}

fn read_section(file: &mut File, section: Section) -> WupResult<Vec<u8>> {
    file.seek(SeekFrom::Start(section.offset))?;
    let mut buf = vec![0u8; section.size as usize];
    file.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zar::ZarWriter;

    fn build_archive_to_path<F>(path: &Path, build: F)
    where
        F: FnOnce(&mut ZarWriter<'_, File>) -> WupResult<()>,
    {
        let mut writer = ZarWriter::new(File::create(path).unwrap(), 2).unwrap();
        build(&mut writer).unwrap();
        writer.finish().unwrap();
    }

    #[test]
    fn round_trips_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("test.wua");
        build_archive_to_path(&archive_path, |w| {
            w.make_dir("0000000000000000", true)?;
            w.make_dir("0000000000000000/meta", true)?;
            w.start_file("0000000000000000/meta/meta.xml")?;
            w.append_data(b"<meta>hello</meta>")?;
            Ok(())
        });

        let mut reader = ZArchiveReader::open(&archive_path).unwrap();
        assert!(reader.has_file("0000000000000000/meta/meta.xml"));
        let bytes = reader.read_file("0000000000000000/meta/meta.xml").unwrap();
        assert_eq!(bytes, b"<meta>hello</meta>");
        let titles = reader.top_level_names();
        assert_eq!(titles, vec!["0000000000000000".to_string()]);
    }

    #[test]
    fn read_at_spans_multiple_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("multi.wua");

        let large: Vec<u8> = (0..200_000).map(|i| (i & 0xFF) as u8).collect();
        let payload = large.clone();
        build_archive_to_path(&archive_path, |w| {
            w.make_dir("title", true)?;
            w.start_file("title/big.bin")?;
            w.append_data(&payload)?;
            Ok(())
        });

        let mut reader = ZArchiveReader::open(&archive_path).unwrap();
        let bytes = reader.read_file("title/big.bin").unwrap();
        assert_eq!(bytes.len(), large.len());
        assert_eq!(bytes, large);
    }
}
