//! NCA section CTR. Counter layout is `ctr_iv (8 bytes BE) ||
//! (offset_in_nca / 16) BE`. The CTR call is symmetric so both
//! encrypt and decrypt go through `apply_ctr`.

use aes::Aes128;
use aes::cipher::{KeyIvInit, StreamCipher};
use ctr::Ctr128BE;

use crate::nintendo::nx::error::{NxError, NxResult};

pub type AesCtr = Ctr128BE<Aes128>;

pub fn apply_ctr(key: &[u8; 16], counter: &[u8; 16], data: &mut [u8]) -> NxResult<()> {
    let mut cipher = AesCtr::new_from_slices(key, counter)
        .map_err(|e| NxError::AesError(format!("Ctr128BE init: {e}")))?;
    cipher.apply_keystream(data);
    Ok(())
}

pub fn counter_for_offset(ctr_iv: &[u8; 8], nca_offset: u64) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(ctr_iv);
    let blocks = nca_offset / 16;
    out[8..].copy_from_slice(&blocks.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_layout_matches_spec() {
        let iv: [u8; 8] = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00, 0x11];
        let c = counter_for_offset(&iv, 0x10);
        assert_eq!(&c[..8], &iv);
        assert_eq!(&c[8..], &[0u8, 0, 0, 0, 0, 0, 0, 1]);

        let c2 = counter_for_offset(&iv, 0x100);
        assert_eq!(&c2[8..], &[0u8, 0, 0, 0, 0, 0, 0, 0x10]);
    }

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
}
