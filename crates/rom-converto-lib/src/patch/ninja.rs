//! NINJA 2 patch application (the `.rup` extension). A fixed 2048-byte
//! header of a `NINJA` magic, a version digit that must be the character
//! `2`, an ignored encoding byte, and ignored free-text metadata fields,
//! then a command stream. Command bytes are 0 for a terminate byte
//! (consumed; the stream simply ends at end of file), 1 for OPEN and 2 for
//! XOR; any other byte is an error. Integers are length-prefixed
//! little-endian (one byte giving the byte count, then that many bytes),
//! including the sizes and the XOR offset and length. OPEN carries a
//! length-prefixed file name (the applier ignores it), a file-type
//! code that must be 0 (raw/binary), the source and target sizes, the MD5
//! of the source and of the target, and, when the sizes differ, an
//! overflow block: a mode letter (`M` or `A`, validated but not compared
//! with the sizes), a length and that many bytes stored complemented.
//! When the target is larger the complemented block is appended past the
//! source copy; when it is smaller the block is read and skipped. Only a
//! single raw entry is applied; the target size is capped at the source
//! size plus 1 GiB; the source MD5 is verified before anything is written
//! and the target MD5 on the result, which is sized to the declared target
//! size. XOR records apply against the output built so far (zero past its
//! end).

use super::io::{self, Range};
use crate::util::{CancelToken, Cancelled};
use anyhow::{Context, Result, bail, ensure};
use std::fs::File;
use std::path::Path;

const HEADER: u64 = 0x800;
const END: u8 = 0;
const OPEN: u8 = 1;
const XOR: u8 = 2;

struct Entry {
    source_size: u64,
    target_size: u64,
    source_md5: [u8; 16],
    target_md5: [u8; 16],
    /// Overflow block when the declared sizes differ: data length and the
    /// data's position in the patch file.
    overflow: Option<(u64, u64)>,
}

pub(super) fn apply(
    patch_path: &Path,
    source_path: &Path,
    out: &File,
    buf: &mut [u8],
    cancel: &CancelToken,
) -> Result<()> {
    let patch = File::open(patch_path).context("opening the NINJA 2 patch")?;
    let patch_len = patch.metadata()?.len();
    ensure!(
        patch_len >= HEADER,
        "not a valid NINJA 2 patch: shorter than its fixed header"
    );
    let mut magic = [0u8; 6];
    io::read_at(&patch, &mut magic, 0, "reading the NINJA 2 header")?;
    ensure!(&magic == b"NINJA2", "not a NINJA 2 patch: bad magic");
    let mut r = Range::new(&patch, HEADER, patch_len);

    let entry = parse_open(&mut r)?;

    let source = File::open(source_path).context("opening the source")?;
    let source_len = source.metadata()?.len();
    ensure!(
        source_len == entry.source_size,
        "source is {source_len} bytes but the entry declares {}",
        entry.source_size
    );
    let actual = io::md5_of_range(&source, 0, source_len, buf, cancel)?;
    ensure!(
        actual == entry.source_md5,
        "source does not match the patch: the MD5 differs"
    );

    io::copy_range(
        &source,
        Some(source_len),
        0,
        out,
        0,
        source_len,
        buf,
        cancel,
    )?;

    // With the source checks passed, a growing entry's overflow block is
    // stored complemented and lands past the source copy; a shrinking
    // entry's block is read and skipped, and the declared target size
    // truncates at the end.
    let mut written = source_len;
    if let Some((len, pos)) = entry.overflow
        && entry.target_size > entry.source_size
    {
        ensure!(
            len <= entry.target_size - entry.source_size,
            "the overflow block overruns the target size"
        );
        let mut or = Range::new(&patch, pos, pos + len);
        let mut done = 0u64;
        while done < len {
            if cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            let n = (buf.len() as u64).min(len - done) as usize;
            or.take(&mut buf[..n])?;
            for byte in buf[..n].iter_mut() {
                *byte = !*byte;
            }
            io::write_at(out, &buf[..n], source_len + done)?;
            done += n as u64;
        }
        written = source_len + len;
    }

    loop {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let Some(command) = r.try_byte()? else {
            break;
        };
        match command {
            XOR => {
                let offset = lp_int(&mut r)?;
                let len = lp_int(&mut r)?;
                ensure!(
                    offset
                        .checked_add(len)
                        .is_some_and(|end| end <= entry.target_size),
                    "an XOR record runs past the target size"
                );
                // The record's bytes stream through the first half of the
                // scratch buffer and XOR against the output built so far
                // (the source copy plus any overflow; zero past it), held
                // in the second half.
                let half = buf.len() / 2;
                let (patch_buf, existing_buf) = buf.split_at_mut(half);
                let mut done = 0u64;
                while done < len {
                    if cancel.is_cancelled() {
                        return Err(Cancelled.into());
                    }
                    let n = (half as u64).min(len - done) as usize;
                    r.take(&mut patch_buf[..n])?;
                    io::xor_padded(
                        out,
                        written,
                        out,
                        offset + done,
                        &patch_buf[..n],
                        existing_buf,
                        cancel,
                    )?;
                    done += n as u64;
                }
                // The record just wrote everything in its range, including
                // the zero-padded part past the previous extent.
                written = written.max(offset + len);
            }
            END => (),
            OPEN => bail!("multiple file entries are not supported"),
            other => bail!("unknown NINJA 2 command {other:#04x}"),
        }
    }

    out.set_len(entry.target_size)
        .context("sizing the patched output")?;
    let actual = io::md5_of_range(out, 0, entry.target_size, buf, cancel)?;
    ensure!(
        actual == entry.target_md5,
        "patched output failed its target MD5"
    );
    Ok(())
}

