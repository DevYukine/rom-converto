use byteorder::{BigEndian, ByteOrder};
use std::io::{Read, Seek, SeekFrom};

use crate::util::aes::aes128_cbc_decrypt_nopad;

use crate::nintendo::ctr::constants::{
    CTR_COMMON_KEYS_HEX, TICKET_COMMON_KEY_IDX_OFFSET, TICKET_SIG_BODY_OFFSET,
    TICKET_TITLE_ID_OFFSET, TICKET_TITLE_KEY_OFFSET,
};
use crate::nintendo::ctr::error::{NintendoCTRError, NintendoCTRResult};
use crate::nintendo::ctr::models::ticket::Ticket;

pub fn gen_iv(cidx: u16) -> [u8; 16] {
    let mut iv: [u8; 16] = [0; 16];
    BigEndian::write_u16(&mut iv[0..2], cidx);

    iv
}

pub fn cbc_decrypt(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8]) -> anyhow::Result<()> {
    aes128_cbc_decrypt_nopad(key, iv, data).map_err(|e| anyhow::anyhow!(e))
}

/// Decrypts an encrypted title key with the common key at `common_key_index`,
/// using the big-endian title id (zero-padded to 16 bytes) as the CBC IV.
pub fn decrypt_title_key(
    common_key_index: usize,
    title_id_be: &[u8; 8],
    mut key: [u8; 16],
) -> NintendoCTRResult<[u8; 16]> {
    let common_key = CTR_COMMON_KEYS_HEX
        .get(common_key_index)
        .ok_or(NintendoCTRError::TicketCommonKeyIndex(common_key_index))?;
    let mut iv = [0u8; 16];
    iv[..8].copy_from_slice(title_id_be);
    cbc_decrypt(common_key, &iv, &mut key).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(key)
}

/// Decrypts a parsed ticket's title key.
pub fn derive_title_key(ticket: &Ticket) -> NintendoCTRResult<[u8; 16]> {
    let td = &ticket.ticket_data;
    let key: [u8; 16] = td
        .title_key
        .as_slice()
        .try_into()
        .map_err(|_| NintendoCTRError::TicketTitleKeyLength(td.title_key.len()))?;
    decrypt_title_key(
        td.common_key_index as usize,
        &td.title_id.to_be_bytes(),
        key,
    )
}

/// `ticket_offset` is the absolute byte offset of the ticket section
/// in the source file.
pub fn derive_title_key_from_ticket<R: Read + Seek>(
    reader: &mut R,
    ticket_offset: u64,
) -> anyhow::Result<[u8; 16]> {
    let sig_body = ticket_offset + TICKET_SIG_BODY_OFFSET;

    reader.seek(SeekFrom::Start(sig_body + TICKET_TITLE_KEY_OFFSET))?;
    let mut enckey = [0u8; 16];
    reader.read_exact(&mut enckey)?;

    reader.seek(SeekFrom::Start(sig_body + TICKET_TITLE_ID_OFFSET))?;
    let mut title_id = [0u8; 8];
    reader.read_exact(&mut title_id)?;

    reader.seek(SeekFrom::Start(sig_body + TICKET_COMMON_KEY_IDX_OFFSET))?;
    let mut cmnkey_idx = [0u8; 1];
    reader.read_exact(&mut cmnkey_idx)?;

    Ok(decrypt_title_key(
        cmnkey_idx[0] as usize,
        &title_id,
        enckey,
    )?)
}

pub fn decrypt_first_ncch_block<R: Read + Seek>(
    reader: &mut R,
    content_offset: u64,
    content_index: u16,
    title_key: &[u8; 16],
) -> anyhow::Result<[u8; 0x200]> {
    reader.seek(SeekFrom::Start(content_offset))?;
    let mut buf = [0u8; 0x200];
    reader.read_exact(&mut buf)?;
    let iv = gen_iv(content_index);
    cbc_decrypt(title_key, &iv, &mut buf)?;
    Ok(buf)
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

    #[test]
    fn title_key_round_trip_through_ticket() {
        use crate::nintendo::ctr::constants::{
            CTR_COMMON_KEYS_HEX, TICKET_COMMON_KEY_IDX_OFFSET, TICKET_SIG_BODY_OFFSET,
            TICKET_TITLE_ID_OFFSET, TICKET_TITLE_KEY_OFFSET,
        };
        use aes::cipher::{BlockModeEncrypt, KeyIvInit};
        use std::io::Cursor;

        let plaintext_title_key = [0xA5u8; 16];
        let title_id_bytes: [u8; 8] = [0x00, 0x04, 0x00, 0x00, 0x00, 0x12, 0x56, 0x00];
        let common_key_idx: u8 = 0;

        let mut iv = [0u8; 16];
        iv[..8].copy_from_slice(&title_id_bytes);
        let mut enc_title_key = plaintext_title_key;
        type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
        let mut enc =
            Aes128CbcEnc::new_from_slices(&CTR_COMMON_KEYS_HEX[common_key_idx as usize], &iv)
                .unwrap();
        for block in enc_title_key.chunks_mut(16) {
            let array_block = <&mut aes::cipher::array::Array<_, _>>::try_from(block)
                .expect("chunk is exactly one AES block");
            enc.encrypt_block(array_block);
        }

        let ticket_offset = 0x100u64;
        let total = (TICKET_SIG_BODY_OFFSET + TICKET_COMMON_KEY_IDX_OFFSET + 1) as usize
            + ticket_offset as usize;
        let mut buf = vec![0u8; total + 0x100];

        let sig_body = ticket_offset + TICKET_SIG_BODY_OFFSET;
        let key_at = (sig_body + TICKET_TITLE_KEY_OFFSET) as usize;
        buf[key_at..key_at + 16].copy_from_slice(&enc_title_key);
        let tid_at = (sig_body + TICKET_TITLE_ID_OFFSET) as usize;
        buf[tid_at..tid_at + 8].copy_from_slice(&title_id_bytes);
        let cmnidx_at = (sig_body + TICKET_COMMON_KEY_IDX_OFFSET) as usize;
        buf[cmnidx_at] = common_key_idx;

        let mut cursor = Cursor::new(buf);
        let recovered = derive_title_key_from_ticket(&mut cursor, ticket_offset).unwrap();
        assert_eq!(recovered, plaintext_title_key);
    }
}
