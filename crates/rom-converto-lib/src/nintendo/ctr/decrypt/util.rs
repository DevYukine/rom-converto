use aes::{
    Aes128,
    cipher::{BlockDecryptMut, KeyIvInit},
};
use block_padding::NoPadding;
use byteorder::{BigEndian, ByteOrder};

pub type Aes128Cbc = cbc::Decryptor<Aes128>;

pub fn gen_iv(cidx: u16) -> [u8; 16] {
    let mut iv: [u8; 16] = [0; 16];
    BigEndian::write_u16(&mut iv[0..2], cidx);

    iv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gen_iv_zero() {
        assert_eq!(gen_iv(0), [0u8; 16]);
    }

    #[test]
    fn gen_iv_one() {
        let iv = gen_iv(1);
        assert_eq!(iv[0], 0x00);
        assert_eq!(iv[1], 0x01);
        assert!(iv[2..].iter().all(|&b| b == 0));
    }

    #[test]
    fn gen_iv_high_byte() {
        let iv = gen_iv(0xFF00);
        assert_eq!(iv[0], 0xFF);
        assert_eq!(iv[1], 0x00);
        assert!(iv[2..].iter().all(|&b| b == 0));
    }

    #[test]
    fn gen_iv_max() {
        let iv = gen_iv(0xFFFF);
        assert_eq!(iv[0], 0xFF);
        assert_eq!(iv[1], 0xFF);
        assert!(iv[2..].iter().all(|&b| b == 0));
    }
}

pub fn cbc_decrypt(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8]) -> anyhow::Result<()> {
    Aes128Cbc::new_from_slices(key, iv)?
        .decrypt_padded_mut::<NoPadding>(data)
        .map_err(|e| anyhow::anyhow!(e))?;

    Ok(())
}
