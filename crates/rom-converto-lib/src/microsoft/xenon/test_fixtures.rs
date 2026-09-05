//! Synthetic XDVDFS images shared by the xenon tests: a sparse-backed
//! disk so a test can probe a real (tens of megabytes in) partition base
//! without allocating one, plus the descriptor/dirent encoders that fill
//! it.

use std::io::{Read, Seek, SeekFrom};

use crate::microsoft::xdvdfs::{
    ATTR_DIRECTORY, SECTOR_SIZE, VOLUME_DESCRIPTOR_SECTOR, VOLUME_MAGIC,
};

/// Synthetic disk backed by sparse regions; holes read as zero.
pub(crate) struct SparseDisk {
    regions: Vec<(u64, Vec<u8>)>,
    pos: u64,
    len: u64,
}

impl SparseDisk {
    pub(crate) fn new() -> Self {
        Self {
            regions: Vec::new(),
            pos: 0,
            len: 0,
        }
    }

    pub(crate) fn put(&mut self, offset: u64, bytes: Vec<u8>) {
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

pub(crate) fn descriptor(root_sector: u32, root_size: u32) -> Vec<u8> {
    let mut d = vec![0u8; 0x800];
    d[0..20].copy_from_slice(VOLUME_MAGIC);
    d[0x14..0x18].copy_from_slice(&root_sector.to_le_bytes());
    d[0x18..0x1C].copy_from_slice(&root_size.to_le_bytes());
    d[0x7EC..0x800].copy_from_slice(VOLUME_MAGIC);
    d
}

pub(crate) fn dirent(
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
pub(crate) fn build_x360_iso() -> (SparseDisk, Vec<(&'static str, Vec<u8>)>) {
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
