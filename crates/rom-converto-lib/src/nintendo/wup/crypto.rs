//! AES-128-CBC helpers and Wii U title key derivation.
//!
//! These are thin wrappers over the `aes` + `cbc` crates, tailored
//! to the two operations the NUS pipeline actually needs:
//! decrypting an encrypted title key from a ticket (using the common
//! key and the title id as IV) and decrypting content blocks in
//! place (using the derived title key).

use aes::{
    Aes128,
    cipher::{BlockDecryptMut, KeyIvInit},
};
use block_padding::NoPadding;
use cbc::Decryptor;

use crate::nintendo::wup::common_keys::WII_U_COMMON_KEY;
use crate::nintendo::wup::error::{WupError, WupResult};

type Aes128CbcDec = Decryptor<Aes128>;

/// Decrypt `data` in place under AES-128-CBC with the given `key`
/// and `iv`. `data.len()` must be a multiple of 16; the function
/// leaves it that size (`NoPadding` semantics).
pub fn aes_cbc_decrypt_in_place(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8]) -> WupResult<()> {
    if !data.len().is_multiple_of(16) {
        return Err(WupError::AesError(format!(
            "AES-CBC data length {} is not a multiple of 16",
            data.len()
        )));
    }
    Aes128CbcDec::new_from_slices(key, iv)
        .map_err(|e| WupError::AesError(e.to_string()))?
        .decrypt_padded_mut::<NoPadding>(data)
        .map_err(|e| WupError::AesError(e.to_string()))?;
    Ok(())
}

/// Decrypt an encrypted title key recovered from a Wii U ticket.
///
/// The decryption uses the retail Wii U common key and an IV built
/// from the ticket's title id: the first 8 bytes of the IV are the
/// big-endian title id, the remaining 8 bytes are zero. This matches
/// `ETicketParser::GetTitleKey` in Cemu's `ncrypto.cpp`.
pub fn decrypt_title_key(encrypted_title_key: &[u8; 16], title_id: u64) -> WupResult<[u8; 16]> {
    let mut iv = [0u8; 16];
    iv[0..8].copy_from_slice(&title_id.to_be_bytes());

    let mut buf = *encrypted_title_key;
    aes_cbc_decrypt_in_place(&WII_U_COMMON_KEY, &iv, &mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncryptMut;
    use cbc::Encryptor;

    type Aes128CbcEnc = Encryptor<Aes128>;

    fn aes_cbc_encrypt_in_place(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8]) {
        Aes128CbcEnc::new_from_slices(key, iv)
            .unwrap()
            .encrypt_padded_mut::<NoPadding>(data, data.len())
            .unwrap();
    }

    #[test]
    fn decrypt_round_trip_block() {
        // Encrypt a known plaintext block, then decrypt with the
        // same key and IV and check we get the original bytes.
        let key = [0x11u8; 16];
        let iv = [0x22u8; 16];
        let plaintext: [u8; 32] = [
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
            24, 25, 26, 27, 28, 29, 30, 31,
        ];
        let mut buf = plaintext;
        aes_cbc_encrypt_in_place(&key, &iv, &mut buf);
        assert_ne!(buf, plaintext);
        aes_cbc_decrypt_in_place(&key, &iv, &mut buf).unwrap();
        assert_eq!(buf, plaintext);
    }

    #[test]
    fn decrypt_rejects_non_block_aligned_length() {
        let key = [0u8; 16];
        let iv = [0u8; 16];
        let mut buf = [0u8; 17];
        let err = aes_cbc_decrypt_in_place(&key, &iv, &mut buf);
        assert!(matches!(err, Err(WupError::AesError(_))));
    }

    #[test]
    fn title_key_decrypts_matching_encryption() {
        // Encrypt a known title key the same way Cemu's ticket
        // builder would (common key + title_id IV), then run our
        // decrypt and assert we recover the original.
        let title_id: u64 = 0x0005_000E_1234_5678;
        let plain_title_key = [
            0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
            0x99, 0x00,
        ];

        let mut iv = [0u8; 16];
        iv[0..8].copy_from_slice(&title_id.to_be_bytes());
        let mut encrypted = plain_title_key;
        aes_cbc_encrypt_in_place(&WII_U_COMMON_KEY, &iv, &mut encrypted);

        let recovered = decrypt_title_key(&encrypted, title_id).unwrap();
        assert_eq!(recovered, plain_title_key);
    }

    #[test]
    fn title_key_iv_differs_per_title_id() {
        // Same encrypted bytes should decrypt to different plain
        // title keys for different title ids because the IV changes.
        let encrypted = [0x42u8; 16];
        let a = decrypt_title_key(&encrypted, 0x0005_000E_0000_0001).unwrap();
        let b = decrypt_title_key(&encrypted, 0x0005_000E_0000_0002).unwrap();
        assert_ne!(a, b);
    }
}
