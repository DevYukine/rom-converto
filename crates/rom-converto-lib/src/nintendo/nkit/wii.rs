//! NKit Wii planning: maps a native NKit Wii image to restored-disc
//! spans.
//!
//! Semantics from `NkitReaderWii.cs` / `NkitWriterWii.cs`: the
//! 0x50000-byte disc header section is verbatim (NKit fields and the
//! partition table patched on restore); partitions follow in order,
//! each as a verbatim 0x20000-byte partition header (data size at
//! 0x2BC restored from the value preserved inside the partition data
//! at 0x210) plus partition data stored decrypted and hash-stripped.
//! Partition data carries its own NKit header at 0x200, an FST-driven
//! gap walk in hashless coordinates (offsets times 4) seeded by the
//! partition's own junk ID, hash-preservation flags right after
//! fst.bin, and the preserved hash sectors appended after the last
//! gap. Inter-partition filler uses disc-coordinate gap records.
//!
//! Scrubbed partition content (constant-byte fill that must survive
//! re-encryption) is not reconstructed here; the whole-image CRC
//! check rejects anything that does not restore byte-exact.

use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

use super::error::{NkitError, NkitResult};
use super::format::{NKIT_MAGIC_VERSION, NkitHeader, clear_nkit_header};
use super::gaps::{parse_junk_file_record, peek_record_type};
use super::reader::{
    NULLS_LEAD, NkitPlan, Span, SpanKind, WiiGroupSpec, WiiPiece, emit, expand_gap_record,
    read_exact_at,
};
use crate::nintendo::rvl::constants::{
    WII_GROUP_PAYLOAD_SIZE, WII_GROUP_TOTAL_SIZE, WII_SECTOR_PAYLOAD_SIZE, WII_SECTOR_SIZE_U64,
};
use crate::nintendo::rvl::partition::read_partition_info;

const WII_HEADER_SECTION: u64 = 0x50000;
const PARTITION_TABLE_OFFSET: usize = 0x40000;
const PARTITION_TABLE_LENGTH: usize = 0x100;
const PDATA_HEADER: u64 = 0x440;
/// NKit stores and restores the partition header (ticket, TMD, cert
/// chain, H3 table) as one fixed 0x20000-byte block.
const PARTITION_HEADER_LEN: u64 = 0x20000;

struct PartitionEntry {
    /// Offset of the entry's disc-offset word inside the header.
    table_value_offset: usize,
    nkit_offset: u64,
    partition_type: u32,
}

fn parse_partition_table(header: &[u8]) -> NkitResult<Vec<PartitionEntry>> {
    let be32 = |off: usize| u32::from_be_bytes(header[off..off + 4].try_into().unwrap());
    let mut out = Vec::new();
    for table in 0..4 {
        let count = be32(PARTITION_TABLE_OFFSET + table * 8) as usize;
        if count == 0 {
            continue;
        }
        let entries = be32(PARTITION_TABLE_OFFSET + table * 8 + 4) as usize * 4;
        for i in 0..count {
            let at = entries + i * 8;
            if at + 8 > header.len() {
                return Err(NkitError::InvalidHeader(
                    "partition table points outside the header section".into(),
                ));
            }
            out.push(PartitionEntry {
                table_value_offset: at,
                nkit_offset: be32(at) as u64 * 4,
                partition_type: be32(at + 4),
            });
        }
    }
    out.sort_by_key(|p| p.nkit_offset);
    Ok(out)
}

/// Hashless byte count for a hashed extent and back.
fn hashed_to_data(hashed: u64) -> u64 {
    hashed / WII_SECTOR_SIZE_U64 * WII_SECTOR_PAYLOAD_SIZE as u64
}

fn flags_len_for(data_size: u64) -> u64 {
    let hashed = data_size / WII_SECTOR_PAYLOAD_SIZE as u64 * WII_SECTOR_SIZE_U64;
    let groups = hashed.div_ceil(WII_GROUP_TOTAL_SIZE);
    groups.div_ceil(32) * 4
}

/// Pieces of one partition's hashless data stream, in data
/// coordinates.
struct PdataPlan {
    pieces: Vec<(u64, u64, WiiPiece)>,
    hashed_size: u64,
    junk_id: [u8; 4],
    disc_num: u8,
    /// Group-index bitmask of preserved hash groups (MSB-first per
    /// byte) plus the source offset of each group's appended sectors.
    preserved: Vec<Option<u64>>,
    src_end: u64,
}

