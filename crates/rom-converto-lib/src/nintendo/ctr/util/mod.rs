use crate::util::{CancelToken, Cancelled, ProgressReporter};
use anyhow::Result;
use binrw::BinResult;
use log::warn;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;

pub mod fs;

pub fn check_cancel(cancel: &CancelToken) -> Result<()> {
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    Ok(())
}

/// Drives a per-file batch over every `exts` file under `input_dir`:
/// progress accounting, a cancel check between files, and turning a
/// per-file failure into a warning so one bad ROM does not abort the run.
/// `labels` is the progress-bar gerund and the failure-message verb, e.g.
/// `("Decrypting", "decrypt")`.
pub async fn run_batch(
    input_dir: &Path,
    exts: &[&str],
    labels: (&str, &str),
    max_depth: Option<usize>,
    total_progress: &dyn ProgressReporter,
    cancel: &CancelToken,
    mut op: impl AsyncFnMut(&Path) -> Result<()>,
) -> Result<()> {
    check_cancel(cancel)?;
    let roms = crate::util::fs::collect_files_with_exts(input_dir, exts, max_depth, cancel)?;
    if roms.is_empty() {
        warn!(
            "No supported ROM files found in {} (looked for {:?})",
            input_dir.display(),
            exts
        );
        return Ok(());
    }

    let (gerund, verb) = labels;
    total_progress.start(roms.len() as u64, &format!("{gerund} {} files", roms.len()));

    for path in roms {
        check_cancel(cancel)?;
        if let Err(err) = op(&path).await {
            if Cancelled::in_chain(&err) || cancel.is_cancelled() {
                return Err(err);
            }
            warn!("Failed to {verb} {}: {err}", path.display());
        }
        total_progress.inc(1);
    }

    total_progress.finish();
    Ok(())
}

/// Mirrors `src`'s position under `output_dir` (see
/// [`crate::util::place_in_dir_mirrored`]) and creates the parent directory.
pub async fn mirrored_output(
    src: &Path,
    input_dir: &Path,
    output_dir: Option<&Path>,
) -> std::io::Result<PathBuf> {
    let output = crate::util::place_in_dir_mirrored(src, input_dir, output_dir);
    if let Some(parent) = output.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    Ok(output)
}

/// Reads `size` bytes from `file`'s current position through `buf` and
/// returns their SHA-256. With `key` set the bytes are AES-128-CBC decrypted
/// in place first, chaining `iv` across chunks, because TMD hashes cover the
/// decrypted content.
pub async fn hash_cbc_stream(
    file: &mut tokio::fs::File,
    key: Option<&[u8; 16]>,
    mut iv: [u8; 16],
    size: u64,
    buf: &mut [u8],
    cancel: &CancelToken,
) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut remaining = size;

    while remaining > 0 {
        check_cancel(cancel)?;
        let to_read = remaining.min(buf.len() as u64) as usize;
        file.read_exact(&mut buf[..to_read]).await?;
        if let Some(key) = key {
            // The next chunk chains off this chunk's last ciphertext block,
            // which in-place decryption is about to overwrite.
            let next_iv: [u8; 16] = buf[to_read - 16..to_read].try_into().expect("16 bytes");
            crate::nintendo::ctr::decrypt::util::cbc_decrypt(key, &iv, &mut buf[..to_read])?;
            iv = next_iv;
        }
        hasher.update(&buf[..to_read]);
        remaining -= to_read as u64;
    }

    Ok(hasher.finalize().into())
}

pub fn align_64(x: u64) -> u64 {
    x.next_multiple_of(64)
}

pub fn align_64_usize(x: usize) -> usize {
    align_64(x as u64) as usize
}

/// True for TWL/DSiWare title ids (title type `0x0004800x`): the high 20
/// bits of the 64-bit title id equal `0x00048`. A TWL content is a modcrypt
/// SRL, not an NCCH, so these titles bypass the NCCH-oriented decrypt,
/// encrypt, info, and CCI conversion paths.
pub fn is_twl_title_id(title_id: u64) -> bool {
    (title_id >> 44) == 0x00048
}

