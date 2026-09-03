//! The KIRK command 7 key seeds and single-block primitive the `NPUMDIMG`
//! path needs.

use aes::Aes128;
use aes::cipher::{BlockCipherDecrypt, KeyInit};

/// KIRK command 7 key seed `0x38`.
pub const KIRK7_KEY_0X38: [u8; 16] = [
    0x12, 0x46, 0x8d, 0x7e, 0x1c, 0x42, 0x20, 0x9b, 0xba, 0x54, 0x26, 0x83, 0x5e, 0xb0, 0x33, 0x03,
];
/// KIRK command 7 key seed `0x39`.
pub const KIRK7_KEY_0X39: [u8; 16] = [
    0xc4, 0x3b, 0xb6, 0xd6, 0x53, 0xee, 0x67, 0x49, 0x3e, 0xa9, 0x5f, 0xbc, 0x0c, 0xed, 0x6f, 0x8a,
];
/// KIRK command 7 key seed `0x63`.
pub const KIRK7_KEY_0X63: [u8; 16] = [
    0x9c, 0x9b, 0x13, 0x72, 0xf8, 0xc6, 0x40, 0xcf, 0x1c, 0x62, 0xf5, 0xd5, 0x92, 0xdd, 0xb5, 0x82,
];

/// Runs KIRK command 7 over a single block: AES-128-CBC decryption under
/// `keyseed` with a zero IV, which for one block is a plain ECB decrypt.
pub fn kirk7_decrypt_block(keyseed: &[u8; 16], block: &mut [u8; 16]) {
    Aes128::new(keyseed.into()).decrypt_block(block.into());
}

#[cfg(test)]
mod tests {
    use aes::cipher::BlockCipherEncrypt;

    use super::*;

    #[test]
    fn undoes_an_aes_encryption_under_each_keyseed() {
        for seed in [&KIRK7_KEY_0X38, &KIRK7_KEY_0X39, &KIRK7_KEY_0X63] {
            let plain = *b"NPUMDIMG fixture";
            let mut block = plain;
            Aes128::new(seed.into()).encrypt_block((&mut block).into());
            assert_ne!(block, plain);
            kirk7_decrypt_block(seed, &mut block);
            assert_eq!(block, plain);
        }
    }

    #[test]
    fn decrypts_a_fixed_block_under_keyseed_0x38() {
        // `openssl enc -aes-128-ecb -K 12468d7e1c42209bba5426835eb03303 -nopad`
        // over 16 zero bytes.
        let mut block = [
            0x2d, 0xb7, 0x4f, 0xd3, 0xc9, 0x55, 0x29, 0xce, 0x55, 0xef, 0x2c, 0x19, 0x24, 0xcb,
            0x33, 0x14,
        ];
        kirk7_decrypt_block(&KIRK7_KEY_0X38, &mut block);
        assert_eq!(block, [0u8; 16]);
    }
}
