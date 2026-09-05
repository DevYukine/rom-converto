//! GoD block/part geometry, and the pre-pass over the source ISO that
//! decides how much of the game partition actually has to be written.

use std::io::{Read, Seek, SeekFrom};

use crate::microsoft::xdvdfs::{
    SECTOR_SIZE, X360_PROBE_BASES, XdvdfsVolume, data_offset, walk_dir_tables,
};

use super::error::{GodError, GodResult};
use super::xex::{ExecutionId, parse_execution_id};

/// Data is hashed and stored in 4 KiB blocks.
pub const BLOCK_SIZE: u64 = 0x1000;

/// Data blocks covered by one sub hash list.
pub const BLOCKS_PER_SUBPART: u64 = 0xCC;

/// Data bytes covered by one sub hash list.
pub const SUBPART_SIZE: u64 = BLOCK_SIZE * BLOCKS_PER_SUBPART;

/// Sub hash lists covered by one part file's master hash list.
pub const SUBPARTS_PER_PART: u64 = 0xCB;

/// Data blocks in a full part file.
pub const BLOCKS_PER_PART: u64 = BLOCKS_PER_SUBPART * SUBPARTS_PER_PART;

/// Blocks a full part file occupies on disk: its data blocks plus one
/// block per sub hash list plus the master hash list.
pub const BLOCKS_PER_PART_FILE: u64 = BLOCKS_PER_PART + SUBPARTS_PER_PART + 1;

/// Smallest partition-relative size a container can have: everything up
/// to and including the volume descriptor sector is always written even
/// when the filesystem itself ends earlier.
const MIN_DATA_SIZE: u64 = 0x21 * 0x800;

const DEFAULT_XEX: &str = "default.xex";

/// What the source ISO's filesystem implies about the container to
/// write: where the game partition starts, how far into it real data
/// reaches, and the identity the header is stamped with.
#[derive(Debug, Clone)]
pub struct GodScan {
    /// Partition base offset within the source file.
    pub base: u64,
    /// Partition-relative trim end: bytes of the partition to write.
    pub data_size: u64,
    pub block_count: u64,
    pub part_count: u64,
    pub execution: ExecutionId,
    /// Title name carried in the executable's XDBF resource, when it has one.
    pub title_name: Option<String>,
}

