//! FST-aware disc usage analysis.
//!
//! Ports libwbfs `wd_build_disc_usage`: walk the disc structure and mark
//! which `0x8000`-byte sectors actually hold data, so the WBFS writer can
//! drop everything else (scrubbing). Encrypted Wii partition data is never
//! all-zero, so a naive "drop zero blocks" pass cannot find the unused
//! space inside a partition; only the FST tells us which sectors are real.
//!
//! Marking is deliberately conservative. Over-marking just stores a few
//! extra sectors; under-marking would drop live data and corrupt the
//! disc, so any parse failure falls back to keeping the whole region.

use std::io::{Read, Seek, SeekFrom};

use crate::nintendo::dol::fst as gc_fst;
use crate::nintendo::dol::is_gamecube;
use crate::nintendo::dol::models::boot_bin::GcBootBin;
use crate::nintendo::rvl::constants::{
    WII_PARTITION_INFO_OFFSET, WII_SECTOR_PAYLOAD_SIZE, WII_SECTOR_SIZE_U64,
};
use crate::nintendo::rvl::disc::{is_wii, read_partition_table};
use crate::nintendo::rvl::fst as wii_fst;
use crate::nintendo::rvl::partition::read_partition_info;
use crate::nintendo::rvl::partition_reader::PartitionPayloadReader;

use super::error::{WbfsError, WbfsResult};

const WII_SECTOR_PAYLOAD_SIZE_U64: u64 = WII_SECTOR_PAYLOAD_SIZE as u64;

/// Disc header, region info, and partition table all sit in the first
/// part of the disc; reserve through here so none of it is scrubbed.
const WII_RESERVED_HEAD: u64 = WII_PARTITION_INFO_OFFSET + 0x10000;

/// A Wii FST never reaches this size; a larger declared size means a
/// corrupt header, so we bail to the conservative fallback instead of
/// allocating it.
const MAX_FST_SIZE: u64 = 64 * 1024 * 1024;

/// Bitset over `0x8000`-byte disc sectors. A set bit means the sector
/// holds real data and must be stored; a clear bit means it can be
/// scrubbed.
pub struct DiscUsage {
    bits: Vec<u64>,
    sector_count: u64,
}

impl DiscUsage {
    pub fn new(disc_size: u64) -> Self {
        let sector_count = disc_size.div_ceil(WII_SECTOR_SIZE_U64);
        let words = (sector_count as usize).div_ceil(64);
        Self {
            bits: vec![0u64; words],
            sector_count,
        }
    }

    pub fn mark_all(&mut self) {
        for word in &mut self.bits {
            *word = u64::MAX;
        }
    }

    fn mark_sector(&mut self, sector: u64) {
        if sector >= self.sector_count {
            return;
        }
        self.bits[(sector / 64) as usize] |= 1u64 << (sector % 64);
    }

    pub fn sector_used(&self, sector: u64) -> bool {
        if sector >= self.sector_count {
            return false;
        }
        (self.bits[(sector / 64) as usize] >> (sector % 64)) & 1 != 0
    }

    /// Mark every sector overlapping the raw-disc byte range
    /// `[byte_off, byte_off + byte_len)`.
    pub(crate) fn mark_range(&mut self, byte_off: u64, byte_len: u64) {
        if byte_len == 0 {
            return;
        }
        let first = byte_off / WII_SECTOR_SIZE_U64;
        let last = (byte_off + byte_len - 1) / WII_SECTOR_SIZE_U64;
        for sector in first..=last {
            self.mark_sector(sector);
        }
    }

    /// True if any disc sector covered by WBFS block `block_idx`
    /// (spanning `sectors_per_block` disc sectors) is used.
    pub fn block_used(&self, block_idx: u64, sectors_per_block: u64) -> bool {
        let start = block_idx * sectors_per_block;
        let end = (start + sectors_per_block).min(self.sector_count);
        (start..end).any(|s| self.sector_used(s))
    }
}

