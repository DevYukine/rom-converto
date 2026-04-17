//! Wii U content decryption: raw and hashed modes.
//!
//! Every NUS content file (`00000000.app`, `00000001.app`, ...) is
//! either a single AES-CBC stream (raw mode) or a sequence of 64 KiB
//! blocks that each contain a 0x400-byte hash prefix plus 0xFC00
//! bytes of payload (hashed mode). Both modes use the title key
//! derived from the ticket. The two modes differ in how the IV is
//! derived and whether the hash prefixes are stripped from the
//! output.
//!
//! Byte layout matches Cemu's FST decryption so every virtual file
//! is recoverable regardless of which mode its cluster uses.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::nintendo::wup::crypto::aes_cbc_decrypt_in_place;
use crate::nintendo::wup::error::{WupError, WupResult};
use crate::nintendo::wup::models::{TmdContentEntry, WupTmd};
use crate::nintendo::wup::nus::fst_parser::{FstClusterHashMode, VirtualFile, VirtualFs};
use crate::nintendo::wup::nus::ticket_parser::TitleKey;

/// Source of encrypted content bytes for one title. Backs both the
/// NUS directory layout (files on disk) and the disc layout (byte
/// ranges inside a GM partition). Raw vs hashed decryption sits one
/// layer above.
pub trait ContentBytesSource {
    /// Return the still-encrypted bytes of the content file with the
    /// given TMD content id (the `.app` filename without extension).
    fn read_encrypted_content(&mut self, content_id: u32) -> WupResult<Vec<u8>>;
}

/// Reads encrypted content files from a NUS-layout directory on
/// disk. Filename resolution goes through
/// [`crate::nintendo::wup::nus::layout::ContentFilenameResolver`] so
/// both `{ID:08x}.app` and extensionless `{ID:08x}` work.
pub struct DirectoryContentSource {
    resolver: crate::nintendo::wup::nus::layout::ContentFilenameResolver,
}

impl DirectoryContentSource {
    /// Build a source rooted at `title_dir`. Tries `{ID:08x}.app`
    /// first, falls back to extensionless `{ID:08x}`.
    pub fn new<P: Into<PathBuf>>(title_dir: P) -> Self {
        Self {
            resolver: crate::nintendo::wup::nus::layout::ContentFilenameResolver::new(
                title_dir.into(),
            ),
        }
    }

    /// Build a source from a pre-walked filename resolver.
    pub fn with_resolver(
        resolver: crate::nintendo::wup::nus::layout::ContentFilenameResolver,
    ) -> Self {
        Self { resolver }
    }
}

impl ContentBytesSource for DirectoryContentSource {
    fn read_encrypted_content(&mut self, content_id: u32) -> WupResult<Vec<u8>> {
        let path = self
            .resolver
            .resolve(content_id)
            .ok_or(WupError::ContentNotFound { content_id })?;
        std::fs::read(&path).map_err(|_| WupError::ContentNotFound { content_id })
    }
}

/// Size of one hashed-mode physical block in bytes.
pub const HASHED_BLOCK_SIZE: usize = 0x10000;
/// Size of the hash prefix inside one hashed-mode block.
pub const HASHED_BLOCK_HASH_SIZE: usize = 0x400;
/// Size of the payload (virtual-visible data) inside one hashed-mode block.
pub const HASHED_BLOCK_DATA_SIZE: usize = HASHED_BLOCK_SIZE - HASHED_BLOCK_HASH_SIZE;
/// Byte size of one H0 SHA-1 hash inside the hash prefix.
pub const HASHED_BLOCK_H0_SIZE: usize = 20;
/// Number of H0 hashes packed into the hash prefix of one block.
pub const HASHED_BLOCK_H0_COUNT: usize = 16;

/// Decrypt a raw-mode content file. The whole file is one AES-CBC
/// stream keyed by the title key; the IV is the cluster index
/// placed in the first two bytes of an otherwise-zero buffer, which
/// matches `FSTVolume::DetermineUnhashedBlockIV` for `blockIndex == 0`.
///
/// `data` is decrypted in place and returned as the same owned
/// buffer. The length must be a multiple of 16; real Wii U content
/// files are always padded to an AES block boundary.
pub fn decrypt_raw_content(
    mut data: Vec<u8>,
    title_key: &TitleKey,
    cluster_index: u16,
) -> WupResult<Vec<u8>> {
    let iv = raw_content_iv(cluster_index);
    aes_cbc_decrypt_in_place(&title_key.0, &iv, &mut data)?;
    Ok(data)
}

