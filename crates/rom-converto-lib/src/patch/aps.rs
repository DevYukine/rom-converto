//! APS patch application. Two unrelated formats share the extension.
//!
//! GBA (`APS1` magic): little-endian original and modified sizes, then
//! 64 KiB records of a little-endian offset, two 16-bit checksum fields
//! (nominal checksums of the source and patched block; read and skipped,
//! never computed or compared), and 65536 XOR bytes. The record's XOR
//! input is the pristine source, reading as zero past its end. A record
//! may reach past the modified size; the result is sized to the declared
//! modified size, which truncates a final record that reaches past it.
//!
//! N64 (`APS10` magic with a type byte of 0 or 1): a type byte (1 records
//! the cartridge identification, 0 is the simple form), an ignored
//! encoding byte, a 50-byte description, a 17-byte identification block
//! when the type is 1 (one original-format byte, 3 cartridge-ID bytes from
//! ROM offsets 0x3C..0x3F, 8 boot-checksum bytes from ROM offsets
//! 0x10..0x18, 5 pad bytes), a little-endian target size, then records of
//! a little-endian offset, a one-byte length and that many literal bytes,
//! with length 0 selecting an RLE form of a fill byte and a one-byte
//! count. The original-format byte is informational (0 and 1 are both
//! valid, 1 being the common native-order case): the cartridge-ID and
//! boot-checksum comparison already rejects a source in the other byte
//! order.
//!
//! Routing: a record-aligned body is applied as GBA first; the N64
//! fallback applies only when the GBA original-size check fails. A GBA
//! patch with such a header applied to a source of the wrong size reports
//! the N64 applier's error under the GBA size-mismatch context.

use super::Format;
use super::io::{self, Range};
use crate::util::{CancelToken, Cancelled};
use anyhow::{Context, Result, ensure};
use std::fs::File;
use std::path::Path;

const GBA_BLOCK: usize = 64 * 1024;

/// True when the header spells `APS10` with a valid type byte.
pub(super) fn aps10_shaped(head: &[u8]) -> bool {
    head.len() >= 6 && &head[..5] == b"APS10" && (head[5] == 0 || head[5] == 1)
}

pub(super) fn apply(
    patch_path: &Path,
    source_path: &Path,
    out: &File,
    kind: Format,
    buf: &mut [u8],
    cancel: &CancelToken,
) -> Result<()> {
    let patch = File::open(patch_path).context("opening the APS patch")?;
    let patch_len = patch.metadata()?.len();
    ensure!(
        patch_len >= 12,
        "not a valid APS patch: shorter than its header"
    );
    let mut head = [0u8; 12];
    io::read_at(&patch, &mut head, 0, "reading the APS header")?;
    let source = File::open(source_path).context("opening the source")?;
    let source_len = source.metadata().context("reading the source")?.len();
    let original = u32::from_le_bytes(head[4..8].try_into().expect("fixed-size slice")) as u64;
    if kind == Format::ApsN64 {
        return n64(
            &patch, patch_len, &head, &source, source_len, out, buf, cancel,
        );
    }
    // A record-aligned body is applied as GBA first. The N64 fallback
    // applies only when the GBA original-size check fails, decided before
    // anything is written; it never triggers on cancellation, IO errors,
    // or mid-stream failures.
    if source_len != original && aps10_shaped(&head) {
        let result = n64(
            &patch, patch_len, &head, &source, source_len, out, buf, cancel,
        );
        return match result {
            // Cancellation is never dressed up as a flavour mismatch.
            Ok(()) => Ok(()),
            Err(err) if Cancelled::in_chain(&err) => Err(err),
            Err(err) => Err(err.context(format!(
                "as a GBA patch the source is {source_len} bytes but the patch was built \
                 for {original}"
            ))),
        };
    }
    gba(
        &patch, patch_len, &head, original, &source, source_len, out, buf, cancel,
    )
}

