//! `NcaWalker` opens an NCA inside a container, XTS-decrypts the
//! header once, derives every section's plaintext AES-128 key, and
//! exposes positional reads that yield decrypted section bytes.
//!
//! The decompressor cares about three section flavours:
//!   - `EncNone` (1) bytes pass through untouched.
//!   - `AesCtr` (3) standard CTR with `initial_ctr_for_offset`.
//!   - `AesCtrEx` (4) BKTR; treated identically to `AesCtr` here,
//!     since per-block IV variation is the patch layer's concern,
//!     not ours.
//!
//! `AesXts` (2) only appears on legacy game-update NCAs and is not yet
//! plumbed through; surfaces as `UnsupportedEncryption(2)`.

use std::fs::File;
use std::sync::Arc;

use aes::Aes128;
use aes::cipher::array::Array;
use aes::cipher::{BlockCipherDecrypt, KeyInit};

use crate::nintendo::nx::constants::{
    ENC_AES_CTR, ENC_AES_CTR_EX, ENC_AES_CTR_EX_SKIP_LAYER_HASH, ENC_AES_CTR_SKIP_LAYER_HASH,
    ENC_NONE, NCA_HEADER_SIZE,
};
use crate::nintendo::nx::crypto::aes_ctr::apply_ctr;
use crate::nintendo::nx::crypto::aes_xts::decrypt_nca_header;
use crate::nintendo::nx::crypto::derive::{body_key, decrypt_key_area};
use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::keys::KeySet;
use crate::nintendo::nx::models::nca::{NcaHeader, initial_ctr_for_offset};
use crate::util::pread::file_read_exact_at;

pub trait NcaInput: Send + Sync {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> NxResult<()>;
}

impl NcaInput for File {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> NxResult<()> {
        Ok(file_read_exact_at(self, buf, offset)?)
    }
}

#[derive(Debug, Clone)]
pub struct NcaSection {
    pub index: usize,
    pub raw_offset: u64,
    pub raw_size: u64,
    pub encryption_type: u8,
    pub key: [u8; 16],
    pub section_ctr_low: u32,
    pub section_ctr_high: u32,
}

pub struct NcaWalker {
    file: Arc<dyn NcaInput>,
    nca_offset: u64,
    nca_size: u64,
    pub header: NcaHeader,
    pub decrypted_header: Box<[u8; NCA_HEADER_SIZE]>,
    pub sections: Vec<NcaSection>,
}

impl NcaWalker {
    pub fn open(
        file: Arc<dyn NcaInput>,
        nca_offset: u64,
        nca_size: u64,
        keys: &KeySet,
    ) -> NxResult<Self> {
        let mut header_buf = Box::new([0u8; NCA_HEADER_SIZE]);
        file.read_exact_at(header_buf.as_mut_slice(), nca_offset)?;
        let header_key = keys.header_key()?;
        decrypt_nca_header(&mut header_buf, header_key)?;
        let header = NcaHeader::parse(&header_buf)?;

        let key_area_kind = header.key_area_kind()?;
        let master_idx = header.master_key_index();
        let body = if header.rights_id.iter().any(|b| *b != 0) {
            // Ticket-protected NCA: ignore the encrypted_key_area and
            // pull the titlekey from the matching .tik. The encrypted
            // titlekey is decrypted with `titlekek_<master_idx>` via
            // a single AES-128-ECB block.
            let encrypted_title_key = keys.title_key(&header.rights_id)?;
            let titlekek = keys.titlekek(master_idx)?;
            let cipher = Aes128::new_from_slice(titlekek)
                .map_err(|e| NxError::AesError(format!("titlekek init: {e}")))?;
            let mut block = Array::try_from(&encrypted_title_key[..])
                .expect("encrypted title key is one AES block");
            cipher.decrypt_block(&mut block);
            let mut out = [0u8; 16];
            out.copy_from_slice(block.as_slice());
            out
        } else {
            let key_area =
                decrypt_key_area(&header.encrypted_key_area, key_area_kind, master_idx, keys)?;
            body_key(&key_area)
        };

        let sections: Vec<NcaSection> = header
            .fs_entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_present())
            .map(|(i, entry)| {
                let fs = header.fs_headers[i];
                NcaSection {
                    index: i,
                    raw_offset: nca_offset + entry.byte_offset(),
                    raw_size: entry.byte_size(),
                    encryption_type: fs.encryption_type,
                    key: body,
                    section_ctr_low: fs.section_ctr_low,
                    section_ctr_high: fs.section_ctr_high,
                }
            })
            .collect();

