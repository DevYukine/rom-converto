//! Pack an Xbox 360 XDVDFS ISO, or an already-extracted game directory,
//! into a ZArchive (`.zar`). Content always lands at the archive root
//! (`default.xex` alongside the game's folders), matching what Xenia
//! expects to mount.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::microsoft::xdvdfs::{
    DirEntry, X360_PROBE_BASES, XdvdfsVolume, data_offset, walk_dir_tables,
};
use crate::microsoft::zar::{ZarSummary, ZarWriter};
use crate::util::CancelToken;
use crate::util::worker_pool::parallelism;

use super::error::{XenonError, XenonResult};

/// Read buffer used while streaming file payloads into the writer.
const COPY_BUF_SIZE: usize = 1 << 20;

/// Outcome of a pack run: the writer's own summary plus whether the tree
/// contains a root-level `default.xex`.
pub struct XenonPackSummary {
    pub zar: ZarSummary,
    pub has_default_xex: bool,
}

/// True if `name` at the archive root is `default.xex`, ASCII
/// case-insensitive (Xenia's own check).
fn is_default_xex(name: &str) -> bool {
    name.eq_ignore_ascii_case("default.xex")
}

fn archive_path(parent: &[String], name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{}/{name}", parent.join("/"))
    }
}

/// Sum of every file's byte size in `input`, used as the progress total.
/// Dispatches on whether `input` is a directory or an ISO file.
pub fn total_input_bytes(input: &Path) -> XenonResult<u64> {
    if input.is_dir() {
        let mut total = 0u64;
        for (_, path, is_dir) in walk_fs_dir(input)? {
            if !is_dir {
                total += std::fs::metadata(&path)?.len();
            }
        }
        Ok(total)
    } else {
        let mut file = std::fs::File::open(input)?;
        let volume = XdvdfsVolume::probe(&mut file, &X360_PROBE_BASES)?;
        let mut total = 0u64;
        walk_dir_tables(&mut file, &volume, |_, entry| {
            if !entry.is_directory() {
                total += entry.size as u64;
            }
            Ok(())
        })?;
        Ok(total)
    }
}

/// Open `output` and pack `input` (an ISO file or a directory) into it.
pub fn pack_blocking(
    input: &Path,
    output: &Path,
    bytes_done: Arc<AtomicU64>,
    cancel: &CancelToken,
) -> XenonResult<XenonPackSummary> {
    let writer = std::io::BufWriter::with_capacity(4 * 1024 * 1024, std::fs::File::create(output)?);
    if input.is_dir() {
        pack_dir(input, writer, &bytes_done, cancel)
    } else {
        pack_iso(std::fs::File::open(input)?, writer, &bytes_done, cancel)
    }
}

/// Pack the XDVDFS filesystem found in `reader` into `writer`.
fn pack_iso<R: Read + Seek, W: Write>(
    mut reader: R,
    writer: W,
    bytes_done: &AtomicU64,
    cancel: &CancelToken,
) -> XenonResult<XenonPackSummary> {
    let volume = XdvdfsVolume::probe(&mut reader, &X360_PROBE_BASES)?;

    // Dirtabs are small; buffer the whole listing before touching file
    // payloads. `walk_dir_tables` visits a directory before any of its
    // children, so this order already puts parent dirs first.
    let mut listing: Vec<(Vec<String>, DirEntry)> = Vec::new();
    walk_dir_tables(&mut reader, &volume, |path, entry| {
        listing.push((path.to_vec(), entry.clone()));
        Ok(())
    })?;

    let has_default_xex = listing.iter().any(|(parent, entry)| {
        parent.is_empty() && !entry.is_directory() && is_default_xex(&entry.name_str())
    });

    let mut zar = ZarWriter::new(writer, parallelism())?;
    let mut buf = vec![0u8; COPY_BUF_SIZE];
    for (parent, entry) in &listing {
        if cancel.is_cancelled() {
            return Err(XenonError::Cancelled);
        }
        let path = archive_path(parent, &entry.name_str());
        if entry.is_directory() {
            zar.make_dir(&path, true)?;
            continue;
        }
        zar.start_file(&path)?;
        reader.seek(SeekFrom::Start(data_offset(&volume, entry.start_sector)))?;
        let mut remaining = entry.size as u64;
        while remaining > 0 {
            if cancel.is_cancelled() {
                return Err(XenonError::Cancelled);
            }
            let take = (buf.len() as u64).min(remaining) as usize;
            reader.read_exact(&mut buf[..take])?;
            zar.append_data(&buf[..take])?;
            bytes_done.fetch_add(take as u64, Ordering::Relaxed);
            remaining -= take as u64;
        }
    }

    let summary = zar.finish()?;
    Ok(XenonPackSummary {
        zar: summary,
        has_default_xex,
    })
}

