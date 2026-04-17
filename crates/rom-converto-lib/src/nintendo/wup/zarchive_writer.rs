//! ZArchive writer.
//!
//! Accepts files and directories incrementally through
//! `start_new_file` / `append_data` / `make_dir`, buffers the raw
//! uncompressed data into 64 KiB blocks, and compresses every block
//! in parallel at [`Self::finalize`] time through the worker pool in
//! [`crate::nintendo::wup::compress_parallel`]. The shape mirrors
//! upstream `ZArchiveWriter` in `zarchivewriter.cpp` so byte layouts
//! stay compatible with `zarchive.exe` and Cemu.
//!
//! Memory footprint is bounded by the sum of the uncompressed
//! file sizes the caller streams in plus per-worker scratch during
//! finalise. For typical Wii U titles (a few GB) this is comfortable
//! on 16 GB+ desktops; a future iteration can swap the in-memory
//! block buffer for a temp file if the workload grows.

use std::io::{Cursor, Write};

use binrw::BinWrite;
use sha2::{Digest, Sha256};

use crate::nintendo::wup::compress_parallel::{
    ZArchiveCompressWork, ZArchiveCompressedBlock, parallel_compress_blocks,
};
use crate::nintendo::wup::constants::{
    COMPRESSED_BLOCK_SIZE, COMPRESSION_OFFSET_RECORD_SIZE, FILE_DIRECTORY_ENTRY_SIZE,
    MAX_ZSTD_LEVEL, MIN_ZSTD_LEVEL, ROOT_NAME_OFFSET_SENTINEL, ZARCHIVE_FOOTER_SIZE,
};
use crate::nintendo::wup::error::{WupError, WupResult};
use crate::nintendo::wup::models::{
    CompressionOffsetRecord, FileDirectoryEntry, ZArchiveFooter, ZArchiveSectionInfo,
};
use crate::nintendo::wup::name_table::NameTableBuilder;
use crate::nintendo::wup::path_tree::PathTree;
use crate::util::worker_pool::Pool;

/// Single-threaded ZArchive writer. Holds the zstd compressor, the
/// accumulating path tree, the offset records table, and the running
/// SHA-256 hasher for the archive integrity field.
///
/// The public API mirrors upstream: `make_dir` / `start_new_file` /
/// `append_data` / `finalize`. The caller is expected to interleave
/// `start_new_file` + `append_data` to stream each virtual file into
/// the archive in any order; file offsets are tracked against a
/// single monotonic global input counter so nearby files can share
/// compressed blocks.
pub struct ZArchiveWriter<W: Write> {
    inner: W,
    hasher: Sha256,
    bytes_written: u64,

    write_buffer: Vec<u8>,
    pending_blocks: Vec<Vec<u8>>,
    current_input_offset: u64,
    offset_records: Vec<CompressionOffsetRecord>,

    tree: PathTree,
    current_file_path: Option<String>,
}

/// Abstract write-side interface: a stream of virtual files whose
/// bytes are eventually compressed into a ZArchive. Two concrete
/// implementations:
///
/// - [`ZArchiveWriter`]: buffered, in-memory. Tests use this
///   because it's easy to wrap around `Vec<u8>`.
/// - [`StreamingSink`]: pool-backed, streams completed 64 KiB
///   blocks straight into a background compress/write pipeline.
///   The production compress path uses this.
///
/// The `compress_*_title` readers (loadiine, NUS, disc) are generic
/// over this trait so the same read logic feeds both backends.
pub trait ArchiveSink {
    /// Create a directory at `path`. Parent directories are created
    /// as needed. Idempotent on existing directories.
    fn make_dir(&mut self, path: &str) -> WupResult<()>;

    /// Open a new virtual file at `path`. The file's global input
    /// offset is pinned to the current writer position, and
    /// subsequent [`Self::append_data`] calls accumulate into it.
    fn start_new_file(&mut self, path: &str) -> WupResult<()>;

    /// Append `data` to the currently active file (the one opened by
    /// the most recent [`Self::start_new_file`] call) and into the
    /// archive's uncompressed block stream.
    fn append_data(&mut self, data: &[u8]) -> WupResult<()>;
}

impl<W: Write> ArchiveSink for ZArchiveWriter<W> {
    fn make_dir(&mut self, path: &str) -> WupResult<()> {
        ZArchiveWriter::make_dir(self, path)
    }

    fn start_new_file(&mut self, path: &str) -> WupResult<()> {
        ZArchiveWriter::start_new_file(self, path)
    }

