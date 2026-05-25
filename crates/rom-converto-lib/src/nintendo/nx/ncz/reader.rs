//! Random-access `NcaInput` adapter over an NCZ file.
//!
//! `NcaWalker` only ever issues positional reads, so we can present
//! the decompressed + re-encrypted NCA byte stream as a virtual file
//! and walk an NCZ without first materialising it to disk. Block-mode
//! NCZ decompresses one covered block per read; solid-mode (single
//! zstd frame) cannot be random-accessed and decompresses the whole
//! payload once on open, then serves reads from that buffer.
//! Re-encryption applies the stored per-section CTR keystream so the
//! bytes match what `NcaWalker` would see on a real encrypted NCA.
//! XTS sections are out of scope; the Control NCA path that drives
//! this adapter only touches CTR / NONE sections.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

use aes::Aes128;
use aes::cipher::{KeyIvInit, StreamCipher};
use ctr::Ctr128BE;

use crate::nintendo::nx::constants::{
    ENC_AES_CTR, ENC_AES_CTR_EX, ENC_AES_CTR_EX_SKIP_LAYER_HASH, ENC_AES_CTR_SKIP_LAYER_HASH,
    ENC_NONE, NCA_PREFIX_SIZE,
};
use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::ncz::header::{NczSectionEntry, read_headers};
use crate::nintendo::nx::walker::NcaInput;
use crate::util::pread::file_read_exact_at;

type AesCtr = Ctr128BE<Aes128>;

enum NczPayload {
    Block(BlockPayload),
    Solid(Vec<u8>),
}

struct BlockPayload {
    file: Arc<File>,
    block_size: u64,
    block_offsets: Vec<u64>,
    block_sizes: Vec<u32>,
    decompressed_payload_size: u64,
}

pub struct NczReader {
    prefix: Box<[u8; NCA_PREFIX_SIZE]>,
    sections: Vec<NczSectionEntry>,
    payload: NczPayload,
}

impl NczReader {
    pub fn open(
        file: Arc<File>,
        nca_offset_in_container: u64,
        ncz_total_size: u64,
    ) -> NxResult<Self> {
        if ncz_total_size < NCA_PREFIX_SIZE as u64 {
            return Err(NxError::IncompleteSection);
        }

        let mut prefix = Box::new([0u8; NCA_PREFIX_SIZE]);
        file_read_exact_at(&file, prefix.as_mut_slice(), nca_offset_in_container)?;

        let mut header_reader = FileSliceReader {
            file: file.clone(),
            base: nca_offset_in_container + NCA_PREFIX_SIZE as u64,
            position: 0,
        };
        let parsed = read_headers(&mut header_reader)?;
        let payload_abs_start = header_reader.base + header_reader.position;

        let payload = match parsed.block {
            Some(block_info) => {
                let block_size = block_info.block_size_bytes();
                let mut block_offsets =
                    Vec::with_capacity(block_info.compressed_block_sizes.len());
                let mut cursor = payload_abs_start;
                for &csz in &block_info.compressed_block_sizes {
                    block_offsets.push(cursor);
                    cursor += u64::from(csz);
                }
                NczPayload::Block(BlockPayload {
                    file: file.clone(),
                    block_size,
                    block_offsets,
                    block_sizes: block_info.compressed_block_sizes,
                    decompressed_payload_size: block_info.decompressed_size as u64,
                })
            }
            None => {
                let payload_compressed_size =
                    ncz_total_size.saturating_sub(payload_abs_start - nca_offset_in_container);
                let mut compressed = vec![0u8; payload_compressed_size as usize];
                file_read_exact_at(&file, &mut compressed, payload_abs_start)?;
                let decompressed = zstd::stream::decode_all(std::io::Cursor::new(&compressed))
                    .map_err(|e| NxError::ZstdError(format!("solid zstd decode: {e}")))?;
                NczPayload::Solid(decompressed)
            }
        };

        Ok(Self {
            prefix,
            sections: parsed.sections,
            payload,
        })
    }

    pub fn decompressed_nca_size(&self) -> u64 {
        let payload_size = match &self.payload {
            NczPayload::Block(b) => b.decompressed_payload_size,
            NczPayload::Solid(v) => v.len() as u64,
        };
        NCA_PREFIX_SIZE as u64 + payload_size
    }

