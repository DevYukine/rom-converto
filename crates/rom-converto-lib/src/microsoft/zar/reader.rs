//! Streaming ZArchive (`.zar`) reader.
//!
//! The offset records, name table, and file tree are loaded into memory
//! on [`ZarReader::open`] (they are a few bytes per entry); block
//! payloads stay on disk and are decompressed on demand. There is no
//! block cache: conversions read each file once, front to back.

use super::format::{
    COMPRESSED_BLOCK_SIZE, CompressionOffsetRecord, ENTRIES_PER_OFFSET_RECORD,
    FILE_DIRECTORY_ENTRY_SIZE, FOOTER_SIZE, FileDirectoryEntry, Footer, OFFSET_RECORD_SIZE,
    Section, ZarError, ZarResult, decode_name_len, split_path,
};
use crate::util::CancelToken;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom, Write};

/// One node of the file tree paired with its full path from the root.
#[derive(Debug, Clone)]
pub struct ZarEntry {
    /// Slash-separated path, root excluded.
    pub path: String,
    /// Index into the file tree, usable with [`ZarReader::read_file`].
    pub index: u32,
    pub is_file: bool,
    /// Offset in the logical concatenated data stream; 0 for
    /// directories.
    pub offset: u64,
    pub size: u64,
}

/// Reader over a ZArchive container.
pub struct ZarReader<R: Read + Seek> {
    inner: R,
    footer: Footer,
    records: Vec<CompressionOffsetRecord>,
    names: Vec<u8>,
    tree: Vec<FileDirectoryEntry>,
    block_count: u64,
}

impl<R: Read + Seek> ZarReader<R> {
    /// Parse the footer and load the offset records, name table, and
    /// file tree.
    pub fn open(mut inner: R) -> ZarResult<Self> {
        let file_size = inner.seek(SeekFrom::End(0))?;
        if file_size < FOOTER_SIZE as u64 {
            return Err(ZarError::TooSmall(file_size));
        }
        inner.seek(SeekFrom::Start(file_size - FOOTER_SIZE as u64))?;
        let mut raw = [0u8; FOOTER_SIZE];
        inner.read_exact(&mut raw)?;
        let footer = Footer::from_bytes(&raw)?;

        if footer.total_size != file_size {
            return Err(ZarError::SizeMismatch {
                declared: footer.total_size,
                actual: file_size,
            });
        }
        for (name, section) in footer.sections() {
            if section
                .offset
                .checked_add(section.size)
                .is_none_or(|end| end > file_size)
            {
                return Err(ZarError::SectionOutOfBounds {
                    name,
                    offset: section.offset,
                    size: section.size,
                });
            }
        }

        let record_bytes = read_section(&mut inner, footer.offset_records)?;
        if !record_bytes.len().is_multiple_of(OFFSET_RECORD_SIZE) {
            return Err(ZarError::BadSectionLength {
                name: "offset records",
                len: footer.offset_records.size,
            });
        }
        let records: Vec<CompressionOffsetRecord> = record_bytes
            .as_chunks::<OFFSET_RECORD_SIZE>()
            .0
            .iter()
            .map(CompressionOffsetRecord::from_bytes)
            .collect();
        if records.is_empty() && footer.compressed_data.size > 0 {
            return Err(ZarError::BadSectionLength {
                name: "offset records",
                len: 0,
            });
        }

        let names = read_section(&mut inner, footer.names)?;

        let tree_bytes = read_section(&mut inner, footer.file_tree)?;
        if tree_bytes.is_empty() || !tree_bytes.len().is_multiple_of(FILE_DIRECTORY_ENTRY_SIZE) {
            return Err(ZarError::BadSectionLength {
                name: "file tree",
                len: footer.file_tree.size,
            });
        }
        let tree: Vec<FileDirectoryEntry> = tree_bytes
            .as_chunks::<FILE_DIRECTORY_ENTRY_SIZE>()
            .0
            .iter()
            .map(FileDirectoryEntry::from_bytes)
            .collect();

        let block_count = count_blocks(&records, footer.compressed_data.size);
        Ok(Self {
            inner,
            footer,
            records,
            names,
            tree,
            block_count,
        })
    }

    /// The archive's parsed footer.
    pub fn footer(&self) -> &Footer {
        &self.footer
    }

    /// Index of the root directory.
    pub fn root(&self) -> u32 {
        0
    }