    fn append_data(&mut self, data: &[u8]) -> WupResult<()> {
        ZArchiveWriter::append_data(self, data)
    }
}

impl<W: Write> ZArchiveWriter<W> {
    /// Create a new writer wrapping `inner`. `level` is the zstd
    /// compression level (0..=22); passing `0` selects the Cemu
    /// default of
    /// [`crate::nintendo::wup::constants::ZARCHIVE_DEFAULT_ZSTD_LEVEL`]
    /// (6). The level is validated here for early error reporting;
    /// the actual compression runs through the caller-provided worker
    /// pool in [`Self::finalize`] / [`Self::drain_pending_blocks`].
    pub fn new(inner: W, level: i32) -> WupResult<Self> {
        if !(MIN_ZSTD_LEVEL..=MAX_ZSTD_LEVEL).contains(&level) {
            return Err(WupError::InvalidCompressionLevel {
                level,
                min: MIN_ZSTD_LEVEL,
                max: MAX_ZSTD_LEVEL,
            });
        }
        Ok(Self {
            inner,
            hasher: Sha256::new(),
            bytes_written: 0,
            write_buffer: Vec::with_capacity(COMPRESSED_BLOCK_SIZE),
            pending_blocks: Vec::new(),
            current_input_offset: 0,
            offset_records: Vec::new(),
            tree: PathTree::new(),
            current_file_path: None,
        })
    }

    /// Create a directory at `path`. Parent directories are created
    /// as needed. Idempotent on existing directories.
    pub fn make_dir(&mut self, path: &str) -> WupResult<()> {
        self.tree.make_dir(path)
    }

    /// Open a new virtual file at `path`. The file's global input
    /// offset is set to the current writer position, and subsequent
    /// [`Self::append_data`] calls accumulate bytes into it.
    ///
    /// There is no explicit "close file" call; calling
    /// `start_new_file` again implicitly finishes the previous file.
    pub fn start_new_file(&mut self, path: &str) -> WupResult<()> {
        self.tree.add_file(path, self.current_input_offset)?;
        self.current_file_path = Some(path.to_string());
        Ok(())
    }

    /// Append `data` to the currently active file (the one opened by
    /// the most recent [`Self::start_new_file`] call) and to the
    /// archive's uncompressed block queue. Bytes are chunked into 64
    /// KiB blocks and buffered in memory until [`Self::finalize`]
    /// hands them to the parallel compressor.
    pub fn append_data(&mut self, data: &[u8]) -> WupResult<()> {
        let total_data_size = data.len() as u64;
        let mut remaining = data;

        while !remaining.is_empty() {
            // Fast path: the partial-buffer is empty and we have at
            // least one full block of data, so copy it straight
            // into a fresh owned block without touching write_buffer.
            if self.write_buffer.is_empty() && remaining.len() >= COMPRESSED_BLOCK_SIZE {
                self.pending_blocks
                    .push(remaining[..COMPRESSED_BLOCK_SIZE].to_vec());
                remaining = &remaining[COMPRESSED_BLOCK_SIZE..];
                continue;
            }

            // Slow path: top up the partial buffer. When it fills to
            // 64 KiB, hand ownership off as a pending block and
            // allocate a fresh buffer for subsequent appends.
            let free_in_buffer = COMPRESSED_BLOCK_SIZE - self.write_buffer.len();
            let to_copy = remaining.len().min(free_in_buffer);
            self.write_buffer.extend_from_slice(&remaining[..to_copy]);
            remaining = &remaining[to_copy..];
            if self.write_buffer.len() == COMPRESSED_BLOCK_SIZE {
                let block = std::mem::replace(
                    &mut self.write_buffer,
                    Vec::with_capacity(COMPRESSED_BLOCK_SIZE),
                );
                self.pending_blocks.push(block);
            }
        }

        // Update the current file's size, if a file is open.
        if let Some(path) = self.current_file_path.clone()
            && let Some(node) = self.tree.get_mut(&path)
        {
            node.file_size += total_data_size;
        }
        self.current_input_offset += total_data_size;

        Ok(())
    }

    /// Number of 64 KiB blocks queued for compression, counting a
    /// partial trailing buffer as one extra block.
    pub fn pending_block_count(&self) -> usize {
        let trailing = if self.write_buffer.is_empty() { 0 } else { 1 };
        self.pending_blocks.len() + trailing
    }

