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

use std::io::Read;
use std::path::PathBuf;

use crate::nintendo::wup::error::{WupError, WupResult};
use crate::nintendo::wup::models::WupTmd;
use crate::nintendo::wup::nus::content_reader::{decrypt_hashed_range, decrypt_raw_range};
use crate::nintendo::wup::nus::fst_parser::{FstClusterHashMode, VirtualFile, VirtualFs};
use crate::nintendo::wup::nus::ticket_parser::TitleKey;

/// Source of encrypted content bytes for one title. Backs both the
/// NUS directory layout (files on disk) and the disc layout (byte
/// ranges inside a GM partition). Raw vs hashed decryption sits one
/// layer above.
pub trait ContentBytesSource {
    fn encrypted_content_len(&mut self, content_id: u32) -> WupResult<u64>;

    fn read_encrypted_range(
        &mut self,
        content_id: u32,
        offset: u64,
        output: &mut [u8],
    ) -> WupResult<()>;

    fn visit_encrypted_content(
        &mut self,
        content_id: u32,
        visitor: &mut dyn FnMut(&mut [u8]) -> WupResult<()>,
    ) -> WupResult<()> {
        let mut remaining = self.encrypted_content_len(content_id)?;
        let mut offset = 0;
        let mut buffer = vec![0; remaining.min(4 * 1024 * 1024) as usize];
        while remaining > 0 {
            let n = remaining.min(buffer.len() as u64) as usize;
            self.read_encrypted_range(content_id, offset, &mut buffer[..n])?;
            visitor(&mut buffer[..n])?;
            offset += n as u64;
            remaining -= n as u64;
        }
        Ok(())
    }
}

/// [`ContentBytesSource`] backed by a NUS-layout title directory on disk.
pub struct DirectoryContentSource {
    resolver: crate::nintendo::wup::nus::layout::ContentFilenameResolver,
    open_file: Option<(u32, std::fs::File)>,
}

impl DirectoryContentSource {
    /// Builds a source rooted at `title_dir`.
    pub fn new<P: Into<PathBuf>>(title_dir: P) -> Self {
        Self {
            resolver: crate::nintendo::wup::nus::layout::ContentFilenameResolver::new(
                title_dir.into(),
            ),
            open_file: None,
        }
    }

    /// Builds a source from an already-resolved filename resolver.
    pub fn with_resolver(
        resolver: crate::nintendo::wup::nus::layout::ContentFilenameResolver,
    ) -> Self {
        Self {
            resolver,
            open_file: None,
        }
    }
}

impl ContentBytesSource for DirectoryContentSource {
    fn encrypted_content_len(&mut self, content_id: u32) -> WupResult<u64> {
        let path = self
            .resolver
            .resolve(content_id)
            .ok_or(WupError::ContentNotFound { content_id })?;
        Ok(std::fs::metadata(path)?.len())
    }

