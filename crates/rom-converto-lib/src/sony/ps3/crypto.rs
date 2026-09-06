//! Per-sector AES-128-CBC crypto for PS3 discs.
//!
//! Every encrypted sector is a self-contained CBC stream (no padding,
//! chain reset each sector). The IV is 12 zero bytes followed by the
//! absolute LBA as a big-endian `u32`.

use crate::util::aes::aes128_cbc_decrypt_nopad;
#[cfg(test)]
use crate::util::aes::aes128_cbc_encrypt_nopad;

use crate::sony::ps3::region::SECTOR_SIZE;

fn sector_iv(lba: u32) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[12..16].copy_from_slice(&lba.to_be_bytes());
    iv
}

/// Decrypt one 2048-byte sector in place.
pub fn decrypt_sector(key: &[u8; 16], lba: u32, buf: &mut [u8; SECTOR_SIZE]) {
    aes128_cbc_decrypt_nopad(key, &sector_iv(lba), buf)
        .expect("sector length is a multiple of the AES block size");
}

/// Encrypt one 2048-byte sector in place (round-trip inverse of
/// [`decrypt_sector`]).
#[cfg(test)]
pub fn encrypt_sector(key: &[u8; 16], lba: u32, buf: &mut [u8]) {
    aes128_cbc_encrypt_nopad(key, &sector_iv(lba), buf)
        .expect("sector length is a multiple of the AES block size");
}