    fn copy_payload(
        &self,
        dest: &mut [u8],
        payload_off: usize,
    ) -> NxResult<usize> {
        match &self.payload {
            NczPayload::Solid(v) => {
                let take = (v.len() - payload_off).min(dest.len());
                dest[..take].copy_from_slice(&v[payload_off..payload_off + take]);
                Ok(take)
            }
            NczPayload::Block(b) => {
                let block_idx = payload_off / b.block_size as usize;
                let in_block = payload_off % b.block_size as usize;
                let block = b.decompress_block(block_idx)?;
                let take = (block.len() - in_block).min(dest.len());
                dest[..take].copy_from_slice(&block[in_block..in_block + take]);
                Ok(take)
            }
        }
    }
}

impl BlockPayload {
    fn decompress_block(&self, block_idx: usize) -> NxResult<Vec<u8>> {
        let offset = self.block_offsets[block_idx];
        let csz = self.block_sizes[block_idx] as usize;
        let mut compressed = vec![0u8; csz];
        file_read_exact_at(&self.file, &mut compressed, offset)?;

        let is_last = block_idx + 1 == self.block_sizes.len();
        let logical_size = if is_last {
            (self.decompressed_payload_size as usize) - block_idx * self.block_size as usize
        } else {
            self.block_size as usize
        };

        if csz == logical_size {
            return Ok(compressed);
        }

        zstd::stream::decode_all(std::io::Cursor::new(&compressed))
            .map_err(|e| NxError::ZstdError(format!("decompress block {block_idx}: {e}")))
    }
}

impl NcaInput for NczReader {
    fn read_exact_at(&self, buf: &mut [u8], abs: u64) -> NxResult<()> {
        let total = self.decompressed_nca_size();
        if abs.saturating_add(buf.len() as u64) > total {
            return Err(NxError::IncompleteSection);
        }

        let mut written = 0usize;
        while written < buf.len() {
            let here = abs + written as u64;
            if here < NCA_PREFIX_SIZE as u64 {
                let take = (NCA_PREFIX_SIZE as u64 - here)
                    .min((buf.len() - written) as u64) as usize;
                buf[written..written + take]
                    .copy_from_slice(&self.prefix[here as usize..here as usize + take]);
                written += take;
                continue;
            }

            let payload_off = (here - NCA_PREFIX_SIZE as u64) as usize;
            let take = self.copy_payload(&mut buf[written..], payload_off)?;
            reencrypt_in_buf(&mut buf[written..written + take], here, &self.sections)?;
            written += take;
        }
        Ok(())
    }
}

fn reencrypt_in_buf(
    buf: &mut [u8],
    start_abs: u64,
    sections: &[NczSectionEntry],
) -> NxResult<()> {
    let mut covered = 0usize;
    while covered < buf.len() {
        let here = start_abs + covered as u64;
        let section = sections.iter().find(|s| {
            let so = s.offset as u64;
            let se = so.saturating_add(s.size as u64);
            s.size > 0 && here >= so && here < se
        });
        let Some(section) = section else {
            covered += 1;
            continue;
        };
        let section_end = (section.offset as u64).saturating_add(section.size as u64);
        let span = ((section_end - here) as usize).min(buf.len() - covered);
        match section.crypto_type as u8 {
            ENC_NONE => {}
            ENC_AES_CTR
            | ENC_AES_CTR_EX
            | ENC_AES_CTR_SKIP_LAYER_HASH
            | ENC_AES_CTR_EX_SKIP_LAYER_HASH => {
                let mut counter = section.crypto_counter;
                let block = here / 16;
                counter[8..16].copy_from_slice(&block.to_be_bytes());
                let mut cipher = AesCtr::new_from_slices(&section.crypto_key, &counter)
                    .map_err(|e| NxError::AesError(format!("Ctr128BE init: {e}")))?;
                cipher.apply_keystream(&mut buf[covered..covered + span]);
            }
            other => return Err(NxError::UnsupportedEncryption(other)),
        }
        covered += span;
    }
    Ok(())
}

struct FileSliceReader {
    file: Arc<File>,
    base: u64,
    position: u64,
}

impl Read for FileSliceReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        file_read_exact_at(&self.file, buf, self.base + self.position)?;
        self.position += buf.len() as u64;
        Ok(buf.len())
    }
}

