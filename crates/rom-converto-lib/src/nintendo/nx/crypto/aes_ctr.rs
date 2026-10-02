//! NCA section CTR. Counter layout is `ctr_iv (8 bytes BE) ||
//! (offset_in_nca / 16) BE`. The CTR call is symmetric so both
//! encrypt and decrypt go through `apply_ctr`.

use aes::Aes128;
use aes::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
use ctr::Ctr128BE;

use crate::nintendo::nx::error::{NxError, NxResult};

/// AES-128 in CTR mode with a 128-bit big-endian counter, as used for
/// NCA section data.
pub type AesCtr = Ctr128BE<Aes128>;

/// Applies the AES-CTR keystream to `data` in place. Symmetric: the
/// same call encrypts or decrypts depending on which side holds the
/// plaintext.
///
/// # Errors
/// Returns [`NxError::AesError`] if `key` or `counter` cannot
/// initialize the cipher (never happens for the fixed 16-byte sizes
/// this crate passes).
pub fn apply_ctr(key: &[u8; 16], counter: &[u8; 16], data: &mut [u8]) -> NxResult<()> {
    let mut cipher = AesCtr::new_from_slices(key, counter)
        .map_err(|e| NxError::AesError(format!("Ctr128BE init: {e}")))?;
    cipher.apply_keystream(data);
    Ok(())
}

/// Builds the 128-bit counter for `nca_offset`: the 8-byte `ctr_iv`
/// followed by the big-endian block index (`nca_offset / 16`). The
/// FsHeader-based builder for whole-section reads is
/// `initial_ctr_for_offset`.
fn counter_for_offset(ctr_iv: &[u8; 8], nca_offset: u64) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(ctr_iv);
    let blocks = nca_offset / 16;
    out[8..].copy_from_slice(&blocks.to_be_bytes());
    out
}

/// Applies the keystream for the section whose counter prefix is
/// `ctr_iv` to `data` sitting at `nca_offset`. Unlike [`apply_ctr`],
/// the offset need not be 16-aligned: the counter starts at the
/// enclosing block and the keystream is advanced past the leading
/// bytes that precede `nca_offset` in that block.
pub fn apply_ctr_at(
    key: &[u8; 16],
    ctr_iv: &[u8; 8],
    nca_offset: u64,
    data: &mut [u8],
) -> NxResult<()> {
    let counter = counter_for_offset(ctr_iv, nca_offset);
    let mut cipher = AesCtr::new_from_slices(key, &counter)
        .map_err(|e| NxError::AesError(format!("Ctr128BE init: {e}")))?;
    cipher
        .try_seek(nca_offset % 16)
        .map_err(|e| NxError::AesError(format!("Ctr128BE seek: {e}")))?;
    cipher.apply_keystream(data);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctr_round_trip() {
        let key = [0x11u8; 16];
        let counter = [0x22u8; 16];
        let original = (0..1024).map(|i| (i & 0xFF) as u8).collect::<Vec<_>>();
        let mut buf = original.clone();
        apply_ctr(&key, &counter, &mut buf).unwrap();
        assert_ne!(buf, original);
        apply_ctr(&key, &counter, &mut buf).unwrap();
        assert_eq!(buf, original);
    }

    #[test]
    fn ctr_resumes_at_offset() {
        let key = [0x11u8; 16];
        let iv = [0x22u8; 8];
        let original: Vec<u8> = (0..2048).map(|i| (i & 0xFF) as u8).collect();
        let mut full = original.clone();
        apply_ctr(&key, &counter_for_offset(&iv, 0), &mut full).unwrap();

        let mut second_half = original[1024..].to_vec();
        apply_ctr(&key, &counter_for_offset(&iv, 1024), &mut second_half).unwrap();
        assert_eq!(&full[1024..], second_half.as_slice());
    }

    #[test]
    fn unaligned_offset_matches_aligned_keystream() {
        let key = [0x11u8; 16];
        let iv = [0x22u8; 8];
        let original: Vec<u8> = (0..256).map(|i| (i & 0xFF) as u8).collect();
        let mut full = original.clone();
        apply_ctr(&key, &counter_for_offset(&iv, 0x100), &mut full).unwrap();

        let mut tail = original[37..].to_vec();
        apply_ctr_at(&key, &iv, 0x100 + 37, &mut tail).unwrap();
        assert_eq!(&full[37..], tail.as_slice());
    }
}
