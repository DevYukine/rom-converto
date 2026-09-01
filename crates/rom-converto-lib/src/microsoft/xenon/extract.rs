//! Extract a ZArchive (`.zar`) into a directory tree.
//!
//! Block decompression is CPU-bound and independent per block, so it
//! runs on a [`crate::util::worker_pool::Pool`]. Files concatenate into
//! one logical stream with no gaps (see [`crate::microsoft::zar`]), so
//! a single sequential cursor can slice each decompressed block across
//! file boundaries as results come back in order.

use std::io::{BufReader, BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::microsoft::zar::format::split_path;
use crate::microsoft::zar::{ZarEntry, ZarReader, decompress_block};
use crate::util::CancelToken;
use crate::util::worker_pool::{Pool, Worker, drive, parallelism};

use super::error::{XenonError, XenonResult};

/// Rejects an archive entry path that could escape `output_dir`: a
/// leading separator (root- or drive-relative), an empty path, a `.`
/// or `..` component, a component carrying a NUL byte, or a component
/// with a Windows drive/colon prefix.
fn validate_entry_path(path: &str) -> XenonResult<()> {
    let unsafe_path = || XenonError::UnsafePath {
        path: path.to_string(),
    };
    if path.starts_with('/') || path.starts_with('\\') {
        return Err(unsafe_path());
    }
    let mut has_component = false;
    for component in split_path(path) {
        has_component = true;
        if component == "."
            || component == ".."
            || component.contains(':')
            || component.contains('\0')
        {
            return Err(unsafe_path());
        }
    }
    if !has_component {
        return Err(unsafe_path());
    }
    Ok(())
}

/// Counts from a completed ZArchive extraction.
pub struct XenonExtractSummary {
    pub file_count: u64,
    pub dir_count: u64,
    pub logical_bytes: u64,
}

/// Sum of every file's logical size in `input`, used as the progress
/// total.
pub fn logical_size(input: &Path) -> XenonResult<u64> {
    let reader = ZarReader::open(BufReader::with_capacity(
        1 << 20,
        std::fs::File::open(input)?,
    ))?;
    Ok(reader
        .entries()?
        .iter()
        .filter(|e| e.is_file)
        .map(|e| e.size)
        .sum())
}

struct BlockDecompressWorker;

impl Worker<(Vec<u8>, bool), Vec<u8>, XenonError> for BlockDecompressWorker {
    fn process(&mut self, (payload, stored_raw): (Vec<u8>, bool)) -> XenonResult<Vec<u8>> {
        Ok(decompress_block(&payload, stored_raw)?)
    }
}

/// Sequential cursor over the archive's logical byte stream, mapping it
/// onto per-file output writers as decompressed blocks arrive in order.
struct ExtractCursor<'a> {
    output_dir: &'a Path,
    files: std::vec::IntoIter<ZarEntry>,
    writer: Option<BufWriter<std::fs::File>>,
    remaining: u64,
    /// Current position in the archive's logical byte stream, checked
    /// against each file's recorded offset since files are expected to
    /// concatenate with no gaps (see the module docs).
    logical_pos: u64,
}

impl<'a> ExtractCursor<'a> {
    fn new(output_dir: &'a Path, files: Vec<ZarEntry>) -> XenonResult<Self> {
        let mut cursor = ExtractCursor {
            output_dir,
            files: files.into_iter(),
            writer: None,
            remaining: 0,
            logical_pos: 0,
        };
        cursor.advance_to_next_file()?;
        Ok(cursor)
    }

