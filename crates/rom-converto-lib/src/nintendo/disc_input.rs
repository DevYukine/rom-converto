//! Unified `Read + Seek` source for Wii and GameCube info readers.
//! `.iso` / `.gcm` open as a plain `BufReader<File>`; `.rvz` opens
//! through [`crate::nintendo::rvz::decompress::RvzDiscReader`] so
//! that only the groups actually touched are decompressed, capping
//! peak disk + memory at a few MB even for a multi-GB Wii title.

use anyhow::{Context, Result};
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::nintendo::rvz::decompress::RvzDiscReader;

pub enum DiscInput {
    File(BufReader<File>),
    Rvz(Box<RvzDiscReader>),
}

impl DiscInput {
    pub fn iso_size(&mut self) -> Result<u64> {
        match self {
            Self::File(r) => {
                let cur = r.stream_position()?;
                let end = r.seek(SeekFrom::End(0))?;
                r.seek(SeekFrom::Start(cur))?;
                Ok(end)
            }
            Self::Rvz(r) => Ok(r.iso_size()),
        }
    }
}

impl Read for DiscInput {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::File(r) => r.read(buf),
            Self::Rvz(r) => r.read(buf),
        }
    }
}

impl Seek for DiscInput {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        match self {
            Self::File(r) => r.seek(from),
            Self::Rvz(r) => r.seek(from),
        }
    }
}

pub fn open_disc_input(path: &Path) -> Result<DiscInput> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    let by_ext = matches!(ext.as_deref(), Some("rvz"));
    let by_magic = is_rvz_magic(path).unwrap_or(false);
    if by_ext || by_magic {
        let reader = RvzDiscReader::open(path)
            .with_context(|| format!("disc_input: open RVZ {}", path.display()))?;
        return Ok(DiscInput::Rvz(Box::new(reader)));
    }
    let file = File::open(path).with_context(|| format!("disc_input: open {}", path.display()))?;
    Ok(DiscInput::File(BufReader::with_capacity(
        4 * 1024 * 1024,
        file,
    )))
}

fn is_rvz_magic(path: &Path) -> std::io::Result<bool> {
    let mut f = File::open(path)?;
    let mut buf = [0u8; 4];
    if f.read(&mut buf)? < 4 {
        return Ok(false);
    }
    Ok(buf == [b'R', b'V', b'Z', 0x01])
}
