//! Random-access decryption of NUS content files.
//!
//! Lets callers decrypt only the byte range that backs a virtual file
//! instead of pulling the whole multi-GB `.app` into RAM. Raw mode
//! (cluster 0 and other AES-CBC-only clusters) seeds the IV from the
//! preceding ciphertext block; hashed mode (cluster index > 0 with
//! `FstClusterHashMode::HashInterleaved`) needs only the 64 KiB
//! blocks covering the requested virtual range because each block
//! carries its own IV inside the hash prefix.

use std::io::{Read, Seek, SeekFrom};

use crate::nintendo::wup::crypto::aes_cbc_decrypt_in_place;
use crate::nintendo::wup::error::WupResult;
use crate::nintendo::wup::nus::content_stream::{
    HASHED_BLOCK_DATA_SIZE, HASHED_BLOCK_H0_COUNT, HASHED_BLOCK_H0_SIZE, HASHED_BLOCK_HASH_SIZE,
    HASHED_BLOCK_SIZE, raw_content_iv,
};
use crate::nintendo::wup::nus::ticket_parser::TitleKey;

pub fn decrypt_raw_range<R: Read + Seek>(
    reader: &mut R,
    title_key: &TitleKey,
    cluster_index: u16,
    byte_offset: u64,
    byte_len: usize,
) -> WupResult<Vec<u8>> {
    if byte_len == 0 {
        return Ok(Vec::new());
    }

    let aligned_offset = byte_offset & !15;
    let head_skip = (byte_offset - aligned_offset) as usize;
    let aligned_end = (byte_offset + byte_len as u64 + 15) & !15;
    let aligned_len = (aligned_end - aligned_offset) as usize;

    let iv = if aligned_offset == 0 {
        reader.seek(SeekFrom::Start(0))?;
        raw_content_iv(cluster_index)
    } else {
        reader.seek(SeekFrom::Start(aligned_offset - 16))?;
        let mut iv = [0u8; 16];
        reader.read_exact(&mut iv)?;
        iv
    };

    let mut buf = vec![0u8; aligned_len];
    reader.read_exact(&mut buf)?;
    aes_cbc_decrypt_in_place(&title_key.0, &iv, &mut buf)?;
    Ok(buf[head_skip..head_skip + byte_len].to_vec())
}

