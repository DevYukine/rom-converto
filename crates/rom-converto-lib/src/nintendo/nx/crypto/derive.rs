//! Plain section-key recovery from the NCA's encrypted_key_area.
//!
//! Each NCA stores four 16-byte AES keys at header offset 0x300, each
//! AES-128-ECB-encrypted under a `key_area_key_<kind>_<master_index>`
//! that the user provides via `prod.keys`. ECB unwrap (single block,
//! no padding) yields the per-section key the body uses for AES-CTR
//! or AES-XTS.

use aes::Aes128;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, KeyInit};

use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::keys::{KeyAreaKind, KeySet};

pub const KEY_AREA_OFFSET: usize = 0x300;
pub const KEY_AREA_KEY_COUNT: usize = 4;
pub const KEY_AREA_KEY_SIZE: usize = 16;
pub const KEY_AREA_TOTAL: usize = KEY_AREA_KEY_COUNT * KEY_AREA_KEY_SIZE;

/// Decrypt the 4 AES keys stored at offset 0x300 of the (already
/// header-XTS-decrypted) NCA. Returns them in slot order; slot 2 is the
/// section key used by every section regardless of section index.
pub fn decrypt_key_area(
    encrypted_key_area: &[u8; KEY_AREA_TOTAL],
    kind: KeyAreaKind,
    master_index: u8,
    keys: &KeySet,
) -> NxResult<[[u8; KEY_AREA_KEY_SIZE]; KEY_AREA_KEY_COUNT]> {
    let kak = keys.key_area_key(kind, master_index)?;
    let cipher = Aes128::new_from_slice(kak)
        .map_err(|e| NxError::AesError(format!("key_area_key Aes128 init: {e}")))?;

    let mut out = [[0u8; KEY_AREA_KEY_SIZE]; KEY_AREA_KEY_COUNT];
    for (i, slot) in out.iter_mut().enumerate() {
        let start = i * KEY_AREA_KEY_SIZE;
        let mut block =
            GenericArray::clone_from_slice(&encrypted_key_area[start..start + KEY_AREA_KEY_SIZE]);
        cipher.decrypt_block(&mut block);
        slot.copy_from_slice(block.as_slice());
    }
    Ok(out)
}

/// NCA section bodies all use slot 2 of the decrypted key area.
pub fn body_key(area: &[[u8; KEY_AREA_KEY_SIZE]; KEY_AREA_KEY_COUNT]) -> [u8; KEY_AREA_KEY_SIZE] {
    area[2]
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncrypt;

    #[test]
    fn round_trip_single_block() {
        let kak = [0x55u8; 16];
        let plain_keys: [[u8; 16]; 4] = [[0x11; 16], [0x22; 16], [0x33; 16], [0x44; 16]];
        let cipher = Aes128::new_from_slice(&kak).unwrap();
        let mut encrypted = [0u8; 64];
        for (i, k) in plain_keys.iter().enumerate() {
            let mut block = GenericArray::clone_from_slice(k);
            cipher.encrypt_block(&mut block);
            encrypted[i * 16..(i + 1) * 16].copy_from_slice(block.as_slice());
        }

        let mut keyset = KeySet::default();
        keyset
            .key_area_keys
            .insert((KeyAreaKind::Application, 0), kak);

        let decrypted = decrypt_key_area(&encrypted, KeyAreaKind::Application, 0, &keyset).unwrap();
        for (i, k) in plain_keys.iter().enumerate() {
            assert_eq!(&decrypted[i], k);
        }
        assert_eq!(body_key(&decrypted), [0x33; 16]);
    }

    #[test]
    fn missing_kak_surfaces_error() {
        let keyset = KeySet::default();
        let area = [0u8; 64];
        let err = decrypt_key_area(&area, KeyAreaKind::Application, 0, &keyset).unwrap_err();
        assert!(matches!(err, NxError::MissingKey { .. }));
    }
}