fn pd_emit(pieces: &mut Vec<(u64, u64, WiiPiece)>, off: u64, len: u64, kind: WiiPiece) {
    if len > 0 {
        pieces.push((off, len, kind));
    }
}

/// Walk one partition's hashless stream starting at `src_base`,
/// mirroring `partitionStreamWrite`.
fn build_pdata_plan<S: Read + Seek>(src: &mut S, src_base: u64) -> NkitResult<PdataPlan> {
    let mut head = vec![0u8; PDATA_HEADER as usize];
    read_exact_at(src, src_base, &mut head)?;

    if head[..4] == [0u8; 4] {
        // Null-ID partition: raw header, a u32 hashed size, one gap.
        let mut sz = [0u8; 4];
        read_exact_at(src, src_base + PDATA_HEADER, &mut sz)?;
        let hashed_size = u32::from_be_bytes(sz) as u64 * 4;
        let data_size = hashed_to_data(hashed_size);
        let mut pieces = Vec::new();
        pd_emit(
            &mut pieces,
            0,
            PDATA_HEADER,
            WiiPiece::Patched(Arc::new(head), 0),
        );
        let mut nkit_pos = src_base + PDATA_HEADER + 4;
        let mut out_pos = PDATA_HEADER;
        let mut nulls_pos = 0u64;
        expand_pd_gap(
            src,
            &mut pieces,
            &mut nkit_pos,
            &mut out_pos,
            &mut nulls_pos,
            true,
        )?;
        if out_pos != data_size {
            return Err(NkitError::InvalidHeader(
                "null-ID partition gap does not cover its data".into(),
            ));
        }
        let groups = hashed_size.div_ceil(WII_GROUP_TOTAL_SIZE) as usize;
        return Ok(PdataPlan {
            pieces,
            hashed_size,
            junk_id: [0u8; 4],
            disc_num: 0,
            preserved: vec![None; groups],
            src_end: nkit_pos,
        });
    }

    if &head[0x200..0x208] != NKIT_MAGIC_VERSION {
        return Err(NkitError::InvalidHeader(
            "partition data carries no NKit header".into(),
        ));
    }
    let hashed_size = u32::from_be_bytes(head[0x210..0x214].try_into().unwrap()) as u64 * 4;
    let data_size = hashed_to_data(hashed_size);
    let junk_id: [u8; 4] = head[..4].try_into().unwrap();
    let disc_num = head[6];
    let dol_addr = u32::from_be_bytes(head[0x420..0x424].try_into().unwrap());
    let fst_off = u32::from_be_bytes(head[0x424..0x428].try_into().unwrap()) as u64 * 4;
    let fst_size = u32::from_be_bytes(head[0x428..0x42C].try_into().unwrap()) as u64 * 4;
    if fst_off < PDATA_HEADER || fst_off + fst_size > data_size {
        return Err(NkitError::InvalidHeader(format!(
            "partition FST {fst_off:#X}+{fst_size:#X} outside its data"
        )));
    }
    clear_nkit_header(&mut head);

    let mut fst = vec![0u8; fst_size as usize];
    read_exact_at(src, src_base + fst_off, &mut fst)?;
    let mut files = super::format::parse_gc_fst(&fst)?;
    for f in &mut files {
        f.data_offset *= 4;
    }
    files.sort_by_key(|f| f.data_offset);

    let flags_len = flags_len_for(data_size);
    let groups = hashed_size.div_ceil(WII_GROUP_TOTAL_SIZE) as usize;
    let mut flags = vec![0u8; flags_len as usize];
    read_exact_at(src, src_base + fst_off + fst_size, &mut flags)?;

    let mut pieces: Vec<(u64, u64, WiiPiece)> = Vec::new();
    let mut head_arc = head;
    let walk_start = (fst_off + fst_size + 3) & !3;
    // The nkit FST carries nkit-internal positions; the restored
    // positions are recomputed as the gaps expand.
    let mut nkit_pos = src_base + walk_start + flags_len;
    let mut out_pos = walk_start;
    let mut nulls_pos = out_pos + NULLS_LEAD;

    for (i, file) in files.iter().enumerate() {
        let target = src_base + file.data_offset;
        if target < nkit_pos {
            return Err(NkitError::InvalidHeader(format!(
                "partition FST file {i} overlaps the previous file"
            )));
        }
        if nkit_pos < target {
            expand_pd_gap(
                src,
                &mut pieces,
                &mut nkit_pos,
                &mut out_pos,
                &mut nulls_pos,
                i == 0,
            )?;
            if nkit_pos > target {
                return Err(NkitError::InvalidGap(
                    "partition gap record extends past the next file".into(),
                ));
            }
            nkit_pos = target;
        }

        if file.size == 0 && peek_record_type(src, nkit_pos).unwrap_or(0) == 3 {
            let rec = parse_junk_file_record(src, nkit_pos)?;
            nkit_pos += rec.consumed;
            let restored_len = (rec.file_len + 3) & !3;
            pd_emit(&mut pieces, out_pos, rec.leading_nulls, WiiPiece::Zeros);
            pd_emit(
                &mut pieces,
                out_pos + rec.leading_nulls,
                restored_len - rec.leading_nulls,
                WiiPiece::Junk {
                    junk_pos: out_pos + rec.leading_nulls,
                },
            );
            patch_wii_fst_entry(&mut fst, file, out_pos, rec.file_len as u32);
            out_pos += restored_len;
            nulls_pos = 0;
        } else {
            let copy_len = ((file.size as u64) + 3) & !3;
            if file.data_offset == dol_addr as u64 * 4 {
                head_arc[0x420..0x424].copy_from_slice(&((out_pos / 4) as u32).to_be_bytes());
            }
            pd_emit(&mut pieces, out_pos, copy_len, WiiPiece::Verbatim(nkit_pos));
            patch_wii_fst_entry(&mut fst, file, out_pos, file.size);
            nkit_pos += copy_len;
            out_pos += copy_len;
            nulls_pos = out_pos + NULLS_LEAD;
        }
    }
    if out_pos < data_size {
        expand_pd_gap(
            src,
            &mut pieces,
            &mut nkit_pos,
            &mut out_pos,
            &mut nulls_pos,
            true,
        )?;
    }
    if out_pos != data_size {
        return Err(NkitError::InvalidHeader(format!(
            "partition data restored to {out_pos:#X} bytes, header declares {data_size:#X}"
        )));
    }

    // Fixed regions: patched header, verbatim up to the FST, the
    // restored FST. Inserted up front so the piece list is ordered.
    let mut fixed = Vec::new();
    pd_emit(
        &mut fixed,
        0,
        PDATA_HEADER,
        WiiPiece::Patched(Arc::new(head_arc), 0),
    );
    pd_emit(
        &mut fixed,
        PDATA_HEADER,
        fst_off - PDATA_HEADER,
        WiiPiece::Verbatim(src_base + PDATA_HEADER),
    );
    pd_emit(
        &mut fixed,
        fst_off,
        fst_size,
        WiiPiece::Patched(Arc::new(fst), 0),
    );
    pd_emit(
        &mut fixed,
        fst_off + fst_size,
        walk_start - (fst_off + fst_size),
        WiiPiece::Verbatim(src_base + fst_off + fst_size),
    );
    fixed.extend(pieces);

    // Preserved hash groups: MSB-first bit per 2 MiB group, sector
    // data appended after the last gap in flagged-group order.
    let mut preserved = vec![None; groups];
    let mut cursor = nkit_pos;
    for (g, slot) in preserved.iter_mut().enumerate() {
        let bit = 0x80 >> (g % 8);
        if flags.get(g / 8).map(|b| b & bit != 0).unwrap_or(false) {
            let blocks = group_blocks(hashed_size, g as u64);
            *slot = Some(cursor);
            cursor += blocks as u64 * 0x400;
        }
    }

    Ok(PdataPlan {
        pieces: fixed,
        hashed_size,
        junk_id,
        disc_num,
        preserved,
        src_end: cursor,
    })
}

