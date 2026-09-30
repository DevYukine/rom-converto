//! PPF 1.0, 2.0 and 3.0 patch application. Each flavour carries its
//! `PPF10`/`PPF20`/`PPF30` magic, an encoding byte, a
//! 50-byte description, then for v3 the image type, block-check and undo
//! flags and for v2 a 4-byte image size. PPF 1.0 has neither image size
//! nor undo data: its records start right after the description. The
//! 1024-byte validation block follows the v2 header always, and the v3
//! header only when the block-check flag is set. Records are a
//! little-endian offset (4 bytes on v1 and v2, 8 on v3), a 1-byte length
//! and that many data bytes, plus the same amount of undo bytes when the
//! undo flag is set; every record must land inside the source image, which
//! a PPF overwrites in place. An optional `@BEGIN_FILE_ID.DIZ` section
//! after the records ends them.

use super::io::{self, Range};
use crate::util::{CancelToken, Cancelled};
use anyhow::{Context, Result, bail, ensure};
use std::fs::File;
use std::path::Path;

const FILE_ID_MARKER: &[u8; 18] = b"@BEGIN_FILE_ID.DIZ";

const VALIDATION_BLOCK: u64 = 1024;

pub(super) fn apply(
    patch_path: &Path,
    source_path: &Path,
    out: &File,
    buf: &mut [u8],
    cancel: &CancelToken,
) -> Result<()> {
    let patch = File::open(patch_path).context("opening the PPF patch")?;
    let patch_len = patch.metadata()?.len();
    let mut head = [0u8; 60];
    io::read_at(&patch, &mut head, 0, "reading the PPF header")?;
    let (v1, v3) = match &head[..5] {
        b"PPF10" => (true, false),
        b"PPF20" => (false, false),
        b"PPF30" => (false, true),
        other => bail!("not a PPF patch: bad magic {other:?}"),
    };
    let undo = v3 && head[58] != 0;
    // The validation block describes the original image; this applier
    // relies on the records alone and skips it. PPF 2.0 carries it
    // unconditionally, PPF 3.0 only with the block-check flag set. PPF 1.0
    // has no validation block and no image-size field: its records start
    // right after the description.
    let start = if v1 {
        56
    } else if v3 {
        60 + if head[57] != 0 { VALIDATION_BLOCK } else { 0 }
    } else {
        60 + VALIDATION_BLOCK
    };
    ensure!(start <= patch_len, "PPF patch ends inside its header");

    let source = File::open(source_path).context("opening the source")?;
    let source_len = source.metadata()?.len();
    if !v3 && !v1 {
        // PPF 2.0 records the exact image size it was built against.
        let declared = u32::from_le_bytes(head[56..60].try_into().expect("fixed-size slice"));
        ensure!(
            declared as u64 == source_len,
            "source is {source_len} bytes but the PPF 2.0 header declares {declared}"
        );
    }
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

    let mut r = Range::new(&patch, start, patch_len);
    loop {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        if r.remaining() == 0 || r.peek::<18>()? == Some(*FILE_ID_MARKER) {
            break;
        }
        let offset = if v3 {
            u64::from_le_bytes(r.read_array::<8>()?)
        } else {
            u32::from_le_bytes(r.read_array::<4>()?) as u64
        };
        let len = r.byte()? as u64;
        offset
            .checked_add(len)
            .filter(|end| *end <= source_len)
            .context("a record runs past the end of the image")?;
        r.take(&mut buf[..len as usize])?;
        io::write_at(out, &buf[..len as usize], offset)?;
        if undo {
            // Skip the paired undo bytes; they restore a previously patched
            // image and play no role in a fresh application.
            r.skip(len)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::CancelToken;
    use super::super::test_support::{apply_err, apply_ok};

    fn v2(records: &[Vec<u8>], tail: &[u8]) -> Vec<u8> {
        let mut patch = b"PPF20".to_vec();
        patch.push(1); // encoding: PPF 2.0
        patch.extend_from_slice(&[b' '; 50]);
        patch.extend(8u32.to_le_bytes()); // image size, must equal the source length
        patch.extend_from_slice(&[0x5A; 1024]); // validation block
        for record in records {
            patch.extend_from_slice(record);
        }
        patch.extend_from_slice(tail);
        patch
    }

    fn v3(records: &[Vec<u8>], undo: bool, block_check: bool, tail: &[u8]) -> Vec<u8> {
        let mut patch = b"PPF30".to_vec();
        patch.push(2); // encoding: PPF 3.0
        patch.extend_from_slice(&[b' '; 50]);
        patch.push(0); // image type: BIN
        patch.push(u8::from(block_check));
        patch.push(u8::from(undo));
        patch.push(0); // dummy
        if block_check {
            patch.extend_from_slice(&[0x5A; 1024]);
        }
        for record in records {
            patch.extend_from_slice(record);
        }
        patch.extend_from_slice(tail);
        patch
    }

    fn v2_record(offset: u32, payload: &[u8]) -> Vec<u8> {
        let mut out = offset.to_le_bytes().to_vec();
        out.push(payload.len() as u8);
        out.extend_from_slice(payload);
        out
    }

    fn v3_record(offset: u64, payload: &[u8], undo_data: Option<&[u8]>) -> Vec<u8> {
        let mut out = offset.to_le_bytes().to_vec();
        out.push(payload.len() as u8);
        out.extend_from_slice(payload);
        if let Some(undo_data) = undo_data {
            out.extend_from_slice(undo_data);
        }
        out
    }

    fn v1(records: &[Vec<u8>], tail: &[u8]) -> Vec<u8> {
        let mut patch = b"PPF10".to_vec();
        patch.push(0); // encoding
        patch.extend_from_slice(&[b' '; 50]);
        for record in records {
            patch.extend_from_slice(record);
        }
        patch.extend_from_slice(tail);
        patch
    }

    #[test]
    fn applies_v1_records_without_a_size_or_validation_block() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // PPF 1.0 declares no image size: any source length is accepted,
        // and the records start right after the description.
        let patch = v1(&[v2_record(2, b"XY")], b"");
        let output = apply_ok(dir.path(), "v1.ppf", &patch, b"abcdefgh", &cancel);
        assert_eq!(output, b"abXYefgh");
        // A differently sized source applies the same way.
        let output = apply_ok(dir.path(), "v1b.ppf", &patch, b"abcdef", &cancel);
        assert_eq!(output, b"abXYef");
    }

    #[test]
    fn applies_v2_records_behind_the_validation_block() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let patch = v2(&[v2_record(2, b"XY"), v2_record(6, b"GH")], &[]);
        let output = apply_ok(dir.path(), "v2.ppf", &patch, b"abcdefgh", &cancel);
        assert_eq!(output, b"abXYefGH");
    }

    #[test]
    fn applies_v3_records_and_skips_the_validation_block() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let patch = v3(
            &[v3_record(1, b"BCD", None), v3_record(5, b"f", None)],
            false,
            true,
            &[],
        );
        let output = apply_ok(dir.path(), "v3.ppf", &patch, b"abcdefgh", &cancel);
        assert_eq!(output, b"aBCDefgh");
    }

    #[test]
    fn v3_undo_bytes_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let patch = v3(
            &[
                v3_record(0, b"Z", Some(b"a")),
                v3_record(3, b"DE", Some(b"cd")),
            ],
            true,
            false,
            &[],
        );
        let output = apply_ok(dir.path(), "undo.ppf", &patch, b"abcdefgh", &cancel);
        assert_eq!(output, b"ZbcDEfgh");
    }

    #[test]
    fn a_file_id_section_ends_the_records() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // Garbage after the marker must not be parsed as records.
        let patch = v3(
            &[v3_record(0, b"Z", None)],
            false,
            false,
            b"@BEGIN_FILE_ID.DIZnot-a-record@END_FILE_ID.DIZ\x04\x00",
        );
        let output = apply_ok(dir.path(), "diz.ppf", &patch, b"abcdefgh", &cancel);
        assert_eq!(output, b"Zbcdefgh");
    }

    #[test]
    fn a_record_cannot_run_past_the_image() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // Eight image bytes, one record claiming bytes 4..12.
        let patch = v2(&[v2_record(4, b"AAAAAAAA")], &[]);
        let err = apply_err(dir.path(), "past.ppf", &patch, b"abcdefgh", &cancel);
        assert!(
            err.to_string().contains("past the end of the image"),
            "{err}"
        );
    }

    #[test]
    fn a_v2_image_size_mismatch_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let mut patch = b"PPF20".to_vec();
        patch.push(1); // encoding: PPF 2.0
        patch.extend_from_slice(&[b' '; 50]);
        patch.extend(9u32.to_le_bytes()); // image size, must equal the source length
        patch.extend_from_slice(&[0x5A; 1024]);
        let err = apply_err(dir.path(), "size.ppf", &patch, b"abcdefgh", &cancel);
        assert!(err.to_string().contains("header declares"), "{err}");
    }

    #[test]
    fn a_truncated_record_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // A record whose length byte promises eight bytes but carries four.
        let mut record = 0u32.to_le_bytes().to_vec();
        record.push(8);
        record.extend_from_slice(b"abcd");
        let patch = v2(&[record], &[]);
        let err = apply_err(dir.path(), "cut.ppf", &patch, b"abcdefgh", &cancel);
        assert!(err.to_string().contains("unexpected end"), "{err}");
    }
}