    /// Fetch one file-tree node.
    pub fn entry(&self, index: u32) -> ZarResult<FileDirectoryEntry> {
        self.tree
            .get(index as usize)
            .copied()
            .ok_or(ZarError::BadNodeIndex(index))
    }

    /// Resolve `path` to a node index. Components split on `/` or `\`
    /// and match case-insensitively over ASCII, as the reference reader
    /// does.
    pub fn lookup(&self, path: &str) -> ZarResult<u32> {
        let mut node = self.root();
        for component in split_path(path) {
            let entry = self.entry(node)?;
            if entry.is_file() {
                return Err(ZarError::PathThroughFile(path.to_string()));
            }
            node = self
                .children(node, &entry)?
                .find(|&index| {
                    self.name_of(index)
                        .is_ok_and(|name| name.eq_ignore_ascii_case(component.as_bytes()))
                })
                .ok_or_else(|| ZarError::NotFound(path.to_string()))?;
        }
        Ok(node)
    }

    /// Every entry below the root, breadth-first, with full paths.
    pub fn entries(&self) -> ZarResult<Vec<ZarEntry>> {
        let mut out = Vec::new();
        let mut queue: Vec<(u32, String)> = vec![(self.root(), String::new())];
        let mut cursor = 0usize;
        while cursor < queue.len() {
            // `children()` already rejects a range that doesn't start
            // after its own node, which rules out cycles; this is a
            // belt-and-suspenders cap on overlapping ranges blowing up
            // the traversal.
            if queue.len() > self.tree.len() {
                return Err(ZarError::CorruptStructure(format!(
                    "file tree traversal visited more than the {} nodes present",
                    self.tree.len()
                )));
            }
            let (index, prefix) = queue[cursor].clone();
            cursor += 1;
            let entry = self.entry(index)?;
            if entry.is_file() {
                continue;
            }
            for child in self.children(index, &entry)? {
                let child_entry = self.entry(child)?;
                let name = String::from_utf8_lossy(self.name_of(child)?).into_owned();
                let path = if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}/{name}")
                };
                out.push(ZarEntry {
                    path: path.clone(),
                    index: child,
                    is_file: child_entry.is_file(),
                    offset: if child_entry.is_file() {
                        child_entry.file_offset()
                    } else {
                        0
                    },
                    size: if child_entry.is_file() {
                        child_entry.file_size()
                    } else {
                        0
                    },
                });
                queue.push((child, path));
            }
        }
        Ok(out)
    }

    /// Number of 64 KiB blocks in the compressed-data section.
    pub fn block_count(&self) -> u64 {
        self.block_count
    }

    /// Read one block's payload without decompressing it. The bool is
    /// set when the block was stored raw, meaning the payload is
    /// already the plain 65536 bytes.
    pub fn read_block_raw(&mut self, index: u64) -> ZarResult<(Vec<u8>, bool)> {
        let (offset, size) = self.block_location(index)?;
        self.inner.seek(SeekFrom::Start(offset))?;
        let mut payload = vec![0u8; size];
        self.inner.read_exact(&mut payload)?;
        Ok((payload, size == COMPRESSED_BLOCK_SIZE))
    }

    /// Read and decompress one block.
    pub fn read_block(&mut self, index: u64) -> ZarResult<Vec<u8>> {
        let (payload, stored_raw) = self.read_block_raw(index)?;
        decompress_block(&payload, stored_raw)
    }

    /// Stream the contents of file node `index` to `out`, returning the
    /// number of bytes written.
    pub fn read_file<W: Write>(&mut self, index: u32, out: &mut W) -> ZarResult<u64> {
        let entry = self.entry(index)?;
        if !entry.is_file() {
            return Err(ZarError::NotAFile(index));
        }
        let size = entry.file_size();
        let mut raw_offset = entry.file_offset();
        let mut remaining = size;
        while remaining > 0 {
            let block_offset = (raw_offset % COMPRESSED_BLOCK_SIZE as u64) as usize;
            let step = remaining.min((COMPRESSED_BLOCK_SIZE - block_offset) as u64) as usize;
            let block = self.read_block(raw_offset / COMPRESSED_BLOCK_SIZE as u64)?;
            out.write_all(&block[block_offset..block_offset + step])?;
            raw_offset += step as u64;
            remaining -= step as u64;
        }
        Ok(size)
    }

    /// Re-hash the archive and compare against the stored digest. The
    /// hash covers every byte before the footer plus the footer itself
    /// with its hash field zeroed.
    pub fn verify_integrity(&mut self, cancel: &CancelToken) -> ZarResult<()> {
        let mut hasher = Sha256::new();
        let mut remaining = self.footer.total_size - FOOTER_SIZE as u64;
        let mut buf = vec![0u8; 1 << 20];
        self.inner.seek(SeekFrom::Start(0))?;
        while remaining > 0 {
            if cancel.is_cancelled() {
                return Err(ZarError::Cancelled);
            }
            let take = (buf.len() as u64).min(remaining) as usize;
            self.inner.read_exact(&mut buf[..take])?;
            hasher.update(&buf[..take]);
            remaining -= take as u64;
        }
        let mut footer = self.footer;
        footer.integrity_hash = [0u8; 32];
        hasher.update(footer.to_bytes());
        if hasher.finalize().as_slice() == self.footer.integrity_hash {
            Ok(())
        } else {
            Err(ZarError::HashMismatch)
        }
    }

    /// Absolute file offset and payload size of one block, following
    /// the record's base offset plus the biased sizes ahead of it.
    fn block_location(&self, index: u64) -> ZarResult<(u64, usize)> {
        if index >= self.block_count {
            return Err(ZarError::BadBlockIndex(index));
        }
        let overflow = || ZarError::CorruptStructure(format!("block {index} location overflows"));
        let record = &self.records[(index / ENTRIES_PER_OFFSET_RECORD as u64) as usize];
        let slot = (index % ENTRIES_PER_OFFSET_RECORD as u64) as usize;
        let mut offset = record.base_offset;
        for size in &record.sizes[..slot] {
            offset = offset.checked_add(*size as u64 + 1).ok_or_else(overflow)?;
        }
        let size = record.sizes[slot] as usize + 1;
        let end = offset.checked_add(size as u64).ok_or_else(overflow)?;
        if end > self.footer.compressed_data.size {
            return Err(ZarError::BadBlockIndex(index));
        }
        let abs_offset = self
            .footer
            .compressed_data
            .offset
            .checked_add(offset)
            .ok_or_else(overflow)?;
        Ok((abs_offset, size))
    }

    /// Children of the directory at `own_index`. The writer always
    /// assigns a directory's children a higher index than the directory
    /// itself (root's children start at 1), so a range that doesn't
    /// start after `own_index` means the tree is corrupt and would
    /// otherwise let a self- or ancestor-referencing directory loop the
    /// BFS in [`Self::entries`] forever.
    fn children(
        &self,
        own_index: u32,
        entry: &FileDirectoryEntry,
    ) -> ZarResult<std::ops::Range<u32>> {
        let start = entry.node_start_index();
        if start <= own_index {
            return Err(ZarError::CorruptStructure(format!(
                "node {own_index}'s directory entry starts at {start}, which is not after itself"
            )));
        }
        let end = start
            .checked_add(entry.count())
            .filter(|&end| end as usize <= self.tree.len())
            .ok_or(ZarError::BadNodeIndex(start))?;
        Ok(start..end)
    }

    fn name_of(&self, index: u32) -> ZarResult<&[u8]> {
        let offset = self.entry(index)?.name_offset() as usize;
        let (len, header) = decode_name_len(&self.names, offset)?;
        self.names
            .get(offset + header..offset + header + len)
            .ok_or(ZarError::BadNameOffset(offset as u32))
    }
}

