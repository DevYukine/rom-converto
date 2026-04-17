//! Unified logical-sector abstraction over WUD, WUX, and split-file
//! disc images.
//!
//! Callers read the disc as a flat stream of 32 KiB sectors regardless
//! of the on-disk container. Opening a path auto-detects the format by
//! magic bytes (WUX) or file extension (WUD/split parts).

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::nintendo::wup::disc::wud_reader::WudReader;
use crate::nintendo::wup::disc::wux_reader::WuxReader;
use crate::nintendo::wup::error::{WupError, WupResult};

/// Logical Wii U disc sector size in bytes. All on-disc layout
/// offsets and sizes are multiples of this value.
pub const SECTOR_SIZE: usize = 0x8000;

/// Total size of a single-layer retail WUD image in bytes.
/// `0x5D3A00000 = 25,025,314,816`.
pub const WUD_SINGLE_LAYER_SIZE: u64 = 0x0005_D3A0_0000;

/// Random-access reader for a logical Wii U disc image. Hides whether
/// the backing storage is a raw WUD, a set of `game_partN.wud` split
/// files, or a WUX deduplicated image.
pub trait DiscSectorSource: Send {
    /// Total number of 32 KiB logical sectors on the disc.
    fn total_sectors(&self) -> u64;

    /// Read one logical sector into `dst`. `dst.len()` must be
    /// exactly [`SECTOR_SIZE`]. `sector_index` must be less than
    /// [`Self::total_sectors`].
    fn read_sector(&mut self, sector_index: u64, dst: &mut [u8]) -> WupResult<()>;

    /// Read a byte range spanning one or more sectors. Default
    /// implementation walks sector-by-sector through a scratch
    /// buffer; specialised readers may override for efficiency but
    /// the default is correct for all conforming readers.
    fn read_bytes(&mut self, offset: u64, out: &mut [u8]) -> WupResult<()> {
        let mut scratch = vec![0u8; SECTOR_SIZE];
        let mut written = 0usize;
        let mut cursor = offset;
        while written < out.len() {
            let sector_index = cursor / SECTOR_SIZE as u64;
            let sector_off = (cursor % SECTOR_SIZE as u64) as usize;
            self.read_sector(sector_index, &mut scratch)?;
            let take = (SECTOR_SIZE - sector_off).min(out.len() - written);
            out[written..written + take].copy_from_slice(&scratch[sector_off..sector_off + take]);
            written += take;
            cursor += take as u64;
        }
        Ok(())
    }
}

/// Open a disc image, picking the right container format
/// automatically.
///
/// Detection order:
/// 1. If the file begins with the WUX magic (`WUX0` + version),
///    treat it as WUX.
/// 2. If the file path ends in `.wud` or is named
///    `game_part1.wud`, treat it as WUD (chained split image if
///    sibling parts exist, single file otherwise).
/// 3. Otherwise, reject as an unsupported format.
pub fn open_disc<P: AsRef<Path>>(path: P) -> WupResult<Box<dyn DiscSectorSource>> {
    let path = path.as_ref();
    if !path.exists() {
        return Err(WupError::MissingRequiredFile(path.to_path_buf()));
    }

    // Peek at the first 8 bytes so we can distinguish WUX0 from raw
    // WUD. We do not rely on file extension for this, only for the
    // split-part chain convention below.
    let mut magic = [0u8; 8];
    {
        let mut f = File::open(path)?;
        let _ = f.read(&mut magic)?;
    }

    if is_wux_magic(&magic) {
        let reader = WuxReader::open(path)?;
        return Ok(Box::new(reader));
    }

    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase);
    let is_wud_named = matches!(ext.as_deref(), Some("wud"));
    let is_split_part1 = stem.as_deref() == Some("game_part1");

    if is_wud_named || is_split_part1 {
        let parts = discover_split_parts(path);
        let reader = WudReader::open_parts(parts)?;
        return Ok(Box::new(reader));
    }

    Err(WupError::UnsupportedDiscFormat(path.to_path_buf()))
}

