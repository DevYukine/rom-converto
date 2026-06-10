//! Synthetic NKit fixtures: a GameCube ISO with a real filesystem
//! (FST, files, junk/zero/mixed gaps, a pure-junk file) and a mirror
//! NKit encoder producing the layout `NkitWriterGc.cs` emits,
//! including the CRC fix-up that makes the container's CRC-32 equal
//! the source image's.

use super::crc::Crc32;
use super::format::{GC_FST_OFFSET_FIELD, GC_FST_SIZE_FIELD, parse_gc_fst};
use crate::nintendo::rvz::packing::LaggedFibonacci;

const JUNK_ID: &[u8; 4] = b"GALE";
const DISC_NUM: u8 = 0;
const BLOCK: usize = 0x100;

fn junk_at(off: u64, len: usize) -> Vec<u8> {
    junk_at_id(JUNK_ID, DISC_NUM, off, len)
}

fn junk_at_id(id: &[u8; 4], disc: u8, off: u64, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    LaggedFibonacci::fill_junk(id, disc, off, &mut buf);
    buf
}

/// Build a small GameCube image with a valid boot header and FST:
/// junk gap, zero gap, mixed gap (junk + garbage + byte fill), a file
/// that is entirely junk, and trailing junk to the image end.
pub(crate) fn make_fake_gc_fs_iso() -> Vec<u8> {
    const IMAGE_SIZE: usize = 0x30000;
    let mut iso = vec![0u8; IMAGE_SIZE];

    iso[..4].copy_from_slice(JUNK_ID);
    iso[6] = DISC_NUM;
    iso[0x1C..0x20].copy_from_slice(&0xC233_9F3Du32.to_be_bytes());

    // Apploader/DOL stand-in: deterministic non-junk bytes.
    for (i, b) in iso[0x440..0x2440].iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(31).wrapping_add(7);
    }

    // FST: root + 4 files (file C is pure junk with a NUL prefix).
    let fst_off = 0x2440usize;
    let files: [(u32, u32); 4] = [
        (0x8000, 0x5000),
        (0x10000, 0x1234),
        (0x18000, 0x6000),
        (0x20000, 0x3000),
    ];
    let n_entries = 1 + files.len();
    let names: &[&str] = &["aaa.bin", "bbb.bin", "ccc.bin", "ddd.bin"];
    let names_len: usize = names.iter().map(|n| n.len() + 1).sum();
    let fst_size = n_entries * 12 + names_len;
    {
        let fst = &mut iso[fst_off..fst_off + fst_size];
        fst[8..12].copy_from_slice(&(n_entries as u32).to_be_bytes());
        let mut name_pos = n_entries * 12;
        for (i, (off, size)) in files.iter().enumerate() {
            let e = (i + 1) * 12;
            let name_rel = (name_pos - n_entries * 12) as u32;
            fst[e..e + 4].copy_from_slice(&name_rel.to_be_bytes());
            fst[e + 4..e + 8].copy_from_slice(&off.to_be_bytes());
            fst[e + 8..e + 12].copy_from_slice(&size.to_be_bytes());
            fst[name_pos..name_pos + names[i].len()].copy_from_slice(names[i].as_bytes());
            name_pos += names[i].len() + 1;
        }
    }
    iso[GC_FST_OFFSET_FIELD..GC_FST_OFFSET_FIELD + 4]
        .copy_from_slice(&(fst_off as u32).to_be_bytes());
    iso[GC_FST_SIZE_FIELD..GC_FST_SIZE_FIELD + 4].copy_from_slice(&(fst_size as u32).to_be_bytes());

    // Gap 0 (fst end .. file A): 0x1C leading NULs then junk.
    let walk_start = (fst_off + fst_size + 3) & !3;
    let g0 = junk_at(walk_start as u64 + 0x1C, 0x8000 - walk_start - 0x1C);
    iso[walk_start + 0x1C..0x8000].copy_from_slice(&g0);

    // File A: compressible-ish content. Gap 1 (A end .. B): zeros.
    for (i, b) in iso[0x8000..0xD000].iter_mut().enumerate() {
        *b = (i / 64) as u8;
    }

    // File B: noise; unaligned size, trailing bytes to 4-byte boundary
    // stay zero. Gap 2 (align4(B end) .. C): mixed.
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    for b in iso[0x10000..0x11234].iter_mut() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *b = state as u8;
    }
    let g2_start = 0x11238usize;
    let garbage_run = 0x300;
    let junk_run = 0x800;
    let fill_run = 0x18000 - g2_start - junk_run - garbage_run;
    for b in iso[g2_start..g2_start + garbage_run].iter_mut() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *b = state as u8;
        if *b == 0 {
            *b = 1;
        }
    }
    let j = junk_at((g2_start + garbage_run) as u64, junk_run);
    iso[g2_start + garbage_run..g2_start + garbage_run + junk_run].copy_from_slice(&j);
    iso[g2_start + garbage_run + junk_run..g2_start + garbage_run + junk_run + fill_run].fill(0xAB);

    // File C: 0x1C NUL bytes then pure junk (a junk file). Gap 3: junk.
    let c_junk = junk_at(0x18000 + 0x1C, 0x6000 - 0x1C);
    iso[0x18000 + 0x1C..0x1E000].copy_from_slice(&c_junk);
    let g3 = junk_at(0x1E000, 0x20000 - 0x1E000);
    iso[0x1E000..0x20000].copy_from_slice(&g3);

    // File D, then 0x1C NULs and trailing junk to the image end.
    for (i, b) in iso[0x20000..0x23000].iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }
    let tail = junk_at(0x23000 + 0x1C, IMAGE_SIZE - 0x23000 - 0x1C);
    iso[0x23000 + 0x1C..].copy_from_slice(&tail);

    iso
}