    fn read_encrypted_range(
        &mut self,
        content_id: u32,
        offset: u64,
        output: &mut [u8],
    ) -> WupResult<()> {
        use std::io::{Seek, SeekFrom};
        if self
            .open_file
            .as_ref()
            .is_none_or(|(id, _)| *id != content_id)
        {
            let path = self
                .resolver
                .resolve(content_id)
                .ok_or(WupError::ContentNotFound { content_id })?;
            let file =
                std::fs::File::open(path).map_err(|_| WupError::ContentNotFound { content_id })?;
            self.open_file = Some((content_id, file));
        }
        let file = &mut self.open_file.as_mut().expect("opened above").1;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(output)?;
        Ok(())
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

#[cfg(test)]
use crate::nintendo::wup::crypto::aes_cbc_decrypt_in_place;

/// Decrypt a raw-mode content file. IV is the cluster index in the first two
/// bytes of an otherwise-zero buffer, matching `FSTVolume::DetermineUnhashedBlockIV`
/// for `blockIndex == 0`. Input length must be a multiple of 16.
#[cfg(test)]
pub fn decrypt_raw_content(
    mut data: Vec<u8>,
    title_key: &TitleKey,
    cluster_index: u16,
) -> WupResult<Vec<u8>> {
    let iv = raw_content_iv(cluster_index);
    aes_cbc_decrypt_in_place(&title_key.0, &iv, &mut data)?;
    Ok(data)
}

/// Decrypt a hashed-mode content file. Returns the virtual data stream with every
/// hash prefix stripped, producing a contiguous buffer of `num_blocks * 0xFC00` bytes.
#[cfg(test)]
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

        let mut hash_part = [0u8; HASHED_BLOCK_HASH_SIZE];
        hash_part.copy_from_slice(&block[..HASHED_BLOCK_HASH_SIZE]);
        let iv_zero = [0u8; 16];
        aes_cbc_decrypt_in_place(&title_key.0, &iv_zero, &mut hash_part)?;

        let iv_offset = (block_idx % HASHED_BLOCK_H0_COUNT) * HASHED_BLOCK_H0_SIZE;
        let data_iv: [u8; 16] = hash_part[iv_offset..iv_offset + 16]
            .try_into()
            .expect("constant slice length is 16 bytes");

        let mut data_part = vec![0u8; HASHED_BLOCK_DATA_SIZE];
        data_part.copy_from_slice(&block[HASHED_BLOCK_HASH_SIZE..]);
        aes_cbc_decrypt_in_place(&title_key.0, &data_iv, &mut data_part)?;

        out.extend_from_slice(&data_part);
    }
    Ok(out)
}
/// IV used for decrypting raw-mode cluster data: cluster index in
/// the first two bytes (big-endian), zeros in the remaining 14.
pub(crate) fn raw_content_iv(cluster_index: u16) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[0] = (cluster_index >> 8) as u8;
    iv[1] = (cluster_index & 0xFF) as u8;
    iv
}

/// A validated file's decryption parameters: hash mode, content id,
/// and the byte range `[start, end)` within that content's physical
/// stream. Returned by [`ContentLoader::validate_file`] so a caller
/// that must validate before touching the filesystem can hand it
/// straight to [`ContentLoader::stream_prepared_file`] instead of
/// paying for `file_extent` a second time.
pub(crate) type FileExtent = (FstClusterHashMode, u32, u64, u64);

/// Bounded range reader for virtual FST file extents from NUS or disc sources.
pub struct ContentLoader<'a, S: ContentBytesSource> {
    source: S,
    title_key: TitleKey,
    tmd: &'a WupTmd,
    fs: &'a VirtualFs,
    raw_range_cache: Option<(u32, u16, u64, Vec<u8>)>,
    hashed_block_cache: Option<(u32, u64, Vec<u8>)>,
}
impl<'a, S: ContentBytesSource> ContentLoader<'a, S> {
    /// Builds a loader over `source` for the title described by `tmd`
    /// and `fs`.
    pub fn new(source: S, title_key: TitleKey, tmd: &'a WupTmd, fs: &'a VirtualFs) -> Self {
        Self {
            source,
            title_key,
            tmd,
            fs,
            raw_range_cache: None,
            hashed_block_cache: None,
        }
    }

    /// Reject files that are shared or extend beyond their content
    /// cluster, returning the decryption extent for reuse by
    /// [`Self::stream_prepared_file`].
    pub fn validate_file(&mut self, file: &VirtualFile) -> WupResult<FileExtent> {
        self.file_extent(file)
    }

