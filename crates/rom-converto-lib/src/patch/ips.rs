//! IPS, IPS32 and EBP patch application.
//!
//! An IPS patch carries a `PATCH` magic, then records of a big-endian
//! offset and byte count until an `EOF` marker;
//! a zero count selects the RLE form (a big-endian run length plus one fill
//! byte). Because a record's offset bytes can spell `EOF` themselves, the
//! marker bytes are treated as a would-be record offset: the size field
//! after them is read and the record is only parsed when it fits with a
//! terminator of its own still to follow and its offset plus payload stays
//! within the growth cap; otherwise the marker ends the stream, applying
//! the truncate length when exactly that many bytes follow it and ignoring
//! any other trailing bytes. Residual: for IPS32, trailing junk after
//! `EEOF` that frames as a record whose offset plus payload lies beyond
//! the growth cap fails the apply. EBP ends at the marker when
//! nothing follows it or the next byte opens a JSON blob (a would-be
//! record whose size high byte is `{` is read as a JSON blob instead),
//! and EBP never truncates. IPS32 swaps in an `IPS32` magic, 4-byte
//! offsets and an `EEOF` marker, with the same single width governing
//! offsets, marker length and truncate length. Every record is bounded by
//! the source size plus the growth cap before it writes.

use super::Format;
use super::io::{self, Range};
use crate::util::{CancelToken, Cancelled};
use anyhow::{Context, Result, ensure};
use std::fs::File;
use std::path::Path;