/// Parses the mandatory `OPEN` entry at the current position. The
/// length-prefixed file name is read and skipped. When the two declared
/// sizes differ, the overflow block that follows the checksums is located
/// (length, data position) and skipped here; the caller writes it into the
/// output once the source checks have passed.
fn parse_open(r: &mut Range) -> Result<Entry> {
    let command = r.byte().context("the patch ends before any file entry")?;
    ensure!(command == OPEN, "expected a file entry");
    let name_len = lp_int(r)?;
    r.skip(name_len)?;
    let entry_type = r.byte()?;
    ensure!(
        entry_type == 0,
        "only raw file patches are supported (entry type {entry_type})"
    );
    let source_size = lp_int(r)?;
    let target_size = lp_int(r)?;
    let source_md5 = r.read_array::<16>()?;
    let target_md5 = r.read_array::<16>()?;
    ensure!(
        target_size <= source_size.saturating_add(io::MAX_GROWTH),
        "target size {target_size} exceeds the growth cap"
    );
    let overflow = if source_size != target_size {
        let mode = r.byte()?;
        ensure!(mode == b'M' || mode == b'A', "unknown overflow mode {mode}");
        let len = lp_int(r)?;
        let pos = r.position();
        r.skip(len)?;
        Some((len, pos))
    } else {
        None
    };
    Ok(Entry {
        source_size,
        target_size,
        source_md5,
        target_md5,
        overflow,
    })
}

/// Reads a length-prefixed little-endian integer: one byte giving the byte
/// count, then that many bytes.
fn lp_int(r: &mut Range) -> Result<u64> {
    let n = r.byte()? as usize;
    ensure!(n <= 8, "an integer spans more than 8 bytes");
    let mut raw = [0u8; 8];
    r.take(&mut raw[..n])?;
    Ok(u64::from_le_bytes(raw))
}

#[cfg(test)]
mod tests {
    use super::super::CancelToken;
    use super::super::test_support::{apply_err, apply_ok};

    fn md5(data: &[u8]) -> [u8; 16] {
        use sha2::Digest as _;
        md_5::Md5::digest(data).into()
    }

    /// One length-prefixed little-endian integer.
    fn lp(value: u64) -> Vec<u8> {
        let bytes = value.to_le_bytes();
        let used = bytes.iter().rposition(|b| *b != 0).map_or(1, |p| p + 1);
        let mut out = vec![used as u8];
        out.extend_from_slice(&bytes[..used]);
        out
    }

    fn header() -> Vec<u8> {
        let mut patch = b"NINJA2".to_vec();
        patch.push(0); // encoding
        patch.extend(std::iter::repeat_n(0, 0x800 - 7));
        patch
    }

    fn entry(
        source: &[u8],
        target: &[u8],
        target_md5_override: Option<[u8; 16]>,
        overflow: Option<(u8, &[u8])>,
    ) -> Vec<u8> {
        entry_named(b"", source, target, target_md5_override, overflow)
    }

    fn entry_named(
        name: &[u8],
        source: &[u8],
        target: &[u8],
        target_md5_override: Option<[u8; 16]>,
        overflow: Option<(u8, &[u8])>,
    ) -> Vec<u8> {
        let mut e = vec![1u8]; // OPEN
        e.extend(lp(name.len() as u64));
        e.extend_from_slice(name);
        e.push(0); // raw/binary entry
        e.extend(lp(source.len() as u64));
        e.extend(lp(target.len() as u64));
        e.extend_from_slice(&md5(source));
        e.extend_from_slice(&target_md5_override.unwrap_or_else(|| md5(target)));
        if let Some((mode, data)) = overflow {
            e.push(mode);
            e.extend(lp(data.len() as u64));
            // The overflow block is stored complemented.
            e.extend(data.iter().map(|b| !b));
        }
        e
    }

