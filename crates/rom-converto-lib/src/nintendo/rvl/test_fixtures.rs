//! Synthetic Wii disc fixtures for tests.

#![cfg(test)]

use crate::nintendo::rvl::constants::{WII_MAGIC, WII_MAGIC_OFFSET};

use crate::nintendo::rvl::common_keys::WII_COMMON_KEY;
use crate::nintendo::rvl::constants::{
    WII_BLOCKS_PER_GROUP, WII_GROUP_TOTAL_SIZE, WII_PARTITION_HEADER_DATA_OFFSET_OFFSET,
    WII_PARTITION_HEADER_DATA_SIZE_OFFSET, WII_PARTITION_INFO_OFFSET, WII_SECTOR_PAYLOAD_SIZE,
    WII_SECTOR_SIZE, WII_TICKET_TITLE_ID_OFFSET, WII_TICKET_TITLE_KEY_OFFSET,
};
use crate::nintendo::rvl::disc::encrypt_sector;
use crate::nintendo::rvl::partition::{HASH_REGION_BYTES, recompute_hash_regions};
use aes::{
    Aes128,
    cipher::{BlockModeEncrypt, KeyIvInit},
};
use block_padding::NoPadding;
use cbc::Encryptor;

/// Build a U8 archive holding `entries`, creating intermediate directories
/// for every slash-separated path.
pub fn build_u8_archive(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    struct Node {
        is_dir: bool,
        name: String,
        data: Vec<u8>,
        children: Vec<Node>,
    }

    fn insert(root: &mut Node, parts: &[&str], data: Vec<u8>) {
        if parts.len() == 1 {
            root.children.push(Node {
                is_dir: false,
                name: parts[0].to_string(),
                data,
                children: Vec::new(),
            });
            return;
        }
        let head = parts[0];
        let idx = match root
            .children
            .iter()
            .position(|c| c.is_dir && c.name == head)
        {
            Some(i) => i,
            None => {
                root.children.push(Node {
                    is_dir: true,
                    name: head.to_string(),
                    data: Vec::new(),
                    children: Vec::new(),
                });
                root.children.len() - 1
            }
        };
        insert(&mut root.children[idx], &parts[1..], data);
    }

    fn intern(table: &mut Vec<u8>, name: &str) -> u32 {
        if name.is_empty() {
            return 0;
        }
        let off = table.len() as u32;
        table.extend_from_slice(name.as_bytes());
        table.push(0);
        off
    }

    // (is_dir, name_offset, size); a directory's size is the exclusive end
    // index of its subtree, a file's is its byte length.
    fn emit(
        node: &Node,
        nodes: &mut Vec<(bool, u32, u32)>,
        table: &mut Vec<u8>,
        payloads: &mut Vec<Vec<u8>>,
    ) {
        let name_off = intern(table, &node.name);
        let idx = nodes.len();
        nodes.push((node.is_dir, name_off, 0));
        if node.is_dir {
            for child in &node.children {
                emit(child, nodes, table, payloads);
            }
            nodes[idx].2 = nodes.len() as u32;
        } else {
            nodes[idx].2 = node.data.len() as u32;
            payloads.push(node.data.clone());
        }
    }

    let mut root = Node {
        is_dir: true,
        name: String::new(),
        data: Vec::new(),
        children: Vec::new(),
    };
    for (path, data) in entries {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        insert(&mut root, &parts, data.clone());
    }

    let mut nodes: Vec<(bool, u32, u32)> = Vec::new();
    let mut string_table: Vec<u8> = vec![0];
    let mut payloads: Vec<Vec<u8>> = Vec::new();
    emit(&root, &mut nodes, &mut string_table, &mut payloads);

    let node_table_off = 0x20usize;
    let node_table_size = nodes.len() * 12;
    let string_table_off = node_table_off + node_table_size;
    let data_off = (string_table_off + string_table.len() + 0x1F) & !0x1F;

    let mut total = data_off;
    let mut file_offsets: Vec<u32> = Vec::new();
    for payload in &payloads {
        file_offsets.push(total as u32);
        total += payload.len();
    }

    let mut out = vec![0u8; total];
    out[0..4].copy_from_slice(&0x55AA_382Du32.to_be_bytes());
    out[4..8].copy_from_slice(&(node_table_off as u32).to_be_bytes());
    out[8..12].copy_from_slice(&((node_table_size + string_table.len()) as u32).to_be_bytes());
    out[12..16].copy_from_slice(&(data_off as u32).to_be_bytes());

    let mut file_cursor = 0usize;
    for (i, (is_dir, name_off, size)) in nodes.iter().enumerate() {
        let off = node_table_off + i * 12;
        let header = ((*is_dir as u32) << 24) | (name_off & 0x00FF_FFFF);
        out[off..off + 4].copy_from_slice(&header.to_be_bytes());
        let data_offset = if *is_dir {
            0
        } else {
            let v = file_offsets[file_cursor];
            file_cursor += 1;
            v
        };
        out[off + 4..off + 8].copy_from_slice(&data_offset.to_be_bytes());
        out[off + 8..off + 12].copy_from_slice(&size.to_be_bytes());
    }

    out[string_table_off..string_table_off + string_table.len()].copy_from_slice(&string_table);
    let mut cursor = data_off;
    for payload in &payloads {
        out[cursor..cursor + payload.len()].copy_from_slice(payload);
        cursor += payload.len();
    }
    out
}

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

    data[WII_MAGIC_OFFSET..WII_MAGIC_OFFSET + 4].copy_from_slice(&WII_MAGIC.to_be_bytes());

    let info = WII_PARTITION_INFO_OFFSET as usize;
    let partition_table_offset = info + 0x100; // arbitrary, must point into the file
    data[info..info + 4].copy_from_slice(&1u32.to_be_bytes());
    data[info + 4..info + 8].copy_from_slice(&((partition_table_offset as u32) >> 2).to_be_bytes());

    data[partition_table_offset..partition_table_offset + 4]
        .copy_from_slice(&((PARTITION_OFFSET as u32) >> 2).to_be_bytes());
    data[partition_table_offset + 4..partition_table_offset + 8]
        .copy_from_slice(&0u32.to_be_bytes());

    let title_id = [0x00, 0x01, 0x00, 0x00, 0x12, 0x34, 0x56, 0x78];
    let plaintext_title_key = [0xA5u8; 16];

    // IV for AES-CBC title key encryption is the title_id zero-padded to 16 bytes.
    let mut iv = [0u8; 16];
    iv[..8].copy_from_slice(&title_id);
    let cipher = Aes128CbcEnc::new_from_slices(&WII_COMMON_KEY, &iv).unwrap();
    let mut enc_key = [0u8; 16];
    cipher
        .encrypt_padded_b2b::<NoPadding>(&plaintext_title_key, &mut enc_key)
        .unwrap();

    let part_off = PARTITION_OFFSET as usize;
    data[part_off + WII_TICKET_TITLE_ID_OFFSET..part_off + WII_TICKET_TITLE_ID_OFFSET + 8]
        .copy_from_slice(&title_id);
    data[part_off + WII_TICKET_TITLE_KEY_OFFSET..part_off + WII_TICKET_TITLE_KEY_OFFSET + 16]
        .copy_from_slice(&enc_key);
    // common_key_index at 0x1F1 stays 0 (standard key).

    let do_word = (DATA_OFFSET_IN_PARTITION >> 2) as u32;
    let ds_word = (data_size >> 2) as u32;
    data[part_off + WII_PARTITION_HEADER_DATA_OFFSET_OFFSET
        ..part_off + WII_PARTITION_HEADER_DATA_OFFSET_OFFSET + 4]
        .copy_from_slice(&do_word.to_be_bytes());
    data[part_off + WII_PARTITION_HEADER_DATA_SIZE_OFFSET
        ..part_off + WII_PARTITION_HEADER_DATA_SIZE_OFFSET + 4]
        .copy_from_slice(&ds_word.to_be_bytes());

    let data_start = part_off + DATA_OFFSET_IN_PARTITION as usize;
    for cluster in 0..n_clusters {
        let payloads: Vec<[u8; WII_SECTOR_PAYLOAD_SIZE]> = (0..64)
            .map(|sector_idx| {
                let mut p = [0u8; WII_SECTOR_PAYLOAD_SIZE];
                let seed = (cluster as u8)
                    .wrapping_mul(31)
                    .wrapping_add(sector_idx as u8);
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

            let off =
                data_start + cluster * WII_GROUP_TOTAL_SIZE as usize + sector_idx * WII_SECTOR_SIZE;
            data[off..off + WII_SECTOR_SIZE].copy_from_slice(&sector);
        }
    }

    data
}