/// Encode a GameCube ISO to the native NKit layout.
pub(crate) fn make_nkit_gc(iso: &[u8]) -> Vec<u8> {
    let fst_off = u32::from_be_bytes(
        iso[GC_FST_OFFSET_FIELD..GC_FST_OFFSET_FIELD + 4]
            .try_into()
            .unwrap(),
    ) as usize;
    let fst_size = u32::from_be_bytes(
        iso[GC_FST_SIZE_FIELD..GC_FST_SIZE_FIELD + 4]
            .try_into()
            .unwrap(),
    ) as usize;
    let mut files = parse_gc_fst(&iso[fst_off..fst_off + fst_size]).unwrap();
    files.sort_by_key(|f| f.data_offset);

    let walk_start = (fst_off + fst_size + 3) & !3;
    let mut out = iso[..walk_start].to_vec();
    let mut fst_patch: Vec<(usize, u32, u32)> = Vec::new();
    let mut src_pos = walk_start;
    let mut nulls_pos = walk_start + 0x1C;

    for (i, f) in files.iter().enumerate() {
        let target = f.data_offset as usize;
        if src_pos < target {
            let lead = gap_lead(target - src_pos, nulls_pos.saturating_sub(src_pos), i == 0);
            write_gap(&mut out, iso, src_pos, target, lead);
        }
        src_pos = target;
        let copy_len = ((f.size as usize) + 3) & !3;
        let content = &iso[src_pos..src_pos + copy_len];

        // Junk-file detection: optional NUL prefix, junk to the end.
        let nulls = content.iter().take_while(|&&b| b == 0).count();
        let is_junk_file = f.size > 0
            && nulls < f.size as usize
            && content[nulls..] == junk_at((src_pos + nulls) as u64, copy_len - nulls)[..];
        if is_junk_file {
            fst_patch.push((f.entry_offset, out.len() as u32, 0));
            out.extend_from_slice(&((((nulls as u32) << 2) | 3).to_be_bytes()));
            out.extend_from_slice(&f.size.to_be_bytes());
            nulls_pos = 0;
        } else {
            fst_patch.push((f.entry_offset, out.len() as u32, f.size));
            out.extend_from_slice(content);
            nulls_pos = src_pos + copy_len + 0x1C;
        }
        src_pos += copy_len;
    }
    if src_pos < iso.len() {
        let lead = gap_lead(iso.len() - src_pos, nulls_pos.saturating_sub(src_pos), true);
        write_gap(&mut out, iso, src_pos, iso.len(), lead);
    }

    for (entry_offset, off, size) in fst_patch {
        let e = fst_off + entry_offset;
        out[e + 4..e + 8].copy_from_slice(&off.to_be_bytes());
        out[e + 8..e + 12].copy_from_slice(&size.to_be_bytes());
    }

    let mut src_crc = Crc32::new();
    src_crc.update(iso);
    let src_crc = src_crc.value();
    out[0x200..0x208].copy_from_slice(b"NKIT v01");
    out[0x208..0x20C].copy_from_slice(&src_crc.to_be_bytes());
    out[0x20C..0x210].fill(0);
    out[0x210..0x214].copy_from_slice(&(iso.len() as u32).to_be_bytes());
    out[0x214..0x218].fill(0);
    out[0x218..0x21C].fill(0);

    let fixup = force_crc(&out, 0x20C, src_crc);
    out[0x20C..0x210].copy_from_slice(&fixup.to_be_bytes());
    out
}