/// Pack the directory tree rooted at `root` into `writer`. Archive paths
/// are forward-slash, relative to `root`, so the tree lands at the
/// archive root exactly as it sits on disk.
fn pack_dir<W: Write>(
    root: &Path,
    writer: W,
    bytes_done: &AtomicU64,
    cancel: &CancelToken,
) -> XenonResult<XenonPackSummary> {
    let listing = walk_fs_dir(root)?;

    let has_default_xex = listing
        .iter()
        .any(|(rel, _, is_dir)| !is_dir && !rel.contains('/') && is_default_xex(rel));

    let mut zar = ZarWriter::new(writer, parallelism())?;
    let mut buf = vec![0u8; COPY_BUF_SIZE];
    for (rel, path, is_dir) in &listing {
        if cancel.is_cancelled() {
            return Err(XenonError::Cancelled);
        }
        if *is_dir {
            zar.make_dir(rel, true)?;
            continue;
        }
        zar.start_file(rel)?;
        let mut file = std::fs::File::open(path)?;
        loop {
            if cancel.is_cancelled() {
                return Err(XenonError::Cancelled);
            }
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            zar.append_data(&buf[..n])?;
            bytes_done.fetch_add(n as u64, Ordering::Relaxed);
        }
    }

    let summary = zar.finish()?;
    Ok(XenonPackSummary {
        zar: summary,
        has_default_xex,
    })
}

/// Recursively lists `root`, returning `(archive-relative forward-slash
/// path, absolute path, is_dir)` for every entry. Each directory is
/// listed before its children, so callers get parents-before-children
/// order for free.
fn walk_fs_dir(root: &Path) -> std::io::Result<Vec<(String, PathBuf, bool)>> {
    let mut out = Vec::new();
    walk_fs_dir_into(root, root, &mut out)?;
    Ok(out)
}

