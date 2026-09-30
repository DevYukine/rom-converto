//! BPS patch application. A `BPS1` magic, three biased variable-length
//! numbers (source size, target size, metadata size), the metadata, then
//! commands until a 12-byte footer
//! of source/target/patch CRC32 values. Every command carries
//! `(length - 1) << 2 | action`; SourceRead copies from the source at the
//! output cursor, TargetRead embeds literal bytes, SourceCopy and TargetCopy
//! move a cursor with a signed relative offset before copying. The footer's
//! patch checksum is verified when the patch is opened; the source checksum
//! and declared source size before anything is written, and the target
//! checksum on the result.

use super::io::{self, Range};
use crate::util::{CancelToken, Cancelled};
use anyhow::{Context, Result, ensure};
use std::fs::File;
use std::path::Path;

pub(super) fn apply(
    patch_path: &Path,
    source_path: &Path,
    out: &File,
    buf: &mut [u8],
    cancel: &CancelToken,
) -> Result<()> {
    let patch = File::open(patch_path).context("opening the BPS patch")?;
    let patch_len = patch.metadata()?.len();
    ensure!(
        patch_len >= 16,
        "not a valid BPS patch: shorter than its footer"
    );
    let mut r = Range::new(&patch, 4, patch_len - 12);
    let source_size = r.varint_bps()?;
    let target_size = r.varint_bps()?;
    let meta_size = r.varint_bps()?;
    r.skip(meta_size)?;

    let source = File::open(source_path).context("opening the source")?;
    let source_len = source.metadata()?.len();
    ensure!(
        source_len == source_size,
        "source is {source_len} bytes but the patch was built for {source_size}"
    );
    ensure!(
        target_size <= source_len.saturating_add(io::MAX_GROWTH),
        "target size {target_size} exceeds the growth cap"
    );

    let mut footer = [0u8; 12];
    io::read_at(
        &patch,
        &mut footer,
        patch_len - 12,
        "reading the BPS footer",
    )?;
    let expected_target = u32::from_le_bytes(footer[4..8].try_into().expect("fixed-size slice"));

    let mut out_pos = 0u64;
    let mut source_rel = 0u64;
    let mut target_rel = 0u64;
    while r.remaining() > 0 {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let data = r.varint_bps()?;
        let action = data & 3;
        let length = (data >> 2) + 1;
        let target_end = |start: u64| {
            start
                .checked_add(length)
                .filter(|end| *end <= target_size)
                .context("command writes past the declared target size")
        };
        match action {
            0 => {
                // SourceRead: the source cursor is the output cursor.
                let end = target_end(out_pos)?;
                ensure!(
                    end <= source_size,
                    "SourceRead runs past the end of the source"
                );
                io::copy_range(&source, None, out_pos, out, out_pos, length, buf, cancel)?;
                out_pos = end;
            }
            1 => {
                // TargetRead: literal bytes carried by the patch.
                let end = target_end(out_pos)?;
                io::transfer(&mut r, out, out_pos, length, buf, cancel)?;
                out_pos = end;
            }
            2 => {
                // SourceCopy: move the source cursor relatively, then copy.
                source_rel = seek(source_rel, r.varint_bps()?)?;
                let source_end = source_rel
                    .checked_add(length)
                    .filter(|end| *end <= source_size)
                    .context("SourceCopy runs past the end of the source")?;
                let end = target_end(out_pos)?;
                io::copy_range(&source, None, source_rel, out, out_pos, length, buf, cancel)?;
                source_rel = source_end;
                out_pos = end;
            }
            3 => {
                // TargetCopy: move the target cursor relatively, then copy
                // from the data already written to the output. Only the
                // first byte must be written: an overlapping forward copy
                // propagates its pattern, which is how runs are encoded.
                target_rel = seek(target_rel, r.varint_bps()?)?;
                ensure!(
                    target_rel < out_pos,
                    "TargetCopy reads target bytes that are not written yet"
                );
                let end = target_end(out_pos)?;
                io::copy_within_file(out, target_rel, out_pos, length, buf, cancel)?;
                target_rel += length;
                out_pos = end;
            }
            _ => unreachable!("action is two bits"),
        }
    }
    ensure!(
        out_pos == target_size,
        "patch produced {out_pos} bytes but the header declares {target_size}"
    );
    let actual = io::crc32_of_range(out, 0, out_pos, buf, cancel)?;
    ensure!(
        actual == expected_target,
        "patched output failed its target checksum: expected {expected_target:08x}, got {actual:08x}"
    );
    Ok(())
}

