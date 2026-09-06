//! In-place AES-128-CBC with no padding, the shape every console crypto
//! path in this crate needs: a whole number of blocks, chained from an
//! explicit IV.

use aes::Aes128;
use aes::cipher::block_padding::{Error as UnpadError, NoPadding};
use aes::cipher::inout::PadError;
use aes::cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyIvInit};

/// Decrypt `buf` in place. Errors only when `buf` is not a whole number
/// of 16-byte blocks.
pub fn aes128_cbc_decrypt_nopad(
    key: &[u8; 16],
    iv: &[u8; 16],
    buf: &mut [u8],
) -> Result<(), UnpadError> {
    cbc::Decryptor::<Aes128>::new(key.into(), iv.into()).decrypt_padded::<NoPadding>(buf)?;
    Ok(())
}

/// Encrypt `buf` in place. Errors only when `buf` is not a whole number
/// of 16-byte blocks.
pub fn aes128_cbc_encrypt_nopad(
    key: &[u8; 16],
    iv: &[u8; 16],
    buf: &mut [u8],
) -> Result<(), PadError> {
    let len = buf.len();
    cbc::Encryptor::<Aes128>::new(key.into(), iv.into()).encrypt_padded::<NoPadding>(buf, len)?;
    Ok(())
}
