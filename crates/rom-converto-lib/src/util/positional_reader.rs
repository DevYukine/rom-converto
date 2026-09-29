//! `Read`/`Seek` adapter over a byte range of a `File` (owned via
//! `Arc<File>` or borrowed as `&File`). Lets a stream decoder or header
//! parser pull a bounded slice of a container entry straight off disk at
//! its known offset, without copying the range into RAM and without ever
//! reading past the entry's declared extent into a sibling entry.

use std::borrow::Borrow;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;

use crate::util::pread::file_read_exact_at;

/// `Read`/`Seek` implementation that streams a fixed byte range of a
/// shared file, starting at `base` and never exposing more than
/// `length` bytes, without loading the range into memory up front.
pub struct PositionalReader<F = Arc<File>> {
    file: F,
    base: u64,
    length: u64,
    position: u64,
}

impl<F: Borrow<File>> PositionalReader<F> {
    /// Creates a reader over `length` bytes of `file` starting at
    /// `offset`.
    pub fn new(file: F, offset: u64, length: u64) -> Self {
        Self {
            file,
            base: offset,
            length,
            position: 0,
        }
    }
}

impl<F: Borrow<File>> Read for PositionalReader<F> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.length.saturating_sub(self.position);
        if remaining == 0 || buf.is_empty() {
            return Ok(0);
        }
        let take = (buf.len() as u64).min(remaining) as usize;
        let offset = self
            .base
            .checked_add(self.position)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "read offset overflows"))?;
        file_read_exact_at(self.file.borrow(), &mut buf[..take], offset)?;
        self.position += take as u64;
        Ok(take)
    }
}

impl<F: Borrow<File>> Seek for PositionalReader<F> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.position = seek_target(self.position, self.length, pos)?;
        Ok(self.position)
    }
}

/// Resolves a `SeekFrom` against the current position and stream length
/// with checked arithmetic, so every in-memory `Seek` impl agrees on
/// the rejection of negative or overflowing targets.
pub fn seek_target(position: u64, length: u64, pos: SeekFrom) -> io::Result<u64> {
    match pos {
        SeekFrom::Start(target) => Some(target),
        SeekFrom::Current(delta) => position.checked_add_signed(delta),
        SeekFrom::End(delta) => length.checked_add_signed(delta),
    }
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before start or past u64"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use tempfile::NamedTempFile;

    #[test]
    fn reads_exact_slice() {
        let mut tmp = NamedTempFile::new().unwrap();
        let payload: Vec<u8> = (0..0x4000).map(|i| (i & 0xFF) as u8).collect();
        tmp.write_all(&payload).unwrap();
        tmp.flush().unwrap();

        let file = Arc::new(File::open(tmp.path()).unwrap());
        let mut reader = PositionalReader::new(file, 0x100, 0x200);
        let mut got = Vec::new();
        reader.read_to_end(&mut got).unwrap();
        assert_eq!(got.len(), 0x200);
        assert_eq!(got.as_slice(), &payload[0x100..0x300]);
    }

    #[test]
    fn returns_zero_after_exhaustion() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(b"abcdefgh").unwrap();
        tmp.flush().unwrap();

        let file = Arc::new(File::open(tmp.path()).unwrap());
        let mut reader = PositionalReader::new(file, 0, 4);
        let mut buf = [0u8; 8];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf[..4], b"abcd");
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn read_rejects_overflowing_base_offset() {
        let tmp = NamedTempFile::new().unwrap();
        let file = Arc::new(File::open(tmp.path()).unwrap());
        let mut reader = PositionalReader::new(file, u64::MAX, 1);
        let err = reader.read(&mut [0u8; 1]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn seek_end_supports_lengths_above_i64_max() {
        let tmp = NamedTempFile::new().unwrap();
        let file = Arc::new(File::open(tmp.path()).unwrap());
        let length = i64::MAX as u64 + 10;
        let mut reader = PositionalReader::new(file, 0, length);
        assert_eq!(reader.seek(SeekFrom::End(-1)).unwrap(), length - 1);
        assert_eq!(reader.seek(SeekFrom::Current(1)).unwrap(), length);
    }
}