/// Decrypt content 0 (the FST cluster). Cluster index 0 gives the
/// all-zero IV the FST header was encrypted with.
pub fn decrypt_content_0(data: Vec<u8>, title_key: &TitleKey) -> WupResult<Vec<u8>> {
    decrypt_raw_content(data, title_key, 0)
}

/// Decrypt a hashed-mode content file. Each 64 KiB block holds a
/// 0x400-byte hash prefix decrypted with IV=0 followed by a 0xFC00
/// data segment decrypted with an IV sourced from the decrypted
/// prefix. Returns the virtual data stream with every hash prefix
/// stripped, i.e. a contiguous buffer of `num_blocks * 0xFC00` bytes.
pub fn decrypt_hashed_content(encrypted: &[u8], title_key: &TitleKey) -> WupResult<Vec<u8>> {
    if !encrypted.len().is_multiple_of(HASHED_BLOCK_SIZE) {
        return Err(WupError::AesError(format!(
            "hashed content length {} is not a multiple of {}",
            encrypted.len(),
            HASHED_BLOCK_SIZE
        )));
    }
    let num_blocks = encrypted.len() / HASHED_BLOCK_SIZE;
    let mut out = Vec::with_capacity(num_blocks * HASHED_BLOCK_DATA_SIZE);
    for block_idx in 0..num_blocks {
        let block = &encrypted[block_idx * HASHED_BLOCK_SIZE..(block_idx + 1) * HASHED_BLOCK_SIZE];

        // Decrypt the hash prefix with IV=0.
        let mut hash_part = [0u8; HASHED_BLOCK_HASH_SIZE];
        hash_part.copy_from_slice(&block[..HASHED_BLOCK_HASH_SIZE]);
        let iv_zero = [0u8; 16];
        aes_cbc_decrypt_in_place(&title_key.0, &iv_zero, &mut hash_part)?;

        // Pick the 16-byte data IV from H0[block_idx % 16].
        let iv_offset = (block_idx % HASHED_BLOCK_H0_COUNT) * HASHED_BLOCK_H0_SIZE;
        let data_iv: [u8; 16] = hash_part[iv_offset..iv_offset + 16]
            .try_into()
            .expect("constant slice length is 16 bytes");

        // Decrypt the data portion with that IV.
        let mut data_part = vec![0u8; HASHED_BLOCK_DATA_SIZE];
        data_part.copy_from_slice(&block[HASHED_BLOCK_HASH_SIZE..]);
        aes_cbc_decrypt_in_place(&title_key.0, &data_iv, &mut data_part)?;

        out.extend_from_slice(&data_part);
    }
    Ok(out)
}

/// IV used for decrypting raw-mode cluster data: cluster index in
/// the first two bytes (big-endian), zeros in the remaining 14.
fn raw_content_iv(cluster_index: u16) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[0] = (cluster_index >> 8) as u8;
    iv[1] = (cluster_index & 0xFF) as u8;
    iv
}

/// Decrypts NUS content files on demand and caches each decrypted
/// cluster so later file lookups inside the same cluster skip the
/// AES work. Drop the loader after streaming the title to free the
/// cache. Generic over [`ContentBytesSource`] so the same caching
/// and extraction logic serves both directory and disc sources.
pub struct ContentLoader<'a, S: ContentBytesSource> {
    source: S,
    title_key: TitleKey,
    tmd: &'a WupTmd,
    fs: &'a VirtualFs,
    cache: HashMap<u16, Vec<u8>>,
}

impl<'a, S: ContentBytesSource> ContentLoader<'a, S> {
    pub fn new(source: S, title_key: TitleKey, tmd: &'a WupTmd, fs: &'a VirtualFs) -> Self {
        Self {
            source,
            title_key,
            tmd,
            fs,
            cache: HashMap::new(),
        }
    }

