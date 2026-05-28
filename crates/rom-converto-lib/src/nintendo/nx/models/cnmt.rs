//! Parser for the Switch `.cnmt` (PackagedContentMeta) binary that lives
//! inside the Meta NCA's section 0. Layout per Switchbrew NCM service.

use crate::nintendo::nx::error::{NxError, NxResult};
use byteorder::{LE, ReadBytesExt};
use std::io::{Cursor, Read};

pub const CNMT_TYPE_SYSTEM_PROGRAM: u8 = 0x01;
pub const CNMT_TYPE_SYSTEM_DATA: u8 = 0x02;
pub const CNMT_TYPE_SYSTEM_UPDATE: u8 = 0x03;
pub const CNMT_TYPE_BOOT_IMAGE_PACKAGE: u8 = 0x04;
pub const CNMT_TYPE_BOOT_IMAGE_PACKAGE_SAFE: u8 = 0x05;
pub const CNMT_TYPE_APPLICATION: u8 = 0x80;
pub const CNMT_TYPE_PATCH: u8 = 0x81;
pub const CNMT_TYPE_ADD_ON_CONTENT: u8 = 0x82;
pub const CNMT_TYPE_DELTA: u8 = 0x83;

#[derive(Debug, Clone)]
pub struct Cnmt {
    pub title_id: u64,
    pub version: u32,
    pub content_type: u8,
    pub extended_header_size: u16,
    pub content_count: u16,
    pub content_meta_count: u16,
    pub attributes: u8,
    pub storage_id: u8,
    pub install_type: u8,
    /// For non-Application/Patch types this is the
    /// `RequiredDownloadSystemVersion`. For Application/Patch the
    /// real `RequiredSystemVersion` lives in the extended header.
    pub required_download_system_version: u64,
    pub extended_header: Vec<u8>,
    pub contents: Vec<CnmtContent>,
}

#[derive(Debug, Clone)]
pub struct CnmtContent {
    pub hash: [u8; 32],
    pub content_id: [u8; 16],
    pub size: u64,
    pub content_type: u8,
    pub id_offset: u8,
}

pub const CNMT_CONTENT_TYPE_META: u8 = 0;
pub const CNMT_CONTENT_TYPE_PROGRAM: u8 = 1;
pub const CNMT_CONTENT_TYPE_DATA: u8 = 2;
pub const CNMT_CONTENT_TYPE_CONTROL: u8 = 3;
pub const CNMT_CONTENT_TYPE_HTML_DOCUMENT: u8 = 4;
pub const CNMT_CONTENT_TYPE_LEGAL_INFORMATION: u8 = 5;
pub const CNMT_CONTENT_TYPE_DELTA_FRAGMENT: u8 = 6;

impl Cnmt {
    pub fn parse(buf: &[u8]) -> NxResult<Self> {
        if buf.len() < 0x20 {
            return Err(NxError::InvalidNcaHeader);
        }
        let mut cur = Cursor::new(buf);
        let title_id = cur.read_u64::<LE>()?;
        let version = cur.read_u32::<LE>()?;
        let content_type = cur.read_u8()?;
        let _reserved1 = cur.read_u8()?;
        let extended_header_size = cur.read_u16::<LE>()?;
        let content_count = cur.read_u16::<LE>()?;
        let content_meta_count = cur.read_u16::<LE>()?;
        let attributes = cur.read_u8()?;
        let storage_id = cur.read_u8()?;
        let install_type = cur.read_u8()?;
        let _reserved2 = cur.read_u8()?;
        let required_download_system_version = cur.read_u64::<LE>()?;

        let mut extended_header = vec![0u8; extended_header_size as usize];
        if extended_header_size > 0 {
            cur.read_exact(&mut extended_header)?;
        }

        let mut contents = Vec::with_capacity(content_count as usize);
        for _ in 0..content_count {
            let mut hash = [0u8; 32];
            cur.read_exact(&mut hash)?;
            let mut content_id = [0u8; 16];
            cur.read_exact(&mut content_id)?;
            let mut size_bytes = [0u8; 6];
            cur.read_exact(&mut size_bytes)?;
            let size = u64::from(size_bytes[0])
                | (u64::from(size_bytes[1]) << 8)
                | (u64::from(size_bytes[2]) << 16)
                | (u64::from(size_bytes[3]) << 24)
                | (u64::from(size_bytes[4]) << 32)
                | (u64::from(size_bytes[5]) << 40);
            let content_type = cur.read_u8()?;
            let id_offset = cur.read_u8()?;
            contents.push(CnmtContent {
                hash,
                content_id,
                size,
                content_type,
                id_offset,
            });
        }

        Ok(Self {
            title_id,
            version,
            content_type,
            extended_header_size,
            content_count,
            content_meta_count,
            attributes,
            storage_id,
            install_type,
            required_download_system_version,
            extended_header,
            contents,
        })
    }

