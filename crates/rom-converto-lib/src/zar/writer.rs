//! Parallel ZArchive (`.zar`) writer.
//!
//! Files concatenate into one logical stream that is cut into 64 KiB
//! blocks; there is no per-file alignment, so a file may start
//! mid-block. Blocks are compressed on a
//! [`crate::util::worker_pool::Pool`] and written back in submission
//! order, which keeps the output byte-for-byte reproducible regardless
//! of which worker finishes first.
//!
//! The output is append-only: the writer never seeks, so every section
//! offset is the position reached while streaming, and the integrity
//! hash is computed over the emitted bytes plus a footer whose hash
//! field is still zero.

use super::format::{
    COMPRESSED_BLOCK_SIZE, CompressionOffsetRecord, ENTRIES_PER_OFFSET_RECORD, FOOTER_SIZE,
    FileDirectoryEntry, Footer, MAX_NAME_LEN, ROOT_NAME_OFFSET_SENTINEL, Section, ZarError,
    ZarResult, encode_name_len, split_path,
};
use crate::util::ProgressReporter;
use crate::util::worker_pool::{Pool, PoolChannelClosed, Worker};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::Write;

/// zstd level every ZArchive producer uses. Also what level `0`
/// selects, matching zstd's own "library default" sentinel.
pub const DEFAULT_COMPRESSION_LEVEL: i32 = 6;

/// Lowest zstd level [`ZarWriter::with_options`] accepts.
pub const MIN_COMPRESSION_LEVEL: i32 = 0;

/// Highest zstd level [`ZarWriter::with_options`] accepts.
pub const MAX_COMPRESSION_LEVEL: i32 = 22;

/// Blocks emitted between two progress `inc` calls. 32 blocks is 2 MiB
/// of input: fine enough for a smooth bar, coarse enough that the
/// event overhead disappears.
const PROGRESS_BATCH_BLOCKS: u64 = 32;

/// Totals reported by [`ZarWriter::finish`].
#[derive(Debug, Clone, Copy)]
pub struct ZarSummary {
    /// Bytes handed to [`ZarWriter::append_data`], padding excluded.
    pub total_input: u64,
    /// Bytes of block payload written to the compressed-data section.
    pub total_compressed: u64,
    pub block_count: u64,
}

/// One compressed block plus, when the block was compressed rather
/// than stored raw, the input buffer it came from so the writer can
/// reuse it for the next block.
struct BlockOut {
    payload: Vec<u8>,
    spare: Option<Vec<u8>>,
}

/// Per-thread block compressor. The zstd context and a
/// `compress_bound`-sized scratch buffer are allocated once per worker
/// and reused for every block.
struct BlockCompressWorker {
    compressor: zstd::bulk::Compressor<'static>,
    scratch: Vec<u8>,
}

impl Worker<Vec<u8>, BlockOut, ZarError> for BlockCompressWorker {
    fn process(&mut self, block: Vec<u8>) -> ZarResult<BlockOut> {
        let len = self
            .compressor
            .compress_to_buffer(&block, &mut self.scratch)
            .map_err(|e| ZarError::Zstd(e.to_string()))?;
        // A payload of exactly COMPRESSED_BLOCK_SIZE bytes is the
        // format's stored-raw marker (`size[] = 0xFFFF`), so a block
        // zstd cannot shrink goes out verbatim.
        Ok(if len >= COMPRESSED_BLOCK_SIZE {
            BlockOut {
                payload: block,
                spare: None,
            }
        } else {
            BlockOut {
                payload: self.scratch[..len].to_vec(),
                spare: Some(block),
            }
        })
    }
}

enum NodeKind {
    Dir { children: Vec<usize> },
    File { offset: u64, size: u64 },
}

struct Node {
    name: Vec<u8>,
    kind: NodeKind,
}

