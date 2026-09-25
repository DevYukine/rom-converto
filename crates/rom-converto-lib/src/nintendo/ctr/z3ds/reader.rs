//! Random-access reader over a seekable-zstd Z3DS payload.
//!
//! Decodes one compressed frame on demand, keyed by the cumulative
//! uncompressed offsets from the tail seek table, so header-only
//! inspection touches a few KiB instead of streaming the whole ROM.

use crate::nintendo::ctr::z3ds::decompress_worker::{Z3dsDecompressWork, plan_decompress_work};
use crate::nintendo::ctr::z3ds::error::Z3dsResult;
use std::io::{self, Read, Seek, SeekFrom};

/// Frames above this are rejected at open so a single random-access
/// decode stays within a bounded memory budget (CIA frames are
/// 32 MiB, default 256 KiB).
const MAX_FRAME_BYTES: u64 = 64 * 1024 * 1024;

/// Reads a seekable-zstd payload at arbitrary offsets, decoding and
/// caching one zstd frame at a time.
pub struct Z3dsReader<R: Read + Seek> {
    inner: R,
    frames: Vec<Z3dsDecompressWork>,
    /// Cumulative uncompressed offset of each frame plus the total
    /// size as the last element.
    starts: Vec<u64>,
    len: u64,
    pos: u64,
    /// Index of the frame currently held in `cache`.
    cached: Option<usize>,
    cache: Vec<u8>,
    scratch: Vec<u8>,
    decoder: zstd::bulk::Decompressor<'static>,
}