/// Build the sector usage map for the logical disc behind `reader`. The
/// reader is left at an unspecified position. Any structural parse
/// failure degrades to marking everything used, which yields a valid
/// (if unscrubbed) container rather than a corrupt one.
pub fn build_disc_usage<R: Read + Seek>(reader: &mut R, disc_size: u64) -> WbfsResult<DiscUsage> {
    let mut usage = DiscUsage::new(disc_size);

    let mut dhead = [0u8; 128];
    reader.seek(SeekFrom::Start(0))?;
    if reader.read_exact(&mut dhead).is_err() {
        usage.mark_all();
        return Ok(usage);
    }

    let marked = if is_wii(&dhead) {
        mark_wii(reader, &mut usage)
    } else if is_gamecube(&dhead) {
        mark_gamecube(reader, &mut usage)
    } else {
        usage.mark_all();
        return Ok(usage);
    };

    if marked.is_err() {
        usage.mark_all();
    }
    Ok(usage)
}

fn wbfs_err<E: std::fmt::Display>(e: E) -> WbfsError {
    WbfsError::Custom(e.to_string())
}

fn mark_wii<R: Read + Seek>(reader: &mut R, usage: &mut DiscUsage) -> WbfsResult<()> {
    usage.mark_range(0, WII_RESERVED_HEAD);

    let entries = read_partition_table(reader).map_err(wbfs_err)?;
    for entry in entries {
        let info = read_partition_info(reader, entry.offset, entry.group, entry.partition_type)
            .map_err(wbfs_err)?;
        // Ticket, TMD, cert chain, and H3 table live before the data.
        usage.mark_range(info.partition_offset, info.data_offset);

        if mark_wii_partition_files(reader, &info, usage).is_err() {
            // Keep the whole partition rather than risk dropping live
            // data on a partition we could not fully parse.
            usage.mark_range(info.data_start(), info.data_size);
        }
    }
    Ok(())
}

fn mark_wii_partition_files<R: Read + Seek>(
    reader: &mut R,
    info: &crate::nintendo::rvl::partition::PartitionInfo,
    usage: &mut DiscUsage,
) -> WbfsResult<()> {
    let base_sector = info.data_start() / WII_SECTOR_SIZE_U64;
    let mut payload = PartitionPayloadReader::new(&mut *reader, info);

    let mut boot = [0u8; 0x440];
    payload.seek(SeekFrom::Start(0))?;
    payload.read_exact(&mut boot)?;
    let fst_offset = (u32::from_be_bytes(boot[0x424..0x428].try_into().unwrap()) as u64) << 2;
    let fst_size = (u32::from_be_bytes(boot[0x428..0x42C].try_into().unwrap()) as u64) << 2;

    // boot.bin, bi2, apploader, main DOL, and the FST all precede the
    // file data on a real disc, so one range covers the front matter.
    mark_payload_range(usage, base_sector, 0, fst_offset + fst_size);

    if fst_size > 0 {
        if fst_size > MAX_FST_SIZE {
            return Err(WbfsError::Custom(format!(
                "implausible Wii FST size {fst_size}"
            )));
        }
        payload.seek(SeekFrom::Start(fst_offset))?;
        let mut fst = vec![0u8; fst_size as usize];
        payload.read_exact(&mut fst)?;
        for node in wii_fst::list_files(&fst).map_err(wbfs_err)? {
            if let wii_fst::FstNode::File { offset, size, .. } = node {
                mark_payload_range(usage, base_sector, offset, size);
            }
        }
    }
    Ok(())
}

/// Map a decrypted-partition byte range to the encrypted disc sectors
/// that carry it and mark them. Payload byte `p` lives in payload
/// sector `p / 0x7C00`, which is stored in encrypted disc sector
/// `base_sector + p / 0x7C00`.
fn mark_payload_range(usage: &mut DiscUsage, base_sector: u64, p_off: u64, p_len: u64) {
    if p_len == 0 {
        return;
    }
    let first = p_off / WII_SECTOR_PAYLOAD_SIZE_U64;
    let last = (p_off + p_len - 1) / WII_SECTOR_PAYLOAD_SIZE_U64;
    for sector in first..=last {
        usage.mark_sector(base_sector + sector);
    }
}

