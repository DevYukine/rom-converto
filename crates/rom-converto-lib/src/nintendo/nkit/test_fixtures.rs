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
    let mut buf = vec![0u8; len];
    LaggedFibonacci::fill_junk(JUNK_ID, DISC_NUM, off, &mut buf);
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

    // Gap 0 (fst end .. file A): junk.
    let walk_start = (fst_off + fst_size + 3) & !3;
    let g0 = junk_at(walk_start as u64, 0x8000 - walk_start);
    iso[walk_start..0x8000].copy_from_slice(&g0);

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
    let junk_run = 0x800;
    let garbage_run = 0x300;
    let fill_run = 0x18000 - g2_start - junk_run - garbage_run;
    let j = junk_at(g2_start as u64, junk_run);
    iso[g2_start..g2_start + junk_run].copy_from_slice(&j);
    for b in iso[g2_start + junk_run..g2_start + junk_run + garbage_run].iter_mut() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *b = state as u8;
        if *b == 0 {
            *b = 1;
        }
    }
    iso[g2_start + junk_run + garbage_run..g2_start + junk_run + garbage_run + fill_run].fill(0xAB);

    // File C: 0x1C NUL bytes then pure junk (a junk file). Gap 3: junk.
    let c_junk = junk_at(0x18000 + 0x1C, 0x6000 - 0x1C);
    iso[0x18000 + 0x1C..0x1E000].copy_from_slice(&c_junk);
    let g3 = junk_at(0x1E000, 0x20000 - 0x1E000);
    iso[0x1E000..0x20000].copy_from_slice(&g3);

    // File D, then trailing junk to the image end.
    for (i, b) in iso[0x20000..0x23000].iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }
    let tail = junk_at(0x23000, IMAGE_SIZE - 0x23000);
    iso[0x23000..].copy_from_slice(&tail);

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

    for f in &files {
        let target = f.data_offset as usize;
        if src_pos < target {
            write_gap(&mut out, iso, src_pos, target);
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
        } else {
            fst_patch.push((f.entry_offset, out.len() as u32, f.size));
            out.extend_from_slice(content);
        }
        src_pos += copy_len;
    }
    if src_pos < iso.len() {
        write_gap(&mut out, iso, src_pos, iso.len());
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

fn write_gap(out: &mut Vec<u8>, iso: &[u8], from: usize, to: usize) {
    let gap = &iso[from..to];
    let len = gap.len();
    assert_eq!(len % 4, 0, "gaps are 4-byte aligned");

    if gap.iter().all(|&b| b == 0) {
        out.extend_from_slice(&((len as u32) | 1).to_be_bytes());
        return;
    }
    if gap[..] == junk_at(from as u64, len)[..] {
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
        let kind = if block == &junk_at((from + off) as u64, n)[..] {
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

/// CRC-32 of a byte slice, for tests.
pub(crate) fn crc_of(bytes: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(bytes);
    c.value()
}

/// Compute the 4-byte big-endian value to place at `off` so the
/// buffer's CRC-32 becomes `target` (what NKit's `CrcForce` does).
/// Brute basis: CRC-32 is GF(2)-linear, so the patch is the solution
/// of a 32x32 bit system.
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
