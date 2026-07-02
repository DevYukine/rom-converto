//! WBFS container write primitives shared by the parallel writer
//! (`crate::nintendo::rvz::decompress::sink::WbfsSink`): the
//! physical-slot layout for a disc, block-0 construction, and the
//! free-block bitmap.

use super::error::{WbfsError, WbfsResult};
use super::format::{
    DISC_HEADER_COPY_SIZE, WBFS_MAGIC, WBFS_VERSION, WII_SECTOR_SIZE, wbfs_sectors_per_disc,
};
use super::usage::DiscUsage;

/// Assign physical slots for every logical block of a disc. A block is
/// stored (gets a slot, counting up from 1) when it is block 0 (it
/// carries the disc header) or the usage map marks it used; otherwise
/// its wlba entry stays 0 (scrubbed). Returns the full-length wlba
/// table and the total physical block count (block 0 + data blocks).
pub(crate) fn compute_layout(
    usage: &DiscUsage,
    disc_size: u64,
    wbfs_sec_sz_s: u8,
) -> WbfsResult<(Vec<u16>, u64)> {
    let wbfs_sec_sz = 1u64 << wbfs_sec_sz_s;
    let entries = wbfs_sectors_per_disc(wbfs_sec_sz_s) as usize;
    let sectors_per_block = wbfs_sec_sz / WII_SECTOR_SIZE;
    let n_logical_blocks = disc_size.div_ceil(wbfs_sec_sz);
    if n_logical_blocks > entries as u64 {
        return Err(WbfsError::DiscTooLarge(disc_size));
    }

    let mut wlba = vec![0u16; entries];
    let mut next_phys: u32 = 1;
    for (i, slot) in wlba.iter_mut().enumerate().take(n_logical_blocks as usize) {
        if i == 0 || usage.block_used(i as u64, sectors_per_block) {
            *slot = next_phys as u16;
            next_phys += 1;
        }
    }
    Ok((wlba, next_phys as u64))
}

/// Build physical block 0: the partition header, the slot table, the
/// per-disc info (disc-header copy + wlba table), and the free-block
/// bitmap. `wlba` must be the full `wbfs_sectors_per_disc`-long table.
pub(crate) fn build_block0(
    hd_sec_sz_s: u8,
    wbfs_sec_sz_s: u8,
    total_blocks: u64,
    disc_header: &[u8; DISC_HEADER_COPY_SIZE],
    wlba: &[u16],
) -> Vec<u8> {
    let hd_sec_sz = 1u64 << hd_sec_sz_s;
    let wbfs_sec_sz = 1u64 << wbfs_sec_sz_s;
    let entries = wbfs_sectors_per_disc(wbfs_sec_sz_s) as usize;
    // `n_hd_sec * hd_sec_sz == total_blocks * wbfs_sec_sz == file size`,
    // the invariant Dolphin validates on open.
    let n_hd_sec = total_blocks << (wbfs_sec_sz_s - hd_sec_sz_s);

    let mut sector0 = vec![0u8; wbfs_sec_sz as usize];
    sector0[0..4].copy_from_slice(&WBFS_MAGIC);
    sector0[4..8].copy_from_slice(&(n_hd_sec as u32).to_be_bytes());
    sector0[8] = hd_sec_sz_s;
    sector0[9] = wbfs_sec_sz_s;
    sector0[10] = WBFS_VERSION;
    // disc slot 0 in use.
    sector0[12] = 1;

    let info_off = hd_sec_sz as usize;
    debug_assert!(
        info_off + DISC_HEADER_COPY_SIZE + entries * 2 <= sector0.len(),
        "disc info must fit inside block 0"
    );
    sector0[info_off..info_off + DISC_HEADER_COPY_SIZE].copy_from_slice(disc_header);
    let wlba_off = info_off + DISC_HEADER_COPY_SIZE;
    for (i, &v) in wlba.iter().enumerate() {
        let o = wlba_off + i * 2;
        sector0[o..o + 2].copy_from_slice(&v.to_be_bytes());
    }

    write_freeblocks(&mut sector0, total_blocks, wbfs_sec_sz, hd_sec_sz_s);
    sector0
}

