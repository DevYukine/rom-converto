//! WUX (deduplicated Wii U disc) reader.
//!
//! WUX is a thin dedup-only container around a raw WUD:
//!
//! ```text
//! 0x00  u32 LE   magic0          == 0x30585557 ("WUX0")
//! 0x04  u32 LE   magic1          == 0x1099D02E
//! 0x08  u32 LE   sectorSize      commonly 0x8000
//! 0x0C  u32 LE   flags           zero in all known files
//! 0x10  u64 LE   uncompressedSize
//! 0x18  ..0x20   padding
//! 0x20  u32 LE * ceil(uncompressedSize/sectorSize)  index table
//!       (each entry is the physical sector index in the pool below)
//! <align up to sectorSize> .. end of file: physical sector pool
//! ```
//!
//! Sectors with identical content share a physical slot, so reading
//! logical sector `L` resolves to `indexTable[L]` and then to the
//! corresponding fixed-size chunk in the pool.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::nintendo::wup::disc::sector_stream::DiscSectorSource;
use crate::nintendo::wup::error::{WupError, WupResult};

const WUX_MAGIC_0: u32 = 0x3058_5557; // "WUX0" little-endian on disk
const WUX_MAGIC_1: u32 = 0x1099_D02E;
const WUX_HEADER_SIZE: u64 = 0x20;

/// Tiny LRU of recently-decoded physical sectors. Workloads that walk
/// the partition TOC and FSTs hit a small working set, so even two
/// entries is a big win over re-reading from disk.
const CACHE_CAPACITY: usize = 4;

pub struct WuxReader {
    file: BufReader<File>,
    logical_sector_count: u64,
    #[allow(dead_code)]
    physical_sector_count: u64,
    sector_size: u64,
    index_table: Vec<u32>,
    pool_offset: u64,
    cache: Vec<CacheEntry>,
}

struct CacheEntry {
    physical_index: u32,
    bytes: Vec<u8>,
    // Monotonic counter; higher = more recently used.
    last_use: u64,
}

impl WuxReader {
    pub fn open<P: AsRef<Path>>(path: P) -> WupResult<Self> {
        let f = File::open(path.as_ref())?;
        let file_len = f.metadata()?.len();
        let mut file = BufReader::new(f);

        let mut header = [0u8; WUX_HEADER_SIZE as usize];
        file.read_exact(&mut header)?;

        let magic0 = u32::from_le_bytes(header[0..4].try_into().unwrap());
        let magic1 = u32::from_le_bytes(header[4..8].try_into().unwrap());
        if magic0 != WUX_MAGIC_0 || magic1 != WUX_MAGIC_1 {
            return Err(WupError::UnsupportedDiscFormat(path.as_ref().to_path_buf()));
        }

        let sector_size = u32::from_le_bytes(header[8..12].try_into().unwrap()) as u64;
        // flags at [12..16] is ignored.
        let uncompressed_size = u64::from_le_bytes(header[16..24].try_into().unwrap());
        // header[24..32] is padding.

        if sector_size == 0 || sector_size > 0x1000_0000 {
            return Err(WupError::UnsupportedDiscFormat(path.as_ref().to_path_buf()));
        }
        if !uncompressed_size.is_multiple_of(sector_size) {
            return Err(WupError::DiscTruncated {
                expected: uncompressed_size.next_multiple_of(sector_size),
                actual: uncompressed_size,
            });
        }

        let logical_sector_count = uncompressed_size / sector_size;

        // Read the index table.
        let index_bytes_len = logical_sector_count
            .checked_mul(4)
            .ok_or(WupError::InvalidFst)? as usize;
        let mut index_bytes = vec![0u8; index_bytes_len];
        file.read_exact(&mut index_bytes)?;
        let index_table: Vec<u32> = index_bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect();

        // Sector pool starts at the first sector-aligned offset past
        // the index table.
        let pool_offset = (WUX_HEADER_SIZE + index_bytes_len as u64).next_multiple_of(sector_size);

        if pool_offset > file_len {
            return Err(WupError::DiscTruncated {
                expected: pool_offset,
                actual: file_len,
            });
        }
        let pool_bytes = file_len - pool_offset;
        if !pool_bytes.is_multiple_of(sector_size) {
            return Err(WupError::DiscTruncated {
                expected: pool_bytes.next_multiple_of(sector_size),
                actual: pool_bytes,
            });
        }
        let physical_sector_count = pool_bytes / sector_size;

        // Every index must land inside the pool.
        if let Some(&max_idx) = index_table.iter().max()
            && (max_idx as u64) >= physical_sector_count
        {
            return Err(WupError::DiscTruncated {
                expected: (max_idx as u64 + 1) * sector_size,
                actual: pool_bytes,
            });
        }

        Ok(Self {
            file,
            logical_sector_count,
            physical_sector_count,
            sector_size,
            index_table,
            pool_offset,
            cache: Vec::with_capacity(CACHE_CAPACITY),
        })
    }