    /// Extract `RequiredSystemVersion` for Application / Patch (lives in
    /// the extended header at offset 0). Returns
    /// [`Self::required_download_system_version`] otherwise.
    pub fn required_system_version(&self) -> u64 {
        match self.content_type {
            CNMT_TYPE_APPLICATION => {
                if self.extended_header.len() >= 0x10 {
                    let mut bytes = [0u8; 8];
                    bytes.copy_from_slice(&self.extended_header[0x08..0x10]);
                    return u64::from_le_bytes(bytes);
                }
                self.required_download_system_version
            }
            CNMT_TYPE_PATCH => {
                if self.extended_header.len() >= 0x10 {
                    let mut bytes = [0u8; 8];
                    bytes.copy_from_slice(&self.extended_header[0x08..0x10]);
                    return u64::from_le_bytes(bytes);
                }
                self.required_download_system_version
            }
            _ => self.required_download_system_version,
        }
    }

    /// Parent Application title id from the Patch / AddOnContent
    /// extended header. `None` for any other content type.
    pub fn base_application_id(&self) -> Option<u64> {
        match self.content_type {
            CNMT_TYPE_PATCH | CNMT_TYPE_ADD_ON_CONTENT => {
                if self.extended_header.len() >= 8 {
                    let mut bytes = [0u8; 8];
                    bytes.copy_from_slice(&self.extended_header[0..8]);
                    Some(u64::from_le_bytes(bytes))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn required_application_version(&self) -> Option<u64> {
        if self.content_type == CNMT_TYPE_ADD_ON_CONTENT && self.extended_header.len() >= 0x0C {
            let mut bytes = [0u8; 4];
            bytes.copy_from_slice(&self.extended_header[0x08..0x0C]);
            return Some(u64::from(u32::from_le_bytes(bytes)));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_minimal_cnmt(ty: u8, content_count: u16) -> Vec<u8> {
        let mut buf = vec![0u8; 0x20 + content_count as usize * 0x38];
        buf[0..8].copy_from_slice(&0x0100ABCDEF000000u64.to_le_bytes());
        buf[8..12].copy_from_slice(&0x10000u32.to_le_bytes());
        buf[12] = ty;
        buf[14..16].copy_from_slice(&0u16.to_le_bytes());
        buf[16..18].copy_from_slice(&content_count.to_le_bytes());
        buf[18..20].copy_from_slice(&0u16.to_le_bytes());
        buf[20] = 0;
        buf[21] = 1;
        buf[22] = 0;
        buf[24..32].copy_from_slice(&0x0000000000010000u64.to_le_bytes());
        buf
    }

    #[test]
    fn parses_header_fields() {
        let buf = build_minimal_cnmt(CNMT_TYPE_APPLICATION, 2);
        let c = Cnmt::parse(&buf).unwrap();
        assert_eq!(c.title_id, 0x0100ABCDEF000000);
        assert_eq!(c.version, 0x10000);
        assert_eq!(c.content_type, CNMT_TYPE_APPLICATION);
        assert_eq!(c.content_count, 2);
        assert_eq!(c.storage_id, 1);
        assert_eq!(c.required_download_system_version, 0x0000000000010000);
        assert_eq!(c.contents.len(), 2);
    }

    #[test]
    fn rejects_truncated() {
        assert!(Cnmt::parse(&[0u8; 4]).is_err());
    }
}