fn gap_lead(size: usize, max_nulls: usize, first_or_last: bool) -> usize {
    if size < max_nulls {
        size
    } else if size >= 0x40000 && !first_or_last {
        0
    } else {
        max_nulls
    }
}

fn write_gap(out: &mut Vec<u8>, iso: &[u8], from: usize, to: usize, lead: usize) {
    write_gap_id(out, iso, from, to, lead, JUNK_ID, DISC_NUM)
}

fn write_gap_id(
    out: &mut Vec<u8>,
    iso: &[u8],
    from: usize,
    to: usize,
    lead: usize,
    id: &[u8; 4],
    disc: u8,
) {
    let gap = &iso[from..to];
    let len = gap.len();
    assert_eq!(len % 4, 0, "gaps are 4-byte aligned");

    if gap.iter().all(|&b| b == 0) {
        out.extend_from_slice(&((len as u32) | 1).to_be_bytes());
        return;
    }
    if gap[..lead].iter().all(|&b| b == 0)
        && gap[lead..] == junk_at_id(id, disc, (from + lead) as u64, len - lead)[..]
    {
        out.extend_from_slice(&(len as u32).to_be_bytes());
        return;
    }

    // Mixed: classify 256-byte blocks, merge runs.
    #[derive(PartialEq, Clone, Copy)]
    enum Kind {
        Junk,
        Fill(u8),
        NonJunk,
    }
    let mut kinds = Vec::new();
    let mut off = 0usize;
    while off < len {
        let n = (len - off).min(BLOCK);
        let block = &gap[off..off + n];
        let kind = if block == &junk_at_id(id, disc, (from + off) as u64, n)[..] {
            Kind::Junk
        } else if block.iter().all(|&b| b == block[0]) {
            Kind::Fill(block[0])
        } else {
            Kind::NonJunk
        };
        kinds.push((kind, n));
        off += n;
    }

    out.extend_from_slice(&((len as u32) | 2).to_be_bytes());
    let mut i = 0usize;
    let mut gap_off = 0usize;
    while i < kinds.len() {
        let (kind, _) = kinds[i];
        let mut blocks = 0usize;
        let mut bytes = 0usize;
        while i < kinds.len() && kinds[i].0 == kind {
            blocks += 1;
            bytes += kinds[i].1;
            i += 1;
        }
        match kind {
            Kind::Junk => out.extend_from_slice(&(blocks as u32).to_be_bytes()),
            Kind::NonJunk => {
                out.extend_from_slice(&((1u32 << 30) | blocks as u32).to_be_bytes());
                out.extend_from_slice(&gap[gap_off..gap_off + bytes]);
            }
            Kind::Fill(b) => out.extend_from_slice(
                &((2u32 << 30) | ((blocks as u32) << 8) | b as u32).to_be_bytes(),
            ),
        }
        gap_off += bytes;
    }
}

