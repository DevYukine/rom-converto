//! Wii disc handling: partition table parsing, ticket title-key handling,
//! sector encryption, and the H0 hash helper used by the RVZ exception list
//! builder.

use crate::nintendo::rvl::common_keys::common_key;
use crate::nintendo::rvl::constants::{
    WII_HASH_SIZE, WII_MAGIC, WII_MAGIC_OFFSET, WII_PARTITION_ENTRY_SIZE, WII_PARTITION_GROUPS,
    WII_PARTITION_INFO_OFFSET, WII_SECTOR_PAYLOAD_SIZE, WII_SECTOR_SIZE,
    WII_TICKET_COMMON_KEY_INDEX_OFFSET, WII_TICKET_SIZE, WII_TICKET_TITLE_ID_OFFSET,
    WII_TICKET_TITLE_KEY_OFFSET,
};
use crate::nintendo::rvz::error::{RvzError, RvzResult};
use aes::{
    Aes128,
    cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit},
};
use block_padding::NoPadding;
use cbc::{Decryptor, Encryptor};
use sha1::{Digest, Sha1};
use std::io::{Read, Seek, SeekFrom};

type Aes128CbcDec = Decryptor<Aes128>;
type Aes128CbcEnc = Encryptor<Aes128>;

/// Returns `true` if the 128-byte disc header belongs to a Wii disc.
pub fn is_wii(dhead: &[u8; 128]) -> bool {
    let bytes: [u8; 4] = dhead[WII_MAGIC_OFFSET..WII_MAGIC_OFFSET + 4]
        .try_into()
        .unwrap();
    u32::from_be_bytes(bytes) == WII_MAGIC
}

/// A single partition entry from the Wii partition table.
#[derive(Debug, Clone, Copy)]
pub struct WiiPartitionEntry {
    /// File offset of the partition in the raw ISO.
    pub offset: u64,
    /// Partition type: 0 = game, 1 = update, 2 = channel, other = custom.
    pub partition_type: u32,
    /// Partition group index (0..4).
    pub group: u8,
}

/// Read the Wii partition table by seeking to `WII_PARTITION_INFO_OFFSET`.
/// Returns every non-empty partition across all 4 groups. Implausibly large
/// partition counts (typically a synthetic or corrupt ISO) cause the group
/// to be skipped rather than aborting with an EOF error.
pub fn read_partition_table<R: Read + Seek>(reader: &mut R) -> RvzResult<Vec<WiiPartitionEntry>> {
    /// Hard cap on partitions per group. Real Wii discs ship with at most
    /// a handful; anything above this is almost certainly garbage.
    const MAX_PARTITIONS_PER_GROUP: u32 = 16;

    reader.seek(SeekFrom::Start(WII_PARTITION_INFO_OFFSET))?;
    let mut group_hdr = [0u8; WII_PARTITION_GROUPS * 8];
    reader.read_exact(&mut group_hdr)?;

    let mut entries = Vec::new();
    for g in 0..WII_PARTITION_GROUPS {
        let off = g * 8;
        let count = u32::from_be_bytes([
            group_hdr[off],
            group_hdr[off + 1],
            group_hdr[off + 2],
            group_hdr[off + 3],
        ]);
        let table_off4 = u32::from_be_bytes([
            group_hdr[off + 4],
            group_hdr[off + 5],
            group_hdr[off + 6],
            group_hdr[off + 7],
        ]);
        if count == 0 || count > MAX_PARTITIONS_PER_GROUP {
            continue;
        }
        let table_off = (table_off4 as u64) << 2;
        if reader.seek(SeekFrom::Start(table_off)).is_err() {
            continue;
        }

        let mut table = vec![0u8; count as usize * WII_PARTITION_ENTRY_SIZE];
        if reader.read_exact(&mut table).is_err() {
            continue;
        }

        for i in 0..count as usize {
            let base = i * WII_PARTITION_ENTRY_SIZE;
            let part_off4 = u32::from_be_bytes([
                table[base],
                table[base + 1],
                table[base + 2],
                table[base + 3],
            ]);
            let part_type = u32::from_be_bytes([
                table[base + 4],
                table[base + 5],
                table[base + 6],
                table[base + 7],
            ]);
            let part_off = (part_off4 as u64) << 2;
            if part_off == 0 {
                continue;
            }
            entries.push(WiiPartitionEntry {
                offset: part_off,
                partition_type: part_type,
                group: g as u8,
            });
        }
    }
    Ok(entries)
}