fn patch_wii_fst_entry(fst: &mut [u8], file: &super::format::FstFile, out_off: u64, size: u32) {
    fst[file.entry_offset + 4..file.entry_offset + 8]
        .copy_from_slice(&((out_off / 4) as u32).to_be_bytes());
    fst[file.entry_offset + 8..file.entry_offset + 12].copy_from_slice(&size.to_be_bytes());
}

fn group_blocks(hashed_size: u64, group: u64) -> usize {
    let start = group * WII_GROUP_TOTAL_SIZE;
    ((hashed_size - start).min(WII_GROUP_TOTAL_SIZE) / WII_SECTOR_SIZE_U64) as usize
}

/// Partition-data gaps reuse the inter-file gap machinery with junk
/// positions in hashless coordinates.
fn expand_pd_gap<S: Read + Seek>(
    src: &mut S,
    pieces: &mut Vec<(u64, u64, WiiPiece)>,
    nkit_pos: &mut u64,
    out_pos: &mut u64,
    nulls_pos: &mut u64,
    first_or_last: bool,
) -> NkitResult<()> {
    let mut spans: Vec<Span> = Vec::new();
    expand_gap_record(
        &mut spans,
        src,
        nkit_pos,
        out_pos,
        nulls_pos,
        first_or_last,
        [0u8; 4],
        0,
    )?;
    for s in spans {
        let kind = match s.kind {
            SpanKind::Zeros => WiiPiece::Zeros,
            SpanKind::Fill(b) => WiiPiece::Fill(b),
            SpanKind::Junk { .. } => WiiPiece::Junk {
                junk_pos: s.out_off,
            },
            SpanKind::Verbatim(off) => WiiPiece::Verbatim(off),
            _ => unreachable!("gap records expand to data pieces"),
        };
        pd_emit(pieces, s.out_off, s.len, kind);
    }
    Ok(())
}