/// Wrap an nkit stream in a GCZ container and force the container's
/// whole-file CRC-32 to the source CRC; NKit places this fix-up in
/// the GCZ header's sub_type field at offset 0x4.
pub(crate) fn make_nkit_gcz(nkit: &[u8], source_crc: u32) -> Vec<u8> {
    let mut gcz = crate::nintendo::gcz::test_fixtures::make_gcz(nkit, 0x8000, 0);
    let fixup = force_crc(&gcz, 0x4, source_crc);
    gcz[0x4..0x8].copy_from_slice(&fixup.to_be_bytes());
    gcz
}

pub(crate) fn crc_of(bytes: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(bytes);
    c.value()
}

/// Compute the 4-byte big-endian value to place at `off` so the
/// buffer's CRC-32 becomes `target` (what NKit's `CrcForce` does).
/// CRC-32 is GF(2)-linear, so the patch is the solution of a 32x32
/// bit system whose basis is probed one bit at a time.
fn force_crc(buf: &[u8], off: usize, target: u32) -> u32 {
    let mut work = buf.to_vec();
    work[off..off + 4].fill(0);
    let crc_of = |b: &[u8]| {
        let mut c = Crc32::new();
        c.update(b);
        c.value()
    };
    let base = crc_of(&work);

    let mut cols = [0u32; 32];
    for (i, col) in cols.iter_mut().enumerate() {
        work[off + i / 8] ^= 1 << (i % 8);
        *col = crc_of(&work) ^ base;
        work[off + i / 8] ^= 1 << (i % 8);
    }

    let target_delta = base ^ target;
    let x = solve_gf2(&cols, target_delta).expect("CRC forcing system must be solvable");

    let mut patch = [0u8; 4];
    for i in 0..32 {
        if x >> i & 1 == 1 {
            patch[i / 8] ^= 1 << (i % 8);
        }
    }
    u32::from_be_bytes(patch)
}

fn solve_gf2(cols: &[u32; 32], target: u32) -> Option<u32> {
    let mut rows = [0u64; 32];
    for (r, row) in rows.iter_mut().enumerate() {
        for (j, col) in cols.iter().enumerate() {
            if col >> r & 1 == 1 {
                *row |= 1 << j;
            }
        }
        if target >> r & 1 == 1 {
            *row |= 1 << 32;
        }
    }
    let mut pivot_of_col = [usize::MAX; 32];
    let mut rank = 0usize;
    for (c, pivot) in pivot_of_col.iter_mut().enumerate() {
        let Some(p) = (rank..32).find(|&i| rows[i] >> c & 1 == 1) else {
            continue;
        };
        rows.swap(rank, p);
        for i in 0..32 {
            if i != rank && rows[i] >> c & 1 == 1 {
                rows[i] ^= rows[rank];
            }
        }
        *pivot = rank;
        rank += 1;
    }
    if rows[rank..].iter().any(|r| r >> 32 & 1 == 1) {
        return None;
    }
    let mut x = 0u32;
    for (c, &pivot) in pivot_of_col.iter().enumerate() {
        if pivot != usize::MAX && rows[pivot] >> 32 & 1 == 1 {
            x |= 1 << c;
        }
    }
    Some(x)
}

use crate::nintendo::rvl::common_keys::WII_COMMON_KEY;
use crate::nintendo::rvl::constants::{
    WII_BLOCKS_PER_GROUP, WII_GROUP_PAYLOAD_SIZE, WII_GROUP_TOTAL_SIZE, WII_MAGIC,
    WII_MAGIC_OFFSET, WII_PARTITION_HEADER_DATA_OFFSET_OFFSET,
    WII_PARTITION_HEADER_DATA_SIZE_OFFSET, WII_PARTITION_INFO_OFFSET, WII_SECTOR_PAYLOAD_SIZE,
    WII_SECTOR_SIZE, WII_TICKET_TITLE_ID_OFFSET, WII_TICKET_TITLE_KEY_OFFSET,
};
use crate::nintendo::rvl::disc::decrypt_sector;
use crate::nintendo::rvl::partition::{
    HASH_REGION_BYTES, recompute_hash_regions_into, reencrypt_cluster_into,
};