/// Decrypt a Wii ticket's title key. The IV is the first 8 bytes of the
/// title id, zero-padded to 16 bytes, and the key is selected via
/// `common_key_index`.
pub fn decrypt_title_key(ticket: &[u8; WII_TICKET_SIZE]) -> RvzResult<[u8; 16]> {
    let common_key_index = ticket[WII_TICKET_COMMON_KEY_INDEX_OFFSET];
    let key = common_key(common_key_index)
        .ok_or(RvzError::UnknownCommonKeyIndex(common_key_index))?;

    let mut iv = [0u8; 16];
    iv[..8].copy_from_slice(
        &ticket[WII_TICKET_TITLE_ID_OFFSET..WII_TICKET_TITLE_ID_OFFSET + 8],
    );

    let mut encrypted = [0u8; 16];
    encrypted
        .copy_from_slice(&ticket[WII_TICKET_TITLE_KEY_OFFSET..WII_TICKET_TITLE_KEY_OFFSET + 16]);

    let cipher = Aes128CbcDec::new_from_slices(key, &iv)
        .map_err(|e| RvzError::AesError(e.to_string()))?;
    let mut buf = encrypted;
    cipher
        .decrypt_padded_mut::<NoPadding>(&mut buf)
        .map_err(|e| RvzError::AesError(format!("title key decrypt: {e}")))?;
    Ok(buf)
}

/// Re-encrypt a title key with the Wii common key. The IV construction
/// matches [`decrypt_title_key`].
pub fn encrypt_title_key(
    ticket: &[u8; WII_TICKET_SIZE],
    title_key: &[u8; 16],
) -> RvzResult<[u8; 16]> {
    let common_key_index = ticket[WII_TICKET_COMMON_KEY_INDEX_OFFSET];
    let key = common_key(common_key_index)
        .ok_or(RvzError::UnknownCommonKeyIndex(common_key_index))?;

    let mut iv = [0u8; 16];
    iv[..8].copy_from_slice(
        &ticket[WII_TICKET_TITLE_ID_OFFSET..WII_TICKET_TITLE_ID_OFFSET + 8],
    );

    let cipher = Aes128CbcEnc::new_from_slices(key, &iv)
        .map_err(|e| RvzError::AesError(e.to_string()))?;
    let mut buf = [0u8; 16];
    buf.copy_from_slice(title_key);
    let mut out = [0u8; 16];
    let ct = cipher
        .encrypt_padded_b2b_mut::<NoPadding>(&buf, &mut out)
        .map_err(|e| RvzError::AesError(format!("title key encrypt: {e}")))?;
    let mut arr = [0u8; 16];
    arr.copy_from_slice(ct);
    Ok(arr)
}

/// Decrypt one 0x8000-byte Wii partition sector in place. On entry `sector`
/// contains the encrypted block as it sits on disc; on return `sector` is
/// `[hash_region (0x400) | plaintext payload (0x7C00)]`.
pub fn decrypt_sector(sector: &mut [u8; WII_SECTOR_SIZE], title_key: &[u8; 16]) -> RvzResult<()> {
    // The payload IV comes from bytes 0x3D0..0x3E0 of the *encrypted* hash
    // region, so grab it before any decryption runs.
    let mut payload_iv = [0u8; 16];
    payload_iv.copy_from_slice(&sector[0x3D0..0x3E0]);

    // Hash region (0x400 bytes, IV=0).
    let mut hash_region = [0u8; WII_HASH_SIZE];
    hash_region.copy_from_slice(&sector[..WII_HASH_SIZE]);
    let hash_iv = [0u8; 16];
    Aes128CbcDec::new_from_slices(title_key, &hash_iv)
        .map_err(|e| RvzError::AesError(e.to_string()))?
        .decrypt_padded_mut::<NoPadding>(&mut hash_region)
        .map_err(|e| RvzError::AesError(format!("hash decrypt: {e}")))?;

    // Payload (0x7C00 bytes).
    let mut payload = [0u8; WII_SECTOR_PAYLOAD_SIZE];
    payload.copy_from_slice(&sector[WII_HASH_SIZE..]);
    Aes128CbcDec::new_from_slices(title_key, &payload_iv)
        .map_err(|e| RvzError::AesError(e.to_string()))?
        .decrypt_padded_mut::<NoPadding>(&mut payload)
        .map_err(|e| RvzError::AesError(format!("payload decrypt: {e}")))?;

    sector[..WII_HASH_SIZE].copy_from_slice(&hash_region);
    sector[WII_HASH_SIZE..].copy_from_slice(&payload);
    Ok(())
}