    /// Compress and write every currently queued full block, then
    /// clear the queue. The partial trailing buffer is left alone so
    /// it can keep accumulating from the next `append_data`. The
    /// caller uses this between title reads to cap peak RAM and to
    /// overlap compression with the next title's file I/O. Cheap
    /// no-op when no full blocks are queued.
    pub fn drain_pending_blocks(
        &mut self,
        pool: &Pool<ZArchiveCompressWork, ZArchiveCompressedBlock, WupError>,
        progress: Option<&dyn crate::util::ProgressReporter>,
    ) -> WupResult<()>
    where
        W: Send,
    {
        if self.pending_blocks.is_empty() {
            return Ok(());
        }
        let pending = std::mem::take(&mut self.pending_blocks);
        parallel_compress_blocks(
            pool,
            pending,
            &mut self.inner,
            &mut self.hasher,
            &mut self.bytes_written,
            &mut self.offset_records,
            progress,
        )
    }

    /// Finalise the archive: parallel-compress every buffered block,
    /// then emit the offset records, name table, file tree, meta
    /// stubs, and the 144-byte footer with its SHA-256 integrity
    /// field. Returns the inner writer and the total archive size in
    /// bytes. `progress`, when present, receives an `inc` per 64 KiB
    /// block compressed.
    pub fn finalize(
        mut self,
        pool: &Pool<ZArchiveCompressWork, ZArchiveCompressedBlock, WupError>,
        progress: Option<&dyn crate::util::ProgressReporter>,
    ) -> WupResult<(W, u64)>
    where
        W: Send,
    {
        // Drop the current-file marker so any padding we AppendData
        // below doesn't count against it.
        self.current_file_path = None;

        // 1. Flush the trailing write buffer by padding it to a
        //    full 64 KiB block, mirroring upstream's behaviour.
        if !self.write_buffer.is_empty() {
            let pad_len = COMPRESSED_BLOCK_SIZE - self.write_buffer.len();
            let padding = vec![0u8; pad_len];
            self.append_data(&padding)?;
        }

        // Start an indeterminate "Finalizing" pulse so inc events
        // from the tail compress don't push the bar past 100%, and so
        // the metadata writes and the file-close OS flush that follow
        // still show activity.
        if let Some(p) = progress {
            p.start(0, "Finalizing archive");
        }

        // 2. Parallel-compress every pending block. This populates
        //    self.offset_records, self.bytes_written, and the hasher
        //    as if we had run the whole thing through the sequential
        //    pipeline.
        let pending = std::mem::take(&mut self.pending_blocks);
        parallel_compress_blocks(
            pool,
            pending,
            &mut self.inner,
            &mut self.hasher,
            &mut self.bytes_written,
            &mut self.offset_records,
            progress,
        )?;

        write_zarchive_tail(
            &mut self.inner,
            &mut self.hasher,
            &mut self.bytes_written,
            &self.offset_records,
            &mut self.tree,
        )?;

        Ok((self.inner, self.bytes_written))
    }
}

