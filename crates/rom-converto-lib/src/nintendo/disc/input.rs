//! Unified `Read + Seek` source for Wii and GameCube info readers.
//! `.iso` / `.gcm` open as a plain `BufReader<File>`; `.rvz` opens
//! through [`crate::nintendo::disc::rvz::decompress::RvzDiscReader`] and
//! `.wbfs` through [`crate::nintendo::disc::wbfs::WbfsReader`] so that only
//! the groups or blocks actually touched are materialised, capping
//! peak disk + memory at a few MB even for a multi-GB Wii title.

use binrw::{BinRead, Endian};
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use thiserror::Error;

use crate::nintendo::disc::gcz::GczReader;
use crate::nintendo::disc::gcz::error::GczError;
use crate::nintendo::disc::legacy::{LegacyFormat, detect_legacy_format};
use crate::nintendo::disc::nkit::NkitReader;
use crate::nintendo::disc::nkit::error::NkitError;
use crate::nintendo::disc::rvz::decompress::RvzDiscReader;
use crate::nintendo::disc::rvz::error::RvzError;
use crate::nintendo::disc::rvz::format::{WIA_FILE_HEAD_SIZE, WiaFileHead};
use crate::nintendo::disc::wbfs::WbfsReader;
use crate::nintendo::disc::wbfs::error::WbfsError;
use crate::nintendo::disc::wia::WiaReader;
use crate::nintendo::disc::wia::error::WiaError;

/// A disc image source that abstracts over the container format, so
/// callers can read a plain byte stream regardless of whether the file
/// is an ISO, RVZ, WBFS, GCZ, WIA, or NKit container.
pub trait DiscReader: Read + Seek {
    /// Size of the logical (decompressed) disc image in bytes.
    fn logical_size(&self) -> u64;

    /// Name of the underlying container format, for display in info and verify output.
    fn container_name(&self) -> &'static str;
}

macro_rules! impl_disc_reader {
    ($ty:ty, $name:literal, $size:ident) => {
        impl DiscReader for $ty {
            fn logical_size(&self) -> u64 {
                self.$size()
            }
            fn container_name(&self) -> &'static str {
                $name
            }
        }
    };
}

impl_disc_reader!(RvzDiscReader, "RVZ", iso_size);
impl_disc_reader!(WbfsReader, "WBFS", disc_size);
impl_disc_reader!(GczReader, "GCZ", data_size);
impl_disc_reader!(WiaReader, "WIA", iso_size);
impl_disc_reader!(NkitReader, "NKit", image_size);

/// A plain `.iso` / `.gcm`: buffered file bytes plus the length taken
/// at open time.
struct IsoFile {
    inner: BufReader<File>,
    size: u64,
}

impl DiscReader for IsoFile {
    fn logical_size(&self) -> u64 {
        self.size
    }
    fn container_name(&self) -> &'static str {
        "ISO"
    }
}

impl Read for IsoFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Seek for IsoFile {
    fn seek(&mut self, from: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(from)
    }
}