    fn load_physical_sector(&mut self, physical_index: u32) -> WupResult<&[u8]> {
        // Touch existing entry.
        if let Some(pos) = self
            .cache
            .iter()
            .position(|e| e.physical_index == physical_index)
        {
            let stamp = self.next_stamp();
            self.cache[pos].last_use = stamp;
            return Ok(&self.cache[pos].bytes);
        }

        // Load fresh, evicting oldest if at capacity.
        let offset = self.pool_offset + physical_index as u64 * self.sector_size;
        self.file.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0u8; self.sector_size as usize];
        self.file.read_exact(&mut bytes)?;

        if self.cache.len() == CACHE_CAPACITY {
            let oldest = self
                .cache
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.last_use)
                .map(|(i, _)| i)
                .unwrap();
            self.cache.swap_remove(oldest);
        }
        let stamp = self.next_stamp();
        self.cache.push(CacheEntry {
            physical_index,
            bytes,
            last_use: stamp,
        });
        Ok(&self.cache.last().unwrap().bytes)
    }

    fn next_stamp(&self) -> u64 {
        // Monotonic from the current max; cheap since cache is tiny.
        self.cache.iter().map(|e| e.last_use).max().unwrap_or(0) + 1
    }

    #[cfg(test)]
    pub(crate) fn physical_sector_count(&self) -> u64 {
        self.physical_sector_count
    }
}

impl DiscSectorSource for WuxReader {
    fn total_sectors(&self) -> u64 {
        self.logical_sector_count
    }

    fn read_sector(&mut self, sector_index: u64, dst: &mut [u8]) -> WupResult<()> {
        assert_eq!(
            dst.len() as u64,
            self.sector_size,
            "sector buffer must match container sector size"
        );
        if sector_index >= self.logical_sector_count {
            return Err(WupError::DiscTruncated {
                expected: (sector_index + 1) * self.sector_size,
                actual: self.logical_sector_count * self.sector_size,
            });
        }
        let physical = self.index_table[sector_index as usize];
        let bytes = self.load_physical_sector(physical)?;
        dst.copy_from_slice(bytes);
        Ok(())
    }
}

