//! The `amctrl` BBMac and BBCipher paths an `NPUMDIMG` image is built on.

use aes::Aes128;
use aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};

use crate::sony::psp::kirk::{KIRK7_KEY_0X38, KIRK7_KEY_0X39, KIRK7_KEY_0X63, kirk7_decrypt_block};

const AMCTL_HASHKEY_3: [u8; 16] = [
    0xe3, 0x50, 0xed, 0x1d, 0x91, 0x0a, 0x1f, 0xd0, 0x29, 0xbb, 0x1c, 0x3e, 0xf3, 0x40, 0x77, 0xfb,
];
const AMCTL_HASHKEY_4: [u8; 16] = [
    0x13, 0x5f, 0xa4, 0x7c, 0xab, 0x39, 0x5b, 0xa4, 0x76, 0xb8, 0xcc, 0xa9, 0x8f, 0x3a, 0x04, 0x45,
];
const AMCTL_HASHKEY_5: [u8; 16] = [
    0x67, 0x8d, 0x7f, 0xa3, 0x2a, 0x9c, 0xa0, 0xd1, 0x50, 0x8a, 0xd8, 0x38, 0x5e, 0x4b, 0x01, 0x7e,
];

/// Runs the BBMac over `data`: an AES-128-CMAC under KIRK key seed `0x38`.
/// The result is key material for [`BbCipher::new`], not a checksum.
pub fn bb_mac(data: &[u8]) -> [u8; 16] {
    cmac(&KIRK7_KEY_0X38, data)
}

/// The BBCipher keystream an `NPUMDIMG` body is decrypted with.
pub struct BbCipher {
    iv: [u8; 16],
}

impl BbCipher {
    /// Derives the cipher state from the header BBMac, the encrypted
    /// version key block and the key modifier block.
    pub fn new(mac: &[u8; 16], version_key: &[u8; 16], key_modifier: &[u8; 16]) -> Self {
        let mut tmp = *version_key;
        kirk7_decrypt_block(&KIRK7_KEY_0X63, &mut tmp);
        kirk7_decrypt_block(&KIRK7_KEY_0X38, &mut tmp);

        let mut iv = [0u8; 16];
        for i in 0..16 {
            iv[i] = mac[i] ^ tmp[i] ^ key_modifier[i] ^ AMCTL_HASHKEY_3[i] ^ AMCTL_HASHKEY_5[i];
        }
        kirk7_decrypt_block(&KIRK7_KEY_0X39, &mut iv);
        for (b, k) in iv.iter_mut().zip(AMCTL_HASHKEY_4) {
            *b ^= k;
        }
        Self { iv }
    }

    /// XORs the cipher keystream over `buffer`, which starts at 16-byte
    /// block `index` of the encrypted region. The keystream depends only on
    /// the position, so this both encrypts and decrypts.
    pub fn apply(&self, index: u32, buffer: &mut [u8]) {
        let aes = Aes128::new(&KIRK7_KEY_0X63.into());

        let mut prev = [0u8; 16];
        if index != 0 {
            prev[..12].copy_from_slice(&self.iv[..12]);
            prev[12..].copy_from_slice(&index.to_le_bytes());
        }

        let mut block = self.iv;
        let mut counter = index;
        for chunk in buffer.chunks_mut(16) {
            counter = counter.wrapping_add(1);
            block[12..].copy_from_slice(&counter.to_le_bytes());

            let mut keystream = block;
            aes.decrypt_block((&mut keystream).into());
            for (i, b) in chunk.iter_mut().enumerate() {
                *b ^= prev[i] ^ keystream[i];
            }
            prev = block;
        }
    }
}

/// AES-128-CMAC per RFC 4493.
fn cmac(key: &[u8; 16], data: &[u8]) -> [u8; 16] {
    let aes = Aes128::new(key.into());

    let mut subkey = [0u8; 16];
    aes.encrypt_block((&mut subkey).into());
    gf_double(&mut subkey);

    let (head, tail) = if !data.is_empty() && data.len().is_multiple_of(16) {
        data.split_at(data.len() - 16)
    } else {
        gf_double(&mut subkey);
        data.split_at(data.len() - data.len() % 16)
    };

    let mut last = [0u8; 16];
    last[..tail.len()].copy_from_slice(tail);
    if tail.len() < 16 {
        last[tail.len()] = 0x80;
    }
    for (b, k) in last.iter_mut().zip(subkey) {
        *b ^= k;
    }

    let mut mac = [0u8; 16];
    for block in head
        .as_chunks::<16>()
        .0
        .iter()
        .chain(std::iter::once(&last))
    {
        for (m, b) in mac.iter_mut().zip(block) {
            *m ^= b;
        }
        aes.encrypt_block((&mut mac).into());
    }
    mac
}