pub fn decrypt_hashed_range<R: Read + Seek>(
    reader: &mut R,
    title_key: &TitleKey,
    virtual_offset: u64,
    virtual_len: usize,
) -> WupResult<Vec<u8>> {
    if virtual_len == 0 {
        return Ok(Vec::new());
    }

    let data_size = HASHED_BLOCK_DATA_SIZE as u64;
    let first_block = virtual_offset / data_size;
    let last_block = (virtual_offset + virtual_len as u64 - 1) / data_size;

    let mut out = Vec::with_capacity(virtual_len);
    let mut current_virt = virtual_offset;
    let mut remaining = virtual_len;

    for block_idx in first_block..=last_block {
        let phys_offset = block_idx * HASHED_BLOCK_SIZE as u64;
        reader.seek(SeekFrom::Start(phys_offset))?;
        let mut block = [0u8; HASHED_BLOCK_SIZE];
        reader.read_exact(&mut block)?;

        let mut hash_part = [0u8; HASHED_BLOCK_HASH_SIZE];
        hash_part.copy_from_slice(&block[..HASHED_BLOCK_HASH_SIZE]);
        let iv_zero = [0u8; 16];
        aes_cbc_decrypt_in_place(&title_key.0, &iv_zero, &mut hash_part)?;

        let iv_offset = (block_idx as usize % HASHED_BLOCK_H0_COUNT) * HASHED_BLOCK_H0_SIZE;
        let data_iv: [u8; 16] = hash_part[iv_offset..iv_offset + 16]
            .try_into()
            .expect("16 bytes");

        let mut data_part = vec![0u8; HASHED_BLOCK_DATA_SIZE];
        data_part.copy_from_slice(&block[HASHED_BLOCK_HASH_SIZE..]);
        aes_cbc_decrypt_in_place(&title_key.0, &data_iv, &mut data_part)?;

        let block_virt_start = block_idx * data_size;
        let in_block_start = (current_virt - block_virt_start) as usize;
        let take = (HASHED_BLOCK_DATA_SIZE - in_block_start).min(remaining);
        out.extend_from_slice(&data_part[in_block_start..in_block_start + take]);
        current_virt += take as u64;
        remaining -= take;
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::wup::nus::content_stream::{
        decrypt_hashed_content, decrypt_raw_content,
    };
    use aes::Aes128;
    use block_padding::NoPadding;
    use cbc::Encryptor;
    use cbc::cipher::{BlockEncryptMut, KeyIvInit};
    use std::io::Cursor;

    type Aes128CbcEnc = Encryptor<Aes128>;

    fn encrypt_in_place(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8]) {
        Aes128CbcEnc::new_from_slices(key, iv)
            .unwrap()
            .encrypt_padded_mut::<NoPadding>(data, data.len())
            .unwrap();
    }

    fn make_title_key() -> TitleKey {
        TitleKey([0xAAu8; 16])
    }

    fn encrypt_raw(plaintext: &[u8], key: &TitleKey, cluster_index: u16) -> Vec<u8> {
        let mut buf = plaintext.to_vec();
        let iv = raw_content_iv(cluster_index);
        encrypt_in_place(&key.0, &iv, &mut buf);
        buf
    }

    #[test]
    fn raw_range_round_trip_from_offset_zero() {
        let key = make_title_key();
        let plain = (0..0x400u32).map(|i| (i & 0xFF) as u8).collect::<Vec<_>>();
        let encrypted = encrypt_raw(&plain, &key, 0);
        let mut cur = Cursor::new(encrypted);
        let got = decrypt_raw_range(&mut cur, &key, 0, 0, 0x40).unwrap();
        assert_eq!(got, &plain[0..0x40]);
    }

    #[test]
    fn raw_range_round_trip_with_aligned_offset() {
        let key = make_title_key();
        let plain = (0..0x1000u32).map(|i| (i & 0xFF) as u8).collect::<Vec<_>>();
        let encrypted = encrypt_raw(&plain, &key, 0);
        let mut cur = Cursor::new(encrypted);
        let got = decrypt_raw_range(&mut cur, &key, 0, 0x40, 0x100).unwrap();
        assert_eq!(got, &plain[0x40..0x140]);
    }

    #[test]
    fn raw_range_round_trip_with_unaligned_offset_and_length() {
        let key = make_title_key();
        let plain = (0..0x2000u32).map(|i| (i & 0xFF) as u8).collect::<Vec<_>>();
        let encrypted = encrypt_raw(&plain, &key, 0);
        let mut cur = Cursor::new(encrypted);
        let got = decrypt_raw_range(&mut cur, &key, 0, 0x57, 0x123).unwrap();
        assert_eq!(got, &plain[0x57..0x57 + 0x123]);
    }

    #[test]
    fn raw_range_matches_whole_buffer_decrypt() {
        let key = make_title_key();
        let plain = (0..0x800u32).map(|i| (i ^ 0x5A) as u8).collect::<Vec<_>>();
        let encrypted = encrypt_raw(&plain, &key, 5);
        let whole = decrypt_raw_content(encrypted.clone(), &key, 5).unwrap();
        let mut cur = Cursor::new(encrypted);
        let part = decrypt_raw_range(&mut cur, &key, 5, 0x200, 0x400).unwrap();
        assert_eq!(part, &whole[0x200..0x600]);
    }

    fn build_hashed_blocks(num_blocks: usize, key: &TitleKey, plaintext: &[u8]) -> Vec<u8> {
        assert!(plaintext.len() <= num_blocks * HASHED_BLOCK_DATA_SIZE);
        let mut encrypted = vec![0u8; num_blocks * HASHED_BLOCK_SIZE];
        for block_idx in 0..num_blocks {
            let block_start = block_idx * HASHED_BLOCK_SIZE;
            let data_start_virt = block_idx * HASHED_BLOCK_DATA_SIZE;
            let data_end_virt =
                (data_start_virt + HASHED_BLOCK_DATA_SIZE).min(plaintext.len());

            let mut hash_part = [0u8; HASHED_BLOCK_HASH_SIZE];
            for h in 0..HASHED_BLOCK_H0_COUNT {
                let slot = h * HASHED_BLOCK_H0_SIZE;
                hash_part[slot] = (block_idx as u8).wrapping_add(h as u8);
                hash_part[slot + 1] = 0x42;
            }
            let iv_zero = [0u8; 16];
            let iv_offset =
                (block_idx % HASHED_BLOCK_H0_COUNT) * HASHED_BLOCK_H0_SIZE;
            let data_iv: [u8; 16] = hash_part[iv_offset..iv_offset + 16]
                .try_into()
                .unwrap();
            let mut hash_enc = hash_part;
            encrypt_in_place(&key.0, &iv_zero, &mut hash_enc);

            let mut data_buf = vec![0u8; HASHED_BLOCK_DATA_SIZE];
            if data_start_virt < plaintext.len() {
                let copy_len = data_end_virt - data_start_virt;
                data_buf[..copy_len].copy_from_slice(&plaintext[data_start_virt..data_end_virt]);
            }
            encrypt_in_place(&key.0, &data_iv, &mut data_buf);

            encrypted[block_start..block_start + HASHED_BLOCK_HASH_SIZE]
                .copy_from_slice(&hash_enc);
            encrypted[block_start + HASHED_BLOCK_HASH_SIZE..block_start + HASHED_BLOCK_SIZE]
                .copy_from_slice(&data_buf);
        }
        encrypted
    }

    #[test]
    fn hashed_range_inside_one_block_matches_whole_decrypt() {
        let key = make_title_key();
        let plain: Vec<u8> = (0..HASHED_BLOCK_DATA_SIZE).map(|i| (i & 0xFF) as u8).collect();
        let encrypted = build_hashed_blocks(1, &key, &plain);
        let whole = decrypt_hashed_content(&encrypted, &key).unwrap();
        let mut cur = Cursor::new(encrypted);
        let got = decrypt_hashed_range(&mut cur, &key, 0x100, 0x80).unwrap();
        assert_eq!(got, &whole[0x100..0x180]);
    }

    #[test]
    fn hashed_range_spans_two_blocks() {
        let key = make_title_key();
        let plain: Vec<u8> = (0..2 * HASHED_BLOCK_DATA_SIZE).map(|i| (i & 0xFF) as u8).collect();
        let encrypted = build_hashed_blocks(2, &key, &plain);
        let whole = decrypt_hashed_content(&encrypted, &key).unwrap();
        let mut cur = Cursor::new(encrypted);
        let start = HASHED_BLOCK_DATA_SIZE - 0x20;
        let len = 0x80;
        let got = decrypt_hashed_range(&mut cur, &key, start as u64, len).unwrap();
        assert_eq!(got, &whole[start..start + len]);
    }

    #[test]
    fn hashed_range_starts_mid_block() {
        let key = make_title_key();
        let plain: Vec<u8> = (0..3 * HASHED_BLOCK_DATA_SIZE).map(|i| (i & 0xFF) as u8).collect();
        let encrypted = build_hashed_blocks(3, &key, &plain);
        let whole = decrypt_hashed_content(&encrypted, &key).unwrap();
        let mut cur = Cursor::new(encrypted);
        let start = 2 * HASHED_BLOCK_DATA_SIZE + 0x10;
        let len = 0x40;
        let got = decrypt_hashed_range(&mut cur, &key, start as u64, len).unwrap();
        assert_eq!(got, &whole[start..start + len]);
    }
}
