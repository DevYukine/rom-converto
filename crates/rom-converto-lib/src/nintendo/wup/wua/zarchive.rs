//! Read-only ZArchive (`.wua`) reader.
//!
//! The format is footer-anchored: the 144-byte [`ZArchiveFooter`] at
//! the tail of the file points at the four index sections plus the
//! compressed data. Data blocks are 64 KiB and zstd-compressed; a
//! sentinel `compressed_size == 64 KiB` means "stored raw" so an
//! incompressible block still fits in one offset-record slot.

use binrw::BinRead;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use crate::nintendo::wup::constants::{
    COMPRESSED_BLOCK_SIZE, COMPRESSION_OFFSET_RECORD_SIZE, ENTRIES_PER_OFFSET_RECORD,
    FILE_DIR_NAME_OFFSET_MASK, FILE_DIRECTORY_ENTRY_SIZE, ZARCHIVE_FOOTER_MAGIC,
    ZARCHIVE_FOOTER_SIZE, ZARCHIVE_FOOTER_VERSION,
};
use crate::nintendo::wup::error::{WupError, WupResult};
use crate::nintendo::wup::models::file_tree::FileDirectoryEntry;
use crate::nintendo::wup::models::footer::ZArchiveFooter;
use crate::nintendo::wup::models::offset_record::CompressionOffsetRecord;

pub struct ZArchiveReader {
    file: File,
    names: Vec<u8>,
    entries: Vec<FileDirectoryEntry>,
    offset_records: Vec<CompressionOffsetRecord>,
    children_by_dir: HashMap<u32, Vec<u32>>,
    compressed_data_size: u64,
}

impl ZArchiveReader {
    pub fn open(path: &Path) -> WupResult<Self> {
        let mut file = File::open(path)?;
        let total = file.metadata()?.len();
        if total < ZARCHIVE_FOOTER_SIZE as u64 {
            return Err(WupError::InvalidZArchive("file shorter than footer".into()));
        }

        file.seek(SeekFrom::Start(total - ZARCHIVE_FOOTER_SIZE as u64))?;
        let mut footer_buf = vec![0u8; ZARCHIVE_FOOTER_SIZE];
        file.read_exact(&mut footer_buf)?;
        let footer = ZArchiveFooter::read(&mut Cursor::new(&footer_buf))
            .map_err(|e| WupError::InvalidZArchive(format!("footer parse: {}", e)))?;

        if footer.magic != ZARCHIVE_FOOTER_MAGIC {
            return Err(WupError::InvalidZArchive(format!(
                "bad footer magic 0x{:08X}",
                footer.magic
            )));
        }
        if footer.version != ZARCHIVE_FOOTER_VERSION {
            return Err(WupError::InvalidZArchive(format!(
                "unsupported zarchive version 0x{:08X}",
                footer.version
            )));
        }
        if footer.total_size != total {
            return Err(WupError::InvalidZArchive(format!(
                "footer total_size {} does not match file length {}",
                footer.total_size, total
            )));
        }

        let names = read_section(&mut file, &footer.section_names)?;

        let entries_bytes = read_section(&mut file, &footer.section_file_tree)?;
        if entries_bytes.len() % FILE_DIRECTORY_ENTRY_SIZE != 0 {
            return Err(WupError::InvalidZArchive(
                "file tree section size not aligned to entry size".into(),
            ));
        }
        let entry_count = entries_bytes.len() / FILE_DIRECTORY_ENTRY_SIZE;
        let mut entries = Vec::with_capacity(entry_count);
        let mut cur = Cursor::new(&entries_bytes);
        for _ in 0..entry_count {
            let e = FileDirectoryEntry::read(&mut cur)
                .map_err(|e| WupError::InvalidZArchive(format!("entry parse: {}", e)))?;
            entries.push(e);
        }

        let offset_records_bytes = read_section(&mut file, &footer.section_offset_records)?;
        if offset_records_bytes.len() % COMPRESSION_OFFSET_RECORD_SIZE != 0 {
            return Err(WupError::InvalidZArchive(
                "offset records section not aligned to record size".into(),
            ));
        }
        let record_count = offset_records_bytes.len() / COMPRESSION_OFFSET_RECORD_SIZE;
        let mut offset_records = Vec::with_capacity(record_count);
        let mut cur = Cursor::new(&offset_records_bytes);
        for _ in 0..record_count {
            let r = CompressionOffsetRecord::read(&mut cur)
                .map_err(|e| WupError::InvalidZArchive(format!("offset record parse: {}", e)))?;
            offset_records.push(r);
        }

        let mut children_by_dir: HashMap<u32, Vec<u32>> = HashMap::new();
        for (idx, entry) in entries.iter().enumerate() {
            if !entry.is_file() {
                let start = entry.node_start_index();
                let count = entry.child_count();
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
            compressed_data_size: footer.section_compressed_data.size,
        })
    }

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

