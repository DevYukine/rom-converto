//! NCA3 header parser. Operates on the 0xC00 plaintext bytes after
//! XTS-decryption (see `crypto::aes_xts`). Field layout follows
//! switchbrew.org/wiki/NCA.

use byteorder::{LE, ReadBytesExt};
use std::io::Cursor;

use crate::nintendo::nx::constants::{
    NCA_FS_ENTRY_OFFSET, NCA_FS_HEADER_OFFSET, NCA_FS_HEADER_STRIDE, NCA_HEADER_SIZE,
    NCA_MAX_SECTIONS, NCA3_MAGIC,
};
use crate::nintendo::nx::crypto::derive::{KEY_AREA_OFFSET, KEY_AREA_TOTAL};
use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::keys::KeyAreaKind;

#[derive(Debug, Clone)]
pub struct NcaHeader {
    pub content_size: u64,
    pub title_id: u64,
    pub content_type: u8,
    pub key_index: u8,
    pub key_generation_old: u8,
    pub key_generation_new: u8,
    pub rights_id: [u8; 16],
    pub fs_entries: [FsEntry; NCA_MAX_SECTIONS],
    pub fs_headers: [FsHeader; NCA_MAX_SECTIONS],
    pub encrypted_key_area: [u8; KEY_AREA_TOTAL],
}

pub const CONTENT_TYPE_PROGRAM: u8 = 0;
pub const CONTENT_TYPE_META: u8 = 1;
pub const CONTENT_TYPE_CONTROL: u8 = 2;
pub const CONTENT_TYPE_MANUAL: u8 = 3;
pub const CONTENT_TYPE_DATA: u8 = 4;
pub const CONTENT_TYPE_PUBLIC_DATA: u8 = 5;

#[derive(Debug, Clone, Copy, Default)]
pub struct FsEntry {
    pub start_sector: u32,
    pub end_sector: u32,
}

impl FsEntry {
    pub fn is_present(&self) -> bool {
        self.end_sector > self.start_sector
    }

    pub fn byte_offset(&self) -> u64 {
        u64::from(self.start_sector) * 0x200
    }

    pub fn byte_size(&self) -> u64 {
        u64::from(self.end_sector - self.start_sector) * 0x200
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FsHeader {
    pub version: u16,
    pub fs_type: u8,
    pub hash_type: u8,
    pub encryption_type: u8,
    pub metadata_hash_type: u8,
    pub section_ctr_low: u32,
    pub section_ctr_high: u32,
}

impl NcaHeader {
    pub fn parse(buf: &[u8; NCA_HEADER_SIZE]) -> NxResult<Self> {
        if buf[0x200..0x204] != NCA3_MAGIC {
            return Err(NxError::InvalidNcaHeader);
        }
        let content_type = buf[0x205];
        let key_index = buf[0x207];
        let content_size = read_u64_at(buf, 0x208);
        let title_id = read_u64_at(buf, 0x210);
        let mut rights_id = [0u8; 16];
        rights_id.copy_from_slice(&buf[0x230..0x240]);

        let key_generation_old = buf[0x206];
        let key_generation_new = buf[0x220];

        let mut fs_entries = [FsEntry::default(); NCA_MAX_SECTIONS];
        for (i, entry) in fs_entries.iter_mut().enumerate() {
            let off = NCA_FS_ENTRY_OFFSET + i * 0x10;
            *entry = FsEntry {
                start_sector: read_u32_at(buf, off),
                end_sector: read_u32_at(buf, off + 4),
            };
        }

        let mut fs_headers = [FsHeader::default(); NCA_MAX_SECTIONS];
        for (i, slot) in fs_headers.iter_mut().enumerate() {
            let off = NCA_FS_HEADER_OFFSET + i * NCA_FS_HEADER_STRIDE;
            *slot = parse_fs_header(&buf[off..off + NCA_FS_HEADER_STRIDE])?;
        }

        let mut encrypted_key_area = [0u8; KEY_AREA_TOTAL];
        encrypted_key_area.copy_from_slice(&buf[KEY_AREA_OFFSET..KEY_AREA_OFFSET + KEY_AREA_TOTAL]);

        Ok(Self {
            content_size,
            title_id,
            content_type,
            key_index,
            key_generation_old,
            key_generation_new,
            rights_id,
            fs_entries,
            fs_headers,
            encrypted_key_area,
        })
    }

    /// Translate the NCA's `key_index` field into the named bucket
    /// the user's `prod.keys` uses for `key_area_key_<kind>_xx`.
    pub fn key_area_kind(&self) -> NxResult<KeyAreaKind> {
        match self.key_index {
            0 => Ok(KeyAreaKind::Application),
            1 => Ok(KeyAreaKind::Ocean),
            2 => Ok(KeyAreaKind::System),
            other => Err(NxError::UnsupportedEncryption(other)),
        }
    }

    /// Master key index used to look up `key_area_key_<kind>_<idx>`
    /// in `prod.keys`. Switch firmware 3.0.0+ moved the wider field to
    /// 0x220; older NCAs zero that slot and store the value at 0x206.
    /// Both indices in the file are 1-based, so we subtract one (the
    /// idx of `master_key_00`).
    pub fn master_key_index(&self) -> u8 {
        let raw = if self.key_generation_new > 2 {
            self.key_generation_new
        } else {
            self.key_generation_old
        };
        raw.saturating_sub(1)
    }
}

fn parse_fs_header(buf: &[u8]) -> NxResult<FsHeader> {
    let mut cur = Cursor::new(buf);
    let version = cur.read_u16::<LE>()?;
    let fs_type = cur.read_u8()?;
    let hash_type = cur.read_u8()?;
    let encryption_type = cur.read_u8()?;
    let metadata_hash_type = cur.read_u8()?;
    let _reserved = cur.read_u16::<LE>()?;
    let section_ctr_low = read_u32_at(buf, 0x140);
    let section_ctr_high = read_u32_at(buf, 0x144);
    Ok(FsHeader {
        version,
        fs_type,
        hash_type,
        encryption_type,
        metadata_hash_type,
        section_ctr_low,
        section_ctr_high,
    })
}

fn read_u32_at(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

fn read_u64_at(buf: &[u8], off: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buf[off..off + 8]);
    u64::from_le_bytes(bytes)
}

/// Build the 16-byte initial CTR for an FsHeader at a given byte offset
/// inside the NCA. Layout is `section_ctr_high BE || section_ctr_low BE
/// || (offset / 16) BE`, matching nsz/hactool.
pub fn initial_ctr_for_offset(fs: &FsHeader, nca_offset: u64) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&fs.section_ctr_high.to_be_bytes());
    out[4..8].copy_from_slice(&fs.section_ctr_low.to_be_bytes());
    out[8..16].copy_from_slice(&(nca_offset / 16).to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctr_layout_high_low_then_offset() {
        let fs = FsHeader {
            section_ctr_low: 0x11223344,
            section_ctr_high: 0xAABBCCDD,
            ..Default::default()
        };
        let c = initial_ctr_for_offset(&fs, 0x1234560);
        assert_eq!(&c[0..4], &[0xAA, 0xBB, 0xCC, 0xDD]);
        assert_eq!(&c[4..8], &[0x11, 0x22, 0x33, 0x44]);
        let blocks = 0x1234560u64 / 16;
        assert_eq!(&c[8..16], &blocks.to_be_bytes());
    }
}
