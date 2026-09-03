//! Bounds-checked big-endian cursor shared by the BRLYT and BRLAN parsers.

use anyhow::{Result, anyhow};
use byteorder::{BE, ByteOrder};

pub(super) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Starts reading `data` at the absolute offset `pos`.
    pub(super) fn at(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    pub(super) fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| anyhow!("offset overflow reading {} bytes", n))?;
        let out = self.data.get(self.pos..end).ok_or_else(|| {
            anyhow!(
                "read of {} bytes at 0x{:X} past end of {} bytes",
                n,
                self.pos,
                self.data.len()
            )
        })?;
        self.pos = end;
        Ok(out)
    }

    pub(super) fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }

    pub(super) fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub(super) fn u16(&mut self) -> Result<u16> {
        Ok(BE::read_u16(self.take(2)?))
    }

    pub(super) fn i16(&mut self) -> Result<i16> {
        Ok(BE::read_i16(self.take(2)?))
    }

    pub(super) fn u32(&mut self) -> Result<u32> {
        Ok(BE::read_u32(self.take(4)?))
    }

    pub(super) fn f32(&mut self) -> Result<f32> {
        Ok(BE::read_f32(self.take(4)?))
    }

    /// Reads a fixed-width NUL-padded name field.
    pub(super) fn fixed_str(&mut self, n: usize) -> Result<String> {
        let raw = self.take(n)?;
        let end = raw.iter().position(|b| *b == 0).unwrap_or(n);
        Ok(String::from_utf8_lossy(&raw[..end]).into_owned())
    }
}

/// Reads the NUL-terminated string at `off`, stopping at the end of `data`.
pub(super) fn cstr(data: &[u8], off: usize) -> Result<String> {
    let tail = data
        .get(off..)
        .ok_or_else(|| anyhow!("string offset 0x{:X} past end", off))?;
    let end = tail.iter().position(|b| *b == 0).unwrap_or(tail.len());
    Ok(String::from_utf8_lossy(&tail[..end]).into_owned())
}