    fn file_extent(&mut self, file: &VirtualFile) -> WupResult<FileExtent> {
        if file.is_shared {
            return Err(WupError::FileInheritedFromOtherTitle {
                path: file.path.clone(),
                cluster_index: file.cluster_index,
            });
        }
        let hash_mode = self
            .fs
            .clusters
            .get(file.cluster_index as usize)
            .ok_or(WupError::InvalidFst)?
            .hash_mode;
        let entry =
            self.tmd
                .content_by_index(file.cluster_index)
                .ok_or(WupError::ContentNotFound {
                    content_id: u32::from(file.cluster_index),
                })?;
        let content_id = entry.content_id;
        let physical_len = self.source.encrypted_content_len(content_id)?;
        let start = u64::from(file.file_offset)
            .checked_mul(u64::from(self.fs.offset_factor))
            .ok_or(WupError::InvalidFst)?;
        let end = start
            .checked_add(u64::from(file.file_size))
            .ok_or(WupError::InvalidFst)?;
        let capacity = match hash_mode {
            FstClusterHashMode::HashInterleaved => {
                if !physical_len.is_multiple_of(HASHED_BLOCK_SIZE as u64) {
                    return Err(WupError::InvalidFst);
                }
                physical_len / HASHED_BLOCK_SIZE as u64 * HASHED_BLOCK_DATA_SIZE as u64
            }
            FstClusterHashMode::Raw | FstClusterHashMode::RawStream => physical_len,
            FstClusterHashMode::Unknown(_) => return Err(WupError::UnsupportedContentMode),
        };
        if end > capacity {
            return Err(WupError::FileInheritedFromOtherTitle {
                path: file.path.clone(),
                cluster_index: file.cluster_index,
            });
        }
        Ok((hash_mode, content_id, start, end))
    }
    /// Streams a validated FST file to a consumer using bounded range reads.
    ///
    /// Returns [`WupError::FileInheritedFromOtherTitle`] when the file
    /// entry is shared or its extent exceeds the cluster's available data,
    /// so update/DLC overlays emit only bytes shipped in that title.
    pub fn stream_file(
        &mut self,
        file: &VirtualFile,
        visitor: impl FnMut(&[u8]) -> WupResult<()>,
    ) -> WupResult<u64> {
        let extent = self.file_extent(file)?;
        self.stream_prepared_file(file, extent, visitor)
    }

    /// Streams a file whose extent was already computed by
    /// [`Self::validate_file`], so callers that must validate before
    /// touching the filesystem do not recompute `file_extent`.
    pub(crate) fn stream_prepared_file(
        &mut self,
        file: &VirtualFile,
        (hash_mode, content_id, start, end): FileExtent,
        mut visitor: impl FnMut(&[u8]) -> WupResult<()>,
    ) -> WupResult<u64> {
        let mut offset = start;
        while offset < end {
            let count = (end - offset).min((1024 * 1024) as u64) as usize;
            match hash_mode {
                FstClusterHashMode::Raw | FstClusterHashMode::RawStream => {
                    let skip =
                        self.decrypt_source_raw(content_id, file.cluster_index, offset, count)?;
                    let cached = &self.raw_range_cache.as_ref().expect("stored above").3;
                    visitor(&cached[skip..skip + count])?;
                }
                FstClusterHashMode::HashInterleaved => {
                    let bytes = self.decrypt_source_hashed(content_id, offset, count)?;
                    visitor(&bytes)?;
                }
                FstClusterHashMode::Unknown(_) => unreachable!(),
            }
            offset += count as u64;
        }
        Ok(u64::from(file.file_size))
    }

