//! Synthetic WIA writer for tests, mirroring the layout Dolphin and
//! wit produce: header chain with SHA-1s, codec-compressed metadata
//! tables, raw groups chunked from sector-aligned region starts, and
//! Wii partition groups stored decrypted and hash-stripped with
//! cluster-local exception lists.

use std::io::{Cursor, Write};

use binrw::{BinWrite, Endian};
use sha1::{Digest, Sha1};

use super::format::{
    WIA_COMPR_BZIP2, WIA_COMPR_LZMA, WIA_COMPR_LZMA2, WIA_COMPR_NONE, WIA_COMPR_PURGE, WIA_VERSION,
    WiaGroup, exception_lists_per_group,
};
use crate::nintendo::rvl::constants::{
    WII_BLOCKS_PER_GROUP, WII_SECTOR_PAYLOAD_SIZE, WII_SECTOR_SIZE, WII_SECTOR_SIZE_U64,
};
use crate::nintendo::rvl::disc::{decrypt_sector, read_partition_table};
use crate::nintendo::rvl::is_wii;
use crate::nintendo::rvl::partition::{
    DecryptedCluster, HASH_REGION_BYTES, HashException, PartitionInfo, build_hash_exceptions,
    read_partition_info, recompute_hash_regions_into,
};
use crate::nintendo::rvz::format::sha1::compute_file_head_hash;
use crate::nintendo::rvz::format::{
    WIA_DISC_SIZE, WIA_FILE_HEAD_SIZE, WiaDisc, WiaFileHead, WiaPart, WiaPartData, WiaRawData,
};