    /// File entries (not subdirectories) directly under `dir_path`.
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

    /// Recursive file walk; returns paths relative to the archive root.
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

    pub fn has_file(&self, path: &str) -> bool {
        self.resolve(path)
            .map(|idx| self.entries[idx as usize].is_file())
            .unwrap_or(false)
    }

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
        let offset = (entry.name_offset_and_type_flag & FILE_DIR_NAME_OFFSET_MASK) as usize;
        if offset >= self.names.len() {
            return Err(WupError::InvalidZArchive("name offset past table".into()));
        }
        let prefix = self.names[offset];
        let (start, len) = if prefix & 0x80 == 0 {
            (offset + 1, prefix as usize)
        } else {
            if offset + 2 > self.names.len() {
                return Err(WupError::InvalidZArchive(
                    "name table truncated on 2-byte prefix".into(),
                ));
            }
            let hi = (prefix & 0x7F) as usize;
            let lo = self.names[offset + 1] as usize;
            (offset + 2, (hi << 8) | lo)
        };
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
            compressed_offset += record.block_size(s) as u64;
        }
        let block_size = record.block_size(slot);
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

fn read_section(
    file: &mut File,
    section: &crate::nintendo::wup::models::footer::ZArchiveSectionInfo,
) -> WupResult<Vec<u8>> {
    file.seek(SeekFrom::Start(section.offset))?;
    let mut buf = vec![0u8; section.size as usize];
    file.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::wup::compress_worker::spawn_zarchive_pool;
    use crate::nintendo::wup::constants::ZARCHIVE_DEFAULT_ZSTD_LEVEL;
    use crate::nintendo::wup::zarchive_writer::ZArchiveWriter;
    use std::io::Write;

    fn build_archive_to_path<F>(path: &Path, build: F)
    where
        F: FnOnce(&mut ZArchiveWriter<Vec<u8>>) -> WupResult<()>,
    {
        let mut writer = ZArchiveWriter::new(Vec::new(), ZARCHIVE_DEFAULT_ZSTD_LEVEL).unwrap();
        build(&mut writer).unwrap();
        let pool = spawn_zarchive_pool(ZARCHIVE_DEFAULT_ZSTD_LEVEL).unwrap();
        let (bytes, _) = writer.finalize(&pool, None).unwrap();
        pool.shutdown();
        let mut file = File::create(path).unwrap();
        file.write_all(&bytes).unwrap();
    }

    #[test]
    fn round_trips_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("test.wua");
        build_archive_to_path(&archive_path, |w| {
            w.make_dir("0000000000000000")?;
            w.make_dir("0000000000000000/meta")?;
            w.start_new_file("0000000000000000/meta/meta.xml")?;
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
            w.make_dir("title")?;
            w.start_new_file("title/big.bin")?;
            w.append_data(&payload)?;
            Ok(())
        });

        let mut reader = ZArchiveReader::open(&archive_path).unwrap();
        let bytes = reader.read_file("title/big.bin").unwrap();
        assert_eq!(bytes.len(), large.len());
        assert_eq!(bytes, large);
    }
}