    /// Decrypts (or reuses the cached decryption of) the aligned raw-mode
    /// range covering `[offset, offset + len)`, storing it in
    /// `raw_range_cache`. Returns the byte offset of the requested range
    /// within that cache so the caller can borrow it directly instead of
    /// copying it out.
    fn decrypt_source_raw(
        &mut self,
        content_id: u32,
        cluster_index: u16,
        offset: u64,
        len: usize,
    ) -> WupResult<usize> {
        let end = offset.checked_add(len as u64).ok_or(WupError::InvalidFst)?;
        let aligned_start = offset & !15;
        let aligned_end = end.checked_add(15).ok_or(WupError::InvalidFst)? & !15;
        let aligned_len = (aligned_end - aligned_start) as usize;
        let cached = self.raw_range_cache.as_ref().and_then(
            |(cached_id, cached_cluster, cached_start, bytes)| {
                let cached_end = *cached_start + bytes.len() as u64;
                (*cached_id == content_id
                    && *cached_cluster == cluster_index
                    && *cached_start < aligned_end
                    && cached_end > aligned_start)
                    .then_some((*cached_start, cached_end, bytes))
            },
        );

        let decrypted = if let Some((cached_start, cached_end, cached_bytes)) = cached {
            let mut decrypted = vec![0; aligned_len];
            let overlap_start = aligned_start.max(cached_start);
            let overlap_end = aligned_end.min(cached_end);
            let overlap_len = (overlap_end - overlap_start) as usize;
            let target_start = (overlap_start - aligned_start) as usize;
            let cache_start = (overlap_start - cached_start) as usize;
            decrypted[target_start..target_start + overlap_len]
                .copy_from_slice(&cached_bytes[cache_start..cache_start + overlap_len]);

            if aligned_start < overlap_start {
                let prefix_len = (overlap_start - aligned_start) as usize;
                let prefix = decrypt_raw_range(
                    |read_offset, output| {
                        self.source
                            .read_encrypted_range(content_id, read_offset, output)
                    },
                    &self.title_key,
                    cluster_index,
                    aligned_start,
                    prefix_len,
                )?;
                decrypted[..prefix_len].copy_from_slice(&prefix);
            }
            if overlap_end < aligned_end {
                let suffix_len = (aligned_end - overlap_end) as usize;
                let suffix = decrypt_raw_range(
                    |read_offset, output| {
                        self.source
                            .read_encrypted_range(content_id, read_offset, output)
                    },
                    &self.title_key,
                    cluster_index,
                    overlap_end,
                    suffix_len,
                )?;
                let suffix_start = (overlap_end - aligned_start) as usize;
                decrypted[suffix_start..].copy_from_slice(&suffix);
            }
            decrypted
        } else {
            decrypt_raw_range(
                |read_offset, output| {
                    self.source
                        .read_encrypted_range(content_id, read_offset, output)
                },
                &self.title_key,
                cluster_index,
                aligned_start,
                aligned_len,
            )?
        };

        self.raw_range_cache = Some((content_id, cluster_index, aligned_start, decrypted));
        Ok((offset - aligned_start) as usize)
    }

