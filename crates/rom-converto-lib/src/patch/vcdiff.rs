//! VCDIFF decoder for the delta file format of RFC 3284.
//!
//! The header magic is `D6 C3 C4 00` followed by a header indicator.
//! Secondary compression and application-defined code tables are rejected
//! with a clear error; an application header (indicator bit 2) is skipped.
//! Each window declares its source segment (`VCD_SOURCE` from the source
//! file, `VCD_TARGET` from the output written so far, never both), a target
//! window size capped at 1 GiB with the total output capped at the source
//! size plus 1 GiB, three length-delimited sections (data for ADDs and
//! RUNs, instructions, COPY addresses), and an optional Adler32 of the
//! decoded window, which sits right after the section lengths. Instructions
//! come from the RFC's default code table; COPY addresses go through the
//! near/same address caches. A COPY may read target bytes written earlier
//! in the same window, including overlapping forward copies, and a match
//! that starts in the source segment may run on into the target window.
//! Every declared size is bounded by its section, the window, or the
//! output written so far before it is used; nothing is allocated from a
//! declared size.

use super::io::{self, Range};
use crate::util::{CancelToken, Cancelled};
use anyhow::{Context, Result, bail, ensure};
use std::fs::File;
use std::path::Path;

const VCD_SOURCE: u8 = 0x01;
const VCD_TARGET: u8 = 0x02;
/// Extension bit carried in the window indicator: four big-endian bytes of
/// Adler32 over the decoded window follow the section lengths.
const VCD_CHECKSUM: u8 = 0x04;

const NOOP: u8 = 0;
const ADD: u8 = 1;
const RUN: u8 = 2;
const COPY: u8 = 3;

#[derive(Clone, Copy)]
struct Instruction {
    inst: u8,
    size: u8,
    mode: u8,
}

