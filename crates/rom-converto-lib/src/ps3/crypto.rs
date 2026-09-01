//! Per-sector AES-128-CBC crypto for PS3 discs.
//!
//! Every encrypted sector is a self-contained CBC stream (no padding,
//! chain reset each sector). The IV is 12 zero bytes followed by the
//! absolute LBA as a big-endian `u32`.

use aes::Aes128;
#[cfg(test)]
use aes::cipher::BlockModeEncrypt;
use aes::cipher::{BlockModeDecrypt, KeyIvInit};
use block_padding::NoPadding;

use crate::ps3::region::SECTOR_SIZE;

type Aes128CbcDec = cbc::Decryptor<Aes128>;
#[cfg(test)]
type Aes128CbcEnc = cbc::Encryptor<Aes128>;

fn sector_iv(lba: u32) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[12..16].copy_from_slice(&lba.to_be_bytes());
    iv
}

/// Decrypt one 2048-byte sector in place.
pub fn decrypt_sector(key: &[u8; 16], lba: u32, buf: &mut [u8; SECTOR_SIZE]) {
    let iv = sector_iv(lba);
    Aes128CbcDec::new_from_slices(key, &iv)
        .expect("16-byte key and iv")
        .decrypt_padded::<NoPadding>(buf)
        .expect("sector length is a multiple of the AES block size");
}

/// Encrypt one 2048-byte sector in place (round-trip inverse of
/// [`decrypt_sector`]).
#[cfg(test)]
pub fn encrypt_sector(key: &[u8; 16], lba: u32, buf: &mut [u8]) {
    let iv = sector_iv(lba);
    let len = buf.len();
    Aes128CbcEnc::new_from_slices(key, &iv)
        .expect("16-byte key and iv")
        .encrypt_padded::<NoPadding>(buf, len)
        .expect("sector length is a multiple of the AES block size");
}