/// Applies a BPS signed relative offset: the sign is bit 0, the magnitude
/// the remaining bits.
fn seek(base: u64, delta: u64) -> Result<u64> {
    let magnitude = delta >> 1;
    let moved = if delta & 1 == 0 {
        base.checked_add(magnitude)
    } else {
        base.checked_sub(magnitude)
    };
    moved.context("a relative offset moves the cursor out of range")
}

#[cfg(test)]
mod tests {
    use super::super::io::test_support::bps_varint;
    use super::super::test_support::{apply_err, apply_ok};
    use super::super::{CancelToken, Patch};
    use crc::{CRC_32_ISO_HDLC, Crc};

    fn crc32(data: &[u8]) -> u32 {
        Crc::<u32>::new(&CRC_32_ISO_HDLC).checksum(data)
    }

    /// Builds a BPS patch from `(action, argument, payload)` steps. Only the
    /// literal action carries bytes inside the patch; the other payloads
    /// define their length.
    fn build(source: &[u8], target: &[u8], steps: &[(&str, Option<u64>, &[u8])]) -> Vec<u8> {
        let mut patch = b"BPS1".to_vec();
        patch.extend(bps_varint(source.len() as u64));
        patch.extend(bps_varint(target.len() as u64));
        patch.extend(bps_varint(0));
        for (action, offset, payload) in steps {
            let length = payload.len() as u64;
            let code = match *action {
                "source_read" => (length - 1) << 2,
                "target_read" => (length - 1) << 2 | 1,
                "source_copy" => (length - 1) << 2 | 2,
                "target_copy" => (length - 1) << 2 | 3,
                other => unreachable!("{other}"),
            };
            patch.extend(bps_varint(code));
            if matches!(*action, "source_copy" | "target_copy") {
                // Signed relative offset: sign in bit 0, magnitude above it.
                let encoded = offset.expect("copy offset") << 1;
                patch.extend(bps_varint(encoded));
            }
            if *action == "target_read" {
                patch.extend_from_slice(payload);
            }
        }
        patch.extend(crc32(source).to_le_bytes());
        patch.extend(crc32(target).to_le_bytes());
        let patch_crc = crc32(&patch);
        patch.extend(patch_crc.to_le_bytes());
        patch
    }

