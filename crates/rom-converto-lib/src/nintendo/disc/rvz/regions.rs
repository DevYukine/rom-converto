//! Region planner.
//!
//! A disc image is split into a sequence of [`DiscRegion`] entries before
//! compression. For GameCube this is just the disc header followed by the
//! body; for Wii the planner intersperses `Raw` regions (unencrypted gaps)
//! with `Partition` regions (encrypted partition data that the Wii pipeline
//! handles separately).
//!
//! The split follows the format's raw-data rule: the very first raw
//! region skips the 0x80-byte disc header, which lives
//! in `wia_disc_t.dhead` instead of in a raw_data entry.

use super::partition_encoded_size;
use crate::nintendo::disc::rvz::error::RvzResult;
use crate::nintendo::rvl::constants::WII_PARTITION_INFO_OFFSET;
use crate::nintendo::rvl::disc::read_partition_table;
use crate::nintendo::rvl::partition::{PartitionInfo, read_partition_info};
use std::io::{Read, Seek};

/// Number of bytes at the start of every disc that live in `wia_disc_t.dhead`
/// rather than in a raw_data region.
pub const DISC_HEADER_SKIP: u64 = 0x80;

/// One region of the input ISO.
#[derive(Debug, Clone)]
pub enum DiscRegion {
    /// A contiguous range of unencrypted bytes that flows through the
    /// regular zstd chunk pipeline.
    Raw { offset: u64, size: u64 },
    /// A Wii partition whose encrypted data is handled by the partition
    /// pipeline.
    Partition(PartitionInfo),
}

/// Ordered list of regions covering the entire disc.
#[derive(Debug, Clone, Default)]
pub struct RegionPlan {
    pub regions: Vec<DiscRegion>,
}

impl RegionPlan {
    /// Plan a GameCube disc as a single raw region (the disc header is
    /// stored separately in `wia_disc_t.dhead`).
    pub fn gamecube(iso_size: u64) -> Self {
        let mut regions = Vec::new();
        if iso_size > DISC_HEADER_SKIP {
            regions.push(DiscRegion::Raw {
                offset: DISC_HEADER_SKIP,
                size: iso_size - DISC_HEADER_SKIP,
            });
        }
        Self { regions }
    }

    /// Plan a Wii disc by reading its partition table and intercutting
    /// raw regions with partition regions. If the disc is too small to hold
    /// a partition table (synthetic test fixtures, malformed inputs), this
    /// falls back to a single raw region just like a GameCube disc.
    pub fn wii<R: Read + Seek>(reader: &mut R, iso_size: u64) -> RvzResult<Self> {
        if iso_size < WII_PARTITION_INFO_OFFSET + 32 {
            return Ok(Self::gamecube(iso_size));
        }
        let entries = read_partition_table(reader)?;
        if entries.is_empty() {
            return Ok(Self::gamecube(iso_size));
        }
        let mut partitions: Vec<PartitionInfo> = Vec::new();
        for e in entries {
            // Skip partitions whose header doesn't fit in the file. Synthetic
            // ISOs that pass the size check above still might not have full
            // partition headers.
            if e.offset + crate::nintendo::rvl::constants::WII_PARTITION_HEADER_SIZE as u64
                > iso_size
            {
                continue;
            }
            let info = read_partition_info(reader, e.offset, e.group, e.partition_type)?;
            // A dump cut inside the partition header/TMD span, or one
            // holding less than a sector of partition data, has nothing to
            // encode as a partition; its bytes stay in the raw region.
            if partition_encoded_size(info.data_start(), info.data_size, iso_size) == 0 {
                continue;
            }
            partitions.push(info);
        }
        if partitions.is_empty() {
            return Ok(Self::gamecube(iso_size));
        }
        // Sort partitions by their data start offset so the planner walks
        // the disc in monotonic order.
        partitions.sort_by_key(|p| p.data_start());

        let mut regions = Vec::new();
        let mut cursor: u64 = DISC_HEADER_SKIP;
        for p in &partitions {
            let data_start = p.data_start();
            if data_start > cursor {
                regions.push(DiscRegion::Raw {
                    offset: cursor,
                    size: data_start - cursor,
                });
            }
            regions.push(DiscRegion::Partition(*p));
            // Advance past the partition's encoded data (clamped to the
            // ISO and sector-truncated), matching the format's reference
            // implementation: sectors past it
            // (a partial last cluster's padding, any sub-sector tail) are
            // NOT in the partition's group range and fall into the
            // following raw_data entry, so they get compressed as raw.
            cursor = data_start + partition_encoded_size(data_start, p.data_size, iso_size);
        }
        if iso_size > cursor {
            regions.push(DiscRegion::Raw {
                offset: cursor,
                size: iso_size - cursor,
            });
        }
        Ok(Self { regions })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamecube_plan_skips_dhead() {
        let plan = RegionPlan::gamecube(1024 * 1024);
        assert_eq!(plan.regions.len(), 1);
        match &plan.regions[0] {
            DiscRegion::Raw { offset, size } => {
                assert_eq!(*offset, DISC_HEADER_SKIP);
                assert_eq!(*size, 1024 * 1024 - DISC_HEADER_SKIP);
            }
            DiscRegion::Partition { .. } => panic!("unexpected partition region"),
        }
    }

    #[test]
    fn tiny_disc_skips_everything() {
        let plan = RegionPlan::gamecube(0x40);
        assert!(plan.regions.is_empty());
    }

    /// A dump cut inside the partition header span: the header fits, its
    /// data start does not, so the planner keeps everything raw instead of
    /// emitting a partition the geometry check would reject.
    #[test]
    fn partition_starting_past_eof_stays_raw() {
        use crate::nintendo::rvl::constants::WII_PARTITION_HEADER_SIZE;
        use crate::nintendo::rvl::test_fixtures::make_fake_wii_iso_with_partition;
        const PARTITION_OFFSET: u64 = 0x050000;
        let iso_size = PARTITION_OFFSET + WII_PARTITION_HEADER_SIZE as u64;
        let iso = make_fake_wii_iso_with_partition(1)[..iso_size as usize].to_vec();
        let plan = RegionPlan::wii(&mut std::io::Cursor::new(iso), iso_size).unwrap();
        assert_eq!(plan.regions.len(), 1);
        assert!(matches!(
            plan.regions[0],
            DiscRegion::Raw { offset: DISC_HEADER_SKIP, size } if size == iso_size - DISC_HEADER_SKIP
        ));

        // Cut exactly at the data start: still nothing to encode.
        const DATA_OFFSET_IN_PARTITION: u64 = 0x020000;
        let iso_size = PARTITION_OFFSET + DATA_OFFSET_IN_PARTITION;
        let iso = make_fake_wii_iso_with_partition(1)[..iso_size as usize].to_vec();
        let plan = RegionPlan::wii(&mut std::io::Cursor::new(iso), iso_size).unwrap();
        assert_eq!(plan.regions.len(), 1);
        assert!(matches!(plan.regions[0], DiscRegion::Raw { .. }));
    }
}