fn gf_double(block: &mut [u8; 16]) {
    let mut carry = 0u8;
    for byte in block.iter_mut().rev() {
        let high = *byte >> 7;
        *byte = (*byte << 1) | carry;
        carry = high;
    }
    block[15] ^= if carry != 0 { 0x87 } else { 0 };
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    use super::*;

    /// Builds a cipher straight from a chosen IV, so a fixture can encrypt
    /// before the header BBMac that would normally derive it exists.
    pub fn cipher_from_iv(iv: [u8; 16]) -> BbCipher {
        BbCipher { iv }
    }

    /// Inverts [`BbCipher::new`]: the version key block a header must carry
    /// for `mac` and `key_modifier` to derive `iv`.
    pub fn version_key_for(iv: &[u8; 16], mac: &[u8; 16], key_modifier: &[u8; 16]) -> [u8; 16] {
        let mut tmp = *iv;
        for (b, k) in tmp.iter_mut().zip(AMCTL_HASHKEY_4) {
            *b ^= k;
        }
        Aes128::new(&KIRK7_KEY_0X39.into()).encrypt_block((&mut tmp).into());
        for i in 0..16 {
            tmp[i] ^= mac[i] ^ key_modifier[i] ^ AMCTL_HASHKEY_3[i] ^ AMCTL_HASHKEY_5[i];
        }
        Aes128::new(&KIRK7_KEY_0X38.into()).encrypt_block((&mut tmp).into());
        Aes128::new(&KIRK7_KEY_0X63.into()).encrypt_block((&mut tmp).into());
        tmp
    }
}

#[cfg(test)]
mod tests {
    use super::test_fixtures::{cipher_from_iv, version_key_for};
    use super::*;

    /// RFC 4493 section 4.
    #[test]
    fn cmac_matches_rfc_4493_vectors() {
        let key = hex_literal::hex!("2b7e151628aed2a6abf7158809cf4f3c");
        let msg = hex_literal::hex!(
            "6bc1bee22e409f96e93d7e117393172a"
            "ae2d8a571e03ac9c9eb76fac45af8e51"
            "30c81c46a35ce411e5fbc1191a0a52ef"
            "f69f2445df4f9b17ad2b417be66c3710"
        );
        for (len, want) in [
            (
                0usize,
                hex_literal::hex!("bb1d6929e95937287fa37d129b756746"),
            ),
            (16, hex_literal::hex!("070a16b46b4d4144f79bdd9dd04a287c")),
            (40, hex_literal::hex!("dfa66747de9ae63030ca32611497c827")),
            (64, hex_literal::hex!("51f0bebf7e3b9d92fc49741779363cfe")),
        ] {
            assert_eq!(cmac(&key, &msg[..len]), want, "len {len}");
        }
    }

    #[test]
    fn new_recovers_the_iv_the_version_key_encodes() {
        let iv = *b"an arbitrary iv.";
        let mac = *b"an arbitrary mac";
        let modifier = *b"key modifier byt";

        let version_key = version_key_for(&iv, &mac, &modifier);
        let cipher = BbCipher::new(&mac, &version_key, &modifier);
        assert_eq!(cipher.iv, iv);
    }

    #[test]
    fn apply_is_its_own_inverse_at_any_index() {
        let cipher = cipher_from_iv(*b"another test iv.");
        for index in [0u32, 1, 0x1234] {
            let plain: Vec<u8> = (0..0x60u16).map(|i| i as u8).collect();
            let mut buf = plain.clone();
            cipher.apply(index, &mut buf);
            assert_ne!(buf, plain);
            cipher.apply(index, &mut buf);
            assert_eq!(buf, plain);
        }
    }

    #[test]
    fn the_keystream_depends_on_the_index() {
        let cipher = cipher_from_iv([0x42; 16]);
        let mut at_zero = [0u8; 32];
        let mut at_one = [0u8; 32];
        cipher.apply(0, &mut at_zero);
        cipher.apply(1, &mut at_one);
        assert_ne!(at_zero, at_one);
    }

    #[test]
    fn apply_covers_a_trailing_partial_block() {
        let cipher = cipher_from_iv([7; 16]);
        let mut buf = [0u8; 20];
        cipher.apply(0, &mut buf);
        assert!(buf[16..].iter().any(|&b| b != 0));
    }
}