/// Streaming ZArchive writer.
///
/// Call [`ZarWriter::make_dir`] and [`ZarWriter::start_file`] to build
/// the tree, [`ZarWriter::append_data`] to feed the active file's
/// bytes, and [`ZarWriter::finish`] to flush the trailing block and
/// emit the trailer sections.
pub struct ZarWriter<'p, W: Write> {
    out: W,
    hasher: Sha256,
    out_pos: u64,
    // Taken in `finish` so the pool can be shut down by value.
    pool: Option<Pool<Vec<u8>, BlockOut, ZarError>>,
    max_in_flight: usize,
    in_flight: usize,
    submit_seq: u64,
    write_seq: u64,
    pending: HashMap<u64, BlockOut>,
    records: Vec<CompressionOffsetRecord>,
    block_buf: Vec<u8>,
    /// Input buffers handed back by workers, reused so the steady
    /// state does not allocate a 64 KiB block per 64 KiB of input.
    spare: Vec<Vec<u8>>,
    input_offset: u64,
    names: Vec<u8>,
    interned: HashMap<Vec<u8>, u32>,
    nodes: Vec<Node>,
    active_file: Option<usize>,
    progress: Option<&'p dyn ProgressReporter>,
    progress_batch: u64,
}

impl<'p, W: Write> ZarWriter<'p, W> {
    /// Create a writer backed by `output`, compressing on `n_threads`
    /// worker threads (see [`crate::util::worker_pool::parallelism`])
    /// at [`DEFAULT_COMPRESSION_LEVEL`].
    pub fn new(output: W, n_threads: usize) -> ZarResult<Self> {
        Self::with_options(output, n_threads, DEFAULT_COMPRESSION_LEVEL, None)
    }

    /// Create a writer at zstd `level` (`0` selects
    /// [`DEFAULT_COMPRESSION_LEVEL`]), reporting one `inc` to
    /// `progress` per [`PROGRESS_BATCH_BLOCKS`] blocks of input
    /// committed to `output`.
    ///
    /// # Errors
    /// Returns [`ZarError::InvalidCompressionLevel`] if `level` is
    /// outside [`MIN_COMPRESSION_LEVEL`]..=[`MAX_COMPRESSION_LEVEL`].
    pub fn with_options(
        output: W,
        n_threads: usize,
        level: i32,
        progress: Option<&'p dyn ProgressReporter>,
    ) -> ZarResult<Self> {
        if !(MIN_COMPRESSION_LEVEL..=MAX_COMPRESSION_LEVEL).contains(&level) {
            return Err(ZarError::InvalidCompressionLevel {
                level,
                min: MIN_COMPRESSION_LEVEL,
                max: MAX_COMPRESSION_LEVEL,
            });
        }
        let level = if level == 0 {
            DEFAULT_COMPRESSION_LEVEL
        } else {
            level
        };
        let n_threads = n_threads.max(1);
        let bound = zstd::zstd_safe::compress_bound(COMPRESSED_BLOCK_SIZE);
        let workers = (0..n_threads)
            .map(|_| {
                Ok(BlockCompressWorker {
                    compressor: zstd::bulk::Compressor::new(level)
                        .map_err(|e| ZarError::Zstd(e.to_string()))?,
                    scratch: vec![0u8; bound],
                })
            })
            .collect::<ZarResult<Vec<_>>>()?;
        Ok(Self {
            out: output,
            hasher: Sha256::new(),
            out_pos: 0,
            pool: Some(Pool::spawn(workers)),
            // Two blocks per thread keeps every worker busy without
            // letting the in-flight set grow with the input size.
            max_in_flight: n_threads * 2,
            in_flight: 0,
            submit_seq: 0,
            write_seq: 0,
            pending: HashMap::new(),
            records: Vec::new(),
            block_buf: Vec::with_capacity(COMPRESSED_BLOCK_SIZE),
            spare: Vec::new(),
            input_offset: 0,
            names: Vec::new(),
            interned: HashMap::new(),
            nodes: vec![Node {
                name: Vec::new(),
                kind: NodeKind::Dir {
                    children: Vec::new(),
                },
            }],
            active_file: None,
            progress,
            progress_batch: 0,
        })
    }

