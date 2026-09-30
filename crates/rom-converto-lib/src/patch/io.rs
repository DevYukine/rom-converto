//! Shared streaming primitives for the patch appliers: bounded positional
//! reads out of a patch file, chunked copies between files, checksum helpers,
//! and the two variable-length integer encodings (the biased BPS/UPS encoding
//! and the RFC 3284 base-128 encoding).
//!
//! Every applier works through fixed-size windows of [`COPY_CHUNK_BYTES`] and
//! positional (`pread`) IO; no applier ever allocates a patch-declared size.

use crate::util::{CancelToken, Cancelled};
use anyhow::{Context, Result, bail};
use std::fs::File;

/// Bulk data moves through windows of this size instead of materialising a
/// patch-controlled length; every declared size is clamped to the window (or
/// to the target size) before any buffer is sized.
pub(super) const COPY_CHUNK_BYTES: usize = 64 * 1024;

/// Stack buffer used by [`Range`] for sequential byte access into a patch
/// file region.
const RANGE_BUFFER: usize = 4096;

/// Bounds-checked positional read with context.
pub(super) fn read_at(file: &File, buf: &mut [u8], offset: u64, what: &str) -> Result<()> {
    crate::util::pread::file_read_exact_at(file, buf, offset)
        .with_context(|| format!("{what}: unexpected end of file"))
}

/// Positional write of a whole buffer.
pub(super) fn write_at(file: &File, buf: &[u8], offset: u64) -> Result<()> {
    crate::util::pread::file_write_all_at(file, buf, offset).context("writing the patched output")
}

/// A buffered sequential reader over a fixed byte range of one file. Reads
/// are bounds-checked against the range, so a malformed stream cannot read
/// past the region it belongs to.
pub(super) struct Range<'a> {
    file: &'a File,
    pos: u64,
    end: u64,
    buf: [u8; RANGE_BUFFER],
    start: usize,
    filled: usize,
}

impl<'a> Range<'a> {
    pub(super) fn new(file: &'a File, start: u64, end: u64) -> Self {
        Self {
            file,
            pos: start,
            end,
            buf: [0; RANGE_BUFFER],
            start: 0,
            filled: 0,
        }
    }

    /// Bytes left in the range.
    pub(super) fn remaining(&self) -> u64 {
        self.end - self.pos + self.buffered() as u64
    }

    /// Absolute file position of the next unread byte.
    pub(super) fn position(&self) -> u64 {
        self.end - self.remaining()
    }

    fn buffered(&self) -> usize {
        self.filled - self.start
    }

    fn refill(&mut self) -> Result<()> {
        debug_assert_eq!(self.start, self.filled);
        self.start = 0;
        self.filled = 0;
        let want = (RANGE_BUFFER as u64).min(self.end - self.pos) as usize;
        if want == 0 {
            bail!("unexpected end of patch data");
        }
        crate::util::pread::file_read_exact_at(self.file, &mut self.buf[..want], self.pos)
            .context("reading the patch")?;
        self.pos += want as u64;
        self.filled = want;
        Ok(())
    }

    pub(super) fn byte(&mut self) -> Result<u8> {
        if self.start == self.filled {
            self.refill()?;
        }
        let byte = self.buf[self.start];
        self.start += 1;
        Ok(byte)
    }

    /// Next byte, or `None` at the end of the range.
    pub(super) fn try_byte(&mut self) -> Result<Option<u8>> {
        if self.start == self.filled && self.pos == self.end {
            return Ok(None);
        }
        self.byte().map(Some)
    }

    /// Reads exactly `out.len()` bytes, erroring when the range is shorter.
    pub(super) fn take(&mut self, out: &mut [u8]) -> Result<()> {
        if out.len() as u64 > self.remaining() {
            bail!("unexpected end of patch data");
        }
        let buffered = self.buffered().min(out.len());
        out[..buffered].copy_from_slice(&self.buf[self.start..self.start + buffered]);
        self.start += buffered;
        let rest = &mut out[buffered..];
        if !rest.is_empty() {
            crate::util::pread::file_read_exact_at(self.file, rest, self.pos)
                .context("reading the patch")?;
            self.pos += rest.len() as u64;
        }
        Ok(())
    }