fn slice_pieces(pieces: &[(u64, u64, WiiPiece)], start: u64, len: u64) -> Vec<(u64, WiiPiece)> {
    let end = start + len;
    let mut out = Vec::new();
    for (off, plen, kind) in pieces {
        let p_end = off + plen;
        if p_end <= start || *off >= end {
            continue;
        }
        let s = start.max(*off);
        let e = end.min(p_end);
        let skip = s - off;
        let kind = match kind {
            WiiPiece::Patched(buf, base) => WiiPiece::Patched(buf.clone(), base + skip),
            WiiPiece::Verbatim(src_off) => WiiPiece::Verbatim(src_off + skip),
            WiiPiece::Junk { junk_pos } => WiiPiece::Junk {
                junk_pos: junk_pos + skip,
            },
            other => other.clone(),
        };
        out.push((e - s, kind));
    }
    debug_assert_eq!(out.iter().map(|(l, _)| l).sum::<u64>(), len);
    out
}

pub(crate) fn build_wii_plan<S: Read + Seek>(
    src: &mut S,
    header: &NkitHeader,
) -> NkitResult<NkitPlan> {
    let nkit_len = src.seek(SeekFrom::End(0))?;
    let image_size = header.image_size;
    let mut disc_header = vec![0u8; WII_HEADER_SECTION as usize];
    read_exact_at(src, 0, &mut disc_header)?;
    let (disc_junk_id, disc_num) = header.junk_identity(&disc_header);
    clear_nkit_header(&mut disc_header);
    disc_header[0x60] = 0;
    disc_header[0x61] = 0;

    let partitions = parse_partition_table(&disc_header)?;
    if partitions.is_empty() {
        return Err(NkitError::InvalidHeader(
            "NKit Wii image declares no partitions".into(),
        ));
    }

    let mut spans: Vec<Span> = Vec::new();
    let mut src_pos = WII_HEADER_SECTION;
    let mut out_pos = WII_HEADER_SECTION;
    let mut warning = None;
    let mut crc_enforce = true;
    let mut prev_type: Option<u32> = None;
    let mut prev_id = disc_junk_id;

    if header.update_partition_crc != 0 {
        // The update partition was removed: an 0x8000-byte filler
        // carries the original partition table; without recovery data
        // the update area restores as zeroes.
        let filler_len = partitions[0].nkit_offset - src_pos;
        let mut filler = vec![0u8; filler_len as usize];
        read_exact_at(src, src_pos, &mut filler)?;
        src_pos += filler_len;
        disc_header[PARTITION_TABLE_OFFSET..PARTITION_TABLE_OFFSET + PARTITION_TABLE_LENGTH]
            .copy_from_slice(&filler[..PARTITION_TABLE_LENGTH]);
        let original = parse_partition_table(&disc_header)?;
        let first_data = original
            .iter()
            .find(|p| p.partition_type != 1)
            .ok_or_else(|| {
                NkitError::InvalidHeader("backup partition table has no data partition".into())
            })?;
        emit(
            &mut spans,
            out_pos,
            first_data.nkit_offset - out_pos,
            SpanKind::Zeros,
        );
        out_pos = first_data.nkit_offset;
        warning = Some(format!(
            "the update partition (CRC32 {:08X}) was removed by NKit and is restored as zeroes; \
             the result is playable but not byte-identical to the original disc",
            header.update_partition_crc
        ));
        crc_enforce = false;
        prev_type = Some(1);
    }

    for part in &partitions {
        if src_pos < part.nkit_offset {
            let zeros_filler = prev_type == Some(1);
            let mut filler_spans: Vec<Span> = Vec::new();
            let mut nulls_pos = out_pos + NULLS_LEAD;
            let junk_id = match prev_type {
                Some(0) => prev_id,
                _ => disc_junk_id,
            };
            expand_gap_record(
                &mut filler_spans,
                src,
                &mut src_pos,
                &mut out_pos,
                &mut nulls_pos,
                true,
                junk_id,
                disc_num,
            )?;
            for mut s in filler_spans {
                if zeros_filler && matches!(s.kind, SpanKind::Junk { .. }) {
                    s.kind = SpanKind::Zeros;
                }
                spans.push(s);
            }
            // Source padding up to the partition start.
            src_pos = part.nkit_offset;
        }

        let mut part_header = vec![0u8; PARTITION_HEADER_LEN as usize];
        read_exact_at(src, src_pos, &mut part_header)?;
        src_pos += PARTITION_HEADER_LEN;

        let pdata = build_pdata_plan(src, src_pos)?;

        // Restore the with-hash data size NKit shrank at 0x2BC.
        part_header[0x2BC..0x2C0].copy_from_slice(&((pdata.hashed_size / 4) as u32).to_be_bytes());

        let info = read_partition_info(
            &mut std::io::Cursor::new(&part_header),
            0,
            0,
            part.partition_type,
        )
        .map_err(|e| NkitError::InvalidHeader(format!("partition header: {e}")))?;
        if info.data_offset != PARTITION_HEADER_LEN {
            return Err(NkitError::InvalidHeader(format!(
                "unsupported partition data offset {:#X}",
                info.data_offset
            )));
        }

        // Patch the partition table entry with the restored offset.
        disc_header[part.table_value_offset..part.table_value_offset + 4]
            .copy_from_slice(&((out_pos / 4) as u32).to_be_bytes());

        emit(
            &mut spans,
            out_pos,
            PARTITION_HEADER_LEN,
            SpanKind::Patched(Arc::new(part_header), 0),
        );
        let data_out = out_pos + PARTITION_HEADER_LEN;
        let groups = pdata.hashed_size.div_ceil(WII_GROUP_TOTAL_SIZE);
        for g in 0..groups {
            let blocks = group_blocks(pdata.hashed_size, g);
            let data_start = g * WII_GROUP_PAYLOAD_SIZE;
            let pieces = slice_pieces(
                &pdata.pieces,
                data_start,
                blocks as u64 * WII_SECTOR_PAYLOAD_SIZE as u64,
            );
            spans.push(Span {
                out_off: data_out + g * WII_GROUP_TOTAL_SIZE,
                len: blocks as u64 * WII_SECTOR_SIZE_U64,
                kind: SpanKind::WiiGroup(Box::new(WiiGroupSpec {
                    pieces,
                    preserved_src: pdata.preserved[g as usize],
                    title_key: info.title_key,
                    blocks,
                    junk_id: pdata.junk_id,
                    disc_num: pdata.disc_num,
                })),
            });
        }

        out_pos = data_out + pdata.hashed_size;
        src_pos = pdata.src_end;
        prev_type = Some(part.partition_type);
        prev_id = pdata.junk_id;
    }

    if src_pos < nkit_len {
        let mut nulls_pos = out_pos + NULLS_LEAD;
        let junk_id = match prev_type {
            Some(0) => prev_id,
            _ => disc_junk_id,
        };
        let zeros_filler = prev_type == Some(1);
        let mut filler_spans: Vec<Span> = Vec::new();
        expand_gap_record(
            &mut filler_spans,
            src,
            &mut src_pos,
            &mut out_pos,
            &mut nulls_pos,
            true,
            junk_id,
            disc_num,
        )?;
        for mut s in filler_spans {
            if zeros_filler && matches!(s.kind, SpanKind::Junk { .. }) {
                s.kind = SpanKind::Zeros;
            }
            spans.push(s);
        }
    }
    if out_pos != image_size {
        return Err(NkitError::InvalidHeader(format!(
            "restored Wii image is {out_pos:#X} bytes but the header declares {image_size:#X}"
        )));
    }

    let mut head_spans = vec![Span {
        out_off: 0,
        len: WII_HEADER_SECTION,
        kind: SpanKind::Patched(Arc::new(disc_header), 0),
    }];
    head_spans.extend(spans);

    Ok(NkitPlan {
        spans: head_spans,
        image_size,
        source_crc: header.source_crc,
        crc_enforce,
        warning,
    })
}