    /// Closes the current writer (if any) and opens the next file that
    /// actually needs block data, creating every file it skips along
    /// the way (zero-size files consume no stream bytes).
    fn advance_to_next_file(&mut self) -> XenonResult<()> {
        if let Some(mut w) = self.writer.take() {
            w.flush()?;
        }
        for entry in self.files.by_ref() {
            if entry.offset != self.logical_pos {
                return Err(XenonError::OffsetMismatch {
                    expected: self.logical_pos,
                    actual: entry.offset,
                });
            }
            let path = self.output_dir.join(&entry.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file = std::fs::File::create(&path)?;
            if entry.size == 0 {
                continue;
            }
            self.writer = Some(BufWriter::new(file));
            self.remaining = entry.size;
            return Ok(());
        }
        self.writer = None;
        self.remaining = 0;
        Ok(())
    }

    /// Writes as much of `block` as belongs to the current file(s),
    /// advancing across file boundaries as needed. Trailing pad past
    /// the last file is silently dropped. Returns the bytes written.
    fn consume_block(&mut self, mut block: &[u8]) -> XenonResult<u64> {
        let mut written = 0u64;
        while !block.is_empty() {
            let Some(writer) = self.writer.as_mut() else {
                break;
            };
            let take = self.remaining.min(block.len() as u64) as usize;
            writer.write_all(&block[..take])?;
            self.remaining -= take as u64;
            self.logical_pos += take as u64;
            written += take as u64;
            block = &block[take..];
            if self.remaining == 0 {
                self.advance_to_next_file()?;
            }
        }
        Ok(written)
    }

    fn finish(mut self) -> XenonResult<()> {
        if let Some(mut w) = self.writer.take() {
            w.flush()?;
        }
        Ok(())
    }
}

/// Extract every entry in `input` into `output_dir`.
pub fn extract_blocking(
    input: &Path,
    output_dir: &Path,
    bytes_done: &AtomicU64,
    cancel: &CancelToken,
) -> XenonResult<XenonExtractSummary> {
    let mut reader = ZarReader::open(std::fs::File::open(input)?)?;
    std::fs::create_dir_all(output_dir)?;

    let mut file_count = 0u64;
    let mut dir_count = 0u64;
    let mut files: Vec<ZarEntry> = Vec::new();
    for entry in reader.entries()? {
        validate_entry_path(&entry.path)?;
        if entry.is_file {
            file_count += 1;
            files.push(entry);
        } else {
            dir_count += 1;
            std::fs::create_dir_all(output_dir.join(&entry.path))?;
        }
    }
    // Files have no gaps between them in the logical stream, so sorted
    // by offset they exactly partition it.
    files.sort_by_key(|e| e.offset);
    let logical_bytes = files.iter().map(|e| e.size).sum();

    let block_count = reader.block_count();
    let n_threads = parallelism();
    let workers: Vec<BlockDecompressWorker> =
        (0..n_threads).map(|_| BlockDecompressWorker).collect();
    let pool: Pool<(Vec<u8>, bool), Vec<u8>, XenonError> = Pool::spawn(workers);

    let mut cursor = ExtractCursor::new(output_dir, files)?;
    let result = drive(
        &pool,
        block_count,
        n_threads * 2,
        |seq| {
            if cancel.is_cancelled() {
                return Err(XenonError::Cancelled);
            }
            Ok(reader.read_block_raw(seq)?)
        },
        |_seq, block| {
            if cancel.is_cancelled() {
                return Err(XenonError::Cancelled);
            }
            let written = cursor.consume_block(&block)?;
            bytes_done.fetch_add(written, Ordering::Relaxed);
            Ok(())
        },
    );
    pool.shutdown();
    result?;
    cursor.finish()?;

    Ok(XenonExtractSummary {
        file_count,
        dir_count,
        logical_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::microsoft::zar::{COMPRESSED_BLOCK_SIZE, ZarWriter};
    use std::sync::Arc;

    #[test]
    fn dir_pack_then_extract_round_trips_content_at_archive_root() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("default.xex"), b"xex-bytes").unwrap();
        std::fs::create_dir(src.path().join("data")).unwrap();
        std::fs::write(src.path().join("data/save.bin"), b"save-bytes").unwrap();

        let work = tempfile::tempdir().unwrap();
        let zar_path = work.path().join("game.zar");
        let cancel = CancelToken::new();
        super::super::pack::pack_blocking(
            src.path(),
            &zar_path,
            Arc::new(AtomicU64::new(0)),
            &cancel,
        )
        .unwrap();

        let out_dir = work.path().join("out");
        let bytes_done = AtomicU64::new(0);
        let summary = extract_blocking(&zar_path, &out_dir, &bytes_done, &cancel).unwrap();
        assert_eq!(summary.file_count, 2);
        assert_eq!(summary.dir_count, 1);

        assert_eq!(
            std::fs::read(out_dir.join("default.xex")).unwrap(),
            b"xex-bytes"
        );
        assert_eq!(
            std::fs::read(out_dir.join("data/save.bin")).unwrap(),
            b"save-bytes"
        );
        assert!(!out_dir.join("game").exists());
    }

    #[test]
    fn extract_handles_a_multi_block_file_starting_mid_block() {
        let head = vec![7u8; 100];
        let big_len = 3 * COMPRESSED_BLOCK_SIZE + 4096;
        let big: Vec<u8> = (0..big_len).map(|i| (i % 251) as u8).collect();

        let mut buf = Vec::new();
        let mut writer = ZarWriter::new(&mut buf, 2).unwrap();
        writer.start_file("head.bin").unwrap();
        writer.append_data(&head).unwrap();
        writer.start_file("big.bin").unwrap();
        writer.append_data(&big).unwrap();
        writer.finish().unwrap();

        let work = tempfile::tempdir().unwrap();
        let zar_path = work.path().join("archive.zar");
        std::fs::write(&zar_path, &buf).unwrap();

        let out_dir = work.path().join("out");
        let bytes_done = AtomicU64::new(0);
        let cancel = CancelToken::new();
        extract_blocking(&zar_path, &out_dir, &bytes_done, &cancel).unwrap();

        assert_eq!(std::fs::read(out_dir.join("head.bin")).unwrap(), head);
        assert_eq!(std::fs::read(out_dir.join("big.bin")).unwrap(), big);
    }

    #[test]
    fn extract_rejects_a_path_traversal_entry() {
        // start_file resolves ".." as a literal directory name rather
        // than sanitizing it, so a maliciously named archive round-trips
        // through the writer just fine; extract must still refuse it.
        let mut buf = Vec::new();
        let mut writer = ZarWriter::new(&mut buf, 1).unwrap();
        writer.start_file("../evil.txt").unwrap();
        writer.append_data(b"pwned").unwrap();
        writer.finish().unwrap();

        let work = tempfile::tempdir().unwrap();
        let zar_path = work.path().join("archive.zar");
        std::fs::write(&zar_path, &buf).unwrap();

        let out_dir = work.path().join("out");
        let bytes_done = AtomicU64::new(0);
        let cancel = CancelToken::new();
        let result = extract_blocking(&zar_path, &out_dir, &bytes_done, &cancel);
        assert!(matches!(result, Err(XenonError::UnsafePath { .. })));
        assert!(!work.path().join("evil.txt").exists());
    }

    #[test]
    fn extract_errors_when_a_file_offset_is_patched() {
        let head = vec![1u8; 4096];
        let tail = vec![2u8; 4096];
        let mut buf = Vec::new();
        let mut writer = ZarWriter::new(&mut buf, 1).unwrap();
        writer.start_file("a.bin").unwrap();
        writer.append_data(&head).unwrap();
        writer.start_file("b.bin").unwrap();
        writer.append_data(&tail).unwrap();
        writer.finish().unwrap();

        let (node, tree_offset) = {
            let reader = ZarReader::open(std::io::Cursor::new(buf.clone())).unwrap();
            (
                reader.lookup("b.bin").unwrap(),
                reader.footer().file_tree.offset,
            )
        };
        // Corrupt b.bin's recorded offset (the high byte of the file
        // entry's low offset word) so it no longer matches the logical
        // stream position the extractor expects.
        let entry_pos = tree_offset as usize + node as usize * 16;
        buf[entry_pos + 4] ^= 0xFF;

        let work = tempfile::tempdir().unwrap();
        let zar_path = work.path().join("archive.zar");
        std::fs::write(&zar_path, &buf).unwrap();

        let out_dir = work.path().join("out");
        let bytes_done = AtomicU64::new(0);
        let cancel = CancelToken::new();
        let result = extract_blocking(&zar_path, &out_dir, &bytes_done, &cancel);
        assert!(matches!(result, Err(XenonError::OffsetMismatch { .. })));
    }
}