pub(super) fn apply(
    patch_path: &Path,
    source_path: &Path,
    out: &File,
    kind: Format,
    buf: &mut [u8],
    cancel: &CancelToken,
) -> Result<()> {
    let ips32 = kind == Format::Ips32;
    let kind_name = match kind {
        Format::Ips => "IPS",
        Format::Ips32 => "IPS32",
        _ => "EBP",
    };
    let patch = File::open(patch_path).context("opening the IPS patch")?;
    let magic: &[u8; 5] = if ips32 { b"IPS32" } else { b"PATCH" };
    let mut head = [0u8; 5];
    io::read_at(&patch, &mut head, 0, "reading the IPS header")?;
    ensure!(
        &head == magic,
        "not an {kind_name} patch: bad magic {:?}",
        String::from_utf8_lossy(&head)
    );

    let source = File::open(source_path).context("opening the source")?;
    let source_len = source.metadata()?.len();
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

    // One width governs the offsets, the marker length and the truncate
    // length: three bytes for IPS, four for IPS32.
    let width = if ips32 { 4 } else { 3 };
    let marker: &[u8] = if ips32 { b"EEOF" } else { b"EOF" };
    let growth_cap = source_len.saturating_add(io::MAX_GROWTH);

    let patch_len = patch.metadata()?.len();
    let mut r = Range::new(&patch, 5, patch_len);
    let mut max_end = source_len;
    let mut truncate: Option<u64> = None;
    loop {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let at_marker = r.peek_eq(marker)?;
        if at_marker {
            let total = r.remaining();
            let after = total - width as u64;
            // The marker bytes can be a record's own offset: parse the
            // record they announce only when it fits with a terminator of
            // its own still to follow. A would-be record that frames
            // structurally but reaches past the growth cap is parsed and
            // hits the per-record growth-cap error (fail closed). EBP ends
            // at the marker when nothing follows it or the next byte opens
            // a JSON blob, and applies the same rule otherwise.
            let mut treat_as_record = false;
            if after >= width as u64 + 2 {
                let mut size_raw = [0u8; 2];
                r.peek_at(width as u64, &mut size_raw)?;
                let size = u16::from_be_bytes(size_raw);
                let payload = if size == 0 { 3 } else { size as u64 };
                let fits_with_terminator = total >= width as u64 + 2 + payload + width as u64;
                treat_as_record = if kind == Format::Ebp {
                    size_raw[0] != b'{' && fits_with_terminator
                } else {
                    fits_with_terminator
                };
            }
            if !treat_as_record {
                r.skip(width as u64)?;
                // For EBP a short blob after the marker is JSON, not a
                // truncate.
                if after == width as u64 && kind != Format::Ebp {
                    truncate = Some(if ips32 {
                        u32::from_be_bytes(r.read_array::<4>()?) as u64
                    } else {
                        let raw = r.read_array::<3>()?;
                        u32::from_be_bytes([0, raw[0], raw[1], raw[2]]) as u64
                    });
                }
                break;
            }
        }
        let offset = if ips32 {
            u32::from_be_bytes(r.read_array::<4>()?) as u64
        } else {
            let raw = r.read_array::<3>()?;
            u32::from_be_bytes([0, raw[0], raw[1], raw[2]]) as u64
        };
        let size = u16::from_be_bytes(r.read_array::<2>()?);
        // The growth cap is checked before this record writes, so a
        // rejected record writes nothing.
        if size == 0 {
            let run = u16::from_be_bytes(r.read_array::<2>()?) as u64;
            let fill = r.byte()?;
            ensure!(
                offset.saturating_add(run) <= growth_cap,
                "the patch grows the output past the growth cap"
            );
            if run > 0 {
                io::write_fill(out, offset, fill, run, buf, cancel)?;
                max_end = max_end.max(offset.saturating_add(run));
            }
        } else {
            ensure!(
                offset.saturating_add(size as u64) <= growth_cap,
                "the patch grows the output past the growth cap"
            );
            r.take(&mut buf[..size as usize])?;
            io::write_at(out, &buf[..size as usize], offset)?;
            max_end = max_end.max(offset.saturating_add(size as u64));
        }
    }
    // A truncate only ever shrinks or keeps the produced content.
    out.set_len(truncate.filter(|t| *t <= max_end).unwrap_or(max_end))
        .context("sizing the patched output")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::CancelToken;
    use super::super::test_support::{apply_err, apply_ok};

    fn record(offset: u32, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&offset.to_be_bytes()[1..]);
        out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    fn rle(offset: u32, run: u16, fill: u8) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&offset.to_be_bytes()[1..]);
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&run.to_be_bytes());
        out.push(fill);
        out
    }

    fn wrap(magic: &[u8], body: &[u8], tail: &[u8]) -> Vec<u8> {
        let mut out = magic.to_vec();
        out.extend_from_slice(body);
        out.extend_from_slice(tail);
        out
    }

    #[test]
    fn applies_records_rle_and_growth() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let source = b"0123456789abcdefghij";
        let mut body = record(2, b"XYZ");
        body.extend(rle(10, 5, b'*'));
        body.extend(record(30, b"tail"));
        let patch = wrap(b"PATCH", &body, b"EOF");
        let output = apply_ok(dir.path(), "grow.ips", &patch, source, &cancel);
        // The record at 30 lands past the source's end: the gap stays zero.
        let mut expected = vec![0u8; 34];
        expected[..20].copy_from_slice(source);
        expected[2..5].copy_from_slice(b"XYZ");
        expected[10..15].fill(b'*');
        expected[30..].copy_from_slice(b"tail");
        assert_eq!(output, expected);
    }

    #[test]
    fn truncate_shrinks_the_result() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let mut body = record(0, b"XY");
        body.extend_from_slice(b"EOF");
        body.extend_from_slice(&4u32.to_be_bytes()[1..]);
        let patch = wrap(b"PATCH", &body, &[]);
        let output = apply_ok(dir.path(), "trim.ips", &patch, b"abcdef", &cancel);
        assert_eq!(output, b"XYcd");
    }

    #[test]
    fn trailing_junk_after_the_marker_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // Two bytes after the marker are too short to announce a record, so
        // the stream ends and the junk is ignored.
        let patch = wrap(b"PATCH", &record(0, b"XY"), b"EOF??");
        let output = apply_ok(dir.path(), "junk.ips", &patch, b"abcdef", &cancel);
        assert_eq!(output, b"XYcdef");
    }

    #[test]
    fn a_tail_that_announces_a_record_without_a_terminator_ends_the_stream() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // The bytes after the marker announce a three-byte payload, but no
        // terminator follows it: the stream ends at the marker and the
        // pre-marker record's bytes survive.
        let patch = wrap(
            b"PATCH",
            &record(0, b"XY"),
            &[b'E', b'O', b'F', 0x00, 0x03, b'a', b'b', b'c'],
        );
        let output = apply_ok(dir.path(), "term.ips", &patch, b"abcdef", &cancel);
        assert_eq!(output, b"XYcdef");
    }

    #[test]
    fn trailing_bytes_reading_as_a_small_truncate_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // Four bytes after the marker read as a small truncate (00 00 04
        // 00); only an exact-width tail truncates, so the junk is ignored.
        let patch = wrap(
            b"PATCH",
            &record(0, b"XY"),
            &[b'E', b'O', b'F', 0x00, 0x00, 0x04, 0x00],
        );
        let output = apply_ok(dir.path(), "junk4.ips", &patch, b"abcdef", &cancel);
        assert_eq!(output, b"XYcdef");
    }

    #[test]
    fn an_rle_shaped_tail_without_a_terminator_ends_the_stream() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // The pre-marker record applies; the bytes after the marker
        // announce an RLE record (size zero, so a three-byte payload) but
        // no terminator follows it, so the tail is ignored.
        let patch = wrap(
            b"PATCH",
            &record(0, b"XY"),
            &[b'E', b'O', b'F', 0x00, 0x00, 0x01, 0x02, 0x03],
        );
        let output = apply_ok(dir.path(), "rle.ips", &patch, b"abcdef", &cancel);
        assert_eq!(output, b"XYcdef");
    }

    #[test]
    fn an_offset_spelling_eof_with_a_fitting_record_stays_a_record() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // Offset 0x454F46 spells "EOF"; the size after it announces one
        // payload byte and a terminator still follows, so the record is
        // parsed and the output grows to the end of its payload.
        let mut body = record(0x454F46, b"!");
        body.extend_from_slice(b"EOF");
        let patch = wrap(b"PATCH", &body, &[]);
        let output = apply_ok(dir.path(), "eof.ips", &patch, b"abcdef", &cancel);
        assert_eq!(output.len(), 0x454F47);
        assert_eq!(output[0x454F46], b'!');
    }

    #[test]
    fn records_after_an_eof_offset_all_apply() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // A record at the "EOF" offset followed by a record at 0x100000:
        // both fit with their terminator, so both apply.
        let mut body = record(0x454F46, b"!");
        body.extend(record(0x100000, b"!"));
        body.extend_from_slice(b"EOF");
        let patch = wrap(b"PATCH", &body, &[]);
        let output = apply_ok(dir.path(), "both.ips", &patch, b"abcdef", &cancel);
        assert_eq!(output.len(), 0x454F47);
        assert_eq!(output[0x454F46], b'!');
        assert_eq!(output[0x100000], b'!');
    }

    #[test]
    fn ips32_uses_four_byte_offsets_eeof_and_a_four_byte_truncate() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // Offset 0x45454F46 spells "EEOF"; the four-byte truncate equals
        // the output length.
        let mut body = 0x0100_0002u32.to_be_bytes().to_vec();
        body.extend(1u16.to_be_bytes());
        body.push(b'Z');
        body.extend_from_slice(b"EEOF");
        body.extend(0x0100_0003u32.to_be_bytes());
        let patch = wrap(b"IPS32", &body, &[]);
        let output = apply_ok(dir.path(), "wide.ips32", &patch, b"abcdef", &cancel);
        // The four-byte truncate equals the output length.
        let mut expected = b"abcdef".to_vec();
        expected.resize(0x0100_0003, 0);
        expected[0x0100_0002] = b'Z';
        assert_eq!(output, expected);
    }

    #[test]
    fn an_ips32_truncate_smaller_than_the_result_shrinks_it() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // 'Z' at 2 over "abcdef", truncated to four bytes: "abZd".
        let mut body = 2u32.to_be_bytes().to_vec();
        body.extend(1u16.to_be_bytes());
        body.push(b'Z');
        body.extend_from_slice(b"EEOF");
        body.extend(4u32.to_be_bytes());
        let patch = wrap(b"IPS32", &body, &[]);
        let output = apply_ok(dir.path(), "shrink.ips32", &patch, b"abcdef", &cancel);
        assert_eq!(output, b"abZd");
    }

    #[test]
    fn an_ips32_truncate_beyond_the_output_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // 'Z' at 2 over "abcdef" with a truncate of 0xFFFFFFFF: the
        // truncate cannot grow the output, so the length is unchanged.
        let mut body = 2u32.to_be_bytes().to_vec();
        body.extend(1u16.to_be_bytes());
        body.push(b'Z');
        body.extend_from_slice(b"EEOF");
        body.extend(0xFFFF_FFFFu32.to_be_bytes());
        let patch = wrap(b"IPS32", &body, &[]);
        let output = apply_ok(dir.path(), "huge.ips32", &patch, b"abcdef", &cancel);
        assert_eq!(output, b"abZdef");
    }

    #[test]
    fn a_would_be_record_past_the_growth_cap_fails_the_apply() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // The EEOF marker bytes read as an offset (0x45454545) whose offset
        // plus announced payload lies far beyond the growth cap: the junk
        // frames structurally, so the apply fails closed with the
        // growth-cap error.
        let mut body = 2u32.to_be_bytes().to_vec();
        body.extend(1u16.to_be_bytes());
        body.push(b'Z');
        body.extend_from_slice(b"EEOF");
        body.extend(0x0001u16.to_be_bytes());
        body.extend_from_slice(b"XXXXXXXXXX");
        let patch = wrap(b"IPS32", &body, &[]);
        let err = apply_err(dir.path(), "junk-cap.ips32", &patch, b"abcdef", &cancel);
        assert!(err.to_string().contains("growth cap"), "{err}");
    }

    #[test]
    fn a_zero_run_record_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // An RLE record with a zero run writes nothing: the marker still
        // ends the stream and the source passes through unchanged.
        let patch = wrap(b"PATCH", &rle(100, 0, b'*'), b"EOF");
        let output = apply_ok(dir.path(), "zero-run.ips", &patch, b"abcdef", &cancel);
        assert_eq!(output, b"abcdef");
    }

    #[test]
    fn a_rejected_growth_record_leaves_the_output_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let source = b"abcdef";
        // An IPS32 record writing one byte just past 1 GiB trips the growth
        // cap. Direct applier call on a pre-filled output file so its
        // length can be inspected after the rejection.
        let mut body = 0x4000_0101u32.to_be_bytes().to_vec();
        body.extend(1u16.to_be_bytes());
        body.push(b'Z');
        body.extend_from_slice(b"EEOF");
        let patch = wrap(b"IPS32", &body, &[]);
        let patch_path = dir.path().join("untouched.ips32");
        std::fs::write(&patch_path, &patch).unwrap();
        let source_path = dir.path().join("source.bin");
        std::fs::write(&source_path, source).unwrap();
        let out_path = dir.path().join("out.bin");
        std::fs::write(&out_path, source).unwrap();
        let out = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&out_path)
            .unwrap();
        let mut buf = vec![0u8; super::io::COPY_CHUNK_BYTES];
        let err = super::apply(
            &patch_path,
            &source_path,
            &out,
            super::Format::Ips32,
            &mut buf,
            &cancel,
        )
        .expect_err("growth cap must reject");
        assert!(err.to_string().contains("growth cap"), "{err}");
        assert_eq!(out.metadata().unwrap().len(), source.len() as u64);

        // The RLE form is capped the same way, also before it writes.
        let mut body = 0x4000_0000u32.to_be_bytes().to_vec();
        body.extend(0u16.to_be_bytes());
        body.extend(0x200u16.to_be_bytes());
        body.push(b'Z');
        body.extend_from_slice(b"EEOF");
        let patch = wrap(b"IPS32", &body, &[]);
        let patch_path = dir.path().join("untouched-rle.ips32");
        std::fs::write(&patch_path, &patch).unwrap();
        std::fs::write(&out_path, source).unwrap();
        let out = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&out_path)
            .unwrap();
        let err = super::apply(
            &patch_path,
            &source_path,
            &out,
            super::Format::Ips32,
            &mut buf,
            &cancel,
        )
        .expect_err("growth cap must reject");
        assert!(err.to_string().contains("growth cap"), "{err}");
        assert_eq!(out.metadata().unwrap().len(), source.len() as u64);
    }

    #[test]
    fn ebp_ends_at_a_json_blob_and_ignores_it() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // A blob over 31.5 KiB is long enough that its opening bytes would
        // fit a would-be record, so this exercises the brace exemption.
        let mut json = b"{\"m\":[".to_vec();
        json.resize(33000, b' ');
        json.push(b'}');
        let mut body = record(3, b"--");
        body.extend_from_slice(b"EOF");
        let patch = wrap(b"PATCH", &body, &json);
        let output = apply_ok(dir.path(), "meta.ebp", &patch, b"abcdef", &cancel);
        assert_eq!(output, b"abc--f");
    }

    #[test]
    fn an_ebp_three_byte_tail_is_never_a_truncate() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // Exactly three bytes after the marker on an EBP patch would read
        // as a truncate on a plain IPS; EBP never truncates, so the patched
        // six bytes survive.
        let mut body = record(0, b"XY");
        body.extend_from_slice(b"EOF");
        let patch = wrap(b"PATCH", &body, &[0x00, 0x00, 0x04]);
        let output = apply_ok(dir.path(), "notrunc.ebp", &patch, b"abcdef", &cancel);
        assert_eq!(output, b"XYcdef");
    }

    #[test]
    fn ebp_applies_a_record_at_the_eof_offset() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // The record's offset spells "EOF"; the byte after the marker is a
        // size byte, not a JSON brace, so the would-be-offset rule applies
        // the record and the JSON blob after the terminator is ignored.
        let mut body = record(0x454F46, b"!");
        body.extend_from_slice(b"EOF");
        let patch = wrap(b"PATCH", &body, br#"{"meta":true}"#);
        let output = apply_ok(dir.path(), "eof.ebp", &patch, b"abcdef", &cancel);
        assert_eq!(output.len(), 0x454F47);
        assert_eq!(output[0x454F46], b'!');
    }

    #[test]
    fn a_missing_end_marker_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let patch = wrap(b"PATCH", &record(0, b"ab"), &[]);
        let err = apply_err(dir.path(), "open.ips", &patch, b"abcdef", &cancel);
        assert!(err.to_string().contains("unexpected end"), "{err}");
    }
}