fn walk_fs_dir_into(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, PathBuf, bool)>,
) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .expect("entry is always under root")
            .to_string_lossy()
            .replace('\\', "/");
        let is_dir = entry.file_type()?.is_dir();
        out.push((rel, path.clone(), is_dir));
        if is_dir {
            walk_fs_dir_into(root, &path, out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::microsoft::xdvdfs::{
        ATTR_DIRECTORY, SECTOR_SIZE, VOLUME_DESCRIPTOR_SECTOR, VOLUME_MAGIC,
    };
    use crate::microsoft::zar::ZarReader;
    use std::io::Cursor;

    /// Synthetic disk backed by sparse regions, mirroring
    /// `xdvdfs::tests::SparseDisk`, so tests can probe a real X360 base
    /// offset without allocating tens of megabytes.
    struct SparseDisk {
        regions: Vec<(u64, Vec<u8>)>,
        pos: u64,
        len: u64,
    }

    impl SparseDisk {
        fn new() -> Self {
            Self {
                regions: Vec::new(),
                pos: 0,
                len: 0,
            }
        }

        fn put(&mut self, offset: u64, bytes: Vec<u8>) {
            self.len = self.len.max(offset + bytes.len() as u64);
            self.regions.push((offset, bytes));
        }
    }

    impl Read for SparseDisk {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pos >= self.len {
                return Ok(0);
            }
            let n = buf.len().min((self.len - self.pos) as usize);
            for (i, b) in buf[..n].iter_mut().enumerate() {
                let abs = self.pos + i as u64;
                *b = self
                    .regions
                    .iter()
                    .find_map(|(start, data)| {
                        if abs >= *start && abs < *start + data.len() as u64 {
                            Some(data[(abs - start) as usize])
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);
            }
            self.pos += n as u64;
            Ok(n)
        }
    }

    impl Seek for SparseDisk {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            let new_pos = match pos {
                SeekFrom::Start(p) => p as i64,
                SeekFrom::End(p) => self.len as i64 + p,
                SeekFrom::Current(p) => self.pos as i64 + p,
            };
            self.pos = new_pos as u64;
            Ok(self.pos)
        }
    }

    fn descriptor(root_sector: u32, root_size: u32) -> Vec<u8> {
        let mut d = vec![0u8; 0x800];
        d[0..20].copy_from_slice(VOLUME_MAGIC);
        d[0x14..0x18].copy_from_slice(&root_sector.to_le_bytes());
        d[0x18..0x1C].copy_from_slice(&root_size.to_le_bytes());
        d[0x7EC..0x800].copy_from_slice(VOLUME_MAGIC);
        d
    }

    fn dirent(
        left: u16,
        right: u16,
        start_sector: u32,
        size: u32,
        attrs: u8,
        name: &[u8],
    ) -> Vec<u8> {
        let mut e = Vec::with_capacity(14 + name.len());
        e.extend_from_slice(&left.to_le_bytes());
        e.extend_from_slice(&right.to_le_bytes());
        e.extend_from_slice(&start_sector.to_le_bytes());
        e.extend_from_slice(&size.to_le_bytes());
        e.push(attrs);
        e.push(name.len() as u8);
        e.extend_from_slice(name);
        e
    }

    /// Builds a synthetic XGD3-based X360 image (base `0x0208_0000`)
    /// containing `default.xex` at the root, a nested subdirectory, and a
    /// file spanning more than one ZArchive compression block. Returns
    /// the disk plus `(path, contents)` for every file.
    fn build_x360_iso() -> (SparseDisk, Vec<(&'static str, Vec<u8>)>) {
        let base = 0x0208_0000u64;
        let root_sector = 4096u32; // relative to base; root dirtab spans 2 sectors
        let sub_sector = 4098u32;

        let xex_data: Vec<u8> = (0..500u32).map(|i| i as u8).collect();
        let big_len = 3 * crate::microsoft::zar::COMPRESSED_BLOCK_SIZE as u32 + 123;
        let big_data: Vec<u8> = (0..big_len).map(|i| (i % 251) as u8).collect();

        // File sector addresses are made up (any base works for a
        // SparseDisk); keep them well past the dirtabs.
        let xex_sector = 5000u32;
        let big_sector = 5100u32;

        let root_size = 2 * SECTOR_SIZE as u32; // two entries, one per sector
        let mut root = vec![0xFFu8; root_size as usize];
        // right = 512 -> child offset 512*4 = 2048, where GAME's dirent sits.
        let xex_entry = dirent(0, 512, xex_sector, xex_data.len() as u32, 0, b"DEFAULT.XEX");
        root[0..xex_entry.len()].copy_from_slice(&xex_entry);
        let sub_entry = dirent(
            0,
            0,
            sub_sector,
            SECTOR_SIZE as u32,
            ATTR_DIRECTORY,
            b"GAME",
        );
        root[2048..2048 + sub_entry.len()].copy_from_slice(&sub_entry);

        let mut sub = vec![0xFFu8; SECTOR_SIZE as usize];
        let big_entry = dirent(0, 0, big_sector, big_data.len() as u32, 0, b"BIG.BIN");
        sub[0..big_entry.len()].copy_from_slice(&big_entry);

        let mut disk = SparseDisk::new();
        disk.put(
            base + VOLUME_DESCRIPTOR_SECTOR as u64 * SECTOR_SIZE,
            descriptor(root_sector, root_size),
        );
        disk.put(base + root_sector as u64 * SECTOR_SIZE, root);
        disk.put(base + sub_sector as u64 * SECTOR_SIZE, sub);
        disk.put(base + xex_sector as u64 * SECTOR_SIZE, xex_data.clone());
        disk.put(base + big_sector as u64 * SECTOR_SIZE, big_data.clone());

        (
            disk,
            vec![("DEFAULT.XEX", xex_data), ("GAME/BIG.BIN", big_data)],
        )
    }

    #[test]
    fn iso_pack_round_trips_through_the_zar_reader() {
        let (disk, files) = build_x360_iso();
        let mut buf = Vec::new();
        let bytes_done = AtomicU64::new(0);
        let cancel = CancelToken::new();
        let summary = pack_iso(disk, std::io::Cursor::new(&mut buf), &bytes_done, &cancel).unwrap();
        assert!(summary.has_default_xex);

        let mut reader = ZarReader::open(Cursor::new(buf)).unwrap();
        reader.verify_integrity(&CancelToken::new()).unwrap();
        for (path, data) in &files {
            let index = reader.lookup(path).unwrap();
            let mut out = Vec::new();
            reader.read_file(index, &mut out).unwrap();
            assert_eq!(&out, data, "contents differ for {path}");
        }
        assert!(bytes_done.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn dir_pack_puts_content_at_the_archive_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("default.xex"), b"xex-bytes").unwrap();
        std::fs::create_dir(dir.path().join("data")).unwrap();
        std::fs::write(dir.path().join("data/save.bin"), b"save-bytes").unwrap();

        let mut buf = Vec::new();
        let bytes_done = AtomicU64::new(0);
        let cancel = CancelToken::new();
        let summary = pack_dir(dir.path(), Cursor::new(&mut buf), &bytes_done, &cancel).unwrap();
        assert!(summary.has_default_xex);

        let reader = ZarReader::open(Cursor::new(buf)).unwrap();
        let root_entries: Vec<String> = reader
            .entries()
            .unwrap()
            .into_iter()
            .filter(|e| !e.path.contains('/'))
            .map(|e| e.path)
            .collect();
        assert!(root_entries.contains(&"default.xex".to_string()));
        assert!(root_entries.contains(&"data".to_string()));
    }

    #[test]
    fn missing_default_xex_is_reported_but_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("readme.txt"), b"no game here").unwrap();

        let mut buf = Vec::new();
        let bytes_done = AtomicU64::new(0);
        let cancel = CancelToken::new();
        let summary = pack_dir(dir.path(), Cursor::new(&mut buf), &bytes_done, &cancel).unwrap();
        assert!(!summary.has_default_xex);
    }
}