const WII_DISC_ID: &[u8; 4] = b"RNKE";
const WII_PART_ID: &[u8; 4] = b"GALE";
const PART_OFFSET: usize = 0x60000;
const PART_DATA_OFFSET: usize = 0x20000;
const N_CLUSTERS: usize = 3;
const TRAILING: usize = 0x8000;

/// Build a Wii ISO with one valid encrypted partition whose decrypted
/// content is a real filesystem with junk gaps, plus a deliberately
/// corrupted hash region in cluster 1 to force NKit hash preservation.
pub(crate) fn make_fake_wii_fs_iso() -> Vec<u8> {
    let data_size = N_CLUSTERS * WII_GROUP_PAYLOAD_SIZE as usize;
    let hashed_size = N_CLUSTERS as u64 * WII_GROUP_TOTAL_SIZE;
    let total = PART_OFFSET + PART_DATA_OFFSET + hashed_size as usize + TRAILING;
    let mut iso = vec![0u8; total];

    iso[..4].copy_from_slice(WII_DISC_ID);
    iso[6] = DISC_NUM;
    iso[WII_MAGIC_OFFSET..WII_MAGIC_OFFSET + 4].copy_from_slice(&WII_MAGIC.to_be_bytes());

    // Partition table: one data partition.
    let info = WII_PARTITION_INFO_OFFSET as usize;
    let table = info + 0x20;
    iso[info..info + 4].copy_from_slice(&1u32.to_be_bytes());
    iso[info + 4..info + 8].copy_from_slice(&((table as u32) >> 2).to_be_bytes());
    iso[table..table + 4].copy_from_slice(&((PART_OFFSET as u32) >> 2).to_be_bytes());

    // Ticket with a recoverable title key (same scheme as the rvl fixture).
    let title_id = [0x00, 0x01, 0x00, 0x00, 0x4E, 0x4B, 0x49, 0x54];
    let plaintext_title_key = [0x5Au8; 16];
    let mut iv = [0u8; 16];
    iv[..8].copy_from_slice(&title_id);
    use aes::cipher::{BlockEncryptMut, KeyIvInit};
    let cipher = cbc::Encryptor::<aes::Aes128>::new_from_slices(&WII_COMMON_KEY, &iv).unwrap();
    let mut enc_key = [0u8; 16];
    cipher
        .encrypt_padded_b2b_mut::<block_padding::NoPadding>(&plaintext_title_key, &mut enc_key)
        .unwrap();
    iso[PART_OFFSET + WII_TICKET_TITLE_ID_OFFSET..PART_OFFSET + WII_TICKET_TITLE_ID_OFFSET + 8]
        .copy_from_slice(&title_id);
    iso[PART_OFFSET + WII_TICKET_TITLE_KEY_OFFSET..PART_OFFSET + WII_TICKET_TITLE_KEY_OFFSET + 16]
        .copy_from_slice(&enc_key);
    let do_word = ((PART_DATA_OFFSET as u64) >> 2) as u32;
    let ds_word = (hashed_size >> 2) as u32;
    iso[PART_OFFSET + WII_PARTITION_HEADER_DATA_OFFSET_OFFSET
        ..PART_OFFSET + WII_PARTITION_HEADER_DATA_OFFSET_OFFSET + 4]
        .copy_from_slice(&do_word.to_be_bytes());
    iso[PART_OFFSET + WII_PARTITION_HEADER_DATA_SIZE_OFFSET
        ..PART_OFFSET + WII_PARTITION_HEADER_DATA_SIZE_OFFSET + 4]
        .copy_from_slice(&ds_word.to_be_bytes());

    // Decrypted partition content: boot header, FST (offsets / 4),
    // two files, junk and zero gaps, trailing junk.
    let mut data = vec![0u8; data_size];
    data[..4].copy_from_slice(WII_PART_ID);
    data[6] = DISC_NUM;
    let fst_off = 0x2440usize;
    let files: [(u32, u32); 2] = [(0x8000, 0x5000), (0x40000, 0x9000)];
    let n_entries = 1 + files.len();
    let fst_size = n_entries * 12 + 16;
    {
        let fst = &mut data[fst_off..fst_off + fst_size];
        fst[8..12].copy_from_slice(&(n_entries as u32).to_be_bytes());
        for (i, (off, size)) in files.iter().enumerate() {
            let e = (i + 1) * 12;
            fst[e + 4..e + 8].copy_from_slice(&(off >> 2).to_be_bytes());
            fst[e + 8..e + 12].copy_from_slice(&size.to_be_bytes());
        }
    }
    data[0x420..0x424].copy_from_slice(&((0x440u32) >> 2).to_be_bytes());
    data[0x424..0x428].copy_from_slice(&((fst_off as u32) >> 2).to_be_bytes());
    data[0x428..0x42C].copy_from_slice(&((fst_size as u32) >> 2).to_be_bytes());
    for (i, b) in data[0x440..0x2440].iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(13).wrapping_add(3);
    }
    let walk = (fst_off + fst_size + 3) & !3;
    let g0 = junk_at_id(
        WII_PART_ID,
        DISC_NUM,
        walk as u64 + 0x1C,
        0x8000 - walk - 0x1C,
    );
    data[walk + 0x1C..0x8000].copy_from_slice(&g0);
    for (i, b) in data[0x8000..0xD000].iter_mut().enumerate() {
        *b = (i % 253) as u8;
    }
    // Zero gap [0xD000, 0x40000) stays zero; file B then trailing junk.
    for (i, b) in data[0x40000..0x49000].iter_mut().enumerate() {
        *b = (i % 247) as u8;
    }
    let tail = junk_at_id(
        WII_PART_ID,
        DISC_NUM,
        0x49000 + 0x1C,
        data_size - 0x49000 - 0x1C,
    );
    data[0x49000 + 0x1C..].copy_from_slice(&tail);

    // Hash and encrypt clusters; corrupt one hash byte in cluster 1
    // so NKit must preserve that group's hash sectors.
    let data_start = PART_OFFSET + PART_DATA_OFFSET;
    let mut payloads = vec![[0u8; WII_SECTOR_PAYLOAD_SIZE]; WII_BLOCKS_PER_GROUP];
    let mut regions = vec![[0u8; HASH_REGION_BYTES]; WII_BLOCKS_PER_GROUP];
    let mut cluster_out = vec![0u8; WII_GROUP_TOTAL_SIZE as usize];
    for c in 0..N_CLUSTERS {
        for (s, p) in payloads.iter_mut().enumerate() {
            let off = c * WII_GROUP_PAYLOAD_SIZE as usize + s * WII_SECTOR_PAYLOAD_SIZE;
            p.copy_from_slice(&data[off..off + WII_SECTOR_PAYLOAD_SIZE]);
        }
        recompute_hash_regions_into(&payloads, &mut regions);
        if c == 1 {
            regions[5][0x37] ^= 0xA5;
        }
        reencrypt_cluster_into(&regions, &payloads, &plaintext_title_key, &mut cluster_out)
            .unwrap();
        let off = data_start + c * WII_GROUP_TOTAL_SIZE as usize;
        iso[off..off + WII_GROUP_TOTAL_SIZE as usize].copy_from_slice(&cluster_out);
    }

    // Filler before the partition and trailing region: disc-level junk
    // with the leading-NUL rule.
    let f0 = junk_at_id(
        WII_DISC_ID,
        DISC_NUM,
        0x50000 + 0x1C,
        PART_OFFSET - 0x50000 - 0x1C,
    );
    iso[0x50000 + 0x1C..PART_OFFSET].copy_from_slice(&f0);
    let t0 = junk_at_id(
        WII_PART_ID,
        DISC_NUM,
        (total - TRAILING + 0x1C) as u64,
        TRAILING - 0x1C,
    );
    iso[total - TRAILING + 0x1C..].copy_from_slice(&t0);

    iso
}