pub(crate) fn make_wia(iso: &[u8], compression: u32, chunk_size: u32) -> Vec<u8> {
    assert_eq!(chunk_size % 0x20_0000, 0, "WIA chunk must be 2 MiB aligned");
    let chunk = chunk_size as u64;
    let iso_size = iso.len() as u64;

    let (compr_data, compr_data_len) = props_for(compression, chunk_size);
    let encode = |payload: &[u8], lists: &[Vec<HashException>]| -> Vec<u8> {
        encode_group(compression, chunk_size, lists, payload)
    };

    struct RawRegion {
        off: u64,
        size: u64,
    }
    let mut raw_regions: Vec<RawRegion> = Vec::new();
    let mut partitions: Vec<PartitionInfo> = Vec::new();

    if is_wii(iso[..0x80].try_into().unwrap()) {
        let mut cur = Cursor::new(iso);
        let entries = read_partition_table(&mut cur).unwrap();
        for e in &entries {
            partitions
                .push(read_partition_info(&mut cur, e.offset, e.group, e.partition_type).unwrap());
        }
        partitions.sort_by_key(|p| p.data_start());
        let mut cursor = 0x80u64;
        for p in &partitions {
            let dstart = p.data_start();
            if dstart > cursor {
                raw_regions.push(RawRegion {
                    off: cursor,
                    size: dstart - cursor,
                });
            }
            cursor = dstart + p.data_size;
        }
        if cursor < iso_size {
            raw_regions.push(RawRegion {
                off: cursor,
                size: iso_size - cursor,
            });
        }
    } else {
        raw_regions.push(RawRegion {
            off: 0x80,
            size: iso_size - 0x80,
        });
    }

    let mut groups: Vec<WiaGroup> = Vec::new();
    let mut group_payloads: Vec<Vec<u8>> = Vec::new();

    let mut raw_entries: Vec<WiaRawData> = Vec::new();
    for region in &raw_regions {
        let effective_start = region.off - region.off % WII_SECTOR_SIZE_U64;
        let region_end = region.off + region.size;
        let n_groups = (region_end - effective_start).div_ceil(chunk) as u32;
        let group_index = groups.len() as u32;
        for i in 0..n_groups {
            let start = effective_start + i as u64 * chunk;
            let end = (start + chunk).min(region_end);
            let payload = &iso[start as usize..end as usize];
            push_group(
                &mut groups,
                &mut group_payloads,
                encode(payload, &[]),
                payload,
            );
        }
        raw_entries.push(WiaRawData {
            raw_data_off: region.off,
            raw_data_size: region.size,
            group_index,
            n_groups,
        });
    }

    let n_lists = exception_lists_per_group(chunk_size);
    let mut part_entries: Vec<WiaPart> = Vec::new();
    for p in &partitions {
        let data_start = p.data_start();
        let total_sectors = (p.data_size / WII_SECTOR_SIZE_U64) as u32;
        let spc = (chunk / WII_SECTOR_SIZE_U64) as u32;
        let pd0_sectors = total_sectors.min(WII_BLOCKS_PER_GROUP as u32);
        let pd1_sectors = total_sectors - pd0_sectors;
        let first_sector = (data_start / WII_SECTOR_SIZE_U64) as u32;

        let mut pd = [WiaPartData {
            first_sector: 0,
            n_sectors: 0,
            group_index: 0,
            n_groups: 0,
        }; 2];
        for (idx, (pd_first, pd_sectors)) in [
            (first_sector, pd0_sectors),
            (first_sector + pd0_sectors, pd1_sectors),
        ]
        .into_iter()
        .enumerate()
        {
            if pd_sectors == 0 {
                continue;
            }
            let pd_groups = pd_sectors.div_ceil(spc);
            let group_index = groups.len() as u32;
            for k in 0..pd_groups {
                let sec0 = k * spc;
                let n_sectors = (pd_sectors - sec0).min(spc);
                let rel0 = (pd_first - first_sector) + sec0;
                assert_eq!(rel0 as usize % WII_BLOCKS_PER_GROUP, 0);

                let mut lists: Vec<Vec<HashException>> = Vec::with_capacity(n_lists);
                let mut payload: Vec<u8> =
                    Vec::with_capacity(n_sectors as usize * WII_SECTOR_PAYLOAD_SIZE);
                let clusters = (n_sectors as usize).div_ceil(WII_BLOCKS_PER_GROUP);
                for c in 0..clusters {
                    let csec0 = rel0 as usize + c * WII_BLOCKS_PER_GROUP;
                    let sec_count =
                        (n_sectors as usize - c * WII_BLOCKS_PER_GROUP).min(WII_BLOCKS_PER_GROUP);
                    let cluster = decrypt_cluster_sectors(
                        iso,
                        data_start + csec0 as u64 * WII_SECTOR_SIZE_U64,
                        sec_count,
                        &p.title_key,
                    );
                    let mut recomputed = vec![[0u8; HASH_REGION_BYTES]; WII_BLOCKS_PER_GROUP];
                    let mut padded = cluster.payloads.clone();
                    padded.resize(WII_BLOCKS_PER_GROUP, [0u8; WII_SECTOR_PAYLOAD_SIZE]);
                    recompute_hash_regions_into(&padded, &mut recomputed);
                    lists.push(build_hash_exceptions(&cluster, &recomputed));
                    for s in 0..sec_count {
                        payload.extend_from_slice(&cluster.payloads[s]);
                    }
                }
                lists.resize(n_lists, Vec::new());
                push_group(
                    &mut groups,
                    &mut group_payloads,
                    encode(&payload, &lists),
                    &payload,
                );
            }
            pd[idx] = WiaPartData {
                first_sector: pd_first,
                n_sectors: pd_sectors,
                group_index,
                n_groups: pd_groups,
            };
        }
        part_entries.push(WiaPart {
            part_key: p.title_key,
            pd,
        });
    }

    // Layout: head, disc, partition table, 4-aligned group data, then
    // the codec-compressed raw-data and group tables.
    let part_off = (WIA_FILE_HEAD_SIZE + WIA_DISC_SIZE) as u64;
    let part_tbl = serialize_all(&part_entries);
    let mut blob: Vec<u8> = Vec::new();
    let blob_base = part_off + part_tbl.len() as u64;
    for (g, data) in groups.iter_mut().zip(&group_payloads) {
        if data.is_empty() {
            continue;
        }
        while !(blob_base + blob.len() as u64).is_multiple_of(4) {
            blob.push(0);
        }
        g.data_off4 = ((blob_base + blob.len() as u64) / 4) as u32;
        blob.extend_from_slice(data);
    }

    let raw_tbl_plain = serialize_all(&raw_entries);
    let raw_tbl = encode(&raw_tbl_plain, &[]);
    let raw_data_off = blob_base + blob.len() as u64;

    let group_tbl_plain = serialize_all(&groups);
    let group_tbl = encode(&group_tbl_plain, &[]);
    let group_off = raw_data_off + raw_tbl.len() as u64;

    let mut disc = WiaDisc {
        disc_type: if partitions.is_empty() { 1 } else { 2 },
        compression,
        compr_level: 0,
        chunk_size,
        dhead: iso[..0x80].try_into().unwrap(),
        n_part: part_entries.len() as u32,
        part_t_size: crate::nintendo::rvz::format::WIA_PART_SIZE as u32,
        part_off,
        part_hash: Sha1::digest(&part_tbl).into(),
        n_raw_data: raw_entries.len() as u32,
        raw_data_off,
        raw_data_size: raw_tbl.len() as u32,
        n_groups: groups.len() as u32,
        group_off,
        group_size: group_tbl.len() as u32,
        compr_data_len,
        compr_data,
    };
    if part_entries.is_empty() {
        disc.part_hash = Sha1::digest([]).into();
    }
    let disc_bytes = serialize_one(&disc);

    let wia_file_size = group_off + group_tbl.len() as u64;
    let mut head = WiaFileHead {
        magic: *b"WIA\x01",
        version: WIA_VERSION,
        version_compatible: WIA_VERSION,
        disc_size: WIA_DISC_SIZE as u32,
        disc_hash: Sha1::digest(&disc_bytes).into(),
        iso_file_size: iso_size,
        wia_file_size,
        file_head_hash: [0u8; 20],
    };
    head.file_head_hash = compute_file_head_hash(&head);

    let mut out = Vec::with_capacity(wia_file_size as usize);
    out.extend_from_slice(&serialize_one(&head));
    out.extend_from_slice(&disc_bytes);
    out.extend_from_slice(&part_tbl);
    out.extend_from_slice(&blob);
    out.extend_from_slice(&raw_tbl);
    out.extend_from_slice(&group_tbl);
    assert_eq!(out.len() as u64, wia_file_size);
    out
}

