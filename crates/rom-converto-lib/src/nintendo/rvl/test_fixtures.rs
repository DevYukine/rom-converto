//! Synthetic Wii disc fixtures for tests.

#![cfg(test)]

use crate::nintendo::rvl::constants::{WII_MAGIC, WII_MAGIC_OFFSET};

use crate::nintendo::rvl::common_keys::WII_COMMON_KEY;
use crate::nintendo::rvl::constants::{
    WII_BLOCKS_PER_GROUP, WII_GROUP_TOTAL_SIZE, WII_PARTITION_HEADER_DATA_OFFSET_OFFSET,
    WII_PARTITION_HEADER_DATA_SIZE_OFFSET, WII_PARTITION_HEADER_SIZE,
    WII_PARTITION_INFO_OFFSET, WII_SECTOR_PAYLOAD_SIZE, WII_SECTOR_SIZE, WII_TICKET_SIZE,
    WII_TICKET_TITLE_ID_OFFSET, WII_TICKET_TITLE_KEY_OFFSET,
};
use crate::nintendo::rvl::disc::encrypt_sector;
use crate::nintendo::rvl::partition::{recompute_hash_regions, HASH_REGION_BYTES};
use aes::{
    Aes128,
    cipher::{BlockEncryptMut, KeyIvInit},
};
use block_padding::NoPadding;
use cbc::Encryptor;

/// Build a fake Wii disc image with the Wii magic at the correct offset and
/// a compressible repeating pattern for the body. The raw-data path accepts
/// this as a valid Wii disc without needing a real partition table.
pub fn make_fake_wii_iso(size: usize) -> Vec<u8> {
    assert!(size >= 0x80, "synthetic Wii ISO must fit the disc header");
    let mut data = vec![0u8; size];
    data[WII_MAGIC_OFFSET..WII_MAGIC_OFFSET + 4].copy_from_slice(&WII_MAGIC.to_be_bytes());
    for (i, b) in data.iter_mut().enumerate().skip(0x80) {
        *b = ((i.wrapping_mul(7)) % 251) as u8;
    }
    data
}

/// Like [`make_fake_wii_iso_with_partition`] but the partition's
/// declared `data_size` is not a multiple of the cluster size: the
/// last cluster carries `last_cluster_sectors` real sectors followed
/// by padding sectors that fall into the raw region immediately after
/// the partition. Exercises the encoder/decoder partial-chunk path
/// that real partitions hit when `data_size` is sub-cluster aligned.
///
/// The physical storage still covers the full last cluster; a
/// trailing raw region of a few clusters is appended after the
/// partition so the padding sectors and any following raw data have
/// somewhere to live.
pub fn make_fake_wii_iso_with_partial_partition(
    n_full_clusters: usize,
    last_cluster_sectors: usize,
) -> Vec<u8> {
    assert!(last_cluster_sectors > 0 && last_cluster_sectors < WII_BLOCKS_PER_GROUP);
    assert!(n_full_clusters >= 1);
    let physical_clusters = n_full_clusters + 1;
    let mut data = make_fake_wii_iso_with_partition(physical_clusters);
    // Rewrite the partition header's data_size to the partial value
    // (full clusters + partial last cluster in sector units). The
    // synthetic fixture's data_offset is `DATA_OFFSET_IN_PARTITION =
    // 0x20000`, partition at `0x050000`, so the header fields live at
    // `PARTITION_OFFSET + WII_PARTITION_HEADER_DATA_SIZE_OFFSET`.
    const PARTITION_OFFSET: usize = 0x050000;
    let partial_data_size: u64 = n_full_clusters as u64 * WII_GROUP_TOTAL_SIZE
        + last_cluster_sectors as u64 * WII_SECTOR_SIZE as u64;
    let ds_word = (partial_data_size >> 2) as u32;
    let ds_off = PARTITION_OFFSET + WII_PARTITION_HEADER_DATA_SIZE_OFFSET;
    data[ds_off..ds_off + 4].copy_from_slice(&ds_word.to_be_bytes());
    data
}