/// Turn a block payload into its 65536 plain bytes. Stored-raw blocks
/// pass through untouched; everything else is a zstd frame that must
/// expand to exactly one block.
pub fn decompress_block(payload: &[u8], stored_raw: bool) -> ZarResult<Vec<u8>> {
    if stored_raw {
        return Ok(payload.to_vec());
    }
    let block = zstd::bulk::decompress(payload, COMPRESSED_BLOCK_SIZE)
        .map_err(|e| ZarError::Zstd(e.to_string()))?;
    if block.len() != COMPRESSED_BLOCK_SIZE {
        return Err(ZarError::BadBlockSize {
            actual: block.len(),
        });
    }
    Ok(block)
}

/// Number of blocks the compressed-data section holds. Unused slots in
/// the final record are zero, so counting stops once the accumulated
/// offset reaches the section size.
fn count_blocks(records: &[CompressionOffsetRecord], section_size: u64) -> u64 {
    let Some(last) = records.last() else {
        return 0;
    };
    let mut count = ((records.len() - 1) * ENTRIES_PER_OFFSET_RECORD) as u64;
    let mut offset = last.base_offset;
    for size in &last.sizes {
        if offset >= section_size {
            break;
        }
        offset += *size as u64 + 1;
        count += 1;
    }
    count
}