pub fn pad_to_align_64(aligned_pos: u64, writer: &mut (impl Write + Seek)) -> BinResult<()> {
    let pos = writer.stream_position()?;
    if aligned_pos > pos {
        std::io::copy(&mut std::io::repeat(0).take(aligned_pos - pos), writer)?;
    }

    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn test_cia_alignment() {
        assert_eq!(align_64(0), 0);
        assert_eq!(align_64(1), 64);
        assert_eq!(align_64(63), 64);
        assert_eq!(align_64(64), 64);
        assert_eq!(align_64(65), 128);
        assert_eq!(align_64(128), 128);
    }

    #[test]
    fn pad_to_align_64_writes_correct_padding() {
        use std::io::Cursor;

        let mut buffer = Cursor::new(vec![0u8; 10]);
        let aligned_pos = 16;

        pad_to_align_64(aligned_pos, &mut buffer).unwrap();
        assert_eq!(buffer.get_ref().len(), 16);
        assert_eq!(&buffer.get_ref()[10..], &[0u8; 6]);
    }

    #[test]
    fn pad_to_align_64_does_nothing_if_already_aligned() {
        use std::io::Cursor;

        let mut buffer = Cursor::new(vec![0u8; 16]);
        let aligned_pos = 16;

        pad_to_align_64(aligned_pos, &mut buffer).unwrap();
        assert_eq!(buffer.get_ref().len(), 16);
    }

    #[test]
    fn is_twl_title_id_matches_hex_prefix_check() {
        assert!(is_twl_title_id(0x0004_8004_0000_0000));
        assert!(is_twl_title_id(0x0004_800F_FFFF_FFFF));
        assert!(!is_twl_title_id(0x0004_0000_0003_0000));
        assert!(!is_twl_title_id(0x0004_9000_0000_0000));
        assert!(!is_twl_title_id(0));
    }

    /// Proves the identity used by `check_cia_not_encrypted`:
    /// `align_64(a) + align_64(b) + ... = chained_align(a, b, ...)`
    /// (that is, summing independently-aligned section sizes equals walking the
    /// CIA layout section-by-section). The identity holds because each partial
    /// sum is itself a multiple of 64, so `align_64(P + s) = P + align_64(s)`
    /// when P ≡ 0 (mod 64).
    #[test]
    fn align_64_sum_equals_chained_alignment() {
        let cases: &[&[u64]] = &[
            // Realistic CIA: header, cert chain, ticket, TMD
            &[0x2020, 0x0A00, 0x0304, 0x0B40],
            // From test_simple_cia_file
            &[0x2020, 0x0A00, 0x0350, 0x0B34],
            // Non-aligned cert chain (synthetic edge case)
            &[0x2020, 0x0A01, 0x0304, 0x0B40],
            // Many small unaligned sections
            &[0x21, 0x47, 0x83, 0xC1, 0x0F],
            // All aligned
            &[0x40, 0x80, 0xC0, 0x100],
            // Single section
            &[0x2020],
            // Includes zero-sized sections
            &[0x2020, 0x0, 0x304, 0x0],
        ];

        for case in cases {
            let sum_independent: u64 = case.iter().map(|&s| align_64(s)).sum();
            let mut chained: u64 = 0;
            for &s in *case {
                chained = align_64(chained + s);
            }
            assert_eq!(
                sum_independent, chained,
                "formulas disagree for case {case:?}: sum={sum_independent:#x}, chained={chained:#x}"
            );
        }
    }

    #[test]
    fn pad_to_align_64_handles_large_padding() {
        use std::io::Cursor;

        let mut buffer = Cursor::new(vec![0u8; 5]);
        let aligned_pos = 1024;

        pad_to_align_64(aligned_pos, &mut buffer).unwrap();
        assert_eq!(buffer.get_ref().len(), 1024);
        assert_eq!(&buffer.get_ref()[5..], vec![0u8; 1019].as_slice());
    }
}