fn push_group(
    groups: &mut Vec<WiaGroup>,
    payload_store: &mut Vec<Vec<u8>>,
    encoded: Vec<u8>,
    plain: &[u8],
) {
    // All-zero groups use the data_size == 0 sentinel.
    if plain.iter().all(|&b| b == 0) {
        groups.push(WiaGroup {
            data_off4: 0,
            data_size: 0,
        });
        payload_store.push(Vec::new());
    } else {
        groups.push(WiaGroup {
            data_off4: 0,
            data_size: encoded.len() as u32,
        });
        payload_store.push(encoded);
    }
}

fn decrypt_cluster_sectors(
    iso: &[u8],
    start: u64,
    sec_count: usize,
    title_key: &[u8; 16],
) -> DecryptedCluster {
    let mut cluster = DecryptedCluster::new();
    for s in 0..sec_count {
        let off = start as usize + s * WII_SECTOR_SIZE;
        let mut sector = [0u8; WII_SECTOR_SIZE];
        sector.copy_from_slice(&iso[off..off + WII_SECTOR_SIZE]);
        decrypt_sector(&mut sector, title_key).unwrap();
        let mut hash = [0u8; HASH_REGION_BYTES];
        hash.copy_from_slice(&sector[..HASH_REGION_BYTES]);
        let mut payload = [0u8; WII_SECTOR_PAYLOAD_SIZE];
        payload.copy_from_slice(&sector[HASH_REGION_BYTES..]);
        cluster.on_disc_hash_regions.push(hash);
        cluster.payloads.push(payload);
    }
    cluster
}

fn serialize_one<T: BinWrite<Args<'static> = ()>>(value: &T) -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    value.write_options(&mut buf, Endian::Big, ()).unwrap();
    buf.into_inner()
}

fn serialize_all<T: BinWrite<Args<'static> = ()>>(values: &[T]) -> Vec<u8> {
    let mut out = Vec::new();
    for v in values {
        out.extend_from_slice(&serialize_one(v));
    }
    out
}

fn serialize_lists(lists: &[Vec<HashException>]) -> Vec<u8> {
    let mut out = Vec::new();
    for list in lists {
        out.extend_from_slice(&(list.len() as u16).to_be_bytes());
        for e in list {
            out.extend_from_slice(&e.offset.to_be_bytes());
            out.extend_from_slice(&e.hash);
        }
    }
    out
}