/// Write the libwbfs free-block bitmap into block 0: one bit per WBFS
/// block, big-endian `u32` words, set = free. Physical block `b` maps
/// to bit `b - 1` (block 0 is the management block and has no bit), so
/// every allocated data block is cleared. USB loaders that add or
/// remove discs read this to find free space; Dolphin ignores it.
///
/// `alloc_block` in libwbfs iterates exactly `n_wbfs_sec / 32` words at
/// `freeblks_lba`, so the bitmap is sized and placed to match.
fn write_freeblocks(sector0: &mut [u8], total_blocks: u64, wbfs_sec_sz: u64, hd_sec_sz_s: u8) {
    let n_wbfs_sec = total_blocks;
    let n_words = (n_wbfs_sec / 32) as usize;
    if n_words == 0 {
        return;
    }
    let freeblks_lba = (wbfs_sec_sz - n_wbfs_sec / 8) >> hd_sec_sz_s;
    let freeblks_off = (freeblks_lba << hd_sec_sz_s) as usize;

    let mut freeblks = vec![0xFFu8; n_words * 4];
    for b in 1..total_blocks {
        let bit = b - 1;
        let word = (bit / 32) as usize;
        if word >= n_words {
            break;
        }
        let base = word * 4;
        let mut w = u32::from_be_bytes(freeblks[base..base + 4].try_into().unwrap());
        w &= !(1u32 << (bit % 32));
        freeblks[base..base + 4].copy_from_slice(&w.to_be_bytes());
    }
    sector0[freeblks_off..freeblks_off + freeblks.len()].copy_from_slice(&freeblks);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::wbfs::format::{WII_MAGIC, WII_MAGIC_OFFSET};

    // 2 MiB blocks so the dual-layer wlba table fits inside block 0,
    // matching the real single-game layout.
    const HD_SECTOR_SHIFT: u8 = 9;
    const WBFS_SECTOR_SHIFT: u8 = 21;

    #[test]
    fn compute_layout_assigns_slots_to_used_blocks() {
        let wbfs_sec_sz = 1u64 << WBFS_SECTOR_SHIFT;
        let disc_size = 4 * wbfs_sec_sz;
        let mut usage = DiscUsage::new(disc_size);
        // Block 2 used; block 0 forced used; blocks 1 and 3 unused.
        usage.mark_range(2 * wbfs_sec_sz, wbfs_sec_sz);
        let (wlba, total_blocks) = compute_layout(&usage, disc_size, WBFS_SECTOR_SHIFT).unwrap();
        assert_eq!(wlba[0], 1, "block 0 is always stored");
        assert_eq!(wlba[1], 0, "unused block is scrubbed");
        assert_eq!(wlba[2], 2, "used block gets the next slot");
        assert_eq!(wlba[3], 0);
        assert_eq!(total_blocks, 3, "block 0 + two data blocks");
    }

    #[test]
    fn build_block0_writes_header_and_table() {
        let mut header = [0u8; DISC_HEADER_COPY_SIZE];
        header[WII_MAGIC_OFFSET..WII_MAGIC_OFFSET + 4].copy_from_slice(&WII_MAGIC.to_be_bytes());
        let entries = wbfs_sectors_per_disc(WBFS_SECTOR_SHIFT) as usize;
        let mut wlba = vec![0u16; entries];
        wlba[0] = 1;
        wlba[2] = 2;
        let total_blocks = 3u64;

        let sector0 = build_block0(
            HD_SECTOR_SHIFT,
            WBFS_SECTOR_SHIFT,
            total_blocks,
            &header,
            &wlba,
        );
        assert_eq!(&sector0[0..4], &WBFS_MAGIC);
        let n_hd_sec = u32::from_be_bytes(sector0[4..8].try_into().unwrap());
        assert_eq!(
            n_hd_sec as u64,
            total_blocks << (WBFS_SECTOR_SHIFT - HD_SECTOR_SHIFT)
        );
        assert_eq!(sector0[8], HD_SECTOR_SHIFT);
        assert_eq!(sector0[9], WBFS_SECTOR_SHIFT);
        assert_eq!(sector0[12], 1);

        let info_off = 1usize << HD_SECTOR_SHIFT;
        assert_eq!(
            &sector0[info_off..info_off + DISC_HEADER_COPY_SIZE],
            &header
        );
        let wlba_off = info_off + DISC_HEADER_COPY_SIZE;
        assert_eq!(
            u16::from_be_bytes(sector0[wlba_off..wlba_off + 2].try_into().unwrap()),
            1
        );
        assert_eq!(
            u16::from_be_bytes(sector0[wlba_off + 4..wlba_off + 6].try_into().unwrap()),
            2
        );
    }

    #[test]
    fn freeblocks_clears_allocated_blocks_and_leaves_rest_free() {
        let wbfs_sec_sz = 1u64 << WBFS_SECTOR_SHIFT;
        let mut sector0 = vec![0u8; wbfs_sec_sz as usize];
        // 64 blocks -> 2 BE u32 words; bits for data blocks 1..63.
        let total_blocks = 64u64;
        write_freeblocks(&mut sector0, total_blocks, wbfs_sec_sz, HD_SECTOR_SHIFT);

        let n_wbfs_sec = total_blocks;
        let freeblks_lba = (wbfs_sec_sz - n_wbfs_sec / 8) >> HD_SECTOR_SHIFT;
        let off = (freeblks_lba << HD_SECTOR_SHIFT) as usize;
        let word0 = u32::from_be_bytes(sector0[off..off + 4].try_into().unwrap());
        let word1 = u32::from_be_bytes(sector0[off + 4..off + 8].try_into().unwrap());

        assert_eq!(word0, 0, "blocks 1..32 all marked used");
        assert_eq!(word1, 1u32 << 31, "block 64 is not allocated, stays free");
    }
}
