//! `Read` adapter over a byte range of an `Arc<File>`. Lets the zstd
//! stream decoder pull a compressed NCZ slice straight out of an NSZ
//! at its known offset without copying the whole slice into RAM.

use std::fs::File;
use std::io::{self, Read};
use std::sync::Arc;

use crate::util::pread::file_read_exact_at;

pub struct PositionalReader {
    file: Arc<File>,
    offset: u64,
    remaining: u64,
}

impl PositionalReader {
    pub fn new(file: Arc<File>, offset: u64, length: u64) -> Self {
        Self {
            file,
            offset,
            remaining: length,
        }
    }
}

impl Read for PositionalReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 || buf.is_empty() {
            return Ok(0);
        }
        let take = (buf.len() as u64).min(self.remaining) as usize;
        file_read_exact_at(&self.file, &mut buf[..take], self.offset)?;
        self.offset += take as u64;
        self.remaining -= take as u64;
        Ok(take)
    }
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
}