/// Returns true when the 8-byte prefix matches the WUX0 container
/// magic (little-endian `0x57555830`, `0x1099D02E`).
pub(crate) fn is_wux_magic(prefix: &[u8]) -> bool {
    prefix.len() >= 8 && &prefix[0..4] == b"WUX0" && prefix[4..8] == [0x2E, 0xD0, 0x99, 0x10]
}

/// Collect `game_part<N>.wud` siblings into a single ordered vec.
/// Returns a singleton when no splits are present.
fn discover_split_parts(first: &Path) -> Vec<PathBuf> {
    let stem = first
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase);
    let parent = first.parent().map(PathBuf::from).unwrap_or_default();
    if stem.as_deref() != Some("game_part1") {
        return vec![first.to_path_buf()];
    }
    let mut parts = vec![first.to_path_buf()];
    for idx in 2u32..=12 {
        let candidate = parent.join(format!("game_part{}.wud", idx));
        if candidate.is_file() {
            parts.push(candidate);
        } else {
            break;
        }
    }
    parts
}

/// Minimal adapter that lets anything implementing `Read + Seek`
/// serve as a `DiscSectorSource` for testing. Not exposed publicly.
#[cfg(test)]
pub(crate) struct InMemoryDisc {
    data: Vec<u8>,
}

#[cfg(test)]
impl InMemoryDisc {
    pub(crate) fn new(data: Vec<u8>) -> Self {
        assert!(
            data.len().is_multiple_of(SECTOR_SIZE),
            "in-memory disc size must be a multiple of the sector size"
        );
        Self { data }
    }
}

#[cfg(test)]
impl DiscSectorSource for InMemoryDisc {
    fn total_sectors(&self) -> u64 {
        (self.data.len() / SECTOR_SIZE) as u64
    }
    fn read_sector(&mut self, sector_index: u64, dst: &mut [u8]) -> WupResult<()> {
        assert_eq!(dst.len(), SECTOR_SIZE);
        let off = sector_index as usize * SECTOR_SIZE;
        dst.copy_from_slice(&self.data[off..off + SECTOR_SIZE]);
        Ok(())
    }
}

/// `Read + Seek` stream that transparently concatenates several
/// fixed-size files. Used by the split-WUD reader and exposed here so
/// the WUD container can wrap either a single file or a chain with
/// the same downstream code.
pub(crate) struct MultiFileReader {
    parts: Vec<FilePart>,
    pos: u64,
    total: u64,
}

struct FilePart {
    file: BufReader<File>,
    start: u64,
    len: u64,
}

impl MultiFileReader {
    pub(crate) fn open(paths: &[PathBuf]) -> WupResult<Self> {
        if paths.is_empty() {
            return Err(WupError::UnsupportedDiscFormat(PathBuf::from("<empty>")));
        }
        let mut parts = Vec::with_capacity(paths.len());
        let mut running = 0u64;
        for p in paths {
            let file = File::open(p)?;
            let len = file.metadata()?.len();
            parts.push(FilePart {
                file: BufReader::new(file),
                start: running,
                len,
            });
            running = running
                .checked_add(len)
                .ok_or_else(|| WupError::DiscTruncated {
                    expected: u64::MAX,
                    actual: running,
                })?;
        }
        Ok(Self {
            parts,
            pos: 0,
            total: running,
        })
    }

    pub(crate) fn total_len(&self) -> u64 {
        self.total
    }
}