    /// Skips `len` bytes, erroring when the range is shorter.
    pub(super) fn skip(&mut self, len: u64) -> Result<()> {
        if len > self.remaining() {
            bail!("unexpected end of patch data");
        }
        // Consume from the buffer first so `pos` never passes `end`.
        let from_buffer = (len.min(self.buffered() as u64)) as usize;
        self.start += from_buffer;
        self.pos += len - from_buffer as u64;
        Ok(())
    }

    /// Narrows the range to its next `len` bytes, consuming them from `self`.
    pub(super) fn split(&mut self, len: u64) -> Result<Range<'a>> {
        let start = self.position();
        self.skip(len)?;
        Ok(Range::new(self.file, start, start + len))
    }

    /// Peeks at the next `N` bytes without consuming them; `None` when fewer
    /// than `N` remain.
    pub(super) fn peek<const N: usize>(&self) -> Result<Option<[u8; N]>> {
        if self.remaining() < N as u64 {
            return Ok(None);
        }
        let mut out = [0; N];
        let from_buffer = self.buffered().min(N);
        out[..from_buffer].copy_from_slice(&self.buf[self.start..self.start + from_buffer]);
        if from_buffer < N {
            crate::util::pread::file_read_exact_at(
                self.file,
                &mut out[from_buffer..],
                self.position() + from_buffer as u64,
            )
            .context("reading the patch")?;
        }
        Ok(Some(out))
    }

    /// True when the next bytes equal `marker`, comparing through the
    /// buffered peek when it covers the marker and falling back to a direct
    /// read at the tail of the range.
    pub(super) fn peek_eq(&self, marker: &[u8]) -> Result<bool> {
        if self.remaining() < marker.len() as u64 {
            return Ok(false);
        }
        if let Some(bytes) = self.peek::<8>()? {
            return Ok(&bytes[..marker.len()] == marker);
        }
        // Fewer than 8 bytes remain: the buffered peek cannot help, so read
        // the marker directly at the cursor.
        let mut tail = [0u8; 8];
        crate::util::pread::file_read_exact_at(
            self.file,
            &mut tail[..marker.len()],
            self.position(),
        )
        .context("reading the patch")?;
        Ok(&tail[..marker.len()] == marker)
    }

    /// Reads the bytes `skip` bytes ahead of the cursor without consuming
    /// anything.
    pub(super) fn peek_at(&self, skip: u64, out: &mut [u8]) -> Result<()> {
        crate::util::pread::file_read_exact_at(self.file, out, self.position() + skip)
            .context("reading the patch")
    }

    /// Reads exactly `N` bytes into a fixed array.
    pub(super) fn read_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut out = [0u8; N];
        self.take(&mut out)?;
        Ok(out)
    }

    /// Biased variable-length integer used by the BPS and UPS specifications:
    /// 7 bits per byte, bit 7 set marks the final byte, and every
    /// non-final group contributes one extra unit.
    pub(super) fn varint_bps(&mut self) -> Result<u64> {
        let overflow = || "variable-length integer overflows 64 bits";
        let mut data: u64 = 0;
        let mut shift: u64 = 1;
        for _ in 0..10 {
            let byte = self.byte()?;
            let group = ((byte & 0x7f) as u64)
                .checked_mul(shift)
                .context(overflow())?;
            data = data.checked_add(group).context(overflow())?;
            if byte & 0x80 != 0 {
                return Ok(data);
            }
            shift = shift.checked_mul(0x80).context(overflow())?;
            data = data.checked_add(shift).context(overflow())?;
        }
        bail!(overflow());
    }

    /// Base-128 integer per RFC 3284 section 2: most significant group first,
    /// every byte but the last has bit 7 set. More than ten bytes or a value
    /// past 64 bits is an error.
    pub(super) fn varint_rfc(&mut self) -> Result<u64> {
        let overflow = || "variable-length integer overflows 64 bits";
        let mut value: u64 = 0;
        for _ in 0..10 {
            let byte = self.byte()?;
            value = value
                .checked_mul(0x80)
                .and_then(|value| value.checked_add((byte & 0x7f) as u64))
                .context(overflow())?;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        bail!(overflow());
    }
}