    /// Return the decrypted byte buffer for the content file at
    /// `cluster_index`. The result is cached so repeated calls
    /// reuse the same `Vec<u8>`.
    pub fn decrypted_cluster(&mut self, cluster_index: u16) -> WupResult<&[u8]> {
        if !self.cache.contains_key(&cluster_index) {
            let cluster =
                self.fs
                    .clusters
                    .get(cluster_index as usize)
                    .ok_or(WupError::ContentNotFound {
                        content_id: u32::from(cluster_index),
                    })?;
            let tmd_entry =
                self.tmd
                    .content_by_index(cluster_index)
                    .ok_or(WupError::ContentNotFound {
                        content_id: u32::from(cluster_index),
                    })?;
            let encrypted = self.source.read_encrypted_content(tmd_entry.content_id)?;
            let decrypted = match cluster.hash_mode {
                FstClusterHashMode::HashInterleaved => {
                    decrypt_hashed_content(&encrypted, &self.title_key)?
                }
                FstClusterHashMode::Raw | FstClusterHashMode::RawStream => {
                    decrypt_raw_content(encrypted, &self.title_key, cluster_index)?
                }
                FstClusterHashMode::Unknown(_) => {
                    return Err(WupError::UnsupportedContentMode);
                }
            };
            self.cache.insert(cluster_index, decrypted);
        }
        Ok(self.cache.get(&cluster_index).expect("just inserted"))
    }

    /// Extract the byte range for one virtual file from its
    /// decrypted cluster. Returns [`WupError::FileInheritedFromOtherTitle`]
    /// when the file entry's `is_shared` flag (FST type bit 7) is set
    /// so update/DLC overlays emit only their own new bytes, or when
    /// the byte range runs past the cluster bound.
    pub fn extract_file(&mut self, file: &VirtualFile) -> WupResult<Vec<u8>> {
        if file.is_shared {
            return Err(WupError::FileInheritedFromOtherTitle {
                path: file.path.clone(),
                cluster_index: file.cluster_index,
            });
        }
        let offset_factor = self.fs.offset_factor as u64;
        let start = u64::from(file.file_offset) * offset_factor;
        let end = start
            .checked_add(u64::from(file.file_size))
            .ok_or(WupError::InvalidFst)?;
        let cluster = self.decrypted_cluster(file.cluster_index)?;
        if (end as usize) > cluster.len() {
            return Err(WupError::FileInheritedFromOtherTitle {
                path: file.path.clone(),
                cluster_index: file.cluster_index,
            });
        }
        Ok(cluster[start as usize..end as usize].to_vec())
    }

    /// TMD entry the FST cluster maps to, for sanity checks or
    /// logging.
    pub fn tmd_entry_for(&self, cluster_index: u16) -> Option<&TmdContentEntry> {
        self.tmd.content_by_index(cluster_index)
    }
}

