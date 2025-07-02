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

pub fn cbc_decrypt(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8]) -> anyhow::Result<()> {
    Aes128Cbc::new_from_slices(key, iv)?
        .decrypt_padded_mut::<NoPadding>(data)
        .map_err(|e| anyhow::anyhow!(e))?;

    Ok(())
}