impl Seek for FileSliceReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        match pos {
            SeekFrom::Start(p) => self.position = p,
            SeekFrom::Current(d) => {
                self.position = (self.position as i64 + d) as u64;
            }
            SeekFrom::End(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "FileSliceReader does not know its end",
                ));
            }
        }
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::nx::constants::{NCZBLOCK_MAGIC, NCZSECTN_MAGIC};
    use byteorder::{LE, WriteBytesExt};
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn build_minimal_ncz_with_one_block(
        prefix_byte: u8,
        plaintext_block: &[u8],
        sections: &[NczSectionEntry],
    ) -> Vec<u8> {
        let mut bytes = vec![prefix_byte; NCA_PREFIX_SIZE];
        bytes.write_all(&NCZSECTN_MAGIC).unwrap();
        bytes.write_i64::<LE>(sections.len() as i64).unwrap();
        for s in sections {
            bytes.write_i64::<LE>(s.offset).unwrap();
            bytes.write_i64::<LE>(s.size).unwrap();
            bytes.write_i64::<LE>(s.crypto_type).unwrap();
            bytes.write_i64::<LE>(0).unwrap();
            bytes.write_all(&s.crypto_key).unwrap();
            bytes.write_all(&s.crypto_counter).unwrap();
        }
        let block_size_exp = 14u8;
        let block_size = 1usize << block_size_exp;
        assert!(plaintext_block.len() <= block_size);

        bytes.write_all(&NCZBLOCK_MAGIC).unwrap();
        bytes.write_u8(1).unwrap();
        bytes.write_u8(0).unwrap();
        bytes.write_u8(0).unwrap();
        bytes.write_u8(block_size_exp).unwrap();
        bytes.write_u32::<LE>(1).unwrap();
        bytes.write_i64::<LE>(plaintext_block.len() as i64).unwrap();
        bytes.write_u32::<LE>(plaintext_block.len() as u32).unwrap();

        bytes.extend_from_slice(plaintext_block);
        bytes
    }

    fn open_reader(bytes: &[u8]) -> NczReader {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(bytes).unwrap();
        tmp.flush().unwrap();
        let file = Arc::new(File::open(tmp.path()).unwrap());
        let len = bytes.len() as u64;
        std::mem::forget(tmp);
        NczReader::open(file, 0, len).unwrap()
    }

    #[test]
    fn enc_none_section_passes_through() {
        let plaintext: Vec<u8> = (0..0x200).map(|i| (i & 0xFF) as u8).collect();
        let section = NczSectionEntry {
            offset: NCA_PREFIX_SIZE as i64,
            size: 0x200,
            crypto_type: ENC_NONE as i64,
            crypto_key: [0; 16],
            crypto_counter: [0; 16],
        };
        let ncz = build_minimal_ncz_with_one_block(0xAA, &plaintext, &[section]);
        let reader = open_reader(&ncz);
        let mut got = vec![0u8; 0x200];
        reader
            .read_exact_at(&mut got, NCA_PREFIX_SIZE as u64)
            .unwrap();
        assert_eq!(got, plaintext);
    }

    #[test]
    fn ctr_section_reencrypts_to_expected_ciphertext() {
        let key = [0x55u8; 16];
        let counter = [0u8; 16];
        let plaintext: Vec<u8> = (0..0x100).map(|i| (i ^ 0x3C) as u8).collect();
        let section = NczSectionEntry {
            offset: NCA_PREFIX_SIZE as i64,
            size: 0x100,
            crypto_type: ENC_AES_CTR as i64,
            crypto_key: key,
            crypto_counter: counter,
        };
        let ncz = build_minimal_ncz_with_one_block(0xAA, &plaintext, &[section]);
        let reader = open_reader(&ncz);
        let mut got = vec![0u8; 0x100];
        reader
            .read_exact_at(&mut got, NCA_PREFIX_SIZE as u64)
            .unwrap();

        let mut expected = plaintext.clone();
        let mut counter_filled = counter;
        let block = (NCA_PREFIX_SIZE as u64) / 16;
        counter_filled[8..16].copy_from_slice(&block.to_be_bytes());
        let mut cipher = AesCtr::new_from_slices(&key, &counter_filled).unwrap();
        cipher.apply_keystream(&mut expected);
        assert_eq!(got, expected);
    }

    #[test]
    fn nca_prefix_passes_through() {
        let section = NczSectionEntry {
            offset: NCA_PREFIX_SIZE as i64,
            size: 0x10,
            crypto_type: ENC_NONE as i64,
            crypto_key: [0; 16],
            crypto_counter: [0; 16],
        };
        let ncz = build_minimal_ncz_with_one_block(0xCD, &[0xEE; 0x10], &[section]);
        let reader = open_reader(&ncz);
        let mut got = vec![0u8; 0x40];
        reader.read_exact_at(&mut got, 0).unwrap();
        assert!(got.iter().all(|b| *b == 0xCD));
    }
}