fn encode_group(
    compression: u32,
    chunk_size: u32,
    lists: &[Vec<HashException>],
    payload: &[u8],
) -> Vec<u8> {
    let exc = serialize_lists(lists);
    match compression {
        WIA_COMPR_NONE => {
            let mut out = exc.clone();
            while !out.len().is_multiple_of(4) {
                out.push(0);
            }
            out.extend_from_slice(payload);
            out
        }
        WIA_COMPR_PURGE => {
            let mut out = exc.clone();
            while !out.len().is_multiple_of(4) {
                out.push(0);
            }
            let mut segments = Vec::new();
            if !payload.iter().all(|&b| b == 0) {
                segments.extend_from_slice(&0u32.to_be_bytes());
                segments.extend_from_slice(&(payload.len() as u32).to_be_bytes());
                segments.extend_from_slice(payload);
            }
            let mut hasher = Sha1::new();
            hasher.update(&exc);
            hasher.update(&segments);
            out.extend_from_slice(&segments);
            let digest: [u8; 20] = hasher.finalize().into();
            out.extend_from_slice(&digest);
            out
        }
        WIA_COMPR_BZIP2 => {
            let mut plain = exc;
            plain.extend_from_slice(payload);
            let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
            enc.write_all(&plain).unwrap();
            enc.finish().unwrap()
        }
        WIA_COMPR_LZMA => {
            let mut plain = exc;
            plain.extend_from_slice(payload);
            let (props, compressed) = lzma_encode_with(&plain, chunk_size as usize);
            let (expected, _) = props_for(WIA_COMPR_LZMA, chunk_size);
            assert_eq!(&props[..], &expected[..5], "LZMA props must be constant");
            compressed
        }
        WIA_COMPR_LZMA2 => {
            let mut plain = exc;
            plain.extend_from_slice(payload);
            let (prop, compressed) = lzma2_encode_with(&plain, chunk_size as usize);
            let (expected, _) = props_for(WIA_COMPR_LZMA2, chunk_size);
            assert_eq!(prop, expected[0], "LZMA2 props must be constant");
            compressed
        }
        other => panic!("unsupported fixture compression {other}"),
    }
}

fn props_for(compression: u32, chunk_size: u32) -> ([u8; 7], u8) {
    let mut data = [0u8; 7];
    match compression {
        WIA_COMPR_LZMA => {
            let (props, _) = lzma_encode_with(&[0u8; 16], chunk_size as usize);
            data[..5].copy_from_slice(&props);
            (data, 5)
        }
        WIA_COMPR_LZMA2 => {
            let (prop, _) = lzma2_encode_with(&[0u8; 16], chunk_size as usize);
            data[0] = prop;
            (data, 1)
        }
        _ => (data, 0),
    }
}

/// One-shot raw LZMA1 encode (no end mark), props derived from the
/// input length, used by codec unit tests.
pub(crate) fn lzma_encode(data: &[u8]) -> ([u8; 5], Vec<u8>) {
    lzma_encode_with(data, data.len().max(16))
}

fn lzma_encode_with(data: &[u8], reduce_size: usize) -> ([u8; 5], Vec<u8>) {
    use lzma_sdk_sys::*;
    unsafe {
        let mut props = CLzmaEncProps::default();
        LzmaEncProps_Init(&mut props);
        props.level = 5;
        props.reduceSize = reduce_size as u64;
        LzmaEncProps_Normalize(&mut props);

        let alloc = Allocator::default();
        let max_out = data.len() + data.len() / 3 + 256;
        let mut out = vec![0u8; max_out];
        let mut out_len = max_out as SizeT;
        let mut props_encoded = [0u8; LZMA_PROPS_SIZE as usize];
        let mut props_size = LZMA_PROPS_SIZE as SizeT;
        let res = LzmaEncode(
            out.as_mut_ptr(),
            &mut out_len,
            data.as_ptr(),
            data.len() as SizeT,
            &props,
            props_encoded.as_mut_ptr(),
            &mut props_size,
            0,
            std::ptr::null_mut(),
            alloc.as_ref(),
            alloc.as_ref(),
        );
        assert_eq!(res, SZ_OK as i32, "LzmaEncode failed");
        out.truncate(out_len as usize);
        (props_encoded, out)
    }
}

/// LZMA2 encode using uncompressed-chunk framing (control 0x01/0x02,
/// 16-bit big-endian length, end marker 0x00). The SDK's LZMA2
/// encoder needs its multi-threaded coder, which `lzma-sdk-sys` does
/// not build, and tests only need a valid stream for the decoder.
pub(crate) fn lzma2_encode(data: &[u8]) -> (u8, Vec<u8>) {
    lzma2_encode_with(data, data.len().max(16))
}

fn lzma2_encode_with(data: &[u8], _reduce_size: usize) -> (u8, Vec<u8>) {
    // Props byte 18 encodes a 2 MiB dictionary: (2 | (p & 1)) << (p / 2 + 11).
    const LZMA2_PROP: u8 = 18;
    let mut out = Vec::with_capacity(data.len() + data.len() / 0x10000 * 3 + 8);
    let mut first = true;
    for chunk in data.chunks(0x10000) {
        out.push(if first { 0x01 } else { 0x02 });
        first = false;
        out.extend_from_slice(&((chunk.len() - 1) as u16).to_be_bytes());
        out.extend_from_slice(chunk);
    }
    out.push(0x00);
    (LZMA2_PROP, out)
}