/// Streams `len` bytes from `source` at `source_offset` into `out` at
/// `out_offset` in [`COPY_CHUNK_BYTES`] windows. With `padded_len`, the
/// source is logically zero-filled past that length (the convention patches
/// use when the target is larger than the source); without it, running past
/// the source's end is an error.
#[allow(clippy::too_many_arguments)]
pub(super) fn copy_range(
    source: &File,
    padded_len: Option<u64>,
    source_offset: u64,
    out: &File,
    out_offset: u64,
    len: u64,
    buf: &mut [u8],
    cancel: &CancelToken,
) -> Result<()> {
    let chunk = buf.len() as u64;
    let mut done = 0u64;
    while done < len {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let n = chunk.min(len - done) as usize;
        let from = source_offset + done;
        match padded_len {
            Some(padded) if from >= padded => {
                buf[..n].fill(0);
            }
            Some(padded) => {
                let live = (padded - from).min(n as u64) as usize;
                read_at(source, &mut buf[..live], from, "reading the source")?;
                buf[live..n].fill(0);
            }
            None => read_at(source, &mut buf[..n], from, "reading the source")?,
        }
        write_at(out, &buf[..n], out_offset + done)?;
        done += n as u64;
    }
    Ok(())
}

/// Copies `len` bytes within one file from `from` to `to`, where the source
/// region has already been written or is being written by this same copy.
/// Overlapping copies reproduce the byte-by-byte forward semantics delta
/// formats rely on: when the cursors are closer than one buffer, the period
/// between them is read once, tiled across the buffer, and written in
/// chunks that each cover whole periods.
pub(super) fn copy_within_file(
    file: &File,
    from: u64,
    to: u64,
    len: u64,
    buf: &mut [u8],
    cancel: &CancelToken,
) -> Result<()> {
    debug_assert!(from < to, "a within-file copy always walks forward");
    if len == 0 {
        return Ok(());
    }
    // A distance of at least the copy length or one buffer never overlaps:
    // plain chunks.
    if to - from >= len || to - from >= buf.len() as u64 {
        let cap = (buf.len() as u64).min(to - from);
        let mut done = 0u64;
        while done < len {
            if cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            let n = cap.min(len - done) as usize;
            read_at(file, &mut buf[..n], from + done, "reading the target data")?;
            write_at(file, &buf[..n], to + done)?;
            done += n as u64;
        }
        return Ok(());
    }
    // Short forward distance: the result is the `to - from` byte period
    // taken at `from`, tiled. Read it once, tile the buffer in place, and
    // write chunks of exactly `span` bytes, a multiple of the period, so
    // every chunk starts on a period boundary; the final short chunk does
    // too, because every earlier chunk covered whole periods.
    let period = (to - from) as usize;
    read_at(file, &mut buf[..period], from, "reading the target data")?;
    for i in period..buf.len() {
        buf[i] = buf[i - period];
    }
    let span = buf.len() - buf.len() % period;
    let mut done = 0u64;
    while done < len {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let n = (span as u64).min(len - done) as usize;
        write_at(file, &buf[..n], to + done)?;
        done += n as u64;
    }
    Ok(())
}

/// Streams `len` bytes out of a patch [`Range`] into `out` at `at`, chunked
/// through `buf`.
pub(super) fn transfer(
    data: &mut Range,
    out: &File,
    at: u64,
    len: u64,
    buf: &mut [u8],
    cancel: &CancelToken,
) -> Result<()> {
    let mut done = 0u64;
    while done < len {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let n = (buf.len() as u64).min(len - done) as usize;
        data.take(&mut buf[..n])?;
        write_at(out, &buf[..n], at + done)?;
        done += n as u64;
    }
    Ok(())
}

/// XORs `data` over the source at `at` (source bytes past the source's end
/// read as zero) and writes the result to `out`. `scratch` must be at least
/// `data.len()` bytes.
pub(super) fn xor_padded(
    source: &File,
    source_len: u64,
    out: &File,
    at: u64,
    data: &[u8],
    scratch: &mut [u8],
    cancel: &CancelToken,
) -> Result<()> {
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    let live = source_len.saturating_sub(at).min(data.len() as u64) as usize;
    if live > 0 {
        read_at(source, &mut scratch[..live], at, "reading the source")?;
    }
    scratch[live..data.len()].fill(0);
    for (byte, delta) in scratch[..data.len()].iter_mut().zip(data) {
        *byte ^= delta;
    }
    write_at(out, &scratch[..data.len()], at)
}