/// Test-only helper that builds a valid WUX file from a sequence of
/// logical sectors. Deduplicates identical sectors exactly the way a
/// real writer would, so dedup behaviour is also covered by tests.
#[cfg(test)]
pub(crate) fn write_wux_for_test(
    path: &Path,
    logical_sectors: &[[u8; crate::nintendo::wup::disc::sector_stream::SECTOR_SIZE]],
) -> std::io::Result<()> {
    use crate::nintendo::wup::disc::sector_stream::SECTOR_SIZE;
    use std::collections::HashMap;
    use std::io::Write;
    let mut file = File::create(path)?;
    let uncompressed = (logical_sectors.len() * SECTOR_SIZE) as u64;
    let mut header = vec![0u8; WUX_HEADER_SIZE as usize];
    header[0..4].copy_from_slice(&WUX_MAGIC_0.to_le_bytes());
    header[4..8].copy_from_slice(&WUX_MAGIC_1.to_le_bytes());
    header[8..12].copy_from_slice(&(SECTOR_SIZE as u32).to_le_bytes());
    header[12..16].copy_from_slice(&0u32.to_le_bytes());
    header[16..24].copy_from_slice(&uncompressed.to_le_bytes());
    file.write_all(&header)?;
    // Build index + physical pool.
    let mut dedup: HashMap<[u8; SECTOR_SIZE], u32> = HashMap::new();
    let mut pool: Vec<[u8; SECTOR_SIZE]> = Vec::new();
    let mut index: Vec<u32> = Vec::with_capacity(logical_sectors.len());
    for sector in logical_sectors {
        if let Some(&existing) = dedup.get(sector) {
            index.push(existing);
        } else {
            let new_idx = pool.len() as u32;
            dedup.insert(*sector, new_idx);
            pool.push(*sector);
            index.push(new_idx);
        }
    }
    for entry in &index {
        file.write_all(&entry.to_le_bytes())?;
    }
    // Pad to sector boundary.
    let cur = WUX_HEADER_SIZE + (index.len() * 4) as u64;
    let aligned = cur.next_multiple_of(SECTOR_SIZE as u64);
    let pad = (aligned - cur) as usize;
    if pad > 0 {
        file.write_all(&vec![0u8; pad])?;
    }
    for sector in &pool {
        file.write_all(sector)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::wup::disc::sector_stream::SECTOR_SIZE;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn rejects_bad_magic() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(&[0xFF; 0x20]).unwrap();
        tmp.flush().unwrap();
        let result = WuxReader::open(tmp.path());
        assert!(matches!(result, Err(WupError::UnsupportedDiscFormat(_))));
    }

    #[test]
    fn reads_back_logical_sectors_in_order() {
        let tmp = NamedTempFile::new().unwrap();
        let mut sectors: Vec<[u8; SECTOR_SIZE]> = Vec::new();
        for v in [0x11u8, 0x22, 0x33, 0x44] {
            sectors.push([v; SECTOR_SIZE]);
        }
        write_wux_for_test(tmp.path(), &sectors).unwrap();
        let mut rdr = WuxReader::open(tmp.path()).unwrap();
        assert_eq!(rdr.total_sectors(), 4);
        let mut buf = vec![0u8; SECTOR_SIZE];
        for (i, v) in [0x11u8, 0x22, 0x33, 0x44].iter().enumerate() {
            rdr.read_sector(i as u64, &mut buf).unwrap();
            assert!(buf.iter().all(|&b| b == *v));
        }
    }

    #[test]
    fn deduplicates_identical_sectors() {
        let tmp = NamedTempFile::new().unwrap();
        let sectors: Vec<[u8; SECTOR_SIZE]> = vec![
            [0xAA; SECTOR_SIZE],
            [0xBB; SECTOR_SIZE],
            [0xAA; SECTOR_SIZE],
            [0xAA; SECTOR_SIZE],
        ];
        write_wux_for_test(tmp.path(), &sectors).unwrap();
        let rdr = WuxReader::open(tmp.path()).unwrap();
        assert_eq!(rdr.total_sectors(), 4);
        assert_eq!(rdr.physical_sector_count(), 2);
    }

    #[test]
    fn lru_cache_serves_repeat_reads() {
        let tmp = NamedTempFile::new().unwrap();
        let sectors: Vec<[u8; SECTOR_SIZE]> = vec![[0xEE; SECTOR_SIZE], [0xFF; SECTOR_SIZE]];
        write_wux_for_test(tmp.path(), &sectors).unwrap();
        let mut rdr = WuxReader::open(tmp.path()).unwrap();
        let mut buf = vec![0u8; SECTOR_SIZE];
        rdr.read_sector(0, &mut buf).unwrap();
        rdr.read_sector(0, &mut buf).unwrap();
        rdr.read_sector(0, &mut buf).unwrap();
        assert!(buf.iter().all(|&b| b == 0xEE));
        assert!(rdr.cache.iter().any(|e| e.last_use >= 3));
    }

    #[test]
    fn out_of_bounds_read_errors() {
        let tmp = NamedTempFile::new().unwrap();
        let sectors: Vec<[u8; SECTOR_SIZE]> = vec![[0x55; SECTOR_SIZE]];
        write_wux_for_test(tmp.path(), &sectors).unwrap();
        let mut rdr = WuxReader::open(tmp.path()).unwrap();
        let mut buf = vec![0u8; SECTOR_SIZE];
        let result = rdr.read_sector(2, &mut buf);
        assert!(matches!(result, Err(WupError::DiscTruncated { .. })));
    }
}