#[allow(clippy::too_many_arguments)]
fn gba(
    patch: &File,
    patch_len: u64,
    head: &[u8; 12],
    original: u64,
    source: &File,
    source_len: u64,
    out: &File,
    buf: &mut [u8],
    cancel: &CancelToken,
) -> Result<()> {
    assert!(
        buf.len() >= GBA_BLOCK,
        "the GBA applier needs a full 64 KiB scratch block"
    );
    ensure!(&head[..4] == b"APS1", "not a GBA APS patch: bad magic");

    let modified = u32::from_le_bytes(head[8..12].try_into().expect("fixed-size slice")) as u64;

    ensure!(
        source_len == original,
        "source is {source_len} bytes but the patch was built for {original}"
    );
    ensure!(
        modified <= source_len.saturating_add(io::MAX_GROWTH),
        "target size {modified} exceeds the growth cap"
    );
    io::copy_range(source, Some(source_len), 0, out, 0, source_len, buf, cancel)?;

    let mut xor = vec![0u8; GBA_BLOCK];
    let mut r = Range::new(patch, 12, patch_len);
    while r.remaining() >= 8 {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let offset = u32::from_le_bytes(r.read_array::<4>()?) as u64;
        // The two checksum fields are skipped without being interpreted.
        r.skip(4)?;
        // A final record cut short by the end of the file applies with the
        // XOR bytes present.
        let n = (r.remaining() as usize).min(GBA_BLOCK);
        r.take(&mut xor[..n])?;
        ensure!(offset < modified, "a record runs past the modified size");
        // The block XORs against the source, reading as zero past its end.
        let live = source_len.saturating_sub(offset).min(n as u64) as usize;
        if live > 0 {
            io::read_at(source, &mut buf[..live], offset, "reading the source")?;
        }
        buf[live..n].fill(0);
        for (byte, delta) in buf[..n].iter_mut().zip(xor.iter()) {
            *byte ^= delta;
        }
        io::write_at(out, &buf[..n], offset)?;
    }
    ensure!(r.remaining() == 0, "the patch ends inside a record");
    out.set_len(modified).context("sizing the patched output")?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn n64(
    patch: &File,
    patch_len: u64,
    head: &[u8; 12],
    source: &File,
    source_len: u64,
    out: &File,
    buf: &mut [u8],
    cancel: &CancelToken,
) -> Result<()> {
    ensure!(
        patch_len >= 61,
        "not a valid N64 APS patch: shorter than its header"
    );
    ensure!(&head[..5] == b"APS10", "not an N64 APS patch: bad magic");
    let kind = head[5];

    let mut pos = 57u64;
    if kind == 1 {
        // Identification block: one original-format byte (informational; 0
        // and 1 are both valid, 1 being the common native-order case), the
        // 3 cartridge-ID bytes at ROM offsets 0x3C..0x3F, the 8
        // boot-checksum bytes at ROM offsets 0x10..0x18, and 5 pad bytes.
        let mut id = [0u8; 17];
        io::read_at(patch, &mut id, pos, "reading the APS identification")?;
        pos += 17;
        let expected_cart = &id[1..4];
        let expected_boot = &id[4..12];
        ensure!(
            source_len >= 0x40,
            "source is too short to carry N64 identification"
        );
        let mut cart = [0u8; 3];
        io::read_at(source, &mut cart, 0x3C, "reading the cartridge ID")?;
        let mut boot = [0u8; 8];
        io::read_at(source, &mut boot, 0x10, "reading the boot checksum")?;
        ensure!(
            cart == expected_cart,
            "source does not match the patch: the cartridge ID differs"
        );
        ensure!(
            boot == expected_boot,
            "source does not match the patch: the boot checksum differs"
        );
    } else {
        ensure!(kind == 0, "unknown APS kind {kind}");
    }
    let mut raw = [0u8; 4];
    io::read_at(patch, &mut raw, pos, "reading the APS target size")?;
    let target = u32::from_le_bytes(raw) as u64;
    pos += 4;
    ensure!(
        target <= source_len.saturating_add(io::MAX_GROWTH),
        "target size {target} exceeds the growth cap"
    );

    io::copy_range(source, Some(source_len), 0, out, 0, source_len, buf, cancel)?;

    let mut r = Range::new(patch, pos, patch_len);
    while r.remaining() > 0 {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let offset = u32::from_le_bytes(r.read_array::<4>()?) as u64;
        let len = r.byte()? as u64;
        // RLE record: a fill byte and a one-byte count.
        let (fill, len) = if len == 0 {
            (Some(r.byte()?), r.byte()? as u64)
        } else {
            (None, len)
        };
        offset
            .checked_add(len)
            .filter(|end| *end <= target)
            .context("a record runs past the target size")?;
        match fill {
            Some(fill) => io::write_fill(out, offset, fill, len, buf, cancel)?,
            None => {
                r.take(&mut buf[..len as usize])?;
                io::write_at(out, &buf[..len as usize], offset)?;
            }
        }
    }
    out.set_len(target).context("sizing the patched output")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::io;
    use super::super::test_support::{apply_err, apply_ok};
    use super::super::{CancelToken, Cancelled, Patch};

    #[test]
    fn gba_round_trips_a_full_block() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let source: Vec<u8> = (0..64 * 1024u32).map(|i| i as u8).collect();
        let target = vec![0x5Au8; 64 * 1024];
        let xor: Vec<u8> = source
            .iter()
            .zip(target.iter())
            .map(|(s, t)| s ^ t)
            .collect();
        let mut patch = b"APS1".to_vec();
        patch.extend((source.len() as u32).to_le_bytes());
        patch.extend((target.len() as u32).to_le_bytes());
        patch.extend(0u32.to_le_bytes());
        patch.extend(0u16.to_le_bytes());
        patch.extend(0u16.to_le_bytes());
        patch.extend_from_slice(&xor);
        let output = apply_ok(dir.path(), "block.aps", &patch, &source, &cancel);
        assert_eq!(output, target);
        let parsed = Patch::open(&dir.path().join("block.aps"), &CancelToken::new()).unwrap();
        assert_eq!(parsed.format(), "aps");
    }

    #[test]
    fn gba_applies_with_arbitrary_checksum_fields() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let source: Vec<u8> = (0..64 * 1024u32).map(|i| i as u8).collect();
        let target = vec![0x5Au8; 64 * 1024];
        let xor: Vec<u8> = source
            .iter()
            .zip(target.iter())
            .map(|(s, t)| s ^ t)
            .collect();
        let mut patch = b"APS1".to_vec();
        patch.extend((source.len() as u32).to_le_bytes());
        patch.extend((target.len() as u32).to_le_bytes());
        patch.extend(0u32.to_le_bytes());
        // The checksum fields hold arbitrary values and are never verified.
        patch.extend(0xBEEFu16.to_le_bytes());
        patch.extend(0xDEADu16.to_le_bytes());
        patch.extend_from_slice(&xor);
        let output = apply_ok(dir.path(), "variant.aps", &patch, &source, &cancel);
        assert_eq!(output, target);
    }

    #[test]
    fn gba_truncates_a_final_record_past_the_modified_size() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let source: Vec<u8> = (0..64 * 1024u32).map(|i| i as u8).collect();
        // Modified size covers only the first half of the block, which the
        // record inverts with an all-ones XOR pattern.
        let modified = 32 * 1024u32;
        let target: Vec<u8> = source
            .iter()
            .map(|b| b ^ 0xFF)
            .take(modified as usize)
            .collect();
        let xor = vec![0xFFu8; 64 * 1024];
        let mut patch = b"APS1".to_vec();
        patch.extend((source.len() as u32).to_le_bytes());
        patch.extend(modified.to_le_bytes());
        patch.extend(0u32.to_le_bytes());
        patch.extend(0u16.to_le_bytes());
        patch.extend(0u16.to_le_bytes());
        patch.extend_from_slice(&xor);
        let output = apply_ok(dir.path(), "tail.aps", &patch, &source, &cancel);
        assert_eq!(output, target);
    }

    /// Builds an N64 APS with the given `kind` and identification format
    /// bytes and the given ROM bytes.
    fn n64_patch(
        rom: &[u8],
        target: &[u8],
        kind: u8,
        format_byte: u8,
        records: &[Vec<u8>],
    ) -> Vec<u8> {
        let mut patch = b"APS10".to_vec();
        patch.push(kind);
        patch.push(0); // encoding
        patch.extend_from_slice(&[b' '; 50]);
        if kind == 1 {
            let mut id = vec![format_byte];
            id.extend_from_slice(&rom[0x3C..0x3F]); // cart ID
            id.extend_from_slice(&rom[0x10..0x18]); // boot checksum
            id.extend_from_slice(&[0u8; 5]); // pad
            patch.extend_from_slice(&id);
        }
        patch.extend((target.len() as u32).to_le_bytes());
        for record in records {
            patch.extend_from_slice(record);
        }
        patch
    }

    #[test]
    fn n64_simple_round_trips_records_and_rle() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let source = b"0123456789";
        let mut records: Vec<Vec<u8>> = Vec::new();
        let mut literal = 2u32.to_le_bytes().to_vec();
        literal.push(2);
        literal.extend_from_slice(b"AB");
        let mut rle = 7u32.to_le_bytes().to_vec();
        rle.extend([0, b'Q', 3]);
        records.push(literal);
        records.push(rle);
        let patch = n64_patch(source, b"01AB456QQQ", 0, 0, &records);
        let output = apply_ok(dir.path(), "simple.aps", &patch, source, &cancel);
        assert_eq!(output, b"01AB456QQQ");
        let parsed = Patch::open(&dir.path().join("simple.aps"), &CancelToken::new()).unwrap();
        assert_eq!(parsed.format(), "aps");
        // N64 APS carries no CRC32, so the name token is the only match.
        assert_eq!(parsed.source_crc(), None);
    }

    #[test]
    fn n64_identification_verifies_the_rom() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let mut rom = vec![0u8; 0x40];
        rom[0x3C..0x3F].copy_from_slice(b"ABE");
        rom[0x10..0x18].copy_from_slice(&42u64.to_le_bytes());
        let patch = n64_patch(&rom, &rom, 1, 0, &[]);
        let output = apply_ok(dir.path(), "id.aps", &patch, &rom, &cancel);
        assert_eq!(output, rom);

        let mut other = rom.clone();
        other[0x3C] ^= 0xFF;
        let err = apply_err(dir.path(), "bad.aps", &patch, &other, &cancel);
        assert!(err.to_string().contains("cartridge ID"), "{err}");
    }

    #[test]
    fn n64_applies_with_a_native_order_format_byte() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let mut rom = vec![0u8; 0x40];
        rom[0x3C..0x3F].copy_from_slice(b"ABE");
        // Format byte 1 (the common native-order case) is informational and
        // applies like any other.
        let patch = n64_patch(&rom, &rom, 1, 1, &[]);
        let id_at = 5 + 1 + 1 + 50;
        assert_eq!(patch[id_at], 1);
        let output = apply_ok(dir.path(), "native.aps", &patch, &rom, &cancel);
        assert_eq!(output, rom);
    }

    #[test]
    fn a_record_aligned_aps10_gba_patch_stays_gba() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // Original size 0x0030: the header bytes spell "APS10" with a
        // valid type byte (0), but the body is exactly one 65544-byte GBA
        // record, so the router picks GBA and it applies.
        let source = vec![0u8; 0x30];
        let mut patch = b"APS1".to_vec();
        patch.extend(0x30u32.to_le_bytes());
        patch.extend(0x30u32.to_le_bytes());
        // One GBA record at offset 0. The record inverts every byte with
        // an all-ones XOR pattern and reaches past the modified size, so
        // the result is the first 0x30 bytes of the inverted block.
        let xor = vec![0xFFu8; 64 * 1024];
        patch.extend(0u32.to_le_bytes());
        patch.extend(0u16.to_le_bytes());
        patch.extend(0u16.to_le_bytes());
        patch.extend_from_slice(&xor);
        assert_eq!(patch.len(), 12 + 65544);
        let path = dir.path().join("aligned.aps");
        std::fs::write(&path, &patch).unwrap();
        let parsed = Patch::open(&path, &CancelToken::new()).unwrap();
        assert_eq!(parsed.format(), "aps");
        let source_path = dir.path().join("source.bin");
        std::fs::write(&source_path, &source).unwrap();
        let output_path = dir.path().join("out.bin");
        parsed
            .apply(&source_path, &output_path, &cancel)
            .expect("apply patch");
        let flipped: Vec<u8> = source.iter().map(|b| b ^ 0xFF).collect();
        assert_eq!(std::fs::read(&output_path).unwrap(), flipped);
    }

    #[test]
    fn a_short_final_record_applies_with_the_bytes_present() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // One full record inverts the first 64 KiB, then a trailing record
        // of 8 + 5 bytes re-inverts five bytes at offset 1; the payload cut
        // short by the end of the file is applied with the bytes present.
        let source: Vec<u8> = (0..64 * 1024u32).map(|i| i as u8).collect();
        let mut patch = b"APS1".to_vec();
        patch.extend((source.len() as u32).to_le_bytes());
        patch.extend((source.len() as u32).to_le_bytes());
        patch.extend(0u32.to_le_bytes());
        patch.extend(0u16.to_le_bytes());
        patch.extend(0u16.to_le_bytes());
        patch.extend_from_slice(&vec![0xFFu8; 64 * 1024]);
        patch.extend(1u32.to_le_bytes());
        patch.extend(0u16.to_le_bytes());
        patch.extend(0u16.to_le_bytes());
        patch.extend_from_slice(b"12345");
        assert_eq!(patch.len(), 12 + 65544 + 13);
        let output = apply_ok(dir.path(), "tail.aps", &patch, &source, &cancel);
        let mut expected = source.clone();
        for byte in expected.iter_mut() {
            *byte ^= 0xFF;
        }
        // The short record writes the source XOR its own payload.
        for (i, b) in b"12345".iter().enumerate() {
            expected[1 + i] = source[1 + i] ^ b;
        }
        assert_eq!(output, expected);
    }

    #[test]
    fn an_aps10_shaped_body_ending_in_a_short_record_stays_gba() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // The header bytes spell "APS10" with a valid type byte and the
        // body ends in a short record, so the router picks GBA even though
        // the body is not a whole number of full records.
        let source = vec![0u8; 0x30];
        let mut patch = b"APS1".to_vec();
        patch.extend(0x30u32.to_le_bytes());
        patch.extend(0x30u32.to_le_bytes());
        patch.extend(0u32.to_le_bytes());
        patch.extend(0u16.to_le_bytes());
        patch.extend(0u16.to_le_bytes());
        patch.extend_from_slice(&vec![0xFFu8; 64 * 1024]);
        // A final record cut short by the end of the file.
        patch.extend(1u32.to_le_bytes());
        patch.extend(0u16.to_le_bytes());
        patch.extend(0u16.to_le_bytes());
        patch.extend_from_slice(b"12345");
        let path = dir.path().join("tail-shape.aps");
        std::fs::write(&path, &patch).unwrap();
        let parsed = Patch::open(&path, &CancelToken::new()).unwrap();
        assert_eq!(parsed.format(), "aps");
        let source_path = dir.path().join("source.bin");
        std::fs::write(&source_path, &source).unwrap();
        let output_path = dir.path().join("out.bin");
        parsed
            .apply(&source_path, &output_path, &cancel)
            .expect("apply patch");
        let mut expected: Vec<u8> = source.iter().map(|b| b ^ 0xFF).collect();
        // The short record writes the source XOR its own payload.
        for (i, b) in b"12345".iter().enumerate() {
            expected[1 + i] = source[1 + i] ^ b;
        }
        assert_eq!(std::fs::read(&output_path).unwrap(), expected);
    }

    #[test]
    fn a_truncated_record_head_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // Fewer than 8 trailing bytes cannot form a record head.
        let source = vec![0u8; 0x31];
        let mut patch = b"APS1".to_vec();
        patch.extend(0x31u32.to_le_bytes());
        patch.extend(0x31u32.to_le_bytes());
        patch.extend_from_slice(&[0u8; 5]);
        let err = apply_err(dir.path(), "stub.aps", &patch, &source, &cancel);
        assert!(err.to_string().contains("ends inside a record"), "{err}");
    }

    #[test]
    fn a_record_aligned_aps10_n64_patch_falls_back_to_n64() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // A kind-0 N64 patch padded with no-op records until the file tiles
        // as exactly one whole GBA record: the router picks GBA, the
        // original-size mismatch falls back to N64, and it applies. One
        // literal record changes the first byte.
        let rom = vec![0u8; 0x40];
        let mut records: Vec<Vec<u8>> = Vec::new();
        // The first literal changes the first byte; the other literals are
        // no-ops writing the same byte.
        for _ in 0..4 {
            let mut literal = 0u32.to_le_bytes().to_vec();
            literal.push(1);
            literal.push(b'Z');
            records.push(literal);
        }
        for _ in 0..9353 {
            let mut rle = 0u32.to_le_bytes().to_vec();
            rle.extend([0, 0, 0]);
            records.push(rle);
        }
        let patch = n64_patch(&rom, &rom, 0, 0, &records);
        assert_eq!(patch.len(), 12 + 65544);
        let path = dir.path().join("fallback.aps");
        std::fs::write(&path, &patch).unwrap();
        let parsed = Patch::open(&path, &CancelToken::new()).unwrap();
        assert!(matches!(parsed.kind, super::super::Format::ApsGba));
        let source_path = dir.path().join("source.bin");
        std::fs::write(&source_path, &rom).unwrap();
        let output_path = dir.path().join("out.bin");
        parsed
            .apply(&source_path, &output_path, &cancel)
            .expect("apply patch");
        let mut expected = rom.clone();
        expected[0] = b'Z';
        assert_eq!(std::fs::read(&output_path).unwrap(), expected);
    }

    #[test]
    fn a_fallback_failure_carries_both_contexts() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let rom = vec![0u8; 0x40];
        // A record-aligned kind-0 N64 patch whose only record writes past
        // its declared target: the GBA route falls back to N64 on the
        // original-size mismatch, and the failure reports both the N64
        // cause and the GBA original size.
        let target = &rom[..4];
        let mut records: Vec<Vec<u8>> = Vec::new();
        // The first literal writes past the declared target; the others are
        // offset-0 no-ops that only pad the file to a record-aligned
        // length (6-byte and 7-byte records mixing to the exact total).
        let mut literal = 10u32.to_le_bytes().to_vec();
        literal.push(1);
        literal.push(b'X');
        records.push(literal);
        for _ in 0..3 {
            let mut literal = 0u32.to_le_bytes().to_vec();
            literal.push(1);
            literal.push(b'X');
            records.push(literal);
        }
        for _ in 0..9353 {
            let mut rle = 0u32.to_le_bytes().to_vec();
            rle.extend([0, 0, 0]);
            records.push(rle);
        }
        let patch = n64_patch(&rom, target, 0, 0, &records);
        assert_eq!(patch.len(), 12 + 65544);
        let patch_path = dir.path().join("ctx.aps");
        std::fs::write(&patch_path, patch).unwrap();
        let source_path = dir.path().join("source.bin");
        std::fs::write(&source_path, &rom).unwrap();
        let out_path = dir.path().join("out.bin");
        let out = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&out_path)
            .unwrap();
        let mut buf = vec![0u8; super::io::COPY_CHUNK_BYTES];
        let err = super::apply(
            &patch_path,
            &source_path,
            &out,
            super::Format::ApsGba,
            &mut buf,
            &cancel,
        )
        .expect_err("the record must run past the target size");
        let rendered = format!("{err:#}");
        assert!(rendered.contains("past the target size"), "{rendered}");
        assert!(
            rendered.contains("as a GBA patch the source is 64 bytes"),
            "{rendered}"
        );
        assert!(rendered.contains("built for 536870960"), "{rendered}");
    }

    #[test]
    fn a_gba_growth_cap_failure_is_not_routed_to_n64() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // Original size 0x30 spells "APS10" with a valid type byte, but
        // the original size matches the source, so this is a GBA patch:
        // its growth-cap failure must not trigger the N64 fallback.
        let source = vec![0u8; 0x30];
        let mut patch = b"APS1".to_vec();
        patch.extend(0x30u32.to_le_bytes());
        let modified = 0x30u64 + io::MAX_GROWTH + 1;
        patch.extend((modified as u32).to_le_bytes());
        let path = dir.path().join("cap.aps");
        std::fs::write(&path, &patch).unwrap();
        let source_path = dir.path().join("source.bin");
        std::fs::write(&source_path, &source).unwrap();
        let parsed = Patch::open(&path, &CancelToken::new()).unwrap();
        let err = parsed
            .apply(&source_path, &dir.path().join("out.bin"), &cancel)
            .expect_err("growth cap must reject");
        assert!(err.to_string().contains("growth cap"), "{err}");
        // The GBA failure must not have been dressed up as a flavour
        // mismatch: no fallback context in the chain.
        let rendered = format!("{err:#}");
        assert!(!rendered.contains("as a GBA patch"), "{rendered}");
    }

    #[test]
    fn a_cancelled_n64_fallback_is_not_dressed_as_a_size_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        let rom = vec![b'Z'; 0x40];
        let mut records: Vec<Vec<u8>> = Vec::new();
        for _ in 0..4 {
            let mut literal = 0u32.to_le_bytes().to_vec();
            literal.push(1);
            literal.push(b'Z');
            records.push(literal);
        }
        for _ in 0..9353 {
            let mut rle = 0u32.to_le_bytes().to_vec();
            rle.extend([0, 0, 0]);
            records.push(rle);
        }
        let patch = n64_patch(&rom, &rom, 0, 0, &records);
        let patch_path = dir.path().join("cancel.aps");
        std::fs::write(&patch_path, patch).unwrap();
        let source_path = dir.path().join("source.bin");
        std::fs::write(&source_path, &rom).unwrap();
        let out_path = dir.path().join("out.bin");
        let out = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&out_path)
            .unwrap();
        let mut buf = vec![0u8; super::io::COPY_CHUNK_BYTES];
        let err = super::apply(
            &patch_path,
            &source_path,
            &out,
            super::Format::ApsGba,
            &mut buf,
            &cancel,
        )
        .expect_err("a pre-cancelled token must cancel");
        assert!(Cancelled::in_chain(&err), "{err:#}");
        assert!(!format!("{err:#}").contains("as a GBA patch"), "{err:#}");
    }

    #[test]
    fn an_aps10_spelling_header_with_an_invalid_type_byte_and_a_wrong_size_source_stays_gba() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // Original size 0x3030: the header bytes spell "APS10" but the type
        // byte (0x30) is not a valid N64 kind, so this is a GBA patch; a
        // wrong-size source surfaces the plain GBA size error.
        let mut patch = b"APS1".to_vec();
        patch.extend(0x3030u32.to_le_bytes());
        patch.extend(0x3030u32.to_le_bytes());
        let path = dir.path().join("size.aps");
        std::fs::write(&path, &patch).unwrap();
        let parsed = Patch::open(&path, &CancelToken::new()).unwrap();
        assert_eq!(parsed.format(), "aps");
        let source_path = dir.path().join("source.bin");
        std::fs::write(&source_path, b"wrong size").unwrap();
        let err = parsed
            .apply(&source_path, &dir.path().join("out.bin"), &cancel)
            .expect_err("wrong-size source must fail");
        assert!(
            err.to_string()
                .contains("but the patch was built for 12336"),
            "{err}"
        );
        assert!(!format!("{err:#}").contains("as a GBA patch"), "{err:#}");
    }

    #[test]
    fn an_aps10_header_with_a_foreign_encoding_byte_routes_as_n64() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // The encoding byte is ignored in flavour detection: the patch
        // routes to the N64 applier, whose record error surfaces dressed in
        // the GBA size-mismatch context.
        let source = b"abc";
        let mut record = 2u32.to_le_bytes().to_vec();
        record.push(4);
        record.extend_from_slice(b"WXYZ");
        let mut patch = n64_patch(source, b"abc", 0, 0, &[record]);
        patch[6] = 1; // a foreign encoding byte
        // Pad to one full GBA record so format detection reads the body as
        // record-aligned GBA, and the N64 applier is reached as fallback.
        patch.resize(12 + 8 + 65536, 0);
        let path = dir.path().join("enc.aps");
        std::fs::write(&path, &patch).unwrap();
        let parsed = Patch::open(&path, &CancelToken::new()).unwrap();
        assert_eq!(parsed.format(), "aps");
        let source_path = dir.path().join("source.bin");
        std::fs::write(&source_path, source).unwrap();
        let err = parsed
            .apply(&source_path, &dir.path().join("out.bin"), &cancel)
            .expect_err("the overrunning record must fail");
        let rendered = format!("{err:#}");
        assert!(rendered.contains("past the target size"), "{rendered}");
        assert!(rendered.contains("as a GBA patch"), "{rendered}");
    }

    #[test]
    fn n64_records_cannot_run_past_the_target_size() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let source = b"abc";
        let mut record = 2u32.to_le_bytes().to_vec();
        record.push(4);
        record.extend_from_slice(b"WXYZ");
        let patch = n64_patch(source, b"abc", 0, 0, &[record]);
        let err = apply_err(dir.path(), "over.aps", &patch, source, &cancel);
        assert!(err.to_string().contains("past the target size"), "{err}");
    }
}