/// Writes `fill` repeated `len` times at `offset`, chunked.
pub(super) fn write_fill(
    file: &File,
    offset: u64,
    fill: u8,
    len: u64,
    buf: &mut [u8],
    cancel: &CancelToken,
) -> Result<()> {
    let filled = (buf.len() as u64).min(len) as usize;
    buf[..filled].fill(fill);
    let mut done = 0u64;
    while done < len {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let n = filled.min((len - done) as usize);
        write_at(file, &buf[..n], offset + done)?;
        done += n as u64;
    }
    Ok(())
}

/// Walks the byte range `[at, at + len)` of a file in buffered chunks,
/// feeding each chunk to `visit`.
pub(super) fn for_chunks(
    file: &File,
    at: u64,
    len: u64,
    buf: &mut [u8],
    cancel: &CancelToken,
    mut visit: impl FnMut(&[u8]),
) -> Result<()> {
    let mut done = 0u64;
    while done < len {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let n = (buf.len() as u64).min(len - done) as usize;
        read_at(file, &mut buf[..n], at + done, "hashing")?;
        visit(&buf[..n]);
        done += n as u64;
    }
    Ok(())
}

/// CRC-32 (IEEE, as used by every checksum-bearing format here) over a byte
/// range of a file, read in chunks.
pub(super) fn crc32_of_range(
    file: &File,
    offset: u64,
    len: u64,
    buf: &mut [u8],
    cancel: &CancelToken,
) -> Result<u32> {
    let mut digest = crate::util::hash::CRC32.digest();
    for_chunks(file, offset, len, buf, cancel, |chunk| digest.update(chunk))?;
    Ok(digest.finalize())
}

/// MD5 over a byte range of a file, read in chunks.
pub(super) fn md5_of_range(
    file: &File,
    offset: u64,
    len: u64,
    buf: &mut [u8],
    cancel: &CancelToken,
) -> Result<[u8; 16]> {
    use sha2::Digest as _;
    let mut hasher = md_5::Md5::new();
    for_chunks(file, offset, len, buf, cancel, |chunk| hasher.update(chunk))?;
    Ok(hasher.finalize().into())
}

/// Upper bound on how far a patch may grow (or declare) its target above
/// the source, shared by every applier with a declared target size.
pub(super) const MAX_GROWTH: u64 = 1 << 30;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn within_file_copy_tiles_a_short_period_byte_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("within.bin");
        // Seed only the one period, then zeros.
        let pattern = [0xAAu8, 0xBB, 0xCC];
        let len = 2 * COPY_CHUNK_BYTES + 1000;
        let mut source = vec![0u8; len];
        source[..pattern.len()].copy_from_slice(&pattern);
        std::fs::write(&path, &source).unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();

        // Copy everything from 0 to a destination one period in: the copy
        // propagates the pattern across the whole remaining file.
        let mut buf = vec![0u8; COPY_CHUNK_BYTES];
        let copy_len = len as u64 - 3;
        copy_within_file(&file, 0, 3, copy_len, &mut buf, &CancelToken::new()).expect("copy");
        drop(file);

        let result = std::fs::read(&path).unwrap();
        for (k, byte) in result.iter().enumerate() {
            assert_eq!(*byte, pattern[k % pattern.len()], "byte {k}");
        }
    }
}

#[cfg(test)]
pub(super) mod test_support {
    /// Encodes a number with the biased BPS/UPS variable-length scheme.
    pub(crate) fn bps_varint(mut data: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let byte = (data & 0x7f) as u8;
            data >>= 7;
            if data == 0 {
                out.push(0x80 | byte);
                break;
            }
            out.push(byte);
            data -= 1;
        }
        out
    }

    /// Encodes a number with the RFC 3284 base-128 scheme.
    pub(crate) fn rfc_varint(value: u64) -> Vec<u8> {
        let mut groups = Vec::new();
        let mut rest = value;
        loop {
            groups.push((rest & 0x7f) as u8);
            rest >>= 7;
            if rest == 0 {
                break;
            }
        }
        groups.reverse();
        let last = groups.len() - 1;
        groups
            .iter()
            .enumerate()
            .map(|(i, &g)| if i == last { g } else { g | 0x80 })
            .collect()
    }
}