/// Encode a Wii ISO to the native NKit layout (no update partition).
pub(crate) fn make_nkit_wii(iso: &[u8]) -> Vec<u8> {
    let data_start = PART_OFFSET + PART_DATA_OFFSET;
    let hashed_size = N_CLUSTERS as u64 * WII_GROUP_TOTAL_SIZE;
    let data_size = N_CLUSTERS * WII_GROUP_PAYLOAD_SIZE as usize;
    let title_key = [0x5Au8; 16];

    // Decrypt the partition into hashless data plus on-disc hash regions.
    let mut data = vec![0u8; data_size];
    let mut on_disc = vec![[0u8; HASH_REGION_BYTES]; N_CLUSTERS * WII_BLOCKS_PER_GROUP];
    for c in 0..N_CLUSTERS {
        for s in 0..WII_BLOCKS_PER_GROUP {
            let off = data_start + c * WII_GROUP_TOTAL_SIZE as usize + s * WII_SECTOR_SIZE;
            let mut sector = [0u8; WII_SECTOR_SIZE];
            sector.copy_from_slice(&iso[off..off + WII_SECTOR_SIZE]);
            decrypt_sector(&mut sector, &title_key).unwrap();
            on_disc[c * WII_BLOCKS_PER_GROUP + s].copy_from_slice(&sector[..HASH_REGION_BYTES]);
            let doff = c * WII_GROUP_PAYLOAD_SIZE as usize + s * WII_SECTOR_PAYLOAD_SIZE;
            data[doff..doff + WII_SECTOR_PAYLOAD_SIZE]
                .copy_from_slice(&sector[HASH_REGION_BYTES..]);
        }
    }

    // Preserved groups: recomputed hashes that do not match on-disc.
    let mut payloads = vec![[0u8; WII_SECTOR_PAYLOAD_SIZE]; WII_BLOCKS_PER_GROUP];
    let mut regions = vec![[0u8; HASH_REGION_BYTES]; WII_BLOCKS_PER_GROUP];
    let mut preserved_flags = [false; N_CLUSTERS];
    let mut preserved_bytes: Vec<u8> = Vec::new();
    for c in 0..N_CLUSTERS {
        for (s, p) in payloads.iter_mut().enumerate() {
            let off = c * WII_GROUP_PAYLOAD_SIZE as usize + s * WII_SECTOR_PAYLOAD_SIZE;
            p.copy_from_slice(&data[off..off + WII_SECTOR_PAYLOAD_SIZE]);
        }
        recompute_hash_regions_into(&payloads, &mut regions);
        if (0..WII_BLOCKS_PER_GROUP).any(|s| on_disc[c * WII_BLOCKS_PER_GROUP + s] != regions[s]) {
            preserved_flags[c] = true;
            for s in 0..WII_BLOCKS_PER_GROUP {
                preserved_bytes.extend_from_slice(&on_disc[c * WII_BLOCKS_PER_GROUP + s]);
            }
        }
    }

    // Partition data stream: NKit pdata header, verbatim to FST,
    // patched FST, flags, file walk with gap records, hash sectors.
    let fst_off = u32::from_be_bytes(data[0x424..0x428].try_into().unwrap()) as usize * 4;
    let fst_size = u32::from_be_bytes(data[0x428..0x42C].try_into().unwrap()) as usize * 4;
    let mut files = parse_gc_fst(&data[fst_off..fst_off + fst_size]).unwrap();
    for f in &mut files {
        f.data_offset *= 4;
    }
    files.sort_by_key(|f| f.data_offset);

    let groups = hashed_size.div_ceil(WII_GROUP_TOTAL_SIZE) as usize;
    let flags_len = groups.div_ceil(32) * 4;
    let mut flags = vec![0u8; flags_len];
    for (g, &p) in preserved_flags.iter().enumerate() {
        if p {
            flags[g / 8] |= 0x80 >> (g % 8);
        }
    }

    let walk = (fst_off + fst_size + 3) & !3;
    let mut pdata = data[..walk].to_vec();
    pdata[0x200..0x208].copy_from_slice(b"NKIT v01");
    pdata[0x210..0x214].copy_from_slice(&((hashed_size / 4) as u32).to_be_bytes());
    pdata.extend_from_slice(&flags);
    let mut fst_patch: Vec<(usize, u32)> = Vec::new();
    let mut src_pos = walk;
    let mut nulls_pos = walk + 0x1C;
    for (i, f) in files.iter().enumerate() {
        let target = f.data_offset as usize;
        if src_pos < target {
            let lead = gap_lead(target - src_pos, nulls_pos.saturating_sub(src_pos), i == 0);
            write_gap_id(
                &mut pdata,
                &data,
                src_pos,
                target,
                lead,
                WII_PART_ID,
                DISC_NUM,
            );
        }
        src_pos = target;
        let copy_len = ((f.size as usize) + 3) & !3;
        fst_patch.push((f.entry_offset, (pdata.len() as u32) >> 2));
        pdata.extend_from_slice(&data[src_pos..src_pos + copy_len]);
        nulls_pos = src_pos + copy_len + 0x1C;
        src_pos += copy_len;
    }
    if src_pos < data.len() {
        let lead = gap_lead(
            data.len() - src_pos,
            nulls_pos.saturating_sub(src_pos),
            true,
        );
        write_gap_id(
            &mut pdata,
            &data,
            src_pos,
            data.len(),
            lead,
            WII_PART_ID,
            DISC_NUM,
        );
    }
    pdata.extend_from_slice(&preserved_bytes);
    for (entry_offset, off_w) in fst_patch {
        let e = fst_off + entry_offset;
        pdata[e + 4..e + 8].copy_from_slice(&off_w.to_be_bytes());
    }

    let mut out = iso[..0x50000].to_vec();
    {
        let lead = gap_lead(PART_OFFSET - 0x50000, 0x1C, true);
        let iso_ref = iso;
        write_gap_id(
            &mut out,
            iso_ref,
            0x50000,
            PART_OFFSET,
            lead,
            WII_DISC_ID,
            DISC_NUM,
        );
    }
    // Pad to 0x8000 like the writer does before each partition.
    while !out.len().is_multiple_of(0x8000) {
        out.push(0);
    }
    let nkit_part_off = out.len();
    out.extend_from_slice(&iso[PART_OFFSET..PART_OFFSET + PART_DATA_OFFSET]);
    let shrunk_len = pdata.len() as u64;
    let p2bc = nkit_part_off + WII_PARTITION_HEADER_DATA_SIZE_OFFSET;
    out.extend_from_slice(&pdata);
    out[p2bc..p2bc + 4].copy_from_slice(&((shrunk_len / 4) as u32).to_be_bytes());
    // Patch the partition table entry to the nkit offset.
    let table = WII_PARTITION_INFO_OFFSET as usize + 0x20;
    out[table..table + 4].copy_from_slice(&((nkit_part_off as u32) >> 2).to_be_bytes());
    {
        let from = iso.len() - TRAILING;
        let lead = gap_lead(TRAILING, 0x1C, true);
        let mut rec = Vec::new();
        write_gap_id(&mut rec, iso, from, iso.len(), lead, WII_PART_ID, DISC_NUM);
        out.extend_from_slice(&rec);
    }

    // Disc-level NKit header plus the CRC fix-up.
    let src_crc = crc_of(iso);
    out[0x60] = 1;
    out[0x61] = 1;
    out[0x200..0x208].copy_from_slice(b"NKIT v01");
    out[0x208..0x20C].copy_from_slice(&src_crc.to_be_bytes());
    out[0x20C..0x210].fill(0);
    out[0x210..0x214].copy_from_slice(&((iso.len() as u32) / 4).to_be_bytes());
    out[0x214..0x218].fill(0);
    out[0x218..0x21C].fill(0);
    let fixup = force_crc(&out, 0x20C, src_crc);
    out[0x20C..0x210].copy_from_slice(&fixup.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn force_crc_hits_target() {
        let mut buf: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
        let target = 0xDEAD_BEEFu32;
        let fixup = force_crc(&buf, 100, target);
        buf[100..104].copy_from_slice(&fixup.to_be_bytes());
        let mut c = Crc32::new();
        c.update(&buf);
        assert_eq!(c.value(), target);
    }
}
