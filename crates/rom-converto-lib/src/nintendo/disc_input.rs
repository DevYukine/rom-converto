//! Unified `Read + Seek` source for Wii and GameCube info readers.
//! `.iso` / `.gcm` open as a plain `BufReader<File>`; `.rvz` opens
//! through [`crate::nintendo::rvz::decompress::RvzDiscReader`] and
//! `.wbfs` through [`crate::nintendo::wbfs::WbfsReader`] so that only
//! the groups or blocks actually touched are materialised, capping
//! peak disk + memory at a few MB even for a multi-GB Wii title.

use anyhow::{Context, Result};
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::nintendo::gcz::GczReader;
use crate::nintendo::legacy_input::{LegacyFormat, detect_legacy_format};
use crate::nintendo::nkit::NkitReader;
use crate::nintendo::rvz::decompress::RvzDiscReader;
use crate::nintendo::wbfs::WbfsReader;
use crate::nintendo::wia::WiaReader;

pub enum DiscInput {
    File(BufReader<File>),
    Rvz(Box<RvzDiscReader>),
    Wbfs(Box<WbfsReader>),
    Gcz(Box<GczReader>),
    Wia(Box<WiaReader>),
    Nkit(Box<NkitReader>),
}

impl DiscInput {
    pub fn container_name(&self) -> &'static str {
        match self {
            Self::File(_) => "ISO",
            Self::Rvz(_) => "RVZ",
            Self::Wbfs(_) => "WBFS",
            Self::Gcz(_) => "GCZ",
            Self::Wia(_) => "WIA",
            Self::Nkit(_) => "NKit",
        }
    }

    pub fn iso_size(&mut self) -> Result<u64> {
        match self {
            Self::File(r) => {
                let cur = r.stream_position()?;
                let end = r.seek(SeekFrom::End(0))?;
                r.seek(SeekFrom::Start(cur))?;
                Ok(end)
            }
            Self::Rvz(r) => Ok(r.iso_size()),
            Self::Wbfs(r) => Ok(r.disc_size()),
            Self::Gcz(r) => Ok(r.data_size()),
            Self::Wia(r) => Ok(r.iso_size()),
            Self::Nkit(r) => Ok(r.image_size()),
        }
    }
}

impl Read for DiscInput {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::File(r) => r.read(buf),
            Self::Rvz(r) => r.read(buf),
            Self::Wbfs(r) => r.read(buf),
            Self::Gcz(r) => r.read(buf),
            Self::Wia(r) => r.read(buf),
            Self::Nkit(r) => r.read(buf),
        }
    }
}

impl Seek for DiscInput {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        match self {
            Self::File(r) => r.seek(from),
            Self::Rvz(r) => r.seek(from),
            Self::Wbfs(r) => r.seek(from),
            Self::Gcz(r) => r.seek(from),
            Self::Wia(r) => r.seek(from),
            Self::Nkit(r) => r.seek(from),
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

    let by_wbfs_ext = matches!(ext.as_deref(), Some("wbfs"));
    let by_wbfs_magic = is_wbfs_magic(path).unwrap_or(false);
    if by_wbfs_ext || by_wbfs_magic {
        let reader = WbfsReader::open(path)
            .with_context(|| format!("disc_input: open WBFS {}", path.display()))?;
        return Ok(DiscInput::Wbfs(Box::new(reader)));
    }

    match detect_legacy_format(path).unwrap_or(None) {
        Some(LegacyFormat::Gcz) => {
            let reader = GczReader::open(path)
                .with_context(|| format!("disc_input: open GCZ {}", path.display()))?;
            return Ok(DiscInput::Gcz(Box::new(reader)));
        }
        Some(LegacyFormat::Wia) => {
            let reader = WiaReader::open(path)
                .with_context(|| format!("disc_input: open WIA {}", path.display()))?;
            return Ok(DiscInput::Wia(Box::new(reader)));
        }
        Some(LegacyFormat::NkitIso) => {
            let reader = NkitReader::open(path)
                .with_context(|| format!("disc_input: open NKit {}", path.display()))?;
            return Ok(DiscInput::Nkit(Box::new(reader)));
        }
        Some(LegacyFormat::NkitGcz) => {
            let gcz = GczReader::open(path)
                .with_context(|| format!("disc_input: open NKit GCZ {}", path.display()))?;
            let reader = NkitReader::from_source(gcz)
                .with_context(|| format!("disc_input: open NKit GCZ {}", path.display()))?;
            return Ok(DiscInput::Nkit(Box::new(reader)));
        }
        None => {}
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

fn is_wbfs_magic(path: &Path) -> std::io::Result<bool> {
    let mut f = File::open(path)?;
    let mut buf = [0u8; 4];
    if f.read(&mut buf)? < 4 {
        return Ok(false);
    }
    Ok(buf == *b"WBFS")
}