/// Inverse of [`decrypt_sector`]. Takes `[hash_region | plaintext payload]`
/// and writes back `[encrypted hash region | encrypted payload]`.
pub fn encrypt_sector(sector: &mut [u8; WII_SECTOR_SIZE], title_key: &[u8; 16]) -> RvzResult<()> {
    // Encrypt the hash region first so the payload IV can be read out of it.
    let hash_iv = [0u8; 16];
    let mut hash_enc = [0u8; WII_HASH_SIZE];
    {
        let hash_cipher = Aes128CbcEnc::new_from_slices(title_key, &hash_iv)
            .map_err(|e| RvzError::AesError(e.to_string()))?;
        let ct = hash_cipher
            .encrypt_padded_b2b_mut::<NoPadding>(&sector[..WII_HASH_SIZE], &mut hash_enc)
            .map_err(|e| RvzError::AesError(format!("hash encrypt: {e}")))?;
        debug_assert_eq!(ct.len(), WII_HASH_SIZE);
    }

    let mut payload_iv = [0u8; 16];
    payload_iv.copy_from_slice(&hash_enc[0x3D0..0x3E0]);

    let mut payload_enc = [0u8; WII_SECTOR_PAYLOAD_SIZE];
    {
        let payload_cipher = Aes128CbcEnc::new_from_slices(title_key, &payload_iv)
            .map_err(|e| RvzError::AesError(e.to_string()))?;
        payload_cipher
            .encrypt_padded_b2b_mut::<NoPadding>(&sector[WII_HASH_SIZE..], &mut payload_enc)
            .map_err(|e| RvzError::AesError(format!("payload encrypt: {e}")))?;
    }

    sector[..WII_HASH_SIZE].copy_from_slice(&hash_enc);
    sector[WII_HASH_SIZE..].copy_from_slice(&payload_enc);
    Ok(())
}

/// Recompute the H0 hash array for a plaintext Wii sector payload. Each
/// 0x400-byte sub-block contributes one SHA-1.
pub fn hash_h0(plaintext: &[u8; WII_SECTOR_PAYLOAD_SIZE]) -> [[u8; 20]; 31] {
    let mut h0 = [[0u8; 20]; 31];
    for (i, chunk) in plaintext.chunks_exact(0x400).enumerate().take(31) {
        let mut hasher = Sha1::new();
        hasher.update(chunk);
        h0[i] = hasher.finalize().into();
    }
    h0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_wii_matches_magic() {
        let mut dhead = [0u8; 128];
        dhead[WII_MAGIC_OFFSET..WII_MAGIC_OFFSET + 4].copy_from_slice(&WII_MAGIC.to_be_bytes());
        assert!(is_wii(&dhead));
    }

    #[test]
    fn is_wii_rejects_zeros() {
        assert!(!is_wii(&[0u8; 128]));
    }

    #[test]
    fn sector_encrypt_decrypt_roundtrips() {
        let key = [0xA5u8; 16];
        let mut plain = [0u8; WII_SECTOR_SIZE];
        for (i, b) in plain.iter_mut().enumerate() {
            *b = i as u8;
        }
        let original = plain;

        let mut buf = plain;
        encrypt_sector(&mut buf, &key).unwrap();
        assert_ne!(buf, original, "ciphertext must differ from plaintext");

        decrypt_sector(&mut buf, &key).unwrap();
        assert_eq!(buf, original);
    }

    #[test]
    fn sector_decrypt_encrypt_roundtrips_on_all_zero_ciphertext() {
        // A Wii partition's "padding" clusters on real discs frequently
        // contain all-zero ciphertext in sectors past the declared
        // data_size. decrypt(0) → junk; then our encoder stores the
        // junk and our decoder is supposed to re-encrypt it back to the
        // original all-zero ciphertext. This test exercises exactly
        // that path: `encrypt_sector(decrypt_sector(C)) == C`.
        let key = [0xA5u8; 16];
        let original = [0u8; WII_SECTOR_SIZE];

        let mut buf = original;
        decrypt_sector(&mut buf, &key).unwrap();
        encrypt_sector(&mut buf, &key).unwrap();
        assert_eq!(buf, original, "decrypt+encrypt must recover the original ciphertext");
    }
}