fn read_section<R: Read + Seek>(inner: &mut R, section: Section) -> ZarResult<Vec<u8>> {
    inner.seek(SeekFrom::Start(section.offset))?;
    let mut buf = vec![0u8; section.size as usize];
    inner.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::microsoft::zar::format::{ROOT_NAME_OFFSET_SENTINEL, encode_name_len};
    use crate::microsoft::zar::writer::ZarWriter;
    use std::io::Cursor;

    /// Deterministic bytes zstd cannot shrink.
    fn incompressible(len: usize) -> Vec<u8> {
        let mut state: u64 = 0x1234_5678_9ABC_DEF0;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 33) as u8
            })
            .collect()
    }

    fn compressible(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| seed.wrapping_add((i % 61) as u8))
            .collect()
    }

    /// Nested dirs, an empty dir, an empty file, a file spanning more
    /// than 16 blocks, and a file that starts mid-block.
    fn sample_archive() -> (Vec<u8>, Vec<(String, Vec<u8>)>) {
        let files = vec![
            ("head.bin".to_string(), compressible(100, 1)),
            ("dir/mid.bin".to_string(), compressible(200_000, 2)),
            (
                "dir/sub/big.bin".to_string(),
                compressible(17 * COMPRESSED_BLOCK_SIZE + 7, 3),
            ),
            ("dir/sub/empty.bin".to_string(), Vec::new()),
            (
                "noise.bin".to_string(),
                incompressible(COMPRESSED_BLOCK_SIZE),
            ),
        ];
        let mut buf = Vec::new();
        let mut writer = ZarWriter::new(&mut buf, 3).expect("writer spawns");
        writer.make_dir("dir/empty", true).expect("dir is created");
        for (path, data) in &files {
            writer.start_file(path).expect("file opens");
            writer.append_data(data).expect("data appends");
        }
        writer.finish().expect("archive finishes");
        (buf, files)
    }

    #[test]
    fn round_trips_a_full_tree() {
        let (buf, files) = sample_archive();
        let mut reader = ZarReader::open(Cursor::new(buf)).expect("archive opens");

        let entries = reader.entries().expect("tree walks");
        let dirs: Vec<&str> = entries
            .iter()
            .filter(|e| !e.is_file)
            .map(|e| e.path.as_str())
            .collect();
        assert_eq!(dirs, vec!["dir", "dir/empty", "dir/sub"]);

        for (path, data) in &files {
            let index = reader.lookup(path).expect("path resolves");
            let entry = reader.entry(index).expect("node exists");
            assert!(entry.is_file());
            assert_eq!(entry.file_size(), data.len() as u64);
            let mut out = Vec::new();
            let written = reader.read_file(index, &mut out).expect("file reads");
            assert_eq!(written, data.len() as u64);
            assert_eq!(&out, data, "contents differ for {path}");
        }

        // A file after a short one starts mid-block.
        let mid = reader.lookup("dir/mid.bin").expect("path resolves");
        assert_eq!(reader.entry(mid).expect("node exists").file_offset(), 100);

        reader
            .verify_integrity(&CancelToken::new())
            .expect("hash matches");
    }

    #[test]
    fn lookup_is_case_insensitive_and_separator_agnostic() {
        let (buf, _) = sample_archive();
        let reader = ZarReader::open(Cursor::new(buf)).expect("archive opens");
        let a = reader.lookup("dir/sub/big.bin").expect("path resolves");
        let b = reader.lookup("\\DIR\\SUB\\BIG.BIN").expect("path resolves");
        assert_eq!(a, b);
        assert!(matches!(
            reader.lookup("dir/missing"),
            Err(ZarError::NotFound(_))
        ));
        assert!(matches!(
            reader.lookup("head.bin/child"),
            Err(ZarError::PathThroughFile(_))
        ));
    }

    #[test]
    fn stored_raw_block_round_trips() {
        let data = incompressible(COMPRESSED_BLOCK_SIZE);
        let mut buf = Vec::new();
        let mut writer = ZarWriter::new(&mut buf, 1).expect("writer spawns");
        writer.start_file("noise.bin").expect("file opens");
        writer.append_data(&data).expect("data appends");
        writer.finish().expect("archive finishes");

        let mut reader = ZarReader::open(Cursor::new(buf)).expect("archive opens");
        assert_eq!(reader.block_count(), 1);
        let (payload, stored_raw) = reader.read_block_raw(0).expect("block reads");
        assert!(stored_raw);
        assert_eq!(payload, data);
        assert_eq!(reader.read_block(0).expect("block decodes"), data);
        assert!(matches!(
            reader.read_block_raw(1),
            Err(ZarError::BadBlockIndex(1))
        ));
    }

    #[test]
    fn verify_integrity_detects_a_flipped_payload_byte() {
        let (mut buf, _) = sample_archive();
        buf[64] ^= 0x01;
        let mut reader = ZarReader::open(Cursor::new(buf)).expect("archive opens");
        assert!(matches!(
            reader.verify_integrity(&CancelToken::new()),
            Err(ZarError::HashMismatch)
        ));
    }

    #[test]
    fn entries_rejects_a_self_referencing_directory_instead_of_hanging() {
        // Node 1 is a directory whose own child range starts at its own
        // index, so a naive BFS would keep re-visiting it forever.
        let root = FileDirectoryEntry::directory(ROOT_NAME_OFFSET_SENTINEL, 1, 1);
        let looped = FileDirectoryEntry::directory(0, 1, 1);
        let mut tree_bytes = Vec::new();
        tree_bytes.extend_from_slice(&root.to_bytes());
        tree_bytes.extend_from_slice(&looped.to_bytes());

        let mut names = Vec::new();
        encode_name_len(4, &mut names);
        names.extend_from_slice(b"loop");

        let mut buf = Vec::new();
        let names_off = buf.len() as u64;
        buf.extend_from_slice(&names);
        let tree_off = buf.len() as u64;
        buf.extend_from_slice(&tree_bytes);
        let meta_off = buf.len() as u64;

        let footer = Footer {
            compressed_data: Section { offset: 0, size: 0 },
            offset_records: Section { offset: 0, size: 0 },
            names: Section {
                offset: names_off,
                size: names.len() as u64,
            },
            file_tree: Section {
                offset: tree_off,
                size: tree_bytes.len() as u64,
            },
            meta_directory: Section {
                offset: meta_off,
                size: 0,
            },
            meta_data: Section {
                offset: meta_off,
                size: 0,
            },
            integrity_hash: [0u8; 32],
            total_size: meta_off + FOOTER_SIZE as u64,
        };
        buf.extend_from_slice(&footer.to_bytes());

        let reader = ZarReader::open(Cursor::new(buf)).expect("archive opens");
        assert!(matches!(
            reader.entries(),
            Err(ZarError::CorruptStructure(_))
        ));
    }

    #[test]
    fn open_rejects_damaged_containers() {
        let (buf, _) = sample_archive();

        let mut bad_magic = buf.clone();
        let end = bad_magic.len();
        bad_magic[end - 4] ^= 0xFF;
        assert!(matches!(
            ZarReader::open(Cursor::new(bad_magic)),
            Err(ZarError::BadMagic(_))
        ));

        let mut bad_version = buf.clone();
        bad_version[end - 8] ^= 0xFF;
        assert!(matches!(
            ZarReader::open(Cursor::new(bad_version)),
            Err(ZarError::BadVersion(_))
        ));

        let mut truncated = buf.clone();
        truncated.truncate(end - 1);
        assert!(ZarReader::open(Cursor::new(truncated)).is_err());

        // Footer declares a size the file does not have.
        let mut bad_size = buf.clone();
        bad_size[end - FOOTER_SIZE + 128..end - FOOTER_SIZE + 136]
            .copy_from_slice(&u64::MAX.to_be_bytes());
        assert!(matches!(
            ZarReader::open(Cursor::new(bad_size)),
            Err(ZarError::SizeMismatch { .. })
        ));

        assert!(matches!(
            ZarReader::open(Cursor::new(buf[..64].to_vec())),
            Err(ZarError::TooSmall(64))
        ));
    }

    #[test]
    fn trailing_pad_is_excluded_from_file_sizes() {
        let mut buf = Vec::new();
        let mut writer = ZarWriter::new(&mut buf, 1).expect("writer spawns");
        writer.start_file("tail.bin").expect("file opens");
        writer.append_data(&[9u8; 100]).expect("data appends");
        writer.finish().expect("archive finishes");

        let mut reader = ZarReader::open(Cursor::new(buf)).expect("archive opens");
        assert_eq!(reader.block_count(), 1);
        let index = reader.lookup("tail.bin").expect("path resolves");
        let mut out = Vec::new();
        reader.read_file(index, &mut out).expect("file reads");
        assert_eq!(out, vec![9u8; 100]);
    }
}
