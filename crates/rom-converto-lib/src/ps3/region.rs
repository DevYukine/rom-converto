//! PS3 ISO region table (sector 0).
//!
//! A decrypted PS3 disc alternates plain and encrypted regions along
//! its sector axis. Sector 0 encodes `N` plain regions (so `2N-1` total,
//! first and last plain) as a flat big-endian `u32` array at `0x0C`.

use crate::ps3::error::{Ps3Error, Ps3Result};

pub const SECTOR_SIZE: usize = 2048;

/// One contiguous run of sectors that is either wholly plain or wholly
/// encrypted. `start` and `last` are inclusive absolute LBAs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub start: u32,
    pub last: u32,
    pub plain: bool,
}

/// Parse the region table from sector 0.
///
/// Returns the ordered regions covering `[0, total_sectors)` and the
/// total sector count.
pub fn parse_region_table(sector0: &[u8]) -> Ps3Result<(Vec<Region>, u32)> {
    if sector0.len() < 4 {
        return Err(Ps3Error::InvalidRegionTable(
            "sector 0 shorter than 4 bytes".into(),
        ));
    }
    let n = u32::from_be_bytes(
        sector0[0..4]
            .try_into()
            .expect("sector0[0..4] is always 4 bytes"),
    );
    if n == 0 {
        return Err(Ps3Error::InvalidRegionTable("region count is zero".into()));
    }

    let total_regions_u64 = 2u64 * n as u64 - 1;
    let need = 0x0Cu64 + total_regions_u64 * 4;
    if need > sector0.len() as u64 {
        return Err(Ps3Error::InvalidRegionTable(format!(
            "region count {n} exceeds sector 0 bounds"
        )));
    }
    let total_regions = total_regions_u64 as u32;

    let mut regions = Vec::with_capacity(total_regions as usize);
    let mut lba: u32 = 0;
    let mut plain = true;
    for j in 0..total_regions {
        let off = 0x0C + j as usize * 4;
        let v = u32::from_be_bytes(
            sector0[off..off + 4]
                .try_into()
                .expect("sector0[off..off + 4] is always 4 bytes"),
        );
        let last = if plain {
            v
        } else {
            v.checked_sub(1).ok_or_else(|| {
                Ps3Error::InvalidRegionTable("encrypted region ends before sector 0".into())
            })?
        };
        if last < lba {
            return Err(Ps3Error::InvalidRegionTable(format!(
                "region {j} ends at sector {last} before its start {lba}"
            )));
        }
        regions.push(Region {
            start: lba,
            last,
            plain,
        });
        lba = last.checked_add(1).ok_or_else(|| {
            Ps3Error::InvalidRegionTable(format!("region {j} ends at the last addressable sector"))
        })?;
        plain = !plain;
    }

    Ok((regions, lba))
}