/// Build a synthetic Wii ISO with one valid encrypted partition
/// containing `n_clusters` clusters of payload. The title key is the
/// constant 0xA5 fill, encrypted with the standard Wii common key so
/// [`crate::nintendo::rvl::partition::read_partition_info`] can
/// recover it.
pub fn make_fake_wii_iso_with_partition(n_clusters: usize) -> Vec<u8> {
    type Aes128CbcEnc = Encryptor<Aes128>;

    // Layout:
    //   0x000000   disc header (Wii magic at 0x18, filler elsewhere)
    //   0x040000   partition info table (1 partition in group 0)
    //   0x050000   partition start (ticket + header padding)
    //              partition data starts at PARTITION_OFFSET + 0x20000
    //   ...        encrypted clusters
    //
    // `data_offset = 0x20000` leaves room for the partition header
    // and is a multiple of the 0x8000 sector size.

    const PARTITION_OFFSET: u64 = 0x050000;
    const DATA_OFFSET_IN_PARTITION: u64 = 0x020000;
    let data_size = n_clusters as u64 * WII_GROUP_TOTAL_SIZE;
    let total_size =
        PARTITION_OFFSET as usize + DATA_OFFSET_IN_PARTITION as usize + data_size as usize;

    let mut data = vec![0u8; total_size];

    // Disc header magic.
    data[WII_MAGIC_OFFSET..WII_MAGIC_OFFSET + 4].copy_from_slice(&WII_MAGIC.to_be_bytes());

    // Partition info table (group 0): count = 1, offset/4 = 0x10000 for the
    // group's partition table, which sits right after the info struct.
    let info = WII_PARTITION_INFO_OFFSET as usize;
    let partition_table_offset = info + 0x100; // arbitrary, must point into the file
    data[info..info + 4].copy_from_slice(&1u32.to_be_bytes()); // count
    data[info + 4..info + 8]
        .copy_from_slice(&((partition_table_offset as u32) >> 2).to_be_bytes());
    // groups 1..3 have count = 0 (already zero).

    // The partition table for group 0: one entry { offset/4, type=0 }.
    data[partition_table_offset..partition_table_offset + 4]
        .copy_from_slice(&((PARTITION_OFFSET as u32) >> 2).to_be_bytes());
    data[partition_table_offset + 4..partition_table_offset + 8]
        .copy_from_slice(&0u32.to_be_bytes()); // partition type = 0

    // Build the ticket: title_id at 0x1DC, then the encrypted title key at
    // 0x1BF derived from a known plaintext via AES-CBC with the common key.
    let title_id = [0x00, 0x01, 0x00, 0x00, 0x12, 0x34, 0x56, 0x78];
    let plaintext_title_key = [0xA5u8; 16];

    // Encrypt the title key with the common key, IV = title_id padded.
    let mut iv = [0u8; 16];
    iv[..8].copy_from_slice(&title_id);
    let cipher = Aes128CbcEnc::new_from_slices(&WII_COMMON_KEY, &iv).unwrap();
    let mut enc_key = [0u8; 16];
    cipher
        .encrypt_padded_b2b_mut::<NoPadding>(&plaintext_title_key, &mut enc_key)
        .unwrap();

    let part_off = PARTITION_OFFSET as usize;
    // Ticket bytes: zero-init the ticket region, then drop in the fields we need.
    data[part_off + WII_TICKET_TITLE_ID_OFFSET..part_off + WII_TICKET_TITLE_ID_OFFSET + 8]
        .copy_from_slice(&title_id);
    data[part_off + WII_TICKET_TITLE_KEY_OFFSET
        ..part_off + WII_TICKET_TITLE_KEY_OFFSET + 16]
        .copy_from_slice(&enc_key);
    // common_key_index byte at 0x1F1 stays 0 (standard key).

    // Partition header data_offset / data_size fields.
    let do_word = (DATA_OFFSET_IN_PARTITION >> 2) as u32;
    let ds_word = (data_size >> 2) as u32;
    data[part_off + WII_PARTITION_HEADER_DATA_OFFSET_OFFSET
        ..part_off + WII_PARTITION_HEADER_DATA_OFFSET_OFFSET + 4]
        .copy_from_slice(&do_word.to_be_bytes());
    data[part_off + WII_PARTITION_HEADER_DATA_SIZE_OFFSET
        ..part_off + WII_PARTITION_HEADER_DATA_SIZE_OFFSET + 4]
        .copy_from_slice(&ds_word.to_be_bytes());

    let _ = WII_PARTITION_HEADER_SIZE;
    let _ = WII_TICKET_SIZE;

    // For each cluster: generate 64 plaintext payloads, recompute
    // the hash hierarchy, then AES-CBC encrypt each sector with the
    // title key. The result matches the on-disc byte layout of a
    // real Wii partition.
    let data_start = part_off + DATA_OFFSET_IN_PARTITION as usize;
    for cluster in 0..n_clusters {
        let payloads: Vec<[u8; WII_SECTOR_PAYLOAD_SIZE]> = (0..64)
            .map(|sector_idx| {
                let mut p = [0u8; WII_SECTOR_PAYLOAD_SIZE];
                let seed = (cluster as u8).wrapping_mul(31).wrapping_add(sector_idx as u8);
                for (i, b) in p.iter_mut().enumerate() {
                    *b = ((i as u8).wrapping_mul(13)).wrapping_add(seed);
                }
                p
            })
            .collect();

        let regions = recompute_hash_regions(&payloads);

        for sector_idx in 0..64 {
            let mut sector = [0u8; WII_SECTOR_SIZE];
            sector[..HASH_REGION_BYTES].copy_from_slice(&regions[sector_idx]);
            sector[HASH_REGION_BYTES..].copy_from_slice(&payloads[sector_idx]);
            encrypt_sector(&mut sector, &plaintext_title_key).unwrap();

            let off = data_start + cluster * WII_GROUP_TOTAL_SIZE as usize
                + sector_idx * WII_SECTOR_SIZE;
            data[off..off + WII_SECTOR_SIZE].copy_from_slice(&sector);
        }
    }

    data
}
