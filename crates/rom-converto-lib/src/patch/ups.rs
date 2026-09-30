//! UPS patch application. A `UPS1` magic, two biased variable-length
//! numbers (source size, target size), then hunks until a 12-byte footer of
//! source/target/patch CRC32
//! values. Each hunk is a biased variable-length skip count (bytes copied
//! verbatim from the source) followed by XOR bytes terminated by a zero
//! byte. The terminator occupies one output position (the target byte
//! equals the source byte there, zero past the source's end), and after the
//! last hunk the source (zero past its end) is copied up to the target
//! size, so a patch may grow or shrink its target. The footer's patch
//! checksum is verified when the patch is opened, the source checksum and
//! declared source size before anything is written, and the target checksum
//! on the result.

use super::io::{self, Range};
use crate::util::{CancelToken, Cancelled};
use anyhow::{Context, Result, bail, ensure};
use std::fs::File;
use std::path::Path;

pub(super) fn apply(
    patch_path: &Path,
    source_path: &Path,
    out: &File,
    buf: &mut [u8],
    cancel: &CancelToken,
) -> Result<()> {
    let patch = File::open(patch_path).context("opening the UPS patch")?;
    let patch_len = patch.metadata()?.len();
    ensure!(
        patch_len >= 16,
        "not a valid UPS patch: shorter than its footer"
    );
    let mut r = Range::new(&patch, 4, patch_len - 12);
    let source_size = r.varint_bps()?;
    let target_size = r.varint_bps()?;

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
        "reading the UPS footer",
    )?;
    let expected_target = u32::from_le_bytes(footer[4..8].try_into().expect("fixed-size slice"));

    let mut out_pos = 0u64;
    while r.remaining() > 0 {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let skip = r.varint_bps()?;
        let hunk_start = out_pos;
        out_pos = out_pos
            .checked_add(skip)
            .context("a hunk offset overflows")?;
        // Bytes past the target size are discarded, so clip the copy.
        let written = skip.min(target_size.saturating_sub(hunk_start));
        io::copy_range(
            &source,
            Some(source_len),
            hunk_start,
            out,
            hunk_start,
            written,
            buf,
            cancel,
        )?;
        // XOR data up to the terminating zero byte. Collect byte by byte
        // (the reader buffers) so the scan never consumes bytes past the
        // terminator that belong to the next hunk; every byte, terminator
        // included, occupies one output position. The closure only captures
        // the output cursor and the shared handles; the collected bytes and
        // the scratch half are arguments, so the collect loop can keep
        // writing the first half.
        let half = buf.len() / 2;
        let (patch_buf, source_buf) = buf.split_at_mut(half);
        let flush = |out_pos: &mut u64, data: &[u8], scratch: &mut [u8]| -> Result<()> {
            if *out_pos < target_size {
                let writable = (data.len() as u64).min(target_size - *out_pos) as usize;
                io::xor_padded(
                    &source,
                    source_len,
                    out,
                    *out_pos,
                    &data[..writable],
                    scratch,
                    cancel,
                )?;
            }
            *out_pos = out_pos
                .checked_add(data.len() as u64)
                .context("a hunk offset overflows")?;
            Ok(())
        };
        let mut n = 0usize;
        loop {
            if cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            let byte = match r.try_byte()? {
                Some(byte) => byte,
                None => bail!("the patch ends inside a hunk"),
            };
            if byte == 0 {
                flush(&mut out_pos, &patch_buf[..n], source_buf)?;
                break;
            }
            patch_buf[n] = byte;
            n += 1;
            if n == half {
                flush(&mut out_pos, &patch_buf[..n], source_buf)?;
                n = 0;
            }
        }
        // The terminator copies one source byte (zero past the end).
        if out_pos < target_size {
            let mut one = [0u8; 1];
            if out_pos < source_len {
                io::read_at(&source, &mut one, out_pos, "reading the source")?;
            }
            io::write_at(out, &one, out_pos)?;
        }
        out_pos = out_pos.checked_add(1).context("a hunk offset overflows")?;
    }
    // After the last hunk the source (zero past its end) fills the target.
    if out_pos < target_size {
        io::copy_range(
            &source,
            Some(source_len),
            out_pos,
            out,
            out_pos,
            target_size - out_pos,
            buf,
            cancel,
        )?;
    }
    out.set_len(target_size)
        .context("sizing the patched output")?;
    let actual = io::crc32_of_range(out, 0, target_size, buf, cancel)?;
    ensure!(
        actual == expected_target,
        "patched output failed its target checksum: expected {expected_target:08x}, got {actual:08x}"
    );
    Ok(())
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

    /// Builds a UPS patch from skip/data hunk pairs; each hunk's XOR data is
    /// followed by the zero terminator.
    fn build(source: &[u8], target: &[u8], hunks: &[(u64, &[u8])]) -> Vec<u8> {
        let mut patch = b"UPS1".to_vec();
        patch.extend(bps_varint(source.len() as u64));
        patch.extend(bps_varint(target.len() as u64));
        for (skip, data) in hunks {
            patch.extend(bps_varint(*skip));
            patch.extend_from_slice(data);
            patch.push(0);
        }
        patch.extend(crc32(source).to_le_bytes());
        patch.extend(crc32(target).to_le_bytes());
        let patch_crc = crc32(&patch);
        patch.extend(patch_crc.to_le_bytes());
        patch
    }

    #[test]
    fn round_trips_hunks_with_terminator_positions() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let source = b"AAAABBBB";
        let target = b"AAAAXXYYZZ";
        // One hunk: skip "AAAA", then XOR the six remaining target bytes
        // (the two past the source's end read as zero), terminator landing
        // past the target and discarded with the tail.
        let patch = build(
            source,
            target,
            &[(
                4,
                &[
                    b'B' ^ b'X',
                    b'B' ^ b'X',
                    b'B' ^ b'Y',
                    b'B' ^ b'Y',
                    b'Z',
                    b'Z',
                ],
            )],
        );
        let output = apply_ok(dir.path(), "x.ups", &patch, source, &cancel);
        assert_eq!(output, target);
        let parsed = Patch::open(&dir.path().join("x.ups"), &CancelToken::new()).unwrap();
        assert_eq!(parsed.format(), "ups");
        assert_eq!(parsed.source_crc(), Some(crc32(source)));
    }

    #[test]
    fn a_shrinking_patch_discards_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let source = b"AAAABBBB";
        // One hunk: skip 'A', XOR three bytes, terminator at position 4
        // lands on the target boundary and is discarded with the tail.
        let patch = build(source, b"AXXX", &[(1, &[b'A' ^ b'X'; 3])]);
        let output = apply_ok(dir.path(), "shrink.ups", &patch, source, &cancel);
        assert_eq!(output, b"AXXX");
    }

    #[test]
    fn an_empty_hunk_copies_one_source_byte() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // Each empty hunk's terminator copies one source byte.
        let patch = build(b"abcd", b"abcd", &[(2, &[]), (0, &[]), (1, &[])]);
        let output = apply_ok(dir.path(), "skip.ups", &patch, b"abcd", &cancel);
        assert_eq!(output, b"abcd");
    }

    #[test]
    fn a_wrong_source_checksum_is_rejected_by_apply() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let patch = build(b"real source", b"target", &[(11, &[])]);
        let err = apply_err(dir.path(), "patch.ups", &patch, b"fake source", &cancel);
        assert!(err.to_string().contains("does not match"), "{err}");
    }

    #[test]
    fn a_terminator_occupies_an_output_position() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // Hunk 1: skip 'a', XOR one byte, terminator copies source[2] 'c'.
        // Hunk 2: skip 0, XOR 'Y' at position 3, past the source's end.
        // Proves the first terminator consumed position 2.
        let patch = build(
            b"abcd",
            b"aXcY",
            &[(1, &[b'b' ^ b'X']), (0, &[b'd' ^ b'Y'])],
        );
        let output = apply_ok(dir.path(), "term.ups", &patch, b"abcd", &cancel);
        assert_eq!(output, b"aXcY");
    }

    #[test]
    fn a_huge_declared_target_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let mut patch = b"UPS1".to_vec();
        patch.extend(bps_varint(3));
        patch.extend(bps_varint(u64::MAX / 2));
        patch.extend(crc32(b"abc").to_le_bytes());
        patch.extend(crc32(b"").to_le_bytes());
        let patch_crc = crc32(&patch);
        patch.extend(patch_crc.to_le_bytes());
        let err = apply_err(dir.path(), "huge.ups", &patch, b"abc", &cancel);
        assert!(err.to_string().contains("growth cap"), "{err}");
    }

    #[test]
    fn the_embedded_footer_crc_wins_over_the_name_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("DEADBEEF.ups");
        std::fs::write(&path, build(b"real source", b"target", &[(11, &[])])).unwrap();
        let parsed = Patch::open(&path, &CancelToken::new()).unwrap();
        assert_eq!(parsed.source_crc(), Some(crc32(b"real source")));

        // Without a readable footer the name token is the only source.
        let path = dir.path().join("00ABCDEF.ups");
        std::fs::write(&path, b"UPS1").unwrap();
        assert_eq!(
            Patch::open(&path, &CancelToken::new())
                .unwrap()
                .source_crc(),
            Some(0x00AB_CDEF)
        );
    }
}