    #[test]
    fn round_trips_every_action() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let source = b"Hello, World!";
        let target = b"Hello, BPS!PS!";
        let patch = build(
            source,
            target,
            &[
                ("source_read", None, b"Hello, "),
                ("target_read", None, b"BPS"),
                // Source cursor sits at 0; reach the '!' at 12.
                ("source_copy", Some(12), b"!"),
                // Output cursor sits at 11; reach the "PS!" at offset 8.
                ("target_copy", Some(8), b"PS!"),
            ],
        );
        let output = apply_ok(dir.path(), "all.bps", &patch, source, &cancel);
        assert_eq!(output, target);
        let parsed = Patch::open(&dir.path().join("all.bps"), &CancelToken::new()).unwrap();
        assert_eq!(parsed.format(), "bps");
        assert_eq!(parsed.source_crc(), Some(crc32(source)));
    }

    #[test]
    fn targetcopy_encodes_runs_through_overlap() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // One literal 'a', then a forward-overlapping copy of six bytes from
        // offset 0 propagates the run.
        let patch = build(
            b"x",
            b"aaaaaaa",
            &[
                ("target_read", None, b"a"),
                ("target_copy", Some(0), b"aaaaaa"),
            ],
        );
        let output = apply_ok(dir.path(), "run.bps", &patch, b"x", &cancel);
        assert_eq!(output, b"aaaaaaa");
    }

    #[test]
    fn targetcopy_walks_backwards_with_a_negative_offset() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // TargetRead "abcd"; TargetCopy +2 (one byte at offset 2, leaving
        // the cursor at 3); TargetCopy -3 (sign bit set, magnitude 3,
        // encoded 7) of two bytes back at offset 0: output "abcd" + "c" +
        // "ab".
        let mut patch = b"BPS1".to_vec();
        patch.extend(bps_varint(3));
        patch.extend(bps_varint(7));
        patch.extend(bps_varint(0));
        patch.extend(bps_varint((4 - 1) << 2 | 1));
        patch.extend_from_slice(b"abcd");
        patch.extend(bps_varint(3));
        patch.extend(bps_varint(4));
        patch.extend(bps_varint((2 - 1) << 2 | 3));
        patch.extend(bps_varint(7));
        patch.extend(crc32(b"xyz").to_le_bytes());
        patch.extend(crc32(b"abcdcab").to_le_bytes());
        let patch_crc = crc32(&patch);
        patch.extend(patch_crc.to_le_bytes());
        let output = apply_ok(dir.path(), "back.bps", &patch, b"xyz", &cancel);
        assert_eq!(output, b"abcdcab");
    }

    #[test]
    fn targetcopy_cannot_start_on_unwritten_data() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let patch = build(b"abc", b"abcabc", &[("target_copy", Some(0), b"abcabc")]);
        let err = apply_err(dir.path(), "future.bps", &patch, b"abc", &cancel);
        assert!(err.to_string().contains("not written yet"), "{err}");
    }

    #[test]
    fn a_sourcecopy_past_the_source_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // SourceCopy with relative offset +10 over a 3-byte source walks the
        // cursor past the source's end.
        let patch = build(b"abc", b"x", &[("source_copy", Some(10), b"x")]);
        let err = apply_err(dir.path(), "past.bps", &patch, b"abc", &cancel);
        assert!(
            err.to_string().contains("past the end of the source"),
            "{err}"
        );
    }

    #[test]
    fn a_declared_target_beyond_the_growth_cap_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // A 3-byte source with a target declared just past the 1 GiB cap.
        let mut patch = b"BPS1".to_vec();
        patch.extend(bps_varint(3));
        patch.extend(bps_varint(3 + (1 << 30) + 1));
        patch.extend(bps_varint(0));
        patch.extend(crc32(b"abc").to_le_bytes());
        patch.extend(crc32(b"").to_le_bytes());
        let patch_crc = crc32(&patch);
        patch.extend(patch_crc.to_le_bytes());
        let err = apply_err(dir.path(), "growth.bps", &patch, b"abc", &cancel);
        assert!(err.to_string().contains("growth cap"), "{err}");
    }

    #[test]
    fn a_command_cannot_write_past_the_declared_target() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let patch = build(b"abc", b"xy", &[("target_read", None, b"xyz")]);
        let err = apply_err(dir.path(), "over.bps", &patch, b"abc", &cancel);
        assert!(
            err.to_string().contains("past the declared target"),
            "{err}"
        );
    }

    #[test]
    fn a_forged_target_checksum_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let mut patch = build(b"abc", b"xyz", &[("target_read", None, b"xyz")]);
        with_target_crc(&mut patch, crc32(b"zzz"));
        let err = apply_err(dir.path(), "crc.bps", &patch, b"abc", &cancel);
        assert!(err.to_string().contains("target checksum"), "{err}");
    }

    /// Overwrites the footer's target checksum (and refreshes the patch
    /// checksum so `open` still accepts the file).
    fn with_target_crc(patch: &mut [u8], target_crc: u32) {
        let len = patch.len();
        patch[len - 8..len - 4].copy_from_slice(&target_crc.to_le_bytes());
        let patch_crc = crc32(&patch[..len - 4]);
        patch[len - 4..].copy_from_slice(&patch_crc.to_le_bytes());
    }
}