/// Convenience constructor for loaders over a NUS directory on disk.
pub fn content_loader_for_directory<'a>(
    title_dir: &Path,
    title_key: TitleKey,
    tmd: &'a WupTmd,
    fs: &'a VirtualFs,
) -> ContentLoader<'a, DirectoryContentSource> {
    ContentLoader::new(DirectoryContentSource::new(title_dir), title_key, tmd, fs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::wup::nus::fst_parser::FstCluster;
    use aes::{
        Aes128,
        cipher::{BlockEncryptMut, KeyIvInit},
    };
    use block_padding::NoPadding;
    use cbc::Encryptor;

    type Aes128CbcEnc = Encryptor<Aes128>;

    fn encrypt_in_place(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8]) {
        Aes128CbcEnc::new_from_slices(key, iv)
            .unwrap()
            .encrypt_padded_mut::<NoPadding>(data, data.len())
            .unwrap();
    }

    #[test]
    fn raw_iv_has_cluster_in_first_two_bytes() {
        let iv = raw_content_iv(0x1234);
        assert_eq!(iv[0], 0x12);
        assert_eq!(iv[1], 0x34);
        for b in &iv[2..] {
            assert_eq!(*b, 0);
        }
    }

    #[test]
    fn raw_round_trip_whole_content() {
        let title_key = TitleKey([0x42u8; 16]);
        let cluster_index = 0u16;
        // Size must be a multiple of 16. Pick something interesting
        // that spans multiple AES blocks.
        let mut plaintext = vec![0u8; 16 * 37];
        for (i, b) in plaintext.iter_mut().enumerate() {
            *b = (i & 0xFF) as u8;
        }
        let mut encrypted = plaintext.clone();
        encrypt_in_place(&title_key.0, &raw_content_iv(cluster_index), &mut encrypted);

        let decrypted = decrypt_raw_content(encrypted, &title_key, cluster_index).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn raw_iv_differs_per_cluster_index() {
        let title_key = TitleKey([0x42u8; 16]);
        let mut plaintext = vec![0u8; 16 * 4];
        for (i, b) in plaintext.iter_mut().enumerate() {
            *b = (i & 0xFF) as u8;
        }

        // Encrypt with cluster 0 and try to decrypt with cluster 1:
        // the output must not equal the original plaintext.
        let mut encrypted = plaintext.clone();
        encrypt_in_place(&title_key.0, &raw_content_iv(0), &mut encrypted);
        let wrong_decrypt = decrypt_raw_content(encrypted.clone(), &title_key, 1).unwrap();
        assert_ne!(wrong_decrypt, plaintext);

        // But decrypting with the matching cluster does recover it.
        let right_decrypt = decrypt_raw_content(encrypted, &title_key, 0).unwrap();
        assert_eq!(right_decrypt, plaintext);
    }

    #[test]
    fn content_0_helper_matches_raw_zero_cluster() {
        let title_key = TitleKey([0xAAu8; 16]);
        let plaintext = vec![0xCCu8; 16 * 20];
        let mut encrypted = plaintext.clone();
        encrypt_in_place(&title_key.0, &raw_content_iv(0), &mut encrypted);

        let via_helper = decrypt_content_0(encrypted.clone(), &title_key).unwrap();
        let via_raw = decrypt_raw_content(encrypted, &title_key, 0).unwrap();
        assert_eq!(via_helper, via_raw);
        assert_eq!(via_helper, plaintext);
    }

    /// Build a synthetic hashed-mode content file: `num_blocks`
    /// blocks of `[hash_prefix: 0x400][data: 0xFC00]`, encrypted
    /// the same way Cemu's content creator does. Returns the
    /// encrypted content plus the plaintext data-only stream the
    /// decryptor is expected to recover.
    fn build_hashed_content(title_key: &TitleKey, num_blocks: usize) -> (Vec<u8>, Vec<u8>) {
        let mut encrypted = vec![0u8; num_blocks * HASHED_BLOCK_SIZE];
        let mut plain_data = Vec::with_capacity(num_blocks * HASHED_BLOCK_DATA_SIZE);
        for block_idx in 0..num_blocks {
            // Plaintext hash prefix: a deterministic pattern so the
            // IVs are reproducible.
            let mut hash_plain = [0u8; HASHED_BLOCK_HASH_SIZE];
            for (i, b) in hash_plain.iter_mut().enumerate() {
                *b = ((block_idx as u32 + 1) * (i as u32 + 1)) as u8;
            }

            // Plaintext data: another deterministic pattern so we
            // can assert the decrypt produces exactly these bytes.
            let mut data_plain = vec![0u8; HASHED_BLOCK_DATA_SIZE];
            for (i, b) in data_plain.iter_mut().enumerate() {
                *b = ((block_idx as u32) * 13 + i as u32) as u8;
            }
            plain_data.extend_from_slice(&data_plain);

            // Data IV = first 16 bytes of H0[block_idx % 16] in
            // the plaintext hash prefix.
            let iv_offset = (block_idx % HASHED_BLOCK_H0_COUNT) * HASHED_BLOCK_H0_SIZE;
            let data_iv: [u8; 16] = hash_plain[iv_offset..iv_offset + 16].try_into().unwrap();

            // Encrypt hash (IV=0), then encrypt data (IV=data_iv).
            let mut hash_enc = hash_plain;
            encrypt_in_place(&title_key.0, &[0u8; 16], &mut hash_enc);
            encrypt_in_place(&title_key.0, &data_iv, &mut data_plain);

            let block_start = block_idx * HASHED_BLOCK_SIZE;
            encrypted[block_start..block_start + HASHED_BLOCK_HASH_SIZE].copy_from_slice(&hash_enc);
            encrypted[block_start + HASHED_BLOCK_HASH_SIZE..block_start + HASHED_BLOCK_SIZE]
                .copy_from_slice(&data_plain);
        }
        (encrypted, plain_data)
    }

    #[test]
    fn hashed_round_trip_single_block() {
        let title_key = TitleKey([0x33u8; 16]);
        let (encrypted, expected) = build_hashed_content(&title_key, 1);
        let decrypted = decrypt_hashed_content(&encrypted, &title_key).unwrap();
        assert_eq!(decrypted.len(), HASHED_BLOCK_DATA_SIZE);
        assert_eq!(decrypted, expected);
    }

    #[test]
    fn hashed_round_trip_multi_block_over_h0_cycle() {
        // 17 blocks exercises the `block_idx % 16` IV selection
        // past its first full cycle so an off-by-one in the mod
        // would show up here.
        let title_key = TitleKey([0x77u8; 16]);
        let (encrypted, expected) = build_hashed_content(&title_key, 17);
        let decrypted = decrypt_hashed_content(&encrypted, &title_key).unwrap();
        assert_eq!(decrypted, expected);
    }

    #[test]
    fn hashed_rejects_non_block_aligned_length() {
        let title_key = TitleKey([0u8; 16]);
        let bad = vec![0u8; HASHED_BLOCK_SIZE + 1];
        let err = decrypt_hashed_content(&bad, &title_key);
        assert!(matches!(err, Err(WupError::AesError(_))));
    }

    #[test]
    fn loader_extract_raw_file_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let title_key = TitleKey([0x11u8; 16]);
        // Create one content file with raw-mode encryption for
        // cluster 0. The file holds the bytes "HELLO_WORLDDDDDD"
        // plus padding to make the total length a multiple of 16.
        let plaintext: Vec<u8> = (0u8..64).collect();
        let mut encrypted = plaintext.clone();
        encrypt_in_place(&title_key.0, &raw_content_iv(0), &mut encrypted);
        std::fs::write(dir.path().join("00000000.app"), &encrypted).unwrap();

        // Build a TMD with one content entry pointing at cluster 0.
        let tmd = WupTmd {
            signature_type: 0,
            tmd_version: 1,
            title_id: 0x0005_000E_0000_0001,
            title_type: 0,
            group_id: 0,
            access_rights: 0,
            title_version: 0,
            boot_index: 0,
            content_info_hash: [0u8; 32],
            contents: vec![TmdContentEntry {
                content_id: 0,
                index: 0,
                flags: crate::nintendo::wup::models::tmd::TmdContentFlags::ENCRYPTED,
                size: 64,
                hash: [0u8; 32],
            }],
        };
        // Build a FST view with one raw-mode cluster and one
        // virtual file covering the middle 32 bytes of the content.
        let fs = VirtualFs {
            offset_factor: 1,
            hash_is_disabled: false,
            clusters: vec![FstCluster {
                offset: 0,
                size: 64,
                owner_title_id: 0x0005_000E_0000_0001,
                group_id: 0,
                hash_mode: FstClusterHashMode::Raw,
            }],
            files: vec![VirtualFile {
                path: "inner.bin".to_string(),
                cluster_index: 0,
                file_offset: 16,
                file_size: 32,
                is_shared: false,
            }],
        };

        let mut loader = content_loader_for_directory(dir.path(), title_key, &tmd, &fs);
        let bytes = loader.extract_file(&fs.files[0]).unwrap();
        assert_eq!(bytes, plaintext[16..48]);
    }

    #[test]
    fn loader_caches_decrypted_cluster_between_file_calls() {
        let dir = tempfile::tempdir().unwrap();
        let title_key = TitleKey([0x22u8; 16]);
        let plaintext: Vec<u8> = (0u8..64).collect();
        let mut encrypted = plaintext.clone();
        encrypt_in_place(&title_key.0, &raw_content_iv(0), &mut encrypted);
        std::fs::write(dir.path().join("00000000.app"), &encrypted).unwrap();

        let tmd = WupTmd {
            signature_type: 0,
            tmd_version: 1,
            title_id: 0x0005_000E_0000_0001,
            title_type: 0,
            group_id: 0,
            access_rights: 0,
            title_version: 0,
            boot_index: 0,
            content_info_hash: [0u8; 32],
            contents: vec![TmdContentEntry {
                content_id: 0,
                index: 0,
                flags: crate::nintendo::wup::models::tmd::TmdContentFlags::ENCRYPTED,
                size: 64,
                hash: [0u8; 32],
            }],
        };
        let fs = VirtualFs {
            offset_factor: 1,
            hash_is_disabled: false,
            clusters: vec![FstCluster {
                offset: 0,
                size: 64,
                owner_title_id: 0x0005_000E_0000_0001,
                group_id: 0,
                hash_mode: FstClusterHashMode::Raw,
            }],
            files: vec![
                VirtualFile {
                    path: "first.bin".to_string(),
                    cluster_index: 0,
                    file_offset: 0,
                    file_size: 16,
                    is_shared: false,
                },
                VirtualFile {
                    path: "second.bin".to_string(),
                    cluster_index: 0,
                    file_offset: 32,
                    file_size: 16,
                    is_shared: false,
                },
            ],
        };

        let mut loader = content_loader_for_directory(dir.path(), title_key, &tmd, &fs);
        let first = loader.extract_file(&fs.files[0]).unwrap();
        // Delete the on-disk file so a fresh decrypt would fail;
        // the second call must be served from the cache.
        std::fs::remove_file(dir.path().join("00000000.app")).unwrap();
        let second = loader.extract_file(&fs.files[1]).unwrap();
        assert_eq!(first, plaintext[0..16]);
        assert_eq!(second, plaintext[32..48]);
    }

    #[test]
    fn loader_returns_content_not_found_for_missing_app() {
        let dir = tempfile::tempdir().unwrap();
        let title_key = TitleKey([0u8; 16]);
        let tmd = WupTmd {
            signature_type: 0,
            tmd_version: 1,
            title_id: 0x0005_000E_0000_0001,
            title_type: 0,
            group_id: 0,
            access_rights: 0,
            title_version: 0,
            boot_index: 0,
            content_info_hash: [0u8; 32],
            contents: vec![TmdContentEntry {
                content_id: 0xDEAD_BEEF,
                index: 0,
                flags: crate::nintendo::wup::models::tmd::TmdContentFlags::ENCRYPTED,
                size: 16,
                hash: [0u8; 32],
            }],
        };
        let fs = VirtualFs {
            offset_factor: 1,
            hash_is_disabled: false,
            clusters: vec![FstCluster {
                offset: 0,
                size: 16,
                owner_title_id: 0x0005_000E_0000_0001,
                group_id: 0,
                hash_mode: FstClusterHashMode::Raw,
            }],
            files: vec![VirtualFile {
                path: "missing.bin".to_string(),
                cluster_index: 0,
                file_offset: 0,
                file_size: 16,
                is_shared: false,
            }],
        };
        let mut loader = content_loader_for_directory(dir.path(), title_key, &tmd, &fs);
        let err = loader.extract_file(&fs.files[0]);
        assert!(matches!(
            err,
            Err(WupError::ContentNotFound {
                content_id: 0xDEAD_BEEF
            })
        ));
    }

    #[test]
    fn loader_skips_files_flagged_shared() {
        let dir = tempfile::tempdir().unwrap();
        let title_key = TitleKey([0x55u8; 16]);
        let plaintext: Vec<u8> = (0u8..32).collect();
        let mut encrypted = plaintext.clone();
        encrypt_in_place(&title_key.0, &raw_content_iv(0), &mut encrypted);
        std::fs::write(dir.path().join("00000000.app"), &encrypted).unwrap();

        let tmd = WupTmd {
            signature_type: 0,
            tmd_version: 1,
            title_id: 0x0005_000E_1010_1E00,
            title_type: 0,
            group_id: 0,
            access_rights: 0,
            title_version: 0,
            boot_index: 0,
            content_info_hash: [0u8; 32],
            contents: vec![TmdContentEntry {
                content_id: 0,
                index: 0,
                flags: crate::nintendo::wup::models::tmd::TmdContentFlags::ENCRYPTED,
                size: 32,
                hash: [0u8; 32],
            }],
        };
        let fs = VirtualFs {
            offset_factor: 1,
            hash_is_disabled: false,
            clusters: vec![FstCluster {
                offset: 0,
                size: 32,
                owner_title_id: 0x0005_0000_1010_1E00,
                group_id: 0,
                hash_mode: FstClusterHashMode::Raw,
            }],
            files: vec![VirtualFile {
                path: "inherited.bin".to_string(),
                cluster_index: 0,
                file_offset: 0,
                file_size: 16,
                is_shared: true,
            }],
        };

        let mut loader = content_loader_for_directory(dir.path(), title_key, &tmd, &fs);
        let err = loader.extract_file(&fs.files[0]);
        assert!(matches!(
            err,
            Err(WupError::FileInheritedFromOtherTitle {
                cluster_index: 0,
                ..
            })
        ));
    }
}