    /// Create the directory `path`. With `recursive`, missing parents
    /// are created too; otherwise every parent must already exist.
    pub fn make_dir(&mut self, path: &str, recursive: bool) -> ZarResult<()> {
        let (parent, name) = self.resolve_parent(path, recursive)?;
        match self.child(parent, name.as_bytes())? {
            Some(existing) if matches!(self.nodes[existing].kind, NodeKind::Dir { .. }) => Ok(()),
            Some(_) => Err(ZarError::DuplicateEntry(path.to_string())),
            None => {
                self.add_dir(parent, name.as_bytes())?;
                Ok(())
            }
        }
    }

    /// Open `path` as the active file. Missing parent directories are
    /// created. Subsequent [`ZarWriter::append_data`] calls append to
    /// this file until the next `start_file`.
    pub fn start_file(&mut self, path: &str) -> ZarResult<()> {
        let (parent, name) = self.resolve_parent(path, true)?;
        if self.child(parent, name.as_bytes())?.is_some() {
            return Err(ZarError::DuplicateEntry(path.to_string()));
        }
        check_name_len(name.as_bytes())?;
        let index = self.nodes.len();
        self.nodes.push(Node {
            name: name.as_bytes().to_vec(),
            kind: NodeKind::File {
                offset: self.input_offset,
                size: 0,
            },
        });
        self.push_child(parent, index);
        self.active_file = Some(index);
        Ok(())
    }

    /// Append bytes to the active file.
    pub fn append_data(&mut self, mut data: &[u8]) -> ZarResult<()> {
        let active = self.active_file.ok_or(ZarError::NoActiveFile)?;
        let NodeKind::File { size, .. } = &mut self.nodes[active].kind else {
            unreachable!("active_file always points at a file node")
        };
        *size += data.len() as u64;
        self.input_offset += data.len() as u64;

        while !data.is_empty() {
            let take = (COMPRESSED_BLOCK_SIZE - self.block_buf.len()).min(data.len());
            self.block_buf.extend_from_slice(&data[..take]);
            data = &data[take..];
            if self.block_buf.len() == COMPRESSED_BLOCK_SIZE {
                let next = self.take_buf();
                let block = std::mem::replace(&mut self.block_buf, next);
                self.submit_block(block)?;
            }
        }
        Ok(())
    }

    /// Pad and write out every block still buffered or in flight, so a
    /// caller that reports progress can tell the end of the data pass
    /// from the metadata tail. [`ZarWriter::finish`] does this itself;
    /// calling it twice is a no-op.
    pub fn flush_data(&mut self) -> ZarResult<()> {
        self.active_file = None;
        if !self.block_buf.is_empty() {
            // The trailing partial block is zero-padded to a full
            // block; the padding lies outside every file's size.
            self.block_buf.resize(COMPRESSED_BLOCK_SIZE, 0);
            let block = std::mem::take(&mut self.block_buf);
            self.submit_block(block)?;
        }
        while self.write_seq < self.submit_seq {
            self.drain_one()?;
        }
        if let Some(reporter) = self.progress
            && self.progress_batch > 0
        {
            reporter.inc(self.progress_batch * COMPRESSED_BLOCK_SIZE as u64);
            self.progress_batch = 0;
        }
        Ok(())
    }