        Ok(Self {
            file,
            nca_offset,
            nca_size,
            header,
            decrypted_header: header_buf,
            sections,
        })
    }

    pub fn nca_offset(&self) -> u64 {
        self.nca_offset
    }

    pub fn nca_size(&self) -> u64 {
        self.nca_size
    }

    pub fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> NxResult<()> {
        self.file.read_exact_at(buf, offset)
    }

    /// 16-byte alignment is required on both `offset_in_section` and
    /// `buf.len()` so the AES-CTR keystream picks up at the right
    /// counter value.
    pub fn read_section_plain(
        &self,
        section: &NcaSection,
        offset_in_section: u64,
        buf: &mut [u8],
    ) -> NxResult<()> {
        debug_assert_eq!(offset_in_section % 16, 0, "CTR offset must be 16-aligned");
        debug_assert_eq!(buf.len() % 16, 0, "CTR length must be 16-aligned");
        let abs = section.raw_offset + offset_in_section;
        self.file.read_exact_at(buf, abs)?;
        match section.encryption_type {
            ENC_NONE => Ok(()),
            ENC_AES_CTR
            | ENC_AES_CTR_EX
            | ENC_AES_CTR_SKIP_LAYER_HASH
            | ENC_AES_CTR_EX_SKIP_LAYER_HASH => {
                let fs_synth = crate::nintendo::nx::models::nca::FsHeader {
                    section_ctr_low: section.section_ctr_low,
                    section_ctr_high: section.section_ctr_high,
                    ..Default::default()
                };
                let counter = initial_ctr_for_offset(&fs_synth, abs - self.nca_offset);
                apply_ctr(&section.key, &counter, buf)?;
                Ok(())
            }
            other => Err(NxError::UnsupportedEncryption(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::nx::constants::{NCA_FS_ENTRY_OFFSET, NCA_FS_HEADER_OFFSET, NCA3_MAGIC};
    use crate::nintendo::nx::crypto::aes_ctr::apply_ctr;
    use crate::nintendo::nx::crypto::aes_xts::encrypt_nca_header;
    use crate::nintendo::nx::keys::KeyAreaKind;
    use crate::nintendo::nx::test_fixtures::{
        TEST_BODY_KEY, encrypt_key_area_block, synthetic_keyset,
    };
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn build_synthetic_nca(plaintext_section: &[u8]) -> Vec<u8> {
        let mut header = [0u8; NCA_HEADER_SIZE];
        header[0x200..0x204].copy_from_slice(&NCA3_MAGIC);
        header[0x207] = 0; // key_index = Application
        header[0x220] = 1; // key_generation_new (master_key_00)

        // Section 0: starts at 0x4000 sectors=>byte 0x800000? Too big.
        // Use 0x4000 bytes from start of NCA (sector 0..N).
        let section_start_byte = 0x4000u64;
        let section_size = plaintext_section.len() as u64;
        let section_end_byte = section_start_byte + section_size;
        let start_sector = (section_start_byte / 0x200) as u32;
        let end_sector = (section_end_byte / 0x200) as u32;

        let entry_off = NCA_FS_ENTRY_OFFSET;
        header[entry_off..entry_off + 4].copy_from_slice(&start_sector.to_le_bytes());
        header[entry_off + 4..entry_off + 8].copy_from_slice(&end_sector.to_le_bytes());

        let fs0_off = NCA_FS_HEADER_OFFSET;
        header[fs0_off + 4] = ENC_AES_CTR;
        let ctr_low: u32 = 0;
        let ctr_high: u32 = 0;
        header[fs0_off + 0x140..fs0_off + 0x144].copy_from_slice(&ctr_low.to_le_bytes());
        header[fs0_off + 0x144..fs0_off + 0x148].copy_from_slice(&ctr_high.to_le_bytes());

        let key_area = encrypt_key_area_block([[0x11; 16], [0x22; 16], TEST_BODY_KEY, [0x44; 16]]);
        header[0x300..0x340].copy_from_slice(&key_area);

        let keys = synthetic_keyset();
        let header_key = keys.header_key().unwrap();
        encrypt_nca_header(&mut header, header_key).unwrap();

        let mut nca = vec![0u8; section_start_byte as usize];
        nca[..NCA_HEADER_SIZE].copy_from_slice(&header);

        let mut encrypted = plaintext_section.to_vec();
        let counter = initial_ctr_for_offset(
            &crate::nintendo::nx::models::nca::FsHeader {
                section_ctr_low: ctr_low,
                section_ctr_high: ctr_high,
                ..Default::default()
            },
            section_start_byte,
        );
        apply_ctr(&TEST_BODY_KEY, &counter, &mut encrypted).unwrap();
        nca.extend_from_slice(&encrypted);
        nca
    }

    #[test]
    fn walker_decrypts_synthetic_section() {
        let plaintext: Vec<u8> = (0..0x200).map(|i| (i & 0xFF) as u8).collect();
        let nca_bytes = build_synthetic_nca(&plaintext);

        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(&nca_bytes).unwrap();
        tmp.flush().unwrap();

        let file = Arc::new(File::open(tmp.path()).unwrap());
        let keys = synthetic_keyset();
        let walker = NcaWalker::open(file, 0, nca_bytes.len() as u64, &keys).unwrap();

        assert_eq!(walker.sections.len(), 1);
        assert_eq!(walker.header.key_index, 0);
        assert_eq!(
            walker.header.key_area_kind().unwrap(),
            KeyAreaKind::Application
        );
        assert_eq!(walker.header.master_key_index(), 0);

        let section = &walker.sections[0];
        let mut decrypted = vec![0u8; plaintext.len()];
        walker
            .read_section_plain(section, 0, &mut decrypted)
            .unwrap();
        assert_eq!(decrypted, plaintext);
    }
}
