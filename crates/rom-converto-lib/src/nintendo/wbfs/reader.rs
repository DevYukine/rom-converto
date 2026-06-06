//! `Read + Seek` view over a WBFS container that reconstructs the
//! logical disc on the fly. Each read resolves the current logical
//! block through the wlba table to a physical block in the file;
//! scrubbed blocks (table entry 0) read back as zeros.
//!
//! Split containers are supported: a `.wbfs` and its `.wbf1` .. `.wbf9`
//! siblings (how FAT32 drives store discs over the 4 GiB file limit)
//! are concatenated into one physical address space, matching Dolphin's
//! `WbfsBlob`. Block 0, with all the metadata, always lives in part 0.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::error::{WbfsError, WbfsResult};
use super::format::{
    DISC_HEADER_COPY_SIZE, WBFS_MAGIC, disc_info_size, reconstruct_disc_size, wbfs_sectors_per_disc,
};

const WBFS_HEAD_SIZE: usize = 12;
const MIN_HD_SECTOR_SHIFT: u8 = 9;
const MAX_HD_SECTOR_SHIFT: u8 = 13;
const MAX_SPLIT_PARTS: u8 = 10;

/// One file in a (possibly split) WBFS container, placed at `base` in
/// the concatenated physical address space.
struct Part {
    file: File,
    base: u64,
    len: u64,
}

pub struct WbfsReader {
    parts: Vec<Part>,
    wbfs_sec_sz: u64,
    wlba: Vec<u16>,
    disc_size: u64,
    pos: u64,
}

impl WbfsReader {
    pub fn open(path: &Path) -> WbfsResult<Self> {
        let mut parts = open_parts(path)?;

        let mut head = [0u8; WBFS_HEAD_SIZE];
        read_at(&mut parts, 0, &mut head)?;
        let magic: [u8; 4] = head[0..4].try_into().unwrap();
        if magic != WBFS_MAGIC {
            return Err(WbfsError::InvalidMagic(magic));
        }
        let hd_sec_sz_s = head[8];
        let wbfs_sec_sz_s = head[9];
        if !(MIN_HD_SECTOR_SHIFT..=MAX_HD_SECTOR_SHIFT).contains(&hd_sec_sz_s) {
            return Err(WbfsError::UnsupportedHdSectorSize(hd_sec_sz_s));
        }
        if wbfs_sec_sz_s < super::format::WII_SECTOR_SIZE_SHIFT {
            return Err(WbfsError::UnsupportedWbfsSectorSize(wbfs_sec_sz_s));
        }

        let hd_sec_sz = 1u64 << hd_sec_sz_s;
        let wbfs_sec_sz = 1u64 << wbfs_sec_sz_s;
        let entries = wbfs_sectors_per_disc(wbfs_sec_sz_s) as usize;

        // The disc slot table fills the rest of HD sector 0. The first
        // used slot identifies the (single) disc in a `.wbfs` file.
        let table_len = (hd_sec_sz as usize) - WBFS_HEAD_SIZE;
        let mut slot_table = vec![0u8; table_len];
        read_at(&mut parts, WBFS_HEAD_SIZE as u64, &mut slot_table)?;
        let slot = slot_table
            .iter()
            .position(|&b| b != 0)
            .ok_or(WbfsError::NoDiscs)?;

        let info_off = hd_sec_sz + slot as u64 * disc_info_size(wbfs_sec_sz_s, hd_sec_sz);
        let mut disc_header = [0u8; DISC_HEADER_COPY_SIZE];
        read_at(&mut parts, info_off, &mut disc_header)?;

        let mut wlba_bytes = vec![0u8; entries * 2];
        read_at(
            &mut parts,
            info_off + DISC_HEADER_COPY_SIZE as u64,
            &mut wlba_bytes,
        )?;
        let wlba: Vec<u16> = wlba_bytes
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();

        let used_size = match wlba.iter().rposition(|&v| v != 0) {
            Some(idx) => (idx as u64 + 1) * wbfs_sec_sz,
            None => 0,
        };
        let disc_size = reconstruct_disc_size(&disc_header, used_size);

        Ok(Self {
            parts,
            wbfs_sec_sz,
            wlba,
            disc_size,
            pos: 0,
        })
    }

    pub fn disc_size(&self) -> u64 {
        self.disc_size
    }

    fn read_some(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.disc_size {
            return Ok(0);
        }
        let want = (buf.len() as u64).min(self.disc_size - self.pos) as usize;
        let block_idx = (self.pos / self.wbfs_sec_sz) as usize;
        let in_block = self.pos % self.wbfs_sec_sz;
        // Never serve across a block boundary in a single call; the
        // next block may map to a non-contiguous physical location.
        let take = (want as u64).min(self.wbfs_sec_sz - in_block) as usize;

        let phys = self.wlba.get(block_idx).copied().unwrap_or(0);
        if phys == 0 {
            for slot in &mut buf[..take] {
                *slot = 0;
            }
        } else {
            let off = phys as u64 * self.wbfs_sec_sz + in_block;
            read_at(&mut self.parts, off, &mut buf[..take])?;
        }
        self.pos += take as u64;
        Ok(take)
    }
}

/// Open the base file plus any `.wbf1` .. `.wbf9` siblings, recording
/// each part's offset in the concatenated address space.
fn open_parts(path: &Path) -> WbfsResult<Vec<Part>> {
    let mut parts = Vec::new();
    let mut base = 0u64;

    let file = File::open(path)?;
    let len = file.metadata()?.len();
    parts.push(Part { file, base, len });
    base += len;

    for idx in 1..MAX_SPLIT_PARTS {
        let Some(sibling) = sibling_path(path, idx) else {
            break;
        };
        match File::open(&sibling) {
            Ok(file) => {
                let len = file.metadata()?.len();
                parts.push(Part { file, base, len });
                base += len;
            }
            Err(_) => break,
        }
    }
    Ok(parts)
}

/// The split-file naming rule replaces the last character of the path
/// with `'0' + idx` (Dolphin `WbfsBlob`), so `game.wbfs` -> `game.wbf1`.
fn sibling_path(path: &Path, idx: u8) -> Option<PathBuf> {
    let s = path.to_str()?;
    let last = *s.as_bytes().last()?;
    if !last.is_ascii() {
        return None;
    }
    let mut bytes = s.as_bytes().to_vec();
    *bytes.last_mut()? = b'0' + idx;
    String::from_utf8(bytes).ok().map(PathBuf::from)
}

/// Read `buf.len()` bytes starting at concatenated offset `off`,
/// crossing part boundaries as needed.
fn read_at(parts: &mut [Part], mut off: u64, buf: &mut [u8]) -> io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        let part = parts
            .iter_mut()
            .find(|p| off >= p.base && off < p.base + p.len)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "wbfs read past end of split set",
                )
            })?;
        let in_part = off - part.base;
        let avail = (part.len - in_part) as usize;
        let n = (buf.len() - done).min(avail);
        part.file.seek(SeekFrom::Start(in_part))?;
        part.file.read_exact(&mut buf[done..done + n])?;
        done += n;
        off += n as u64;
    }
    Ok(())
}

impl Read for WbfsReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_some(buf)
    }
}

impl Seek for WbfsReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let new_pos: i128 = match from {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
            SeekFrom::End(d) => self.disc_size as i128 + d as i128,
        };
        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek to negative offset",
            ));
        }
        self.pos = new_pos as u64;
        Ok(self.pos)
    }
}
