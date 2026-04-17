//! Raw WUD (and split `game_part<N>.wud`) disc image reader.
//!
//! A raw WUD is a byte-for-byte copy of the optical disc. Retail
//! single-layer images are exactly `0x5D3A00000` bytes; some tools
//! split them into `game_part1.wud` ... `game_part<N>.wud` at 2 GiB
//! boundaries. Both variants present the same logical stream.

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use crate::nintendo::wup::disc::sector_stream::{
    DiscSectorSource, MultiFileReader, SECTOR_SIZE, WUD_SINGLE_LAYER_SIZE,
};
use crate::nintendo::wup::error::{WupError, WupResult};

/// Raw WUD reader. Owns the open file handle(s) and serves sector
/// reads directly.
pub struct WudReader {
    inner: MultiFileReader,
    sector_count: u64,
}

impl WudReader {
    /// Open a WUD image from one or more part files in order. Pass a
    /// single-element slice for a non-split image.
    pub fn open_parts(parts: Vec<PathBuf>) -> WupResult<Self> {
        let inner = MultiFileReader::open(&parts)?;
        let total = inner.total_len();
        if total == 0 || !total.is_multiple_of(SECTOR_SIZE as u64) {
            return Err(WupError::DiscTruncated {
                expected: WUD_SINGLE_LAYER_SIZE,
                actual: total,
            });
        }
        // We do not hard-reject non-retail sizes since homebrew and
        // pre-production discs exist. We only require sector alignment.
        Ok(Self {
            inner,
            sector_count: total / SECTOR_SIZE as u64,
        })
    }
}

impl DiscSectorSource for WudReader {
    fn total_sectors(&self) -> u64 {
        self.sector_count
    }

    fn read_sector(&mut self, sector_index: u64, dst: &mut [u8]) -> WupResult<()> {
        assert_eq!(
            dst.len(),
            SECTOR_SIZE,
            "sector buffer must be {} bytes",
            SECTOR_SIZE
        );
        if sector_index >= self.sector_count {
            return Err(WupError::DiscTruncated {
                expected: (sector_index + 1) * SECTOR_SIZE as u64,
                actual: self.sector_count * SECTOR_SIZE as u64,
            });
        }
        let offset = sector_index * SECTOR_SIZE as u64;
        self.inner.seek(SeekFrom::Start(offset))?;
        self.inner.read_exact(dst)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_sectors(file: &mut NamedTempFile, pattern: u8, count: u64) {
        let buf = vec![pattern; SECTOR_SIZE];
        for _ in 0..count {
            file.write_all(&buf).unwrap();
        }
    }

    #[test]
    fn single_file_wud_reads_back_sectors() {
        let mut f = NamedTempFile::new().unwrap();
        write_sectors(&mut f, 0xAA, 1);
        write_sectors(&mut f, 0xBB, 1);
        let mut rdr = WudReader::open_parts(vec![f.path().to_path_buf()]).unwrap();
        assert_eq!(rdr.total_sectors(), 2);
        let mut buf = vec![0u8; SECTOR_SIZE];
        rdr.read_sector(0, &mut buf).unwrap();
        assert!(buf.iter().all(|&b| b == 0xAA));
        rdr.read_sector(1, &mut buf).unwrap();
        assert!(buf.iter().all(|&b| b == 0xBB));
    }

    #[test]
    fn split_wud_chains_parts_transparently() {
        let mut a = NamedTempFile::new().unwrap();
        let mut b = NamedTempFile::new().unwrap();
        write_sectors(&mut a, 0x11, 2);
        write_sectors(&mut b, 0x22, 3);
        let mut rdr =
            WudReader::open_parts(vec![a.path().to_path_buf(), b.path().to_path_buf()]).unwrap();
        assert_eq!(rdr.total_sectors(), 5);
        let mut buf = vec![0u8; SECTOR_SIZE];
        rdr.read_sector(1, &mut buf).unwrap();
        assert!(buf.iter().all(|&b| b == 0x11));
        rdr.read_sector(4, &mut buf).unwrap();
        assert!(buf.iter().all(|&b| b == 0x22));
    }

    #[test]
    fn misaligned_size_is_rejected() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(&[0u8; SECTOR_SIZE + 1]).unwrap();
        let result = WudReader::open_parts(vec![f.path().to_path_buf()]);
        assert!(matches!(result, Err(WupError::DiscTruncated { .. })));
    }

    #[test]
    fn reads_beyond_end_error_out() {
        let mut f = NamedTempFile::new().unwrap();
        write_sectors(&mut f, 0xCC, 1);
        let mut rdr = WudReader::open_parts(vec![f.path().to_path_buf()]).unwrap();
        let mut buf = vec![0u8; SECTOR_SIZE];
        let result = rdr.read_sector(1, &mut buf);
        assert!(matches!(result, Err(WupError::DiscTruncated { .. })));
    }
}