/// Walks the source filesystem to find the trim end and the root
/// `default.xex`, then parses that executable's execution id.
pub fn scan<R: Read + Seek>(reader: &mut R) -> GodResult<GodScan> {
    let volume = XdvdfsVolume::probe(reader, &X360_PROBE_BASES)?;

    let mut data_size =
        MIN_DATA_SIZE.max(volume.root_sector as u64 * SECTOR_SIZE + volume.root_size as u64);
    let mut xex = None;
    walk_dir_tables(reader, &volume, |parent, entry| {
        data_size = data_size.max(entry.start_sector as u64 * SECTOR_SIZE + entry.size as u64);
        if parent.is_empty()
            && !entry.is_directory()
            && entry.name_str().eq_ignore_ascii_case(DEFAULT_XEX)
        {
            xex = Some((entry.start_sector, entry.size));
        }
        Ok(())
    })?;

    let (sector, size) = xex.ok_or(GodError::MissingDefaultXex)?;

    // A dirent can claim more data than the source file actually holds;
    // catch that before allocating for a read that size, and before the
    // part writer would fail later with an opaque UnexpectedEof.
    // `data_size` already spans every dirent, the executable included.
    let actual = reader.seek(SeekFrom::End(0))?;
    let expected = volume.base + data_size;
    if expected > actual {
        return Err(GodError::TruncatedImage { expected, actual });
    }

    reader.seek(SeekFrom::Start(data_offset(&volume, sector)))?;
    let mut buf = vec![0u8; size as usize];
    reader.read_exact(&mut buf)?;

    let block_count = data_size.div_ceil(BLOCK_SIZE);
    Ok(GodScan {
        base: volume.base,
        data_size,
        block_count,
        part_count: block_count.div_ceil(BLOCKS_PER_PART),
        execution: parse_execution_id(&buf)?,
        title_name: crate::microsoft::xex::read_xex_info(&buf).and_then(|info| info.title_name),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::microsoft::xdvdfs::VOLUME_DESCRIPTOR_SECTOR;
    use crate::microsoft::xenon::test_fixtures::{SparseDisk, descriptor, dirent};

    fn sample() -> ExecutionId {
        ExecutionId {
            media_id: 0x1122_3344,
            title_id: 0x4541_08A7,
            platform: 2,
            executable_type: 1,
            disc_number: 1,
            disc_count: 1,
        }
    }

    /// Base-0 image whose single-sector root dirtab holds `DEFAULT.XEX`
    /// and, optionally, one more file at `(sector, size)`.
    fn image(root_sector: u32, xex_sector: u32, extra: Option<(u32, u32)>) -> SparseDisk {
        let xex = super::super::xex::synthetic_xex(&sample());
        let mut root = vec![0xFFu8; SECTOR_SIZE as usize];
        // right = 8 -> child offset 8*4 = 32, past DEFAULT.XEX's dirent.
        let right = if extra.is_some() { 8 } else { 0 };
        let entry = dirent(0, right, xex_sector, xex.len() as u32, 0, b"DEFAULT.XEX");
        root[..entry.len()].copy_from_slice(&entry);
        if let Some((sector, size)) = extra {
            let entry = dirent(0, 0, sector, size, 0, b"BIG.BIN");
            root[32..32 + entry.len()].copy_from_slice(&entry);
        }

        let mut disk = SparseDisk::new();
        disk.put(
            VOLUME_DESCRIPTOR_SECTOR as u64 * SECTOR_SIZE,
            descriptor(root_sector, SECTOR_SIZE as u32),
        );
        disk.put(root_sector as u64 * SECTOR_SIZE, root);
        disk.put(xex_sector as u64 * SECTOR_SIZE, xex);
        if let Some((sector, size)) = extra {
            // Backing bytes for the extra file, so the disk's apparent
            // length actually reaches as far as its dirent claims.
            disk.put(sector as u64 * SECTOR_SIZE, vec![0u8; size as usize]);
        }
        disk
    }

    #[test]
    fn geometry_constants_agree() {
        assert_eq!(SUBPART_SIZE, 0xCC000);
        assert_eq!(BLOCKS_PER_PART, 0xA1C4);
        assert_eq!(BLOCKS_PER_PART_FILE, 0xA290);
    }

    #[test]
    fn trim_end_is_the_last_file_end() {
        let scan = scan(&mut image(40, 50, Some((60, 1000)))).unwrap();
        assert_eq!(scan.base, 0);
        assert_eq!(scan.data_size, 60 * SECTOR_SIZE + 1000);
        assert_eq!(scan.execution, sample());
    }

    #[test]
    fn trim_end_floors_at_the_volume_descriptor_sector() {
        // Everything (root dirtab at sector 20, the XEX at sector 25)
        // ends well before the descriptor sector's own end.
        let scan = scan(&mut image(20, 25, None)).unwrap();
        assert_eq!(scan.data_size, MIN_DATA_SIZE);
        assert_eq!(scan.block_count, MIN_DATA_SIZE.div_ceil(BLOCK_SIZE));
        assert_eq!(scan.part_count, 1);
    }

    #[test]
    fn part_count_splits_once_past_a_full_part() {
        let scan = scan(&mut image(40, 50, Some((100_000, 1)))).unwrap();
        assert_eq!(scan.data_size, 100_000 * SECTOR_SIZE + 1);
        assert_eq!(scan.block_count, 50_001);
        assert_eq!(scan.part_count, 2);
    }

    #[test]
    fn a_truncated_source_reports_expected_and_actual_lengths() {
        let xex = super::super::xex::synthetic_xex(&sample());
        let xex_len = xex.len() as u64;
        let mut root = vec![0xFFu8; SECTOR_SIZE as usize];
        let entry = dirent(0, 8, 50, xex.len() as u32, 0, b"DEFAULT.XEX");
        root[..entry.len()].copy_from_slice(&entry);
        let big_entry = dirent(0, 0, 60, 1_000_000, 0, b"BIG.BIN");
        root[32..32 + big_entry.len()].copy_from_slice(&big_entry);

        let mut disk = SparseDisk::new();
        disk.put(
            VOLUME_DESCRIPTOR_SECTOR as u64 * SECTOR_SIZE,
            descriptor(40, SECTOR_SIZE as u32),
        );
        disk.put(40 * SECTOR_SIZE, root);
        disk.put(50 * SECTOR_SIZE, xex);
        // BIG.BIN's dirent claims far more data than the disk holds.

        let expected = 60 * SECTOR_SIZE + 1_000_000;
        let actual = 50 * SECTOR_SIZE + xex_len;
        match scan(&mut disk) {
            Err(GodError::TruncatedImage {
                expected: got_expected,
                actual: got_actual,
            }) => {
                assert_eq!(got_expected, expected);
                assert_eq!(got_actual, actual);
            }
            other => panic!("expected TruncatedImage, got {other:?}"),
        }
    }

    #[test]
    fn errors_when_the_root_has_no_default_xex() {
        let mut root = vec![0xFFu8; SECTOR_SIZE as usize];
        let entry = dirent(0, 0, 50, 1000, 0, b"BIG.BIN");
        root[..entry.len()].copy_from_slice(&entry);
        let mut disk = SparseDisk::new();
        disk.put(
            VOLUME_DESCRIPTOR_SECTOR as u64 * SECTOR_SIZE,
            descriptor(40, SECTOR_SIZE as u32),
        );
        disk.put(40 * SECTOR_SIZE, root);

        assert!(matches!(scan(&mut disk), Err(GodError::MissingDefaultXex)));
    }
}
