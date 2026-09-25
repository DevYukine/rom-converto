//! Bounded `Read + Seek` over one `[start, start+len)` byte range of an
//! inner reader. Lets the info readers parse a tar member straight out of
//! a bundle file at its known offset without copying the member to disk.

use std::io::{self, Read, Seek, SeekFrom};

/// Reader over `len` bytes of `inner` starting at `start`. Reads clip at
/// the slice end, and every read re-seeks the inner reader to the current
/// slice position so interleaved use of the inner reader cannot desync it.
pub struct SliceReader<R: Read + Seek> {
    inner: R,
    start: u64,
    len: u64,
    pos: u64,
}

impl<R: Read + Seek> SliceReader<R> {
    /// Creates a reader over `len` bytes of `inner` starting at `start`.
    pub fn new(inner: R, start: u64, len: u64) -> Self {
        Self {
            inner,
            start,
            len,
            pos: 0,
        }
    }
}

impl<R: Read + Seek> Read for SliceReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let take = (buf.len() as u64).min(self.len - self.pos) as usize;
        self.inner.seek(SeekFrom::Start(self.start + self.pos))?;
        let n = self.inner.read(&mut buf[..take])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Read + Seek> Seek for SliceReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let target = match pos {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::Current(delta) => i128::from(self.pos) + i128::from(delta),
            // `End` is relative to the slice end, not the inner reader's.
            SeekFrom::End(delta) => i128::from(self.len) + i128::from(delta),
        };
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "slice seek before start",
            ));
        }
        self.pos = target as u64;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const DATA: &[u8] = b"0123456789abcdef";

    #[test]
    fn reads_never_cross_the_slice_end() {
        let mut reader = SliceReader::new(Cursor::new(DATA), 4, 6);
        let mut got = Vec::new();
        reader.read_to_end(&mut got).unwrap();
        assert_eq!(got, b"456789");
    }

    #[test]
    fn seek_supports_start_current_and_end() {
        let mut reader = SliceReader::new(Cursor::new(DATA), 2, 8);
        let mut byte = [0u8; 1];

        assert_eq!(reader.seek(SeekFrom::Start(3)).unwrap(), 3);
        reader.read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"5");

        assert_eq!(reader.seek(SeekFrom::Current(-2)).unwrap(), 2);
        reader.read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"4");

        assert_eq!(reader.seek(SeekFrom::End(-1)).unwrap(), 7);
        reader.read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"9");
    }

    #[test]
    fn negative_seek_is_an_error() {
        let mut reader = SliceReader::new(Cursor::new(DATA), 4, 6);
        assert!(reader.seek(SeekFrom::Current(-1)).is_err());
        assert!(reader.seek(SeekFrom::End(-7)).is_err());
        assert_eq!(reader.seek(SeekFrom::End(-6)).unwrap(), 0);
    }

    #[test]
    fn reads_after_seeking_past_the_end_yield_zero() {
        let mut reader = SliceReader::new(Cursor::new(DATA), 0, 4);
        assert_eq!(reader.seek(SeekFrom::Start(100)).unwrap(), 100);
        let mut buf = [0u8; 8];
        assert_eq!(reader.read(&mut buf).unwrap(), 0);
    }
}