    fn decrypt_source_hashed(
        &mut self,
        content_id: u32,
        offset: u64,
        len: usize,
    ) -> WupResult<Vec<u8>> {
        let data_size = HASHED_BLOCK_DATA_SIZE as u64;
        let mut out = Vec::with_capacity(len);
        let mut current = offset;
        let end = offset.checked_add(len as u64).ok_or(WupError::InvalidFst)?;
        while current < end {
            let block_index = current / data_size;
            if self
                .hashed_block_cache
                .as_ref()
                .is_none_or(|(cached_id, cached_index, _)| {
                    *cached_id != content_id || *cached_index != block_index
                })
            {
                let block_start = block_index * data_size;
                let payload = decrypt_hashed_range(
                    |read_offset, output| {
                        self.source
                            .read_encrypted_range(content_id, read_offset, output)
                    },
                    &self.title_key,
                    block_start,
                    HASHED_BLOCK_DATA_SIZE,
                )?;
                self.hashed_block_cache = Some((content_id, block_index, payload));
            }
            let within = (current % data_size) as usize;
            let n = ((end - current) as usize).min(HASHED_BLOCK_DATA_SIZE - within);
            let payload = &self.hashed_block_cache.as_ref().expect("cached above").2;
            out.extend_from_slice(&payload[within..within + n]);
            current += n as u64;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::wup::models::TmdContentEntry;
    use crate::nintendo::wup::nus::fst_parser::FstCluster;
    use aes::{
        Aes128,
        cipher::{BlockModeEncrypt, KeyIvInit},
    };
    use block_padding::NoPadding;
    use cbc::Encryptor;

    type Aes128CbcEnc = Encryptor<Aes128>;

    fn encrypt_in_place(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8]) {
        Aes128CbcEnc::new_from_slices(key, iv)
            .unwrap()
            .encrypt_padded::<NoPadding>(data, data.len())
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

            // Plaintext data: another deterministic pattern so the
            // test can assert the decrypt produces exactly these bytes.
            let mut data_plain = vec![0u8; HASHED_BLOCK_DATA_SIZE];
            for (i, b) in data_plain.iter_mut().enumerate() {
                *b = ((block_idx as u32) * 13 + i as u32) as u8;
            }
            plain_data.extend_from_slice(&data_plain);

            let iv_offset = (block_idx % HASHED_BLOCK_H0_COUNT) * HASHED_BLOCK_H0_SIZE;
            let data_iv: [u8; 16] = hash_plain[iv_offset..iv_offset + 16].try_into().unwrap();

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
    fn hashed_stream_matches_full_decrypt_across_h0_wrap_and_chunk_boundary() {
        struct BytesSource(Vec<u8>, usize);

        impl ContentBytesSource for BytesSource {
            fn encrypted_content_len(&mut self, _content_id: u32) -> WupResult<u64> {
                Ok(self.0.len() as u64)
            }

            fn read_encrypted_range(
                &mut self,
                _content_id: u32,
                offset: u64,
                output: &mut [u8],
            ) -> WupResult<()> {
                self.1 += 1;
                let start = usize::try_from(offset).map_err(|_| WupError::InvalidFst)?;
                output.copy_from_slice(
                    self.0
                        .get(start..start + output.len())
                        .ok_or(WupError::InvalidFst)?,
                );
                Ok(())
            }
        }

        let title_key = TitleKey([0x5Au8; 16]);
        let (encrypted, _) = build_hashed_content(&title_key, 34);
        let full_decrypt = decrypt_hashed_content(&encrypted, &title_key).unwrap();
        let start = HASHED_BLOCK_DATA_SIZE * 15 - 32;
        let file_size = 1_100_000usize;
        let file = VirtualFile {
            path: "hashed.bin".to_string(),
            cluster_index: 0,
            file_offset: start as u32,
            file_size: file_size as u32,
            is_shared: false,
        };
        let adjacent = VirtualFile {
            path: "adjacent.bin".to_string(),
            cluster_index: 0,
            file_offset: (start + file_size) as u32,
            file_size: 128,
            is_shared: false,
        };
        let fs = VirtualFs {
            offset_factor: 1,
            hash_is_disabled: false,
            clusters: vec![FstCluster {
                offset: 0,
                size: encrypted.len() as u32,
                owner_title_id: 0,
                group_id: 0,
                hash_mode: FstClusterHashMode::HashInterleaved,
            }],
            files: vec![file.clone(), adjacent.clone()],
        };
        let tmd = WupTmd {
            signature_type: 0,
            tmd_version: 1,
            title_id: 0,
            title_type: 0,
            group_id: 0,
            access_rights: 0,
            title_version: 0,
            boot_index: 0,
            content_info_hash: [0; 32],
            contents: vec![TmdContentEntry {
                content_id: 7,
                index: 0,
                flags: crate::nintendo::wup::models::tmd::TmdContentFlags::ENCRYPTED
                    | crate::nintendo::wup::models::tmd::TmdContentFlags::HASHED,
                size: encrypted.len() as u64,
                hash: [0; 32],
            }],
        };
        let mut loader = ContentLoader::new(BytesSource(encrypted, 0), title_key, &tmd, &fs);
        let mut streamed = Vec::new();
        loader
            .stream_file(&file, |chunk| {
                streamed.extend_from_slice(chunk);
                Ok(())
            })
            .unwrap();
        assert_eq!(streamed, full_decrypt[start..start + file_size]);
        let reads = loader.source.1;
        loader.stream_file(&adjacent, |_| Ok(())).unwrap();
        assert_eq!(loader.source.1, reads);
    }

    #[test]
    fn raw_stream_reuses_cached_plaintext_for_overlapping_file_ranges() {
        struct BytesSource {
            bytes: Vec<u8>,
            reads: usize,
        }

        impl ContentBytesSource for BytesSource {
            fn encrypted_content_len(&mut self, _content_id: u32) -> WupResult<u64> {
                Ok(self.bytes.len() as u64)
            }

            fn read_encrypted_range(
                &mut self,
                _content_id: u32,
                offset: u64,
                output: &mut [u8],
            ) -> WupResult<()> {
                self.reads += 1;
                let start = usize::try_from(offset).map_err(|_| WupError::InvalidFst)?;
                output.copy_from_slice(
                    self.bytes
                        .get(start..start + output.len())
                        .ok_or(WupError::InvalidFst)?,
                );
                Ok(())
            }
        }

        let title_key = TitleKey([0x2Au8; 16]);
        let plaintext: Vec<u8> = (0..128).collect();
        let mut encrypted = plaintext.clone();
        encrypt_in_place(&title_key.0, &raw_content_iv(0), &mut encrypted);
        let first = VirtualFile {
            path: "first.bin".to_string(),
            cluster_index: 0,
            file_offset: 8,
            file_size: 64,
            is_shared: false,
        };
        let overlapping = VirtualFile {
            path: "overlapping.bin".to_string(),
            cluster_index: 0,
            file_offset: 24,
            file_size: 16,
            is_shared: false,
        };
        let fs = VirtualFs {
            offset_factor: 1,
            hash_is_disabled: false,
            clusters: vec![FstCluster {
                offset: 0,
                size: encrypted.len() as u32,
                owner_title_id: 0,
                group_id: 0,
                hash_mode: FstClusterHashMode::Raw,
            }],
            files: vec![first.clone(), overlapping.clone()],
        };
        let tmd = WupTmd {
            signature_type: 0,
            tmd_version: 1,
            title_id: 0,
            title_type: 0,
            group_id: 0,
            access_rights: 0,
            title_version: 0,
            boot_index: 0,
            content_info_hash: [0; 32],
            contents: vec![TmdContentEntry {
                content_id: 7,
                index: 0,
                flags: crate::nintendo::wup::models::tmd::TmdContentFlags::ENCRYPTED,
                size: encrypted.len() as u64,
                hash: [0; 32],
            }],
        };
        let mut loader = ContentLoader::new(
            BytesSource {
                bytes: encrypted,
                reads: 0,
            },
            title_key,
            &tmd,
            &fs,
        );
        let mut first_bytes = Vec::new();
        loader
            .stream_file(&first, |chunk| {
                first_bytes.extend_from_slice(chunk);
                Ok(())
            })
            .unwrap();
        assert_eq!(first_bytes, plaintext[8..72]);
        let reads = loader.source.reads;

        let mut overlap_bytes = Vec::new();
        loader
            .stream_file(&overlapping, |chunk| {
                overlap_bytes.extend_from_slice(chunk);
                Ok(())
            })
            .unwrap();
        assert_eq!(overlap_bytes, plaintext[24..40]);
        assert_eq!(loader.source.reads, reads);
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

        let mut loader = ContentLoader::new(
            DirectoryContentSource::new(dir.path()),
            title_key,
            &tmd,
            &fs,
        );
        let mut bytes = Vec::new();
        loader
            .stream_file(&fs.files[0], |chunk| {
                bytes.extend_from_slice(chunk);
                Ok(())
            })
            .unwrap();
        assert_eq!(bytes, plaintext[16..48]);
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
        let mut loader = ContentLoader::new(
            DirectoryContentSource::new(dir.path()),
            title_key,
            &tmd,
            &fs,
        );
        let err = loader.stream_file(&fs.files[0], |_| Ok(()));
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

        let mut loader = ContentLoader::new(
            DirectoryContentSource::new(dir.path()),
            title_key,
            &tmd,
            &fs,
        );
        let mut emitted = false;
        let err = loader.stream_file(&fs.files[0], |_| {
            emitted = true;
            Ok(())
        });
        assert!(!emitted);
        assert!(matches!(
            err,
            Err(WupError::FileInheritedFromOtherTitle {
                cluster_index: 0,
                ..
            })
        ));
        let mut out_of_range = fs.clone();
        out_of_range.files[0].is_shared = false;
        out_of_range.files[0].file_size = 64;
        let mut loader = ContentLoader::new(
            DirectoryContentSource::new(dir.path()),
            title_key,
            &tmd,
            &out_of_range,
        );
        let mut emitted = false;
        let err = loader.stream_file(&out_of_range.files[0], |_| {
            emitted = true;
            Ok(())
        });
        assert!(!emitted);
        assert!(matches!(
            err,
            Err(WupError::FileInheritedFromOtherTitle { .. })
        ));
    }
}