fn mark_gamecube<R: Read + Seek>(reader: &mut R, usage: &mut DiscUsage) -> WbfsResult<()> {
    reader.seek(SeekFrom::Start(0))?;
    let boot = GcBootBin::read(reader).map_err(wbfs_err)?;
    let fst_offset = boot.fst_offset as u64;
    let fst_size = boot.fst_size as u64;

    // Header, bi2, apploader, DOL, and FST sit at the front of the disc.
    usage.mark_range(0, fst_offset + fst_size);

    if fst_size > 0 {
        if fst_size > MAX_FST_SIZE {
            return Err(WbfsError::Custom(format!(
                "implausible GameCube FST size {fst_size}"
            )));
        }
        reader.seek(SeekFrom::Start(fst_offset))?;
        let mut fst = vec![0u8; fst_size as usize];
        reader.read_exact(&mut fst)?;
        for node in gc_fst::list_files(&fst).map_err(wbfs_err)? {
            if let gc_fst::FstNode::File { offset, size, .. } = node {
                // GameCube discs are unencrypted: FST offsets are raw
                // disc byte offsets.
                usage.mark_range(offset, size);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_and_reads_back_single_sector() {
        let mut usage = DiscUsage::new(4 * WII_SECTOR_SIZE_U64);
        assert!(!usage.sector_used(2));
        usage.mark_sector(2);
        assert!(usage.sector_used(2));
        assert!(!usage.sector_used(1));
        assert!(!usage.sector_used(3));
    }

    #[test]
    fn mark_range_covers_overlapping_sectors() {
        let mut usage = DiscUsage::new(8 * WII_SECTOR_SIZE_U64);
        // One byte into sector 1 through one byte into sector 3.
        usage.mark_range(WII_SECTOR_SIZE_U64 + 1, 2 * WII_SECTOR_SIZE_U64);
        assert!(!usage.sector_used(0));
        assert!(usage.sector_used(1));
        assert!(usage.sector_used(2));
        assert!(usage.sector_used(3));
        assert!(!usage.sector_used(4));
    }

    #[test]
    fn block_used_checks_every_covered_sector() {
        let mut usage = DiscUsage::new(256 * WII_SECTOR_SIZE_U64);
        // 64 sectors per 2 MiB block. Mark one sector inside block 1.
        usage.mark_sector(64 + 17);
        assert!(!usage.block_used(0, 64));
        assert!(usage.block_used(1, 64));
        assert!(!usage.block_used(2, 64));
    }

    #[test]
    fn mark_all_sets_every_sector() {
        let mut usage = DiscUsage::new(3 * WII_SECTOR_SIZE_U64);
        usage.mark_all();
        assert!(usage.sector_used(0));
        assert!(usage.sector_used(1));
        assert!(usage.sector_used(2));
    }

    #[test]
    fn gamecube_marks_front_matter_and_files_but_not_the_tail() {
        use crate::nintendo::dol::constants::{GAMECUBE_MAGIC, GAMECUBE_MAGIC_OFFSET};
        use std::io::Cursor;

        let sector = WII_SECTOR_SIZE_U64 as usize;
        let mut disc = vec![0u8; 8 * sector];
        disc[GAMECUBE_MAGIC_OFFSET..GAMECUBE_MAGIC_OFFSET + 4]
            .copy_from_slice(&GAMECUBE_MAGIC.to_be_bytes());

        // Minimal FST at sector 1 listing one file at sector 4.
        let mut fst = Vec::new();
        fst.extend_from_slice(&[1, 0, 0, 0]); // root: type 1, name offset 0
        fst.extend_from_slice(&0u32.to_be_bytes()); // root parent
        fst.extend_from_slice(&2u32.to_be_bytes()); // total entries
        fst.push(0); // file entry: type 0
        fst.extend_from_slice(&[0, 0, 0]); // name offset 0
        fst.extend_from_slice(&0x20000u32.to_be_bytes()); // file offset (raw, GC)
        fst.extend_from_slice(&0x100u32.to_be_bytes()); // file size
        fst.extend_from_slice(b"f\0");

        let fst_offset: u32 = 0x8000;
        disc[0x424..0x428].copy_from_slice(&fst_offset.to_be_bytes());
        disc[0x428..0x42C].copy_from_slice(&(fst.len() as u32).to_be_bytes());
        disc[fst_offset as usize..fst_offset as usize + fst.len()].copy_from_slice(&fst);
        disc[0x20000..0x20000 + 0x100].fill(0xAB);

        let usage = build_disc_usage(&mut Cursor::new(disc.clone()), disc.len() as u64).unwrap();
        assert!(usage.sector_used(0), "disc header");
        assert!(usage.sector_used(1), "FST");
        assert!(usage.sector_used(4), "FST-listed file");
        assert!(!usage.sector_used(2), "gap before file is unused");
        assert!(!usage.sector_used(6), "zero tail is unused");
    }
}
