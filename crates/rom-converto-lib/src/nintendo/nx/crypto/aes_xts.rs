//! NCA header crypto: 0x200-byte sectors, sector address 0 at the
//! start of the NCA. Switch uses a *big-endian* sector tweak, opposite
//! the IEEE 1619 standard that `xts_mode::get_tweak_default` follows;
//! confirmed against `nsz/aes128.py::AESXTS.get_tweak`. Without the
//! BE tweak the first sector decrypts correctly (sector 0 = all zeros
//! either way) but every later sector lands on garbage and the NCA3
//! magic at offset 0x200 fails to validate.

use aes::Aes128;
use aes::cipher::KeyInit;
use aes::cipher::array::Array;
use xts_mode::Xts128;

use crate::nintendo::nx::constants::{NCA_HEADER_SIZE, NCA_XTS_SECTOR};
use crate::nintendo::nx::error::{NxError, NxResult};

fn nca_tweak(sector_index: u128) -> Array<u8, aes::cipher::consts::U16> {
    Array(sector_index.to_be_bytes())
}

fn make_cipher(header_key: &[u8; 32]) -> NxResult<Xts128<Aes128>> {
    let c1 = Aes128::new_from_slice(&header_key[..16])
        .map_err(|e| NxError::AesError(format!("Aes128 lower half: {e}")))?;
    let c2 = Aes128::new_from_slice(&header_key[16..])
        .map_err(|e| NxError::AesError(format!("Aes128 upper half: {e}")))?;
    Ok(Xts128::<Aes128>::new(c1, c2))
}

pub fn decrypt_nca_header(buf: &mut [u8; NCA_HEADER_SIZE], header_key: &[u8; 32]) -> NxResult<()> {
    let xts = make_cipher(header_key)?;
    xts.decrypt_area(buf, NCA_XTS_SECTOR, 0, nca_tweak);
    Ok(())
}

pub fn encrypt_nca_header(buf: &mut [u8; NCA_HEADER_SIZE], header_key: &[u8; 32]) -> NxResult<()> {
    let xts = make_cipher(header_key)?;
    xts.encrypt_area(buf, NCA_XTS_SECTOR, 0, nca_tweak);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::nx::constants::NCA3_MAGIC;

    fn synthetic_header_key() -> [u8; 32] {
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = i as u8;
        }
        k
    }

    #[test]
    fn xts_round_trip_preserves_bytes() {
        let key = synthetic_header_key();
        let mut buf = [0u8; NCA_HEADER_SIZE];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (i & 0xFF) as u8;
        }
        let plaintext = buf;
        encrypt_nca_header(&mut buf, &key).unwrap();
        assert_ne!(buf, plaintext, "encryption changed nothing");
        decrypt_nca_header(&mut buf, &key).unwrap();
        assert_eq!(buf, plaintext, "round trip lost bytes");
    }

    #[test]
    fn magic_at_0x200_round_trips() {
        let key = synthetic_header_key();
        let mut buf = [0u8; NCA_HEADER_SIZE];
        buf[0x200..0x204].copy_from_slice(&NCA3_MAGIC);
        let original = buf;
        encrypt_nca_header(&mut buf, &key).unwrap();
        decrypt_nca_header(&mut buf, &key).unwrap();
        assert_eq!(&buf[0x200..0x204], &NCA3_MAGIC);
        assert_eq!(buf, original);
    }
}