impl<R: Read + Seek> Z3dsReader<R> {
    /// Plans frames from the payload's seek table and rejects frames
    /// too large for bounded random access.
    pub fn open(mut inner: R, payload_offset: u64, compressed_size: u64) -> Z3dsResult<Self> {
        let frames = plan_decompress_work(&mut inner, payload_offset, compressed_size)?;
        for frame in &frames {
            if u64::from(frame.uncompressed_size) > MAX_FRAME_BYTES
                || u64::from(frame.compressed_size) > MAX_FRAME_BYTES
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "z3ds frame too large for random access",
                )
                .into());
            }
        }
        let mut starts = Vec::with_capacity(frames.len() + 1);
        let mut total = 0u64;
        starts.push(total);
        for frame in &frames {
            total = total
                .checked_add(u64::from(frame.uncompressed_size))
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "z3ds seek table uncompressed sizes overflow u64",
                    )
                })?;
            starts.push(total);
        }
        Ok(Self {
            inner,
            frames,
            starts,
            len: total,
            pos: 0,
            cached: None,
            cache: Vec::new(),
            scratch: Vec::new(),
            decoder: zstd::bulk::Decompressor::new()?,
        })
    }

    /// Total uncompressed size from the seek table.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<R: Read + Seek> Read for Z3dsReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.len {
            return Ok(0);
        }
        let idx = self.starts.partition_point(|start| *start <= self.pos) - 1;
        if self.cached != Some(idx) {
            // Invalidate first so a failed decode cannot leave the cache
            // mismatched with `cached`.
            self.cached = None;
            let (file_offset, compressed_size, uncompressed_size) = {
                let frame = &self.frames[idx];
                (
                    frame.file_offset,
                    frame.compressed_size,
                    frame.uncompressed_size,
                )
            };
            self.inner.seek(SeekFrom::Start(file_offset))?;
            self.scratch.resize(compressed_size as usize, 0);
            self.inner.read_exact(&mut self.scratch)?;
            // decompress_to_buffer caps output at capacity and sets len
            // to the decoded size, so reserve exactly the declared size.
            self.cache.clear();
            self.cache.reserve(uncompressed_size as usize);
            self.decoder
                .decompress_to_buffer(&self.scratch, &mut self.cache)?;
            if self.cache.len() != uncompressed_size as usize {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "z3ds frame {idx} decoded to {} bytes, seek table says {uncompressed_size}",
                        self.cache.len()
                    ),
                ));
            }
            self.cached = Some(idx);
        }
        let n = buf.len().min((self.starts[idx + 1] - self.pos) as usize);
        let offset = (self.pos - self.starts[idx]) as usize;
        buf[..n].copy_from_slice(&self.cache[offset..offset + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Read + Seek> Seek for Z3dsReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let next = match pos {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        match next {
            Some(n) => {
                self.pos = n;
                Ok(n)
            }
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before start or past u64",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::ctr::z3ds::seekable::{FrameEntry, write_seek_table};
    use std::io::Cursor;

    const FRAME: usize = 4096;

    fn original() -> Vec<u8> {
        (0u8..=99).cycle().take(12_345).collect()
    }

    /// Compresses `data` into independent 4096-byte frames followed by
    /// the seek table, mirroring the seekable-zstd payload layout.
    fn payload(data: &[u8]) -> Vec<u8> {
        let mut compressor = zstd::bulk::Compressor::new(0).unwrap();
        let mut out = Vec::new();
        let mut entries = Vec::new();
        for chunk in data.chunks(FRAME) {
            let compressed = compressor.compress(chunk).unwrap();
            entries.push(FrameEntry {
                compressed_size: compressed.len() as u32,
                decompressed_size: chunk.len() as u32,
            });
            out.extend_from_slice(&compressed);
        }
        write_seek_table(&mut out, &entries).unwrap();
        out
    }

    fn reader(payload: Vec<u8>) -> Z3dsReader<Cursor<Vec<u8>>> {
        let len = payload.len() as u64;
        Z3dsReader::open(Cursor::new(payload), 0, len).unwrap()
    }

    /// Byte offset of the `frame`-th seek-table entry's compressed_size
    /// field: skippable magic + size (8) plus preceding 8-byte entries.
    fn table_entry_offset(payload_len: usize, frame: usize) -> usize {
        let table_start = payload_len - (8 + 4 * 8 + 9);
        table_start + 8 + frame * 8
    }

    #[test]
    fn len_matches_seek_table_total() {
        let z = reader(payload(&original()));
        assert_eq!(z.len(), 12_345);
        assert!(!z.is_empty());
    }

    #[test]
    fn reads_span_frame_boundaries() {
        let original = original();
        let mut z = reader(payload(&original));
        z.seek(SeekFrom::Start(3000)).unwrap();
        let mut buf = vec![0u8; 5000];
        z.read_exact(&mut buf).unwrap();
        assert_eq!(buf, original[3000..8000]);

        z.seek(SeekFrom::Start(0)).unwrap();
        let mut whole = Vec::new();
        z.read_to_end(&mut whole).unwrap();
        assert_eq!(whole, original);
    }

    #[test]
    fn seek_end_and_current_work() {
        let original = original();
        let mut z = reader(payload(&original));
        z.seek(SeekFrom::End(-10)).unwrap();
        let mut tail = vec![0u8; 10];
        z.read_exact(&mut tail).unwrap();
        assert_eq!(tail, original[12_335..]);

        z.seek(SeekFrom::Current(-30)).unwrap();
        let mut mid = vec![0u8; 20];
        z.read_exact(&mut mid).unwrap();
        assert_eq!(mid, original[12_315..12_335]);

        // Reading at or past the end is legal and yields zero bytes.
        z.seek(SeekFrom::End(0)).unwrap();
        assert_eq!(z.read(&mut [0u8; 4]).unwrap(), 0);

        let err = z.seek(SeekFrom::End(-20_000)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn open_requires_seek_table() {
        let mut p = payload(&original());
        // Drop the 4-entry seek table (8 + 4 * 8 + 9 bytes).
        p.truncate(p.len() - (8 + 4 * 8 + 9));
        let len = p.len() as u64;
        let result = Z3dsReader::open(Cursor::new(p), 0, len);
        assert!(
            result.is_err(),
            "open must reject a payload with no seek table"
        );
    }

    #[test]
    fn read_rejects_seek_table_size_lie() {
        let mut p = payload(&original());
        // Claim the 57-byte tail frame is one byte longer; the decoded
        // length then disagrees with the seek table at read time.
        let entry = table_entry_offset(p.len(), 3);
        let lied = u32::from_le_bytes(p[entry + 4..entry + 8].try_into().unwrap()) + 1;
        p[entry + 4..entry + 8].copy_from_slice(&lied.to_le_bytes());

        let mut z = reader(p);
        z.seek(SeekFrom::Start(3 * FRAME as u64)).unwrap();
        let err = z.read_exact(&mut [0u8; 57]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn open_rejects_oversized_frame() {
        let mut p = payload(&original());
        // The compressed-size sum still matches, but the claimed
        // uncompressed size exceeds the random-access frame cap.
        let entry = table_entry_offset(p.len(), 3);
        p[entry + 4..entry + 8].copy_from_slice(&(MAX_FRAME_BYTES as u32 + 1).to_le_bytes());
        let len = p.len() as u64;
        let result = Z3dsReader::open(Cursor::new(p), 0, len);
        assert!(result.is_err(), "open must reject frames over the cap");
    }
}