/// Error from opening or sizing a disc image. Each container reader's
/// own error is kept whole so callers can match on the cause (and on
/// [`std::io::ErrorKind`]) instead of a flattened string.
#[derive(Debug, Error)]
pub enum DiscInputError {
    #[error("disc_input: {op} {}", path.display())]
    Io {
        op: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    BinRw(#[from] binrw::Error),

    #[error(transparent)]
    Rvz(#[from] Box<RvzError>),

    #[error(transparent)]
    Wbfs(#[from] WbfsError),

    #[error(transparent)]
    Gcz(#[from] GczError),

    #[error(transparent)]
    Wia(#[from] Box<WiaError>),

    #[error(transparent)]
    Nkit(#[from] NkitError),
}

impl From<RvzError> for DiscInputError {
    fn from(err: RvzError) -> Self {
        Self::Rvz(Box::new(err))
    }
}

impl From<WiaError> for DiscInputError {
    fn from(err: WiaError) -> Self {
        Self::Wia(Box::new(err))
    }
}

impl From<DiscInputError> for RvzError {
    fn from(err: DiscInputError) -> Self {
        match err {
            DiscInputError::Io { source, .. } => Self::IoError(source),
            DiscInputError::BinRw(e) => Self::BinRWError(e),
            DiscInputError::Rvz(e) => *e,
            DiscInputError::Wbfs(e) => Self::Wbfs(e),
            DiscInputError::Gcz(e) => Self::Gcz(e),
            DiscInputError::Wia(e) => Self::Wia(e),
            DiscInputError::Nkit(e) => Self::Nkit(e),
        }
    }
}

/// Result alias for the disc input helpers.
pub type DiscInputResult<T> = Result<T, DiscInputError>;

fn io_err<'a>(op: &'static str, path: &'a Path) -> impl Fn(std::io::Error) -> DiscInputError + 'a {
    move |source| DiscInputError::Io {
        op,
        path: path.to_path_buf(),
        source,
    }
}

/// Opens `path` as a disc image, detecting the container format from its
/// extension or magic bytes and falling back to a plain file otherwise.
pub fn open_disc_input(path: &Path) -> DiscInputResult<Box<dyn DiscReader>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());

    if matches!(ext.as_deref(), Some("rvz")) || is_magic(path, &[b'R', b'V', b'Z', 0x01]) {
        return Ok(Box::new(RvzDiscReader::open(path)?));
    }

    if matches!(ext.as_deref(), Some("wbfs")) || is_magic(path, b"WBFS") {
        return Ok(Box::new(WbfsReader::open(path)?));
    }

    match detect_legacy_format(path).unwrap_or(None) {
        Some(LegacyFormat::Gcz) => return Ok(Box::new(GczReader::open(path)?)),
        Some(LegacyFormat::Wia) => return Ok(Box::new(WiaReader::open(path)?)),
        Some(LegacyFormat::NkitIso) => return Ok(Box::new(NkitReader::open(path)?)),
        Some(LegacyFormat::NkitGcz) => {
            let gcz = GczReader::open(path)?;
            return Ok(Box::new(NkitReader::from_source(gcz)?));
        }
        None => {}
    }

    let file = File::open(path).map_err(io_err("open", path))?;
    let size = file.metadata().map_err(io_err("stat", path))?.len();
    Ok(Box::new(IsoFile {
        inner: BufReader::with_capacity(4 * 1024 * 1024, file),
        size,
    }))
}

/// Logical (decompressed) size of the disc image at `path`, read from
/// container headers alone. Progress totals use this instead of
/// [`open_disc_input`], which parses the full group tables and, for
/// NKit, builds the whole restore plan.
pub fn disc_size_of(path: &Path) -> DiscInputResult<u64> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());

    if matches!(ext.as_deref(), Some("rvz")) || is_magic(path, &[b'R', b'V', b'Z', 0x01]) {
        let mut head_bytes = [0u8; WIA_FILE_HEAD_SIZE];
        File::open(path)
            .and_then(|mut f| f.read_exact(&mut head_bytes))
            .map_err(io_err("read RVZ head", path))?;
        let head = WiaFileHead::read_options(&mut Cursor::new(&head_bytes[..]), Endian::Big, ())?;
        return Ok(head.iso_file_size);
    }

    if matches!(ext.as_deref(), Some("wbfs")) || is_magic(path, b"WBFS") {
        return Ok(WbfsReader::open(path)?.disc_size());
    }

    match detect_legacy_format(path).unwrap_or(None) {
        Some(LegacyFormat::Gcz) => Ok(GczReader::data_size_of(path)?),
        Some(LegacyFormat::Wia) => Ok(WiaReader::iso_size_of(path)?),
        Some(LegacyFormat::NkitIso) => Ok(NkitReader::image_size_of(path)?),
        Some(LegacyFormat::NkitGcz) => {
            let dhead = crate::nintendo::disc::gcz::gcz_logical_prefix(path, 0x440)?;
            Ok(crate::nintendo::disc::nkit::format::NkitHeader::parse(&dhead)?.image_size)
        }
        None => Ok(std::fs::metadata(path).map_err(io_err("stat", path))?.len()),
    }
}

/// True when `path` opens with `magic`. A short or unreadable file
/// simply does not match, leaving detection to the next candidate.
fn is_magic(path: &Path, magic: &[u8; 4]) -> bool {
    let mut buf = [0u8; 4];
    File::open(path)
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok()
        && buf == *magic
}