    fn xor_record(offset: u64, data: &[u8]) -> Vec<u8> {
        let mut out = vec![2u8]; // XOR
        out.extend(lp(offset));
        out.extend(lp(data.len() as u64));
        out.extend_from_slice(data);
        out
    }

    fn build(
        source: &[u8],
        target: &[u8],
        records: &[Vec<u8>],
        target_md5_override: Option<[u8; 16]>,
        overflow: Option<(u8, &[u8])>,
    ) -> Vec<u8> {
        let mut patch = header();
        patch.extend(entry(source, target, target_md5_override, overflow));
        for record in records {
            patch.extend_from_slice(record);
        }
        patch.extend(0u8.to_le_bytes()); // END
        patch
    }

    #[test]
    fn round_trips_xor_records() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let source = b"0123456789";
        let mut target = source.to_vec();
        target[4..8].iter_mut().for_each(|b| *b ^= 0xFF);
        let patch = build(source, &target, &[xor_record(4, &[0xFF; 4])], None, None);
        let output = apply_ok(dir.path(), "patch.rup", &patch, source, &cancel);
        assert_eq!(output, target);
    }

    #[test]
    fn an_overflow_block_fills_the_target_beyond_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let source = b"0123456789";
        let target = b"0123456789TAIL!";
        let patch = build(source, target, &[], None, Some((b'M', b"TAIL!")));
        let output = apply_ok(dir.path(), "over.rup", &patch, source, &cancel);
        assert_eq!(output, target);
    }

    #[test]
    fn an_xor_record_wins_over_the_overflow_block() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // The overflow block and an XOR record target the same bytes past
        // the source; the overflow is written first so the XOR record wins.
        let source = b"0123456789";
        let target = b"0123456789PQ";
        let patch = build(
            source,
            target,
            &[xor_record(10, &[b'A' ^ b'P', b'B' ^ b'Q'])],
            None,
            Some((b'A', b"AB")),
        );
        let output = apply_ok(dir.path(), "order.rup", &patch, source, &cancel);
        assert_eq!(output, target);
    }

    #[test]
    fn a_shrinking_entry_skips_its_overflow_block() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // target < source: the overflow block is read and skipped; the
        // result is the source truncated to the target size.
        let patch = build(b"0123456789", b"01234", &[], None, Some((b'M', b"6789")));
        let output = apply_ok(dir.path(), "shrink.rup", &patch, b"0123456789", &cancel);
        assert_eq!(output, b"01234");
    }

    #[test]
    fn the_mode_letter_is_not_compared_with_the_sizes() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // Either letter frames the block; the sizes decide the direction.
        let patch = build(b"0123456789", b"01234", &[], None, Some((b'A', b"6789")));
        let output = apply_ok(dir.path(), "letter.rup", &patch, b"0123456789", &cancel);
        assert_eq!(output, b"01234");
    }

    #[test]
    fn a_wrong_md5_with_an_overflow_block_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // Apply to a same-length file whose bytes differ: the source MD5
        // gate must fire before the overflow block is written, leaving the
        // output file empty.
        let patch = build(
            b"0123456789",
            b"0123456789AB",
            &[],
            None,
            Some((b'A', b"AB")),
        );
        let patch_path = dir.path().join("guard.rup");
        std::fs::write(&patch_path, patch).unwrap();
        let source_path = dir.path().join("source.bin");
        std::fs::write(&source_path, b"XXXXXXXXXX").unwrap();
        let out_path = dir.path().join("output.bin");
        let out = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&out_path)
            .unwrap();
        let mut buf = vec![0u8; super::io::COPY_CHUNK_BYTES];
        let err = super::apply(&patch_path, &source_path, &out, &mut buf, &cancel)
            .expect_err("mismatched source must fail");
        assert!(err.to_string().contains("MD5"), "{err}");
        assert_eq!(out.metadata().unwrap().len(), 0);
    }

    #[test]
    fn xor_records_extend_and_read_the_written_extent() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // Source "abc" with a zero-length overflow block to a 6-byte
        // target: the first XOR record zero-pads past the source, and the
        // second XORs against the first record's output.
        let source = b"abc";
        let target = b"abc\xFF\x12\x34";
        let patch = build(
            source,
            target,
            &[
                xor_record(3, &[0xFF; 3]),
                xor_record(4, &[0xFF ^ 0x12, 0xFF ^ 0x34]),
            ],
            None,
            Some((b'M', b"")),
        );
        let patch_path = dir.path().join("track.rup");
        std::fs::write(&patch_path, patch).unwrap();
        let source_path = dir.path().join("source.bin");
        std::fs::write(&source_path, source).unwrap();
        let out_path = dir.path().join("output.bin");
        let out = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&out_path)
            .unwrap();
        let mut buf = vec![0u8; super::io::COPY_CHUNK_BYTES];
        super::apply(&patch_path, &source_path, &out, &mut buf, &cancel).expect("apply patch");
        drop(out);
        assert_eq!(std::fs::read(&out_path).unwrap(), target);
    }

    #[test]
    fn an_unknown_overflow_mode_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let patch = build(b"abc", b"abcdef", &[], None, Some((b'X', b"ABC")));
        let err = apply_err(dir.path(), "mode.rup", &patch, b"abc", &cancel);
        assert!(err.to_string().contains("unknown overflow mode"), "{err}");
    }

    #[test]
    fn a_typed_entry_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let mut patch = build(b"abc", b"abc", &[], None, None);
        patch[0x800 + 2] = 3; // the file-type code
        let err = apply_err(dir.path(), "typed.rup", &patch, b"abc", &cancel);
        assert!(err.to_string().contains("only raw file patches"), "{err}");
    }

    #[test]
    fn a_named_entry_applies_with_its_name_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // The length-prefixed name is skipped, whether empty (`00`) or
        // spelled out (`01 03 'r' 'o' 'm'`).
        let patch = header();
        let mut with_name = patch.clone();
        with_name.extend(entry_named(b"rom", b"abc", b"abc", None, None));
        with_name.push(0); // END
        let output = apply_ok(dir.path(), "named.rup", &with_name, b"abc", &cancel);
        assert_eq!(output, b"abc");
        let mut empty_lp = patch;
        empty_lp.extend(entry_named(&[], b"abc", b"abc", None, None));
        empty_lp.push(0); // END
        let output = apply_ok(dir.path(), "empty.rup", &empty_lp, b"abc", &cancel);
        assert_eq!(output, b"abc");
    }

    #[test]
    fn a_wrong_source_md5_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // Equal sizes, so no overflow block is expected; the source bytes
        // differ from the entry's MD5.
        let patch = build(b"the real source", b"123456789012345", &[], None, None);
        let err = apply_err(dir.path(), "wrong.rup", &patch, b"a fake source!!", &cancel);
        assert!(err.to_string().contains("MD5 differs"), "{err}");
    }

    #[test]
    fn a_forged_target_md5_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let patch = build(
            b"abc",
            b"xyz",
            &[xor_record(0, b"xyz")],
            Some(md5(b"nope")),
            None,
        );
        let err = apply_err(dir.path(), "forge.rup", &patch, b"abc", &cancel);
        assert!(err.to_string().contains("target MD5"), "{err}");
    }

    #[test]
    fn a_second_open_entry_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let mut patch = build(b"abc", b"abc", &[], None, None);
        // Splice a second OPEN entry before END.
        let end = patch.len() - 1;
        let second = entry(b"abc", b"abc", None, None);
        patch.splice(end..end, second);
        let err = apply_err(dir.path(), "two.rup", &patch, b"abc", &cancel);
        assert!(err.to_string().contains("multiple file entries"), "{err}");
    }

    #[test]
    fn an_xor_record_cannot_run_past_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let patch = build(b"abc", b"abc", &[xor_record(1, b"WXYZ")], None, None);
        let err = apply_err(dir.path(), "over.rup", &patch, b"abc", &cancel);
        assert!(err.to_string().contains("past the target size"), "{err}");
    }

    #[test]
    fn a_patch_may_end_without_a_terminate_byte() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // The stream simply ends at end of file.
        let mut patch = build(b"abc", b"abc", &[], None, None);
        patch.pop(); // drop the terminate byte
        let output = apply_ok(dir.path(), "open.rup", &patch, b"abc", &cancel);
        assert_eq!(output, b"abc");
    }

    #[test]
    fn a_terminate_byte_consumes_and_continues() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // A terminate byte mid-stream: consumed, and the following command
        // still applies.
        let mut patch = header();
        patch.extend(entry(b"abc", b"xyz", None, None));
        patch.push(0); // terminate byte
        patch.extend_from_slice(&xor_record(0, &[b'a' ^ b'x', b'b' ^ b'y', b'c' ^ b'z']));
        patch.push(0); // END
        let output = apply_ok(dir.path(), "term.rup", &patch, b"abc", &cancel);
        assert_eq!(output, b"xyz");
    }

    #[test]
    fn a_huge_declared_target_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let mut patch = header();
        let mut e = vec![1u8];
        e.push(0); // single file
        e.push(0); // raw/binary entry
        e.extend(lp(3));
        e.extend(lp(u64::MAX / 2));
        e.extend(md5(b"abc"));
        e.extend(md5(b""));
        patch.extend_from_slice(&e);
        let err = apply_err(dir.path(), "huge.rup", &patch, b"abc", &cancel);
        assert!(err.to_string().contains("growth cap"), "{err}");
    }
}