impl Read for MultiFileReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.total || out.is_empty() {
            return Ok(0);
        }
        // Binary search the part containing pos.
        let mut idx = 0usize;
        for (i, part) in self.parts.iter().enumerate() {
            if self.pos >= part.start && self.pos < part.start + part.len {
                idx = i;
                break;
            }
        }
        let part = &mut self.parts[idx];
        let local_off = self.pos - part.start;
        part.file.seek(SeekFrom::Start(local_off))?;
        let remaining_in_part = (part.len - local_off) as usize;
        let take = out.len().min(remaining_in_part);
        let n = part.file.read(&mut out[..take])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for MultiFileReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let new = match pos {
            SeekFrom::Start(o) => o,
            SeekFrom::End(o) => (self.total as i64 + o) as u64,
            SeekFrom::Current(o) => (self.pos as i64 + o) as u64,
        };
        self.pos = new;
        Ok(new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn detects_wux_magic_exact_bytes() {
        let mut buf = [0u8; 8];
        buf[..4].copy_from_slice(b"WUX0");
        buf[4..8].copy_from_slice(&[0x2E, 0xD0, 0x99, 0x10]);
        assert!(is_wux_magic(&buf));
    }

    #[test]
    fn rejects_near_miss_wux_magic() {
        let mut buf = [0u8; 8];
        buf[..4].copy_from_slice(b"WUX1");
        buf[4..8].copy_from_slice(&[0x2E, 0xD0, 0x99, 0x10]);
        assert!(!is_wux_magic(&buf));
    }

    #[test]
    fn rejects_short_prefix() {
        assert!(!is_wux_magic(&[0u8; 4]));
    }

    #[test]
    fn open_disc_rejects_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does_not_exist.wud");
        let result = open_disc(&missing);
        assert!(matches!(result, Err(WupError::MissingRequiredFile(_))));
    }

    #[test]
    fn open_disc_rejects_unknown_extension() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(&[0u8; 16]).unwrap();
        let result = open_disc(tmp.path());
        assert!(matches!(result, Err(WupError::UnsupportedDiscFormat(_))));
    }

    #[test]
    fn multifile_reader_concatenates_parts() {
        let mut a = NamedTempFile::new().unwrap();
        let mut b = NamedTempFile::new().unwrap();
        a.write_all(&[1u8; 100]).unwrap();
        b.write_all(&[2u8; 50]).unwrap();
        let paths = vec![a.path().to_path_buf(), b.path().to_path_buf()];
        let mut rd = MultiFileReader::open(&paths).unwrap();
        assert_eq!(rd.total_len(), 150);
        let mut out = [0u8; 150];
        let mut total = 0;
        while total < out.len() {
            let n = rd.read(&mut out[total..]).unwrap();
            if n == 0 {
                break;
            }
            total += n;
        }
        assert_eq!(&out[..100], &[1u8; 100]);
        assert_eq!(&out[100..], &[2u8; 50]);
    }

    #[test]
    fn multifile_reader_seeks_across_parts() {
        let mut a = NamedTempFile::new().unwrap();
        let mut b = NamedTempFile::new().unwrap();
        a.write_all(&[1u8; 100]).unwrap();
        b.write_all(&[2u8; 100]).unwrap();
        let paths = vec![a.path().to_path_buf(), b.path().to_path_buf()];
        let mut rd = MultiFileReader::open(&paths).unwrap();
        rd.seek(SeekFrom::Start(150)).unwrap();
        let mut out = [0u8; 10];
        rd.read_exact(&mut out).unwrap();
        assert_eq!(out, [2u8; 10]);
    }

    #[test]
    fn default_read_bytes_stitches_sector_crossing_reads() {
        let mut disc_bytes = vec![0u8; SECTOR_SIZE * 3];
        for (i, b) in disc_bytes.iter_mut().enumerate() {
            *b = (i & 0xFF) as u8;
        }
        let mut src = InMemoryDisc::new(disc_bytes.clone());
        let mut out = vec![0u8; SECTOR_SIZE + 100];
        src.read_bytes(SECTOR_SIZE as u64 - 50, &mut out).unwrap();
        assert_eq!(
            &out[..],
            &disc_bytes[SECTOR_SIZE - 50..SECTOR_SIZE - 50 + out.len()]
        );
    }
}