pub(super) fn apply(
    patch_path: &Path,
    source_path: &Path,
    out: &File,
    buf: &mut [u8],
    cancel: &CancelToken,
) -> Result<()> {
    let patch = File::open(patch_path).context("opening the VCDIFF patch")?;
    let patch_len = patch.metadata()?.len();
    ensure!(patch_len >= 5, "not a VCDIFF patch: too short");
    let mut head = [0u8; 5];
    io::read_at(&patch, &mut head, 0, "reading the VCDIFF header")?;
    ensure!(
        head[0] == 0xD6 && head[1] == 0xC3 && head[2] == 0xC4 && head[3] == 0x00,
        "not a VCDIFF patch: bad magic"
    );
    let indicator = head[4];
    ensure!(
        indicator & 0x01 == 0,
        "VCDIFF patch uses secondary compression, which is not supported"
    );
    ensure!(
        indicator & 0x02 == 0,
        "VCDIFF patch carries an application-defined code table, which is not supported"
    );

    let mut r = Range::new(&patch, 5, patch_len);
    if indicator & 0x04 != 0 {
        let len = r.varint_rfc()?;
        r.skip(len)?;
    }

    let source = File::open(source_path).context("opening the source")?;
    let source_len = source.metadata()?.len();
    let table = default_code_table();
    let mut written = 0u64;
    while r.remaining() > 0 {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        decode_window(
            &mut r,
            &source,
            source_len,
            out,
            &mut written,
            &table,
            buf,
            cancel,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn decode_window(
    r: &mut Range,
    source: &File,
    source_len: u64,
    out: &File,
    written: &mut u64,
    table: &[(Instruction, Instruction); 256],
    buf: &mut [u8],
    cancel: &CancelToken,
) -> Result<()> {
    let indicator = r.byte()?;
    ensure!(
        indicator & !(VCD_SOURCE | VCD_TARGET | VCD_CHECKSUM) == 0,
        "unknown window indicator bits {indicator:#04x}"
    );
    ensure!(
        indicator & (VCD_SOURCE | VCD_TARGET) != (VCD_SOURCE | VCD_TARGET),
        "VCD_SOURCE and VCD_TARGET cannot both be set"
    );
    let (seg_len, seg_pos) = if indicator & (VCD_SOURCE | VCD_TARGET) != 0 {
        (r.varint_rfc()?, r.varint_rfc()?)
    } else {
        (0, 0)
    };
    let segment_in_target = indicator & VCD_TARGET != 0;
    if segment_in_target {
        ensure!(
            seg_pos
                .checked_add(seg_len)
                .is_some_and(|end| end <= *written),
            "the target source segment reaches past the output written so far"
        );
    } else {
        ensure!(
            seg_pos
                .checked_add(seg_len)
                .is_some_and(|end| end <= source_len),
            "the source segment reaches past the source file"
        );
    }

    let delta_len = r.varint_rfc()?;
    let mut body = r.split(delta_len)?;
    let target_size = body.varint_rfc()?;
    ensure!(
        target_size <= io::MAX_GROWTH,
        "target window of {target_size} bytes exceeds the {} MiB cap",
        io::MAX_GROWTH >> 20
    );
    ensure!(
        written
            .checked_add(target_size)
            .is_some_and(|total| total <= source_len.saturating_add(io::MAX_GROWTH)),
        "the total output exceeds the source size plus the {} MiB cap",
        io::MAX_GROWTH >> 20
    );
    let delta_indicator = body.byte()?;
    ensure!(
        delta_indicator == 0,
        "compressed window sections are not supported"
    );
    let data_len = body.varint_rfc()?;
    let inst_len = body.varint_rfc()?;
    let addr_len = body.varint_rfc()?;
    let sections = data_len
        .checked_add(inst_len)
        .and_then(|total| total.checked_add(addr_len))
        .context("window section lengths overflow")?;
    // The optional Adler32 sits right after the section lengths, before the
    // data section.
    let expected_checksum = if indicator & VCD_CHECKSUM != 0 {
        Some(u32::from_be_bytes(body.read_array::<4>()?))
    } else {
        None
    };
    ensure!(
        sections == body.remaining(),
        "window section lengths disagree with the delta encoding"
    );
    let mut data = body.split(data_len)?;
    let mut inst = body.split(inst_len)?;
    let mut addr = body.split(addr_len)?;

    let mut near = [0u64; 4];
    let mut same = [0u64; 3 * 256];
    let mut next_slot = 0usize;
    let window_start = *written;
    let mut wpos = 0u64;
    while inst.remaining() > 0 {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let index = inst.byte()? as usize;
        let (first, second) = table[index];
        for instruction in [first, second] {
            if instruction.inst == NOOP {
                continue;
            }
            let size = if instruction.size == 0 {
                inst.varint_rfc()?
            } else {
                instruction.size as u64
            };
            let target_end = wpos
                .checked_add(size)
                .filter(|end| *end <= target_size)
                .context("an instruction writes past the target window")?;
            match instruction.inst {
                ADD => {
                    io::transfer(&mut data, out, window_start + wpos, size, buf, cancel)?;
                    wpos = target_end;
                }
                RUN => {
                    let fill = data.byte()?;
                    io::write_fill(out, window_start + wpos, fill, size, buf, cancel)?;
                    wpos = target_end;
                }
                COPY => {
                    let here = seg_len + wpos;
                    let address = decode_address(
                        &mut addr,
                        &mut near,
                        &mut same,
                        &mut next_slot,
                        instruction.mode,
                        here,
                    )?;
                    // The cursor must sit inside the address space built so
                    // far: the source segment plus the target bytes already
                    // written. A copy may run past `here` into bytes this
                    // same instruction is still writing.
                    ensure!(
                        address < here,
                        "COPY reads target bytes that are not written yet"
                    );
                    let dest = window_start + wpos;
                    if address < seg_len {
                        // The match starts in the source segment and may run
                        // on into the target window being constructed.
                        let from_seg = (seg_len - address).min(size);
                        if segment_in_target {
                            io::copy_within_file(
                                out,
                                seg_pos + address,
                                dest,
                                from_seg,
                                buf,
                                cancel,
                            )?;
                        } else {
                            io::copy_range(
                                source,
                                Some(source_len),
                                seg_pos + address,
                                out,
                                dest,
                                from_seg,
                                buf,
                                cancel,
                            )?;
                        }
                        if from_seg < size {
                            io::copy_within_file(
                                out,
                                window_start,
                                dest + from_seg,
                                size - from_seg,
                                buf,
                                cancel,
                            )?;
                        }
                    } else {
                        io::copy_within_file(
                            out,
                            window_start + (address - seg_len),
                            dest,
                            size,
                            buf,
                            cancel,
                        )?;
                    }
                    wpos = target_end;
                }
                other => bail!("unknown instruction {other}"),
            }
        }
    }
    ensure!(
        wpos == target_size,
        "window produced {wpos} bytes but declares {target_size}"
    );
    *written += wpos;
    if let Some(expected) = expected_checksum {
        let actual = adler32_of(out, window_start, wpos, buf, cancel)?;
        ensure!(
            actual == expected,
            "window Adler32 checksum mismatch: expected {expected:#010x}, got {actual:#010x}"
        );
    }
    Ok(())
}

/// Decodes a COPY address per RFC 3284 section 5.3 and updates the caches.
fn decode_address(
    addr: &mut Range,
    near: &mut [u64; 4],
    same: &mut [u64; 3 * 256],
    next_slot: &mut usize,
    mode: u8,
    here: u64,
) -> Result<u64> {
    let address = match mode {
        0 => addr.varint_rfc()?,
        1 => here
            .checked_sub(addr.varint_rfc()?)
            .context("a HERE-mode COPY address is negative")?,
        2..=5 => near[mode as usize - 2]
            .checked_add(addr.varint_rfc()?)
            .context("a NEAR-mode COPY address overflows")?,
        6..=8 => {
            let slot = (mode as usize - 6) * 256 + addr.byte()? as usize;
            same[slot]
        }
        _ => bail!("invalid COPY address mode {mode}"),
    };
    near[*next_slot] = address;
    *next_slot = (*next_slot + 1) % near.len();
    same[(address % same.len() as u64) as usize] = address;
    Ok(address)
}

/// Builds the default code table of RFC 3284 section 5.4: a RUN, 18 ADDs,
/// 9 modes of 16 COPYs, ADD+COPY pairs, and COPY+ADD pairs.
fn default_code_table() -> [(Instruction, Instruction); 256] {
    fn put(
        table: &mut [(Instruction, Instruction); 256],
        index: &mut usize,
        first: Instruction,
        second: Instruction,
    ) {
        table[*index] = (first, second);
        *index += 1;
    }
    let noop = Instruction {
        inst: NOOP,
        size: 0,
        mode: 0,
    };
    let mut table = [(noop, noop); 256];
    let mut index = 0usize;
    put(
        &mut table,
        &mut index,
        Instruction {
            inst: RUN,
            size: 0,
            mode: 0,
        },
        noop,
    );
    for size in 0..=17u8 {
        put(
            &mut table,
            &mut index,
            Instruction {
                inst: ADD,
                size,
                mode: 0,
            },
            noop,
        );
    }
    for mode in 0..=8u8 {
        put(
            &mut table,
            &mut index,
            Instruction {
                inst: COPY,
                size: 0,
                mode,
            },
            noop,
        );
        for size in 4..=18u8 {
            put(
                &mut table,
                &mut index,
                Instruction {
                    inst: COPY,
                    size,
                    mode,
                },
                noop,
            );
        }
    }
    for mode in 0..=5u8 {
        for add in 1..=4u8 {
            for size in 4..=6u8 {
                put(
                    &mut table,
                    &mut index,
                    Instruction {
                        inst: ADD,
                        size: add,
                        mode: 0,
                    },
                    Instruction {
                        inst: COPY,
                        size,
                        mode,
                    },
                );
            }
        }
    }
    for mode in 6..=8u8 {
        for add in 1..=4u8 {
            put(
                &mut table,
                &mut index,
                Instruction {
                    inst: ADD,
                    size: add,
                    mode: 0,
                },
                Instruction {
                    inst: COPY,
                    size: 4,
                    mode,
                },
            );
        }
    }
    for mode in 0..=8u8 {
        put(
            &mut table,
            &mut index,
            Instruction {
                inst: COPY,
                size: 4,
                mode,
            },
            Instruction {
                inst: ADD,
                size: 1,
                mode: 0,
            },
        );
    }
    debug_assert_eq!(index, 256);
    table
}

/// Adler32 over a byte range of a file, with the modulo applied per chunk
/// (5552 bytes is the largest run that cannot overflow 32 bits).
fn adler32_of(file: &File, at: u64, len: u64, buf: &mut [u8], cancel: &CancelToken) -> Result<u32> {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    io::for_chunks(file, at, len, buf, cancel, |chunk| {
        // 5552 bytes is the longest run that cannot overflow the 32-bit
        // accumulators, so fold the modulo once per sub-slice.
        for sub in chunk.chunks(5552) {
            for &byte in sub {
                a += byte as u32;
                b += a;
            }
            a %= 65_521;
            b %= 65_521;
        }
    })?;
    Ok((b << 16) | a)
}

#[cfg(test)]
mod tests {
    use super::super::io::test_support::rfc_varint;
    use super::super::test_support::{apply_err, apply_ok};
    use super::super::{CancelToken, Patch};

    const MAGIC: &[u8] = &[0xD6, 0xC3, 0xC4, 0x00];

    fn adler32(data: &[u8]) -> u32 {
        let mut a: u32 = 1;
        let mut b: u32 = 0;
        for &byte in data {
            a = (a + byte as u32) % 65_521;
            b = (b + a) % 65_521;
        }
        (b << 16) | a
    }

    /// Assembles a window: an indicator, an optional source segment, and
    /// (target size, data, instructions, addresses, [checksum]). The
    /// checksum, when present, sits right after the section lengths.
    fn window(
        indicator: u8,
        seg: Option<(u64, u64)>,
        target: u64,
        data: &[u8],
        inst: &[u8],
        addr: &[u8],
        checksum: Option<u32>,
    ) -> Vec<u8> {
        let mut body = rfc_varint(target);
        body.push(0); // delta indicator: no compressed sections
        body.extend(rfc_varint(data.len() as u64));
        body.extend(rfc_varint(inst.len() as u64));
        body.extend(rfc_varint(addr.len() as u64));
        if let Some(checksum) = checksum {
            body.extend(checksum.to_be_bytes());
        }
        body.extend_from_slice(data);
        body.extend_from_slice(inst);
        body.extend_from_slice(addr);
        let mut out = vec![indicator];
        if let Some((seg_len, seg_pos)) = seg {
            out.extend(rfc_varint(seg_len));
            out.extend(rfc_varint(seg_pos));
        }
        out.extend(rfc_varint(body.len() as u64));
        out.extend(body);
        out
    }

    #[test]
    fn round_trips_add_run_copy_and_overlapping_copy() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let source = b"abcdef";
        let mut patch = MAGIC.to_vec();
        patch.push(0); // header indicator
        // Window 1 from the source segment: ADD "wx", RUN 'q' x2,
        // COPY 4 from address 0 -> "wxqqabcd".
        patch.extend(window(
            0x01,
            Some((6, 0)),
            8,
            b"wxq",
            &[3, 0, 2, 20],
            &[0],
            None,
        ));
        // Window 2 with no segment: ADD "A", COPY 3 from address 0 with a
        // one-byte forward overlap -> "AAAA".
        patch.extend(window(0x00, None, 4, b"A", &[2, 19, 3], &[0], None));
        let output = apply_ok(dir.path(), "delta.vcdiff", &patch, source, &cancel);
        assert_eq!(output, b"wxqqabcdAAAA");
        let parsed = Patch::open(&dir.path().join("delta.vcdiff"), &CancelToken::new()).unwrap();
        assert_eq!(parsed.format(), "vcdiff");
    }

    #[test]
    fn copy_crossing_from_the_segment_into_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // Segment "abcd" (4 bytes): ADD "XY", then COPY 6 from address 2.
        // The match covers segment bytes 2..4 and continues into the target
        // window's first two bytes: target "XY" + "cd" + "XY" (8 bytes).
        let patch_bytes = window(0x01, Some((4, 0)), 8, b"XY", &[3, 19, 6], &[2], None);
        let mut patch = MAGIC.to_vec();
        patch.push(0);
        patch.extend(patch_bytes);
        let output = apply_ok(dir.path(), "cross.vcdiff", &patch, b"abcd", &cancel);
        // Six copied bytes: segment "cd", the window's "XY", then the
        // copy's own output repeats with period four: "cdXYcd".
        assert_eq!(output, b"XYcdXYcd");
    }

    #[test]
    fn a_target_side_copy_cannot_read_unwritten_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // No segment: every address is target-side, and address 0 with
        // nothing written yet must fail.
        let patch_bytes = window(0x00, None, 3, &[], &[19, 3], &[0], None);
        let mut patch = MAGIC.to_vec();
        patch.push(0);
        patch.extend(patch_bytes);
        let err = apply_err(dir.path(), "future.vcdiff", &patch, b"abc", &cancel);
        assert!(err.to_string().contains("not written yet"), "{err}");
    }

    #[test]
    fn verifies_the_window_adler32() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let decoded = b"wxqqabcd";
        let mut patch = MAGIC.to_vec();
        patch.push(0);
        patch.extend(window(
            0x01 | 0x04,
            Some((6, 0)),
            8,
            b"wxq",
            &[3, 0, 2, 20],
            &[0],
            Some(adler32(decoded)),
        ));
        let output = apply_ok(dir.path(), "sum.vcdiff", &patch, b"abcdef", &cancel);
        assert_eq!(output, decoded);

        let mut bad = MAGIC.to_vec();
        bad.push(0);
        bad.extend(window(
            0x01 | 0x04,
            Some((6, 0)),
            8,
            b"wxq",
            &[3, 0, 2, 20],
            &[0],
            Some(adler32(b"wrong!!!")),
        ));
        let err = apply_err(dir.path(), "badsum.vcdiff", &bad, b"abcdef", &cancel);
        assert!(err.to_string().contains("Adler32"), "{err}");
    }

    #[test]
    fn rejects_secondary_compression() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let mut patch = MAGIC.to_vec();
        patch.push(0x01);
        patch.extend(window(0x00, None, 1, &[], &[2], &[], None));
        let err = apply_err(dir.path(), "lzma.vcdiff", &patch, b"abc", &cancel);
        assert!(err.to_string().contains("secondary compression"), "{err}");
    }

    #[test]
    fn rejects_a_custom_code_table() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let mut patch = MAGIC.to_vec();
        patch.push(0x02);
        patch.extend(window(0x00, None, 1, &[], &[2], &[], None));
        let err = apply_err(dir.path(), "table.vcdiff", &patch, b"abc", &cancel);
        assert!(err.to_string().contains("code table"), "{err}");
    }

    #[test]
    fn rejects_a_huge_target_window_without_materialising_it() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let mut patch = MAGIC.to_vec();
        patch.push(0);
        patch.extend(window(0x00, None, (1 << 30) + 1, &[], &[], &[], None));
        let err = apply_err(dir.path(), "huge.vcdiff", &patch, b"abc", &cancel);
        assert!(err.to_string().contains("cap"), "{err}");
    }

    #[test]
    fn the_total_output_cannot_outgrow_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // A small first window writes 10 bytes; the second window's target
        // of exactly 1 GiB passes the per-window cap but breaks the total
        // cap over the 3-byte source.
        let mut patch = MAGIC.to_vec();
        patch.push(0);
        patch.extend(window(0x00, None, 10, b"0", &[0, 10], &[], None));
        patch.extend(window(0x00, None, 1 << 30, &[], &[], &[], None));
        let err = apply_err(dir.path(), "total.vcdiff", &patch, b"abc", &cancel);
        assert!(err.to_string().contains("cap"), "{err}");
    }

    #[test]
    fn an_add_beyond_the_data_section_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // Opcode 1: ADD with a separately coded size of 5, but no data.
        let patch_bytes = window(0x00, None, 5, &[], &[1, 5], &[], None);
        let mut patch = MAGIC.to_vec();
        patch.push(0);
        patch.extend(patch_bytes);
        let err = apply_err(dir.path(), "short.vcdiff", &patch, b"abc", &cancel);
        assert!(err.to_string().contains("unexpected end"), "{err}");
    }
}
