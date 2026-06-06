//! WBFS container layout constants and shared layout math.
//!
//! WBFS (Wii Backup File System) stores a Wii or GameCube disc as a
//! sparse array of fixed-size blocks. Block 0 holds the partition
//! header, the disc slot table, and one per-disc info structure; the
//! info structure carries a copy of the disc header plus a big-endian
//! `u16` table (`wlba`) mapping each logical block of the disc to the
//! physical block that stores it. A table entry of 0 means the block
//! was scrubbed (all zero) and is not stored.
//!
//! Reference: libwbfs (`libwbfs.h`, `libwbfs.c`).

/// `WBFS` partition magic at offset 0 of the file.
pub const WBFS_MAGIC: [u8; 4] = *b"WBFS";

/// Layout version written into new containers. libwbfs ships `1`.
pub const WBFS_VERSION: u8 = 1;

/// Size in bytes of a raw (pre-decryption) Wii disc sector.
pub const WII_SECTOR_SIZE: u64 = 0x8000;

/// `log2(WII_SECTOR_SIZE)`. The wlba table granularity is expressed
/// relative to this when scaling to the container's block size.
pub const WII_SECTOR_SIZE_SHIFT: u8 = 15;

/// Number of `0x8000`-byte Wii sectors covered by the wlba table. The
/// table is sized for a dual-layer disc regardless of the real game
/// size, so a single value drives the table length for any block size.
pub const WII_SECTORS_PER_DISC: u64 = 143432 * 2;

/// Canonical full-image size of a single-layer Wii disc.
pub const WII_SINGLE_LAYER_SIZE: u64 = 143432 * WII_SECTOR_SIZE;

/// Canonical full-image size of a dual-layer Wii disc.
pub const WII_DUAL_LAYER_SIZE: u64 = 0x1FB4E0000;

/// Canonical full-image size of a GameCube disc.
pub const GAMECUBE_DISC_SIZE: u64 = 0x57058000;

/// Wii disc magic `0x5D1C9EA3` lives at offset 0x18 of the disc header.
pub const WII_MAGIC_OFFSET: usize = 0x18;
pub const WII_MAGIC: u32 = 0x5D1C9EA3;

/// GameCube disc magic `0xC2339F3D` lives at offset 0x1C.
pub const GAMECUBE_MAGIC_OFFSET: usize = 0x1C;
pub const GAMECUBE_MAGIC: u32 = 0xC233_9F3D;

/// Bytes of the disc header copied into each per-disc info structure.
pub const DISC_HEADER_COPY_SIZE: usize = 0x100;

/// HD sector size shift and WBFS block size shift used when writing new
/// containers: 512-byte HD sectors, 2 MiB blocks. 2 MiB blocks keep the
/// wlba table well under the `u16` entry ceiling for both disc layers
/// while staying a common, widely-read choice.
pub const DEFAULT_HD_SECTOR_SHIFT: u8 = 9;
pub const DEFAULT_WBFS_SECTOR_SHIFT: u8 = 21;

/// Round `value` up to the next multiple of `align` (a power of two is
/// not required).
pub fn align_up(value: u64, align: u64) -> u64 {
    value.div_ceil(align) * align
}

/// Number of wlba entries for a container whose block size is
/// `1 << wbfs_sec_sz_s`.
pub fn wbfs_sectors_per_disc(wbfs_sec_sz_s: u8) -> u64 {
    WII_SECTORS_PER_DISC >> (wbfs_sec_sz_s - WII_SECTOR_SIZE_SHIFT)
}

/// Byte size of one per-disc info structure (header copy plus wlba
/// table), aligned up to the HD sector size.
pub fn disc_info_size(wbfs_sec_sz_s: u8, hd_sec_sz: u64) -> u64 {
    let raw = DISC_HEADER_COPY_SIZE as u64 + wbfs_sectors_per_disc(wbfs_sec_sz_s) * 2;
    align_up(raw, hd_sec_sz)
}

/// Reconstruct the canonical full-image size of a disc stored in a
/// WBFS container. WBFS does not record the original ISO size, so the
/// value is inferred from the disc header magic and the highest block
/// that actually holds data.
pub fn reconstruct_disc_size(disc_header: &[u8], used_size: u64) -> u64 {
    let read_magic = |off: usize| -> u32 {
        if disc_header.len() >= off + 4 {
            u32::from_be_bytes(disc_header[off..off + 4].try_into().unwrap())
        } else {
            0
        }
    };
    let is_wii = read_magic(WII_MAGIC_OFFSET) == WII_MAGIC;
    let is_gamecube = read_magic(GAMECUBE_MAGIC_OFFSET) == GAMECUBE_MAGIC;

    let canonical = if is_gamecube && !is_wii {
        GAMECUBE_DISC_SIZE
    } else if used_size <= WII_SINGLE_LAYER_SIZE {
        WII_SINGLE_LAYER_SIZE
    } else {
        WII_DUAL_LAYER_SIZE
    };
    canonical.max(used_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn align_up_rounds_to_multiple() {
        assert_eq!(align_up(0, 512), 0);
        assert_eq!(align_up(1, 512), 512);
        assert_eq!(align_up(512, 512), 512);
        assert_eq!(align_up(513, 512), 1024);
        assert_eq!(align_up(9220, 512), 9728);
    }

    #[test]
    fn table_length_scales_with_block_size() {
        // 2 MiB blocks: 286864 >> 6 = 4482 entries.
        assert_eq!(wbfs_sectors_per_disc(21), 4482);
        // 32 KiB blocks (== Wii sector): full table.
        assert_eq!(wbfs_sectors_per_disc(15), WII_SECTORS_PER_DISC);
    }

    #[test]
    fn single_layer_size_matches_known_constant() {
        assert_eq!(WII_SINGLE_LAYER_SIZE, 4_699_979_776);
    }

    #[test]
    fn reconstruct_picks_single_layer_for_small_wii() {
        let mut header = vec![0u8; 0x100];
        header[WII_MAGIC_OFFSET..WII_MAGIC_OFFSET + 4].copy_from_slice(&WII_MAGIC.to_be_bytes());
        assert_eq!(
            reconstruct_disc_size(&header, 16 * 1024 * 1024),
            WII_SINGLE_LAYER_SIZE
        );
    }

    #[test]
    fn reconstruct_picks_dual_layer_when_used_exceeds_single() {
        let mut header = vec![0u8; 0x100];
        header[WII_MAGIC_OFFSET..WII_MAGIC_OFFSET + 4].copy_from_slice(&WII_MAGIC.to_be_bytes());
        assert_eq!(
            reconstruct_disc_size(&header, WII_SINGLE_LAYER_SIZE + 1),
            WII_DUAL_LAYER_SIZE
        );
    }

    #[test]
    fn reconstruct_picks_gamecube_size() {
        let mut header = vec![0u8; 0x100];
        header[GAMECUBE_MAGIC_OFFSET..GAMECUBE_MAGIC_OFFSET + 4]
            .copy_from_slice(&GAMECUBE_MAGIC.to_be_bytes());
        assert_eq!(reconstruct_disc_size(&header, 1024), GAMECUBE_DISC_SIZE);
    }
}