/// Write every archive section after the compressed-block stream:
/// the compressed-data-bounds marker, offset records, name table,
/// file tree, empty meta stubs, and the 144-byte footer with its
/// final SHA-256 integrity hash. Shared between [`ZArchiveWriter`]
/// and the streaming path so both produce byte-identical archives.
///
/// `inner`, `hasher`, and `bytes_written` all advance as the tail is
/// written. `tree` is sorted in place before the BFS walk.
pub(crate) fn write_zarchive_tail<W: Write>(
    inner: &mut W,
    hasher: &mut Sha256,
    bytes_written: &mut u64,
    offset_records: &[CompressionOffsetRecord],
    tree: &mut PathTree,
) -> WupResult<()> {
    let section_compressed_data = ZArchiveSectionInfo::new(0, *bytes_written);

    // 8-byte align the offset records section.
    while !bytes_written.is_multiple_of(8) {
        inner.write_all(&[0u8])?;
        hasher.update([0u8]);
        *bytes_written += 1;
    }

    let offset_records_start = *bytes_written;
    for record in offset_records {
        let mut cursor = Cursor::new(Vec::with_capacity(COMPRESSION_OFFSET_RECORD_SIZE));
        record.write(&mut cursor)?;
        let bytes = cursor.into_inner();
        debug_assert_eq!(bytes.len(), COMPRESSION_OFFSET_RECORD_SIZE);
        inner.write_all(&bytes)?;
        hasher.update(&bytes);
        *bytes_written += bytes.len() as u64;
    }
    let section_offset_records =
        ZArchiveSectionInfo::new(offset_records_start, *bytes_written - offset_records_start);

    tree.sort();
    let mut name_table = NameTableBuilder::new();
    let bfs = tree.bfs_entries();
    let mut tree_entries: Vec<FileDirectoryEntry> = Vec::with_capacity(bfs.len());
    for (i, entry) in bfs.iter().enumerate() {
        let fd_entry = if i == 0 {
            FileDirectoryEntry::new_directory(
                ROOT_NAME_OFFSET_SENTINEL,
                entry.node_start_index,
                entry.node.children.len() as u32,
            )
        } else {
            let name_offset = name_table.intern(&entry.node.name)?;
            if entry.node.is_file {
                FileDirectoryEntry::new_file(
                    name_offset,
                    entry.node.file_offset,
                    entry.node.file_size,
                )
            } else {
                FileDirectoryEntry::new_directory(
                    name_offset,
                    entry.node_start_index,
                    entry.node.children.len() as u32,
                )
            }
        };
        tree_entries.push(fd_entry);
    }

    let names_start = *bytes_written;
    let name_table_bytes = name_table.into_bytes();
    inner.write_all(&name_table_bytes)?;
    hasher.update(&name_table_bytes);
    *bytes_written += name_table_bytes.len() as u64;
    let section_names = ZArchiveSectionInfo::new(names_start, *bytes_written - names_start);

    let file_tree_start = *bytes_written;
    for entry in &tree_entries {
        let mut cursor = Cursor::new(Vec::with_capacity(FILE_DIRECTORY_ENTRY_SIZE));
        entry.write(&mut cursor)?;
        let bytes = cursor.into_inner();
        debug_assert_eq!(bytes.len(), FILE_DIRECTORY_ENTRY_SIZE);
        inner.write_all(&bytes)?;
        hasher.update(&bytes);
        *bytes_written += bytes.len() as u64;
    }
    let section_file_tree =
        ZArchiveSectionInfo::new(file_tree_start, *bytes_written - file_tree_start);

    let section_meta_directory = ZArchiveSectionInfo::new(*bytes_written, 0);
    let section_meta_data = ZArchiveSectionInfo::new(*bytes_written, 0);

    let mut footer = ZArchiveFooter::new(
        section_compressed_data,
        section_offset_records,
        section_names,
        section_file_tree,
        section_meta_directory,
        section_meta_data,
        *bytes_written + ZARCHIVE_FOOTER_SIZE as u64,
    );
    let mut scratch = Cursor::new(Vec::with_capacity(ZARCHIVE_FOOTER_SIZE));
    footer.write(&mut scratch)?;
    let scratch_bytes = scratch.into_inner();
    debug_assert_eq!(scratch_bytes.len(), ZARCHIVE_FOOTER_SIZE);
    hasher.update(&scratch_bytes);
    let final_hash: [u8; 32] = hasher.clone().finalize().into();

    footer.integrity_hash = final_hash;
    let mut final_cursor = Cursor::new(Vec::with_capacity(ZARCHIVE_FOOTER_SIZE));
    footer.write(&mut final_cursor)?;
    let final_bytes = final_cursor.into_inner();
    debug_assert_eq!(final_bytes.len(), ZARCHIVE_FOOTER_SIZE);
    inner.write_all(&final_bytes)?;
    *bytes_written += final_bytes.len() as u64;

    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    //! The tests double as a simple ZArchive spec for the writer:
    //! every structural property the upstream reader checks has a
    //! corresponding assertion here. The [`test_reader`] submodule
    //! is exposed `pub(crate)` so tests in `compress` and `nus`
    //! can round-trip archives through it without duplicating the
    //! parse logic.

    use super::*;
    use crate::nintendo::wup::compress_parallel::spawn_zarchive_pool;
    use crate::nintendo::wup::constants::{
        COMPRESSED_BLOCK_SIZE, ZARCHIVE_DEFAULT_ZSTD_LEVEL, ZARCHIVE_FOOTER_MAGIC,
        ZARCHIVE_FOOTER_SIZE, ZARCHIVE_FOOTER_VERSION,
    };

    /// Convenience: build a writer over a `Vec<u8>` and return the
    /// finalised archive bytes.
    fn build_archive<F>(build: F) -> Vec<u8>
    where
        F: FnOnce(&mut ZArchiveWriter<Vec<u8>>) -> WupResult<()>,
    {
        let mut writer = ZArchiveWriter::new(Vec::new(), ZARCHIVE_DEFAULT_ZSTD_LEVEL).unwrap();
        build(&mut writer).unwrap();
        let pool = spawn_zarchive_pool(ZARCHIVE_DEFAULT_ZSTD_LEVEL).unwrap();
        let (inner, total) = writer.finalize(&pool, None).unwrap();
        pool.shutdown();
        assert_eq!(
            total,
            inner.len() as u64,
            "writer reported size must match bytes produced"
        );
        inner
    }

    #[test]
    fn empty_archive_is_footer_only() {
        let bytes = build_archive(|_| Ok(()));
        let reader = test_reader::TestReader::open(&bytes).unwrap();
        // No files, no compressed data, just a root entry.
        assert_eq!(reader.footer.magic, ZARCHIVE_FOOTER_MAGIC);
        assert_eq!(reader.footer.version, ZARCHIVE_FOOTER_VERSION);
        assert_eq!(reader.footer.total_size, bytes.len() as u64);
        assert_eq!(reader.footer.section_compressed_data.offset, 0);
        assert_eq!(reader.footer.section_compressed_data.size, 0);
        assert_eq!(reader.file_tree.len(), 1);
        assert!(!reader.file_tree[0].is_file());
        // Root entry uses the sentinel name offset.
        assert_eq!(
            reader.file_tree[0].name_offset_and_type_flag,
            ROOT_NAME_OFFSET_SENTINEL
        );
    }

    #[test]
    fn single_tiny_file_round_trips() {
        let bytes = build_archive(|w| {
            w.start_new_file("hello.txt")?;
            w.append_data(b"hi")?;
            Ok(())
        });
        let reader = test_reader::TestReader::open(&bytes).unwrap();
        // Two entries: root + file.
        assert_eq!(reader.file_tree.len(), 2);
        let file_entry = reader.find_file("hello.txt").unwrap();
        assert!(file_entry.is_file());
        assert_eq!(file_entry.file_offset(), 0);
        assert_eq!(file_entry.file_size(), 2);
        assert_eq!(reader.extract_file("hello.txt"), b"hi");
    }

    #[test]
    fn multiple_small_files_share_one_block() {
        let bytes = build_archive(|w| {
            w.start_new_file("a.txt")?;
            w.append_data(b"alpha")?;
            w.start_new_file("b.txt")?;
            w.append_data(b"bravo")?;
            w.start_new_file("c.txt")?;
            w.append_data(b"charlie")?;
            Ok(())
        });
        let reader = test_reader::TestReader::open(&bytes).unwrap();
        assert_eq!(reader.extract_file("a.txt"), b"alpha");
        assert_eq!(reader.extract_file("b.txt"), b"bravo");
        assert_eq!(reader.extract_file("c.txt"), b"charlie");
        // All three files should live in one compressed block since
        // 5+5+7 = 17 bytes is well under 64 KiB.
        assert_eq!(reader.offset_records.len(), 1);
    }

    #[test]
    fn file_exactly_64_kib_stays_in_one_block() {
        let payload = vec![0x5Au8; COMPRESSED_BLOCK_SIZE];
        let bytes = build_archive(|w| {
            w.start_new_file("exact.bin")?;
            w.append_data(&payload)?;
            Ok(())
        });
        let reader = test_reader::TestReader::open(&bytes).unwrap();
        let extracted = reader.extract_file("exact.bin");
        assert_eq!(extracted.len(), COMPRESSED_BLOCK_SIZE);
        assert_eq!(extracted, payload);
        // One block, one record.
        assert_eq!(reader.offset_records.len(), 1);
    }

    #[test]
    fn file_64_kib_plus_one_spans_two_blocks() {
        let mut payload = vec![0xA5u8; COMPRESSED_BLOCK_SIZE + 1];
        *payload.last_mut().unwrap() = 0x42;
        let bytes = build_archive(|w| {
            w.start_new_file("big.bin")?;
            w.append_data(&payload)?;
            Ok(())
        });
        let reader = test_reader::TestReader::open(&bytes).unwrap();
        let extracted = reader.extract_file("big.bin");
        assert_eq!(extracted.len(), COMPRESSED_BLOCK_SIZE + 1);
        assert_eq!(extracted, payload);
    }

    #[test]
    fn payload_spanning_17_blocks_creates_second_offset_record() {
        // 17 full blocks exceeds ENTRIES_PER_OFFSET_RECORD (16) so a
        // second CompressionOffsetRecord must be produced.
        let payload_size = COMPRESSED_BLOCK_SIZE * 17;
        let mut payload = vec![0u8; payload_size];
        for (i, b) in payload.iter_mut().enumerate() {
            *b = (i & 0xFF) as u8;
        }
        let bytes = build_archive(|w| {
            w.start_new_file("huge.bin")?;
            w.append_data(&payload)?;
            Ok(())
        });
        let reader = test_reader::TestReader::open(&bytes).unwrap();
        assert_eq!(reader.offset_records.len(), 2);
        let extracted = reader.extract_file("huge.bin");
        assert_eq!(extracted.len(), payload_size);
        assert_eq!(extracted, payload);
        // Second record's base_offset should be strictly greater
        // than the first's.
        assert!(
            reader.offset_records[1].base_offset > reader.offset_records[0].base_offset,
            "second offset record must start after the first"
        );
    }

    #[test]
    fn nested_directories_round_trip() {
        let bytes = build_archive(|w| {
            w.make_dir("meta")?;
            w.make_dir("code")?;
            w.make_dir("content")?;
            w.start_new_file("meta/meta.xml")?;
            w.append_data(b"<meta/>")?;
            w.start_new_file("code/app.xml")?;
            w.append_data(b"<app/>")?;
            w.start_new_file("code/cos.xml")?;
            w.append_data(b"<cos/>")?;
            w.start_new_file("content/data.bin")?;
            w.append_data(&[0u8; 100])?;
            Ok(())
        });
        let reader = test_reader::TestReader::open(&bytes).unwrap();
        assert_eq!(reader.extract_file("meta/meta.xml"), b"<meta/>");
        assert_eq!(reader.extract_file("code/app.xml"), b"<app/>");
        assert_eq!(reader.extract_file("code/cos.xml"), b"<cos/>");
        assert_eq!(reader.extract_file("content/data.bin"), vec![0u8; 100]);
    }

    #[test]
    fn append_in_multiple_chunks_matches_single_append() {
        // Appending a payload in several chunks must produce the
        // same file bytes as appending it in one call. This covers
        // the partial-buffer path through store_block and the
        // block-aligned fast path at the same time.
        let payload: Vec<u8> = (0..200_000u32).map(|i| (i & 0xFF) as u8).collect();
        let bytes_multi = build_archive(|w| {
            w.start_new_file("data.bin")?;
            w.append_data(&payload[..123])?;
            w.append_data(&payload[123..50_000])?;
            w.append_data(&payload[50_000..131_072])?;
            w.append_data(&payload[131_072..])?;
            Ok(())
        });
        let reader = test_reader::TestReader::open(&bytes_multi).unwrap();
        assert_eq!(reader.extract_file("data.bin"), payload);
    }

    #[test]
    fn deterministic_output_on_identical_input() {
        let a = build_archive(|w| {
            w.start_new_file("a.txt")?;
            w.append_data(b"alpha")?;
            w.start_new_file("b.txt")?;
            w.append_data(b"bravo")?;
            Ok(())
        });
        let b = build_archive(|w| {
            w.start_new_file("a.txt")?;
            w.append_data(b"alpha")?;
            w.start_new_file("b.txt")?;
            w.append_data(b"bravo")?;
            Ok(())
        });
        assert_eq!(a, b, "writer output must be deterministic");
    }

    #[test]
    fn footer_magic_and_version_at_end() {
        let bytes = build_archive(|_| Ok(()));
        let tail = &bytes[bytes.len() - 8..];
        assert_eq!(&tail[0..4], &ZARCHIVE_FOOTER_VERSION.to_be_bytes());
        assert_eq!(&tail[4..8], &ZARCHIVE_FOOTER_MAGIC.to_be_bytes());
    }

    #[test]
    fn total_size_matches_bytes_on_disk() {
        let bytes = build_archive(|w| {
            w.start_new_file("a.txt")?;
            w.append_data(&[0u8; 77])?;
            Ok(())
        });
        let reader = test_reader::TestReader::open(&bytes).unwrap();
        assert_eq!(reader.footer.total_size, bytes.len() as u64);
    }

    #[test]
    fn integrity_hash_matches_recomputed() {
        // The reader's job is to recompute the SHA-256 over the
        // whole file with the integrity_hash field zeroed and
        // compare against what the writer stored. We mirror that
        // check directly here.
        use sha2::{Digest, Sha256};
        let bytes = build_archive(|w| {
            w.make_dir("meta")?;
            w.start_new_file("meta/meta.xml")?;
            w.append_data(b"<meta/>")?;
            Ok(())
        });
        let reader = test_reader::TestReader::open(&bytes).unwrap();
        let stored = reader.footer.integrity_hash;

        let mut zeroed = bytes.clone();
        // integrity_hash lives at footer offset 96..128; in absolute
        // file terms that's file_size - 144 + 96 .. file_size - 144 + 128.
        let footer_start = bytes.len() - ZARCHIVE_FOOTER_SIZE;
        for b in &mut zeroed[footer_start + 96..footer_start + 128] {
            *b = 0;
        }
        let mut hasher = Sha256::new();
        hasher.update(&zeroed);
        let recomputed: [u8; 32] = hasher.finalize().into();
        assert_eq!(
            stored, recomputed,
            "integrity hash must match recomputation"
        );
    }

    #[test]
    fn section_bounds_are_in_archive() {
        let bytes = build_archive(|w| {
            w.start_new_file("a.txt")?;
            w.append_data(b"hello")?;
            Ok(())
        });
        let reader = test_reader::TestReader::open(&bytes).unwrap();
        let file_size = bytes.len() as u64;
        for section in [
            reader.footer.section_compressed_data,
            reader.footer.section_offset_records,
            reader.footer.section_names,
            reader.footer.section_file_tree,
            reader.footer.section_meta_directory,
            reader.footer.section_meta_data,
        ] {
            assert!(
                section.offset + section.size <= file_size,
                "section out of bounds: {section:?}, file size {file_size}"
            );
        }
    }

    #[test]
    fn invalid_compression_level_rejected() {
        // ZArchiveWriter deliberately does not derive Debug (it owns
        // a zstd context and a writer W that may not be Debug), so
        // we can't call `unwrap_err` here. `matches!` pattern match
        // on the Result's error variant is sufficient.
        assert!(matches!(
            ZArchiveWriter::new(Vec::new(), -1),
            Err(WupError::InvalidCompressionLevel { .. })
        ));
        assert!(matches!(
            ZArchiveWriter::new(Vec::new(), 23),
            Err(WupError::InvalidCompressionLevel { .. })
        ));
    }

    #[test]
    fn offset_records_section_is_8_byte_aligned() {
        // Upstream pads with zero bytes between the compressed data
        // and offset records sections so the 40-byte records table
        // starts on an 8-byte boundary.
        let bytes = build_archive(|w| {
            // Make the compressed section an odd number of bytes so
            // the padding path actually runs.
            w.start_new_file("x.txt")?;
            w.append_data(b"odd")?;
            Ok(())
        });
        let reader = test_reader::TestReader::open(&bytes).unwrap();
        assert_eq!(
            reader.footer.section_offset_records.offset % 8,
            0,
            "offset records must start on an 8-byte boundary"
        );
    }

    // ----------------------------------------------------------------
    // Minimal ZArchive reader used only in tests. Not part of the
    // public API; a production reader would live in its own module
    // with error handling rather than `.unwrap()`.
    // ----------------------------------------------------------------
    pub(crate) mod test_reader {
        use crate::nintendo::wup::constants::{
            COMPRESSED_BLOCK_SIZE, ENTRIES_PER_OFFSET_RECORD, ZARCHIVE_FOOTER_MAGIC,
            ZARCHIVE_FOOTER_SIZE, ZARCHIVE_FOOTER_VERSION,
        };
        use crate::nintendo::wup::error::WupResult;
        use crate::nintendo::wup::models::{
            file_tree::FileDirectoryEntry, footer::ZArchiveFooter,
            offset_record::CompressionOffsetRecord,
        };
        use binrw::BinRead;
        use std::io::Cursor;

        pub struct TestReader<'a> {
            bytes: &'a [u8],
            pub footer: ZArchiveFooter,
            pub offset_records: Vec<CompressionOffsetRecord>,
            pub file_tree: Vec<FileDirectoryEntry>,
            name_table: &'a [u8],
        }

        impl<'a> TestReader<'a> {
            pub fn open(bytes: &'a [u8]) -> WupResult<Self> {
                let footer_start = bytes.len() - ZARCHIVE_FOOTER_SIZE;
                let mut cursor = Cursor::new(&bytes[footer_start..]);
                let footer = ZArchiveFooter::read(&mut cursor)?;
                assert_eq!(footer.magic, ZARCHIVE_FOOTER_MAGIC);
                assert_eq!(footer.version, ZARCHIVE_FOOTER_VERSION);
                assert_eq!(footer.total_size, bytes.len() as u64);

                let records_slice = &bytes[footer.section_offset_records.offset as usize
                    ..(footer.section_offset_records.offset + footer.section_offset_records.size)
                        as usize];
                let mut cur = Cursor::new(records_slice);
                let mut records = Vec::new();
                while cur.position() < records_slice.len() as u64 {
                    records.push(CompressionOffsetRecord::read(&mut cur)?);
                }

                let name_table = &bytes[footer.section_names.offset as usize
                    ..(footer.section_names.offset + footer.section_names.size) as usize];

                let tree_slice = &bytes[footer.section_file_tree.offset as usize
                    ..(footer.section_file_tree.offset + footer.section_file_tree.size) as usize];
                let mut cur = Cursor::new(tree_slice);
                let mut entries = Vec::new();
                while cur.position() < tree_slice.len() as u64 {
                    entries.push(FileDirectoryEntry::read(&mut cur)?);
                }

                Ok(Self {
                    bytes,
                    footer,
                    offset_records: records,
                    file_tree: entries,
                    name_table,
                })
            }

            fn read_name(&self, offset: u32) -> String {
                let offset = offset as usize;
                let len = self.name_table[offset] as usize;
                let start = offset + 1;
                String::from_utf8_lossy(&self.name_table[start..start + len]).into_owned()
            }

            pub fn find_file(&self, path: &str) -> Option<&FileDirectoryEntry> {
                let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
                self.find_recursive(0, &components)
            }

            fn find_recursive(
                &self,
                node_idx: usize,
                components: &[&str],
            ) -> Option<&FileDirectoryEntry> {
                if components.is_empty() {
                    return Some(&self.file_tree[node_idx]);
                }
                let node = &self.file_tree[node_idx];
                if node.is_file() {
                    return None;
                }
                let start = node.node_start_index() as usize;
                let count = node.child_count() as usize;
                let (first, rest) = components.split_first().unwrap();
                for i in start..start + count {
                    let child = &self.file_tree[i];
                    let name = self.read_name(child.name_offset());
                    if name == *first {
                        return self.find_recursive(i, rest);
                    }
                }
                None
            }

            fn block_compressed_location(&self, block_idx: usize) -> (usize, usize) {
                let record_idx = block_idx / ENTRIES_PER_OFFSET_RECORD;
                let slot = block_idx % ENTRIES_PER_OFFSET_RECORD;
                let record = &self.offset_records[record_idx];
                let mut offset = record.base_offset as usize;
                for i in 0..slot {
                    offset += record.block_size(i);
                }
                (offset, record.block_size(slot))
            }

            fn decompress_block(&self, block_idx: usize) -> Vec<u8> {
                let (offset, size) = self.block_compressed_location(block_idx);
                let compressed = &self.bytes[offset..offset + size];
                if size == COMPRESSED_BLOCK_SIZE {
                    compressed.to_vec()
                } else {
                    let mut decompressor = zstd::bulk::Decompressor::new().unwrap();
                    let mut out = vec![0u8; COMPRESSED_BLOCK_SIZE];
                    let n = decompressor
                        .decompress_to_buffer(compressed, &mut out)
                        .unwrap();
                    assert_eq!(
                        n, COMPRESSED_BLOCK_SIZE,
                        "every decompressed block must round-trip to a full 64 KiB"
                    );
                    out
                }
            }

            pub fn extract_file(&self, path: &str) -> Vec<u8> {
                let entry = self
                    .find_file(path)
                    .unwrap_or_else(|| panic!("file not found in archive: {path}"));
                assert!(entry.is_file());
                let file_offset = entry.file_offset();
                let file_size = entry.file_size();
                if file_size == 0 {
                    return Vec::new();
                }
                let first_block = (file_offset / COMPRESSED_BLOCK_SIZE as u64) as usize;
                let last_block =
                    ((file_offset + file_size - 1) / COMPRESSED_BLOCK_SIZE as u64) as usize;
                let mut out = Vec::with_capacity(file_size as usize);
                for block_idx in first_block..=last_block {
                    let block = self.decompress_block(block_idx);
                    let block_start = (block_idx * COMPRESSED_BLOCK_SIZE) as u64;
                    let block_end = block_start + COMPRESSED_BLOCK_SIZE as u64;
                    let file_end = file_offset + file_size;
                    let slice_start = file_offset.max(block_start) - block_start;
                    let slice_end = file_end.min(block_end) - block_start;
                    out.extend_from_slice(&block[slice_start as usize..slice_end as usize]);
                }
                out
            }
        }
    }
}
