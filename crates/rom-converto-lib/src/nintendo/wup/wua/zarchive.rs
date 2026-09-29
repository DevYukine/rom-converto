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
use std::io::{Read, Seek, SeekFrom};
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
    children_by_dir: HashMap<u32, std::ops::Range<u32>>,
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
        for section in [
            footer.names,
            footer.file_tree,
            footer.offset_records,
            footer.compressed_data,
        ] {
            validate_section(section, total)?;
        }

        let names = read_section(&mut file, footer.names, total)?;
        let entries_bytes = read_section(&mut file, footer.file_tree, total)?;

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

        let record_bytes = read_section(&mut file, footer.offset_records, total)?;
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

        let mut children_by_dir: HashMap<u32, std::ops::Range<u32>> = HashMap::new();
        for (idx, entry) in entries.iter().enumerate() {
            if !entry.is_file() {
                let start = entry.node_start_index();
                let end = start.checked_add(entry.count()).ok_or_else(|| {
                    WupError::InvalidZArchive("directory child span overflow".into())
                })?;
                let end = end.min(u32::try_from(entries.len()).unwrap_or(u32::MAX));
                children_by_dir.insert(idx as u32, start..end);
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
            .clone()
            .filter_map(|idx| {
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
            .clone()
            .filter_map(|child_idx| {
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
        let mut stack = vec![(root_idx, trimmed.to_string())];
        let mut visited = std::collections::HashSet::from([root_idx]);
        while let Some((idx, prefix)) = stack.pop() {
            let Some(children) = self.children_by_dir.get(&idx) else {
                continue;
            };
            for child_idx in children.clone() {
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
                } else if visited.insert(child_idx) {
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

    /// Reads the full file into memory. Intended for small metadata files.
    ///
    /// # Errors
    /// Returns [`WupError::InvalidZArchive`] if `path` does not exist
    /// or names a directory, or its size cannot be represented/allocated.
    pub fn read_file(&mut self, path: &str) -> WupResult<Vec<u8>> {
        let entry = self.file_entry(path)?;
        let file_size = entry.file_size();
        self.validate_file_extent(entry)?;
        let capacity = usize::try_from(file_size)
            .map_err(|_| WupError::InvalidZArchive("file is too large for memory".into()))?;
        let mut out = Vec::new();
        out.try_reserve_exact(capacity)
            .map_err(|_| WupError::InvalidZArchive("unable to allocate file contents".into()))?;
        self.read_at_to(entry.file_offset(), file_size, &mut out)?;
        Ok(out)
    }

    fn file_entry(&self, path: &str) -> WupResult<FileDirectoryEntry> {
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
        Ok(entry)
    }

    fn resolve(&self, path: &str) -> Option<u32> {
        let mut cursor: u32 = 0;
        for component in path.split('/').filter(|s| !s.is_empty()) {
            let children = self.children_by_dir.get(&cursor)?;
            let mut found = None;
            for child_idx in children.clone() {
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

    fn validate_file_extent(&self, entry: FileDirectoryEntry) -> WupResult<()> {
        if entry.file_size() == 0 {
            return Ok(());
        }
        let extent_end = entry
            .file_offset()
            .checked_add(entry.file_size())
            .ok_or_else(|| WupError::InvalidZArchive("file extent overflow".into()))?;
        // The compressed byte size times the block size is an
        // astronomically loose ceiling (every real archive compresses).
        // The actual number of blocks a file's offset can ever resolve to
        // is bounded by how many offset records the footer declared.
        let max_uncompressed = (self.offset_records.len() as u64)
            .checked_mul(ENTRIES_PER_OFFSET_RECORD as u64)
            .and_then(|blocks| blocks.checked_mul(COMPRESSED_BLOCK_SIZE as u64))
            .ok_or_else(|| WupError::InvalidZArchive("archive extent overflow".into()))?;
        if extent_end > max_uncompressed {
            return Err(WupError::InvalidZArchive(
                "file extent past archive data".into(),
            ));
        }
        Ok(())
    }

    fn read_at_to<W: std::io::Write>(
        &mut self,
        file_offset: u64,
        file_size: u64,
        out: &mut W,
    ) -> WupResult<()> {
        if file_size == 0 {
            return Ok(());
        }
        let block_bytes = COMPRESSED_BLOCK_SIZE as u64;
        let mut remaining = file_size;
        let mut absolute = file_offset;
        while remaining > 0 {
            let block_index = absolute / block_bytes;
            let in_block_off = (absolute % block_bytes) as usize;
            let block = self.read_block(block_index)?;
            if in_block_off >= block.len() {
                return Err(WupError::InvalidZArchive(
                    "file range past decompressed block".into(),
                ));
            }
            let take =
                (block.len() - in_block_off).min(usize::try_from(remaining).unwrap_or(usize::MAX));
            out.write_all(&block[in_block_off..in_block_off + take])?;
            absolute = absolute
                .checked_add(take as u64)
                .ok_or_else(|| WupError::InvalidZArchive("file range offset overflow".into()))?;
            remaining -= take as u64;
        }
        Ok(())
    }

    fn read_block(&mut self, block_index: u64) -> WupResult<Vec<u8>> {
        let record_index = block_index / ENTRIES_PER_OFFSET_RECORD as u64;
        let record_index = usize::try_from(record_index).map_err(|_| {
            WupError::InvalidZArchive(format!("block {} past offset record table", block_index))
        })?;
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
            decode_compressed_block(&buf)
        }
    }
}

fn decode_compressed_block(bytes: &[u8]) -> WupResult<Vec<u8>> {
    // Try the bounded bulk decompressor first: it skips growing an
    // output buffer for the common single-frame, <=64 KiB case. A
    // block can also be several concatenated frames (or a skippable
    // frame first), which the bulk decompressor can't handle; on any
    // error fall back to the streaming decoder, matching develop's
    // unconditional `decode_all` semantics exactly, error included.
    zstd::bulk::decompress(bytes, COMPRESSED_BLOCK_SIZE)
        .or_else(|_| zstd::stream::decode_all(std::io::Cursor::new(bytes)))
        .map_err(|e| WupError::InvalidZArchive(format!("zstd decode: {}", e)))
}

fn validate_section(section: Section, archive_len: u64) -> WupResult<()> {
    let end = section
        .offset
        .checked_add(section.size)
        .ok_or_else(|| WupError::InvalidZArchive("section extent overflow".into()))?;
    if end > archive_len {
        return Err(WupError::InvalidZArchive(
            "section extends past archive".into(),
        ));
    }
    Ok(())
}

fn read_section(file: &mut File, section: Section, archive_len: u64) -> WupResult<Vec<u8>> {
    validate_section(section, archive_len)?;
    let size = usize::try_from(section.size)
        .map_err(|_| WupError::InvalidZArchive("section too large for memory".into()))?;
    file.seek(SeekFrom::Start(section.offset))?;
    let mut buf = vec![0u8; size];
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
    fn validates_section_extents_before_reading() {
        assert!(validate_section(Section { offset: 8, size: 2 }, 10).is_ok());
        assert!(matches!(
            validate_section(Section { offset: 9, size: 2 }, 10),
            Err(WupError::InvalidZArchive(_))
        ));
        assert!(matches!(
            validate_section(
                Section {
                    offset: u64::MAX,
                    size: 1
                },
                u64::MAX
            ),
            Err(WupError::InvalidZArchive(_))
        ));
    }

    #[test]
    fn walk_files_stops_on_directory_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("archive");
        File::create(&file_path).unwrap();
        let reader = ZArchiveReader {
            file: File::open(file_path).unwrap(),
            names: vec![0, 1, b'a'],
            entries: vec![
                FileDirectoryEntry::directory(0, 1, 1),
                FileDirectoryEntry::directory(1, 0, 1),
            ],
            offset_records: Vec::new(),
            children_by_dir: HashMap::from([(0, 1..2), (1, 0..1)]),
            compressed_data_size: 0,
        };
        assert!(reader.walk_files("a").is_empty());
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

    #[test]
    fn compressed_block_larger_than_logical_block_is_decoded() {
        let data: Vec<u8> = (0..=u8::MAX)
            .cycle()
            .take(COMPRESSED_BLOCK_SIZE + 1)
            .collect();
        let compressed = zstd::bulk::compress(&data, 1).unwrap();
        assert_eq!(decode_compressed_block(&compressed).unwrap(), data);
    }

    #[test]
    fn validate_file_extent_ceiling_derives_from_offset_record_capacity_not_compressed_size() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("archive");
        File::create(&file_path).unwrap();
        let reader = ZArchiveReader {
            file: File::open(&file_path).unwrap(),
            names: Vec::new(),
            entries: Vec::new(),
            // One offset record can only ever address
            // ENTRIES_PER_OFFSET_RECORD * COMPRESSED_BLOCK_SIZE = 1 MiB of
            // decompressed data, no matter how large the compressed
            // payload the footer claims.
            offset_records: vec![CompressionOffsetRecord::default()],
            children_by_dir: HashMap::new(),
            compressed_data_size: 10_000_000_000,
        };
        // A file claiming 5 MiB comfortably clears the old
        // compressed_data_size * COMPRESSED_BLOCK_SIZE ceiling (~655 TB
        // here) but exceeds what the single offset record can ever back.
        let entry = FileDirectoryEntry::file(0, 0, 5 * 1024 * 1024).unwrap();
        assert!(matches!(
            reader.validate_file_extent(entry),
            Err(WupError::InvalidZArchive(_))
        ));
    }
}