    /// Flush the trailing block and emit the offset records, name
    /// table, file tree, meta sections, and footer.
    pub fn finish(mut self) -> ZarResult<ZarSummary> {
        self.flush_data()?;
        if let Some(pool) = self.pool.take() {
            pool.shutdown();
        }

        let compressed_data = Section {
            offset: 0,
            size: self.out_pos,
        };
        let block_count = self.write_seq;
        let pad = (8 - (self.out_pos % 8)) % 8;
        self.write_out(&[0u8; 8][..pad as usize])?;

        let records_off = self.out_pos;
        for record in std::mem::take(&mut self.records) {
            self.write_out(&record.to_bytes())?;
        }
        // Interning happens here, so the tree has to be built before
        // the name table it fills can be written.
        let tree = self.build_tree()?;
        let names_off = self.out_pos;
        let names = std::mem::take(&mut self.names);
        self.write_out(&names)?;

        let tree_off = self.out_pos;
        for entry in tree {
            self.write_out(&entry.to_bytes())?;
        }
        let meta_off = self.out_pos;

        let mut footer = Footer {
            compressed_data,
            offset_records: Section {
                offset: records_off,
                size: names_off - records_off,
            },
            names: Section {
                offset: names_off,
                size: tree_off - names_off,
            },
            file_tree: Section {
                offset: tree_off,
                size: meta_off - tree_off,
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
        // The hash covers every emitted byte followed by the footer
        // with a zeroed hash field, so it can be computed before the
        // real footer goes out.
        self.hasher.update(footer.to_bytes());
        let digest = self.hasher.finalize_reset();
        footer.integrity_hash.copy_from_slice(&digest);
        self.out.write_all(&footer.to_bytes())?;
        self.out.flush()?;

        Ok(ZarSummary {
            total_input: self.input_offset,
            total_compressed: compressed_data.size,
            block_count,
        })
    }

    /// Write bytes to the output, feeding the running hash and the
    /// tracked position.
    fn write_out(&mut self, bytes: &[u8]) -> ZarResult<()> {
        self.out.write_all(bytes)?;
        self.hasher.update(bytes);
        self.out_pos += bytes.len() as u64;
        Ok(())
    }

    fn submit_block(&mut self, block: Vec<u8>) -> ZarResult<()> {
        let pool = self
            .pool
            .as_ref()
            .ok_or(ZarError::WorkerPool(PoolChannelClosed))?;
        pool.submit(self.submit_seq, block)?;
        self.submit_seq += 1;
        self.in_flight += 1;
        while self.in_flight >= self.max_in_flight {
            self.drain_one()?;
        }
        Ok(())
    }

    /// Take one worker result and emit every block that has become
    /// contiguous with the write cursor.
    fn drain_one(&mut self) -> ZarResult<()> {
        let pool = self
            .pool
            .as_ref()
            .ok_or(ZarError::WorkerPool(PoolChannelClosed))?;
        let (seq, result) = pool.recv();
        self.in_flight -= 1;
        self.pending.insert(seq, result?);
        while let Some(out) = self.pending.remove(&self.write_seq) {
            self.emit_block(&out.payload)?;
            self.write_seq += 1;
            // A stored-raw block was emitted straight out of its input
            // buffer, so recycle `payload` in that case and `spare`
            // otherwise. Bounded by `max_in_flight`: every submitted
            // block took a buffer out of the pool first.
            let reuse = out.spare.unwrap_or(out.payload);
            if reuse.capacity() >= COMPRESSED_BLOCK_SIZE {
                self.spare.push(reuse);
            }
        }
        Ok(())
    }

    /// An empty buffer with room for a full block, recycled when a
    /// worker has handed one back.
    fn take_buf(&mut self) -> Vec<u8> {
        match self.spare.pop() {
            Some(mut buf) => {
                buf.clear();
                buf
            }
            None => Vec::with_capacity(COMPRESSED_BLOCK_SIZE),
        }
    }

    fn emit_block(&mut self, payload: &[u8]) -> ZarResult<()> {
        let slot = (self.write_seq as usize) % ENTRIES_PER_OFFSET_RECORD;
        if slot == 0 {
            self.records.push(CompressionOffsetRecord {
                base_offset: self.out_pos,
                sizes: [0u16; ENTRIES_PER_OFFSET_RECORD],
            });
        }
        let record = self
            .records
            .last_mut()
            .expect("a record is opened before the first block");
        // Sizes are stored biased by one so a 65536-byte stored-raw
        // block fits in a u16.
        record.sizes[slot] = (payload.len() - 1) as u16;
        self.write_out(payload)?;
        if let Some(reporter) = self.progress {
            self.progress_batch += 1;
            if self.progress_batch >= PROGRESS_BATCH_BLOCKS {
                reporter.inc(self.progress_batch * COMPRESSED_BLOCK_SIZE as u64);
                self.progress_batch = 0;
            }
        }
        Ok(())
    }

    /// Resolve `path`'s parent directory, creating missing components
    /// when `create` is set, and return it with the final component.
    fn resolve_parent<'a>(&mut self, path: &'a str, create: bool) -> ZarResult<(usize, &'a str)> {
        let components: Vec<&str> = split_path(path).collect();
        let (name, parents) = components
            .split_last()
            .ok_or_else(|| ZarError::InvalidPath(path.to_string()))?;
        let mut node = 0usize;
        for component in parents {
            node = match self.child(node, component.as_bytes())? {
                Some(existing) => existing,
                None if create => self.add_dir(node, component.as_bytes())?,
                None => return Err(ZarError::NotFound(path.to_string())),
            };
        }
        Ok((node, name))
    }

    fn child(&self, parent: usize, name: &[u8]) -> ZarResult<Option<usize>> {
        match &self.nodes[parent].kind {
            NodeKind::File { .. } => Err(ZarError::PathThroughFile(
                String::from_utf8_lossy(&self.nodes[parent].name).into_owned(),
            )),
            NodeKind::Dir { children } => Ok(children
                .iter()
                .copied()
                .find(|&index| self.nodes[index].name == name)),
        }
    }

    fn add_dir(&mut self, parent: usize, name: &[u8]) -> ZarResult<usize> {
        check_name_len(name)?;
        let index = self.nodes.len();
        self.nodes.push(Node {
            name: name.to_vec(),
            kind: NodeKind::Dir {
                children: Vec::new(),
            },
        });
        self.push_child(parent, index);
        Ok(index)
    }

    fn push_child(&mut self, parent: usize, index: usize) {
        let NodeKind::Dir { children } = &mut self.nodes[parent].kind else {
            unreachable!("callers resolve the parent as a directory first")
        };
        children.push(index);
    }

    /// Intern `name` in the name table, returning its byte offset.
    /// Names are deduplicated; the emission order is the tree order
    /// [`ZarWriter::build_tree`] walks in.
    fn intern(&mut self, name: &[u8]) -> ZarResult<u32> {
        if let Some(&offset) = self.interned.get(name) {
            return Ok(offset);
        }
        let offset = u32::try_from(self.names.len())
            .ok()
            .filter(|&offset| offset < ROOT_NAME_OFFSET_SENTINEL)
            .ok_or(ZarError::NameTableTooLarge)?;
        encode_name_len(name.len(), &mut self.names);
        self.names.extend_from_slice(name);
        self.interned.insert(name.to_vec(), offset);
        Ok(offset)
    }

    /// Serialize the file tree. Pass one walks it breadth-first,
    /// sorting each directory's children and assigning them a
    /// contiguous index range; pass two interns each node's name and
    /// emits the entries in that same order, so node 0 is the root,
    /// every child range is valid, and the name table lands in the
    /// breadth-first order the reference writer produces.
    fn build_tree(&mut self) -> ZarResult<Vec<FileDirectoryEntry>> {
        let mut order = vec![0usize];
        let mut ranges = vec![(0u32, 0u32); self.nodes.len()];
        let mut cursor = 0usize;
        while cursor < order.len() {
            let node = order[cursor];
            cursor += 1;
            let NodeKind::Dir { children } = &self.nodes[node].kind else {
                continue;
            };
            let mut sorted = children.clone();
            // Byte order breaks fold ties so the layout does not depend
            // on the order `read_dir` happened to hand the names over.
            sorted.sort_by(|&a, &b| {
                let (a, b) = (&self.nodes[a].name, &self.nodes[b].name);
                cmp_ascii_ci(a, b).then_with(|| a.cmp(b))
            });
            ranges[node] = (order.len() as u32, sorted.len() as u32);
            order.extend(sorted);
        }

        let mut entries = Vec::with_capacity(order.len());
        for (i, &node) in order.iter().enumerate() {
            // The root has no name; the reference reader insists on
            // the sentinel offset there.
            let name_offset = if i == 0 {
                ROOT_NAME_OFFSET_SENTINEL
            } else {
                self.intern(&self.nodes[node].name.clone())?
            };
            entries.push(match self.nodes[node].kind {
                NodeKind::File { offset, size } => {
                    FileDirectoryEntry::file(name_offset, offset, size)?
                }
                NodeKind::Dir { .. } => {
                    FileDirectoryEntry::directory(name_offset, ranges[node].0, ranges[node].1)
                }
            });
        }
        Ok(entries)
    }
}

/// Reject a name the reference reader cannot decode. Checked when the
/// node is created so a bad path fails before any data is packed.
fn check_name_len(name: &[u8]) -> ZarResult<()> {
    if name.len() >= MAX_NAME_LEN {
        return Err(ZarError::NameTooLong(name.len()));
    }
    Ok(())
}

/// Sibling ordering: ascending, case-insensitive over ASCII A-Z only.
fn cmp_ascii_ci(a: &[u8], b: &[u8]) -> Ordering {
    a.iter()
        .map(|c| c.to_ascii_lowercase())
        .cmp(b.iter().map(|c| c.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zar::format::{OFFSET_RECORD_SIZE, decode_name_len};

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

    fn footer_of(buf: &[u8]) -> Footer {
        let mut raw = [0u8; FOOTER_SIZE];
        raw.copy_from_slice(&buf[buf.len() - FOOTER_SIZE..]);
        Footer::from_bytes(&raw).expect("writer emits a valid footer")
    }

    fn records_of(buf: &[u8], footer: &Footer) -> Vec<CompressionOffsetRecord> {
        let section = footer.offset_records;
        (0..section.size as usize / OFFSET_RECORD_SIZE)
            .map(|i| {
                let start = section.offset as usize + i * OFFSET_RECORD_SIZE;
                let mut raw = [0u8; OFFSET_RECORD_SIZE];
                raw.copy_from_slice(&buf[start..start + OFFSET_RECORD_SIZE]);
                CompressionOffsetRecord::from_bytes(&raw)
            })
            .collect()
    }

    fn write_single_file(name: &str, data: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut writer = ZarWriter::new(&mut buf, 2).expect("writer spawns");
        writer.start_file(name).expect("file opens");
        writer.append_data(data).expect("data appends");
        writer.finish().expect("archive finishes");
        buf
    }

    #[test]
    fn spanning_16_blocks_opens_a_second_record() {
        let data = vec![0x5Au8; 17 * COMPRESSED_BLOCK_SIZE];
        let buf = write_single_file("big.bin", &data);
        let footer = footer_of(&buf);
        let records = records_of(&buf, &footer);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].base_offset, 0);

        // Every stored size is `len - 1`, so the biased sizes of the
        // first 16 blocks must add up to the second record's base.
        let first_run: u64 = records[0].sizes.iter().map(|&s| s as u64 + 1).sum();
        assert_eq!(records[1].base_offset, first_run);
        assert_eq!(
            first_run + records[1].sizes[0] as u64 + 1,
            footer.compressed_data.size
        );
    }

    #[test]
    fn incompressible_block_is_stored_raw() {
        let data = incompressible(COMPRESSED_BLOCK_SIZE);
        let buf = write_single_file("noise.bin", &data);
        let footer = footer_of(&buf);
        let records = records_of(&buf, &footer);
        assert_eq!(records[0].sizes[0], 0xFFFF);
        assert_eq!(footer.compressed_data.size, COMPRESSED_BLOCK_SIZE as u64);
        assert_eq!(&buf[..COMPRESSED_BLOCK_SIZE], &data[..]);
    }

    #[test]
    fn trailing_block_is_padded_but_file_size_is_not() {
        let buf = write_single_file("small.bin", &[7u8; 100]);
        let footer = footer_of(&buf);
        assert_eq!(records_of(&buf, &footer).len(), 1);

        let tree_start = footer.file_tree.offset as usize;
        let mut raw = [0u8; 16];
        // Node 1 is the only child of the root.
        raw.copy_from_slice(&buf[tree_start + 16..tree_start + 32]);
        let entry = FileDirectoryEntry::from_bytes(&raw);
        assert!(entry.is_file());
        assert_eq!(entry.file_size(), 100);
    }

    #[test]
    fn siblings_are_sorted_case_insensitively() {
        let mut buf = Vec::new();
        let mut writer = ZarWriter::new(&mut buf, 2).expect("writer spawns");
        for name in ["zeta", "Zeta", "alpha", "Beta"] {
            writer.start_file(name).expect("file opens");
            writer.append_data(b"x").expect("data appends");
        }
        writer.finish().expect("archive finishes");

        let footer = footer_of(&buf);
        let tree = footer.file_tree.offset as usize;
        let names =
            footer.names.offset as usize..(footer.names.offset + footer.names.size) as usize;
        let table = &buf[names];
        let sorted: Vec<String> = (1..5)
            .map(|i| {
                let mut raw = [0u8; 16];
                raw.copy_from_slice(&buf[tree + i * 16..tree + (i + 1) * 16]);
                let entry = FileDirectoryEntry::from_bytes(&raw);
                let offset = entry.name_offset() as usize;
                let (len, header) = decode_name_len(table, offset).expect("name decodes");
                String::from_utf8_lossy(&table[offset + header..offset + header + len]).into_owned()
            })
            .collect();
        // "Zeta"/"zeta" are equal under the fold, so byte order decides
        // and the insertion order they were added in does not.
        assert_eq!(sorted, vec!["alpha", "Beta", "Zeta", "zeta"]);
        // Names are interned in tree order, not the order the files
        // were added, so the table matches what Cemu's writer emits.
        assert_eq!(table, b"\x05alpha\x04Beta\x04Zeta\x04zeta");
    }

    #[test]
    fn names_of_127_bytes_are_accepted_and_128_rejected() {
        let mut buf = Vec::new();
        let mut writer = ZarWriter::new(&mut buf, 1).expect("writer spawns");
        writer.start_file(&"a".repeat(127)).expect("127 fits");
        assert!(matches!(
            writer.start_file(&"b".repeat(128)),
            Err(ZarError::NameTooLong(128))
        ));
        writer.finish().expect("archive finishes");
    }

    #[test]
    fn make_dir_rejects_paths_through_a_file() {
        let mut buf = Vec::new();
        let mut writer = ZarWriter::new(&mut buf, 1).expect("writer spawns");
        writer.start_file("blocker").expect("file opens");
        assert!(matches!(
            writer.make_dir("blocker/child", true),
            Err(ZarError::PathThroughFile(_))
        ));
    }

    #[test]
    fn make_dir_without_recursion_requires_the_parent() {
        let mut buf = Vec::new();
        let mut writer = ZarWriter::new(&mut buf, 1).expect("writer spawns");
        assert!(matches!(
            writer.make_dir("a/b", false),
            Err(ZarError::NotFound(_))
        ));
        writer.make_dir("a", false).expect("root child is created");
        writer.make_dir("a/b", false).expect("parent now exists");
    }

    #[test]
    fn summary_counts_input_bytes_and_blocks() {
        let mut buf = Vec::new();
        let mut writer = ZarWriter::new(&mut buf, 2).expect("writer spawns");
        writer.start_file("a.bin").expect("file opens");
        writer
            .append_data(&vec![0u8; COMPRESSED_BLOCK_SIZE + 10])
            .expect("data appends");
        let summary = writer.finish().expect("archive finishes");
        assert_eq!(summary.total_input, COMPRESSED_BLOCK_SIZE as u64 + 10);
        assert_eq!(summary.block_count, 2);
        assert!(summary.total_compressed > 0);
    }
}
