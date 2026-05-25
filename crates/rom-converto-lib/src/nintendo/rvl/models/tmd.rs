//! Wii Title Metadata (TMD) parser.
//!
//! Layout per wiibrew.org/wiki/Title_metadata. Wii TMDs use RSA-2048
//! SHA-1 signatures (type `0x10001`) so the header starts 0x140 bytes
//! into the file.

use anyhow::{Result, anyhow};
use byteorder::{BE, ReadBytesExt};
use std::io::{Cursor, Read};

pub const WII_TMD_HEADER_OFFSET: usize = 0x140;
pub const WII_TMD_CONTENT_RECORD_SIZE: usize = 0x24;

#[derive(Debug, Clone)]
pub struct WiiTmd {
    pub signature_issuer: String,
    pub version: u8,
    pub ca_crl_version: u8,
    pub signer_crl_version: u8,
    pub system_version: u64,
    pub title_id: u64,
    pub title_type: u32,
    pub group_id: u16,
    pub region: u16,
    pub ratings: [u8; 16],
    pub access_rights: u32,
    pub title_version: u16,
    pub content_count: u16,
    pub boot_index: u16,
    pub contents: Vec<WiiTmdContent>,
}

#[derive(Debug, Clone)]
pub struct WiiTmdContent {
    pub content_id: u32,
    pub index: u16,
    pub content_type: u16,
    pub size: u64,
    pub hash: [u8; 20],
}

impl WiiTmd {
    pub fn parse(buf: &[u8]) -> Result<Self> {
        if buf.len() < WII_TMD_HEADER_OFFSET + 0xA4 {
            return Err(anyhow!("Wii TMD too small ({} bytes)", buf.len()));
        }
        let header = &buf[WII_TMD_HEADER_OFFSET..];

        let signature_issuer = read_c_string(&header[0x00..0x40]);
        let version = header[0x40];
        let ca_crl_version = header[0x41];
        let signer_crl_version = header[0x42];

        let mut cur = Cursor::new(&header[0x44..]);
        let system_version = cur.read_u64::<BE>()?;
        let title_id = cur.read_u64::<BE>()?;
        let title_type = cur.read_u32::<BE>()?;
        let group_id = cur.read_u16::<BE>()?;
        let _padding = cur.read_u16::<BE>()?;
        let region = cur.read_u16::<BE>()?;

        let mut ratings = [0u8; 16];
        cur.read_exact(&mut ratings)?;
        // Skip reserved + IPC mask + reserved (12 + 12 + 18 = 42 bytes).
        let mut skip = [0u8; 42];
        cur.read_exact(&mut skip)?;

        let access_rights = cur.read_u32::<BE>()?;
        let title_version = cur.read_u16::<BE>()?;
        let content_count = cur.read_u16::<BE>()?;
        let boot_index = cur.read_u16::<BE>()?;
        let _minor = cur.read_u16::<BE>()?;

        let content_start = WII_TMD_HEADER_OFFSET + 0xA4;
        let needed = content_start + content_count as usize * WII_TMD_CONTENT_RECORD_SIZE;
        if buf.len() < needed {
            return Err(anyhow!(
                "Wii TMD truncated: need {} bytes for {} content records, have {}",
                needed,
                content_count,
                buf.len()
            ));
        }
        let mut contents = Vec::with_capacity(content_count as usize);
        for i in 0..content_count as usize {
            let base = content_start + i * WII_TMD_CONTENT_RECORD_SIZE;
            let mut crec = Cursor::new(&buf[base..base + WII_TMD_CONTENT_RECORD_SIZE]);
            let content_id = crec.read_u32::<BE>()?;
            let index = crec.read_u16::<BE>()?;
            let content_type = crec.read_u16::<BE>()?;
            let size = crec.read_u64::<BE>()?;
            let mut hash = [0u8; 20];
            crec.read_exact(&mut hash)?;
            contents.push(WiiTmdContent {
                content_id,
                index,
                content_type,
                size,
                hash,
            });
        }

        Ok(Self {
            signature_issuer,
            version,
            ca_crl_version,
            signer_crl_version,
            system_version,
            title_id,
            title_type,
            group_id,
            region,
            ratings,
            access_rights,
            title_version,
            content_count,
            boot_index,
            contents,
        })
    }

    pub fn region_name(&self) -> &'static str {
        match self.region {
            0 => "Japan",
            1 => "USA",
            2 => "PAL",
            3 => "RegionFree",
            4 => "Korea",
            _ => "Unknown",
        }
    }

    /// IOS slot derived from `system_version`. The lower 32 bits hold
    /// the IOS title id; the upper bits are always `0x00000001` for
    /// retail titles.
    pub fn ios_slot(&self) -> Option<u32> {
        let low = (self.system_version & 0xFFFF_FFFF) as u32;
        if low == 0 { None } else { Some(low) }
    }
}

fn read_c_string(buf: &[u8]) -> String {
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    buf[..end].iter().map(|&b| b as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use byteorder::WriteBytesExt;

    fn build_tmd(title_id: u64, title_version: u16, content_count: u16) -> Vec<u8> {
        let header_size = WII_TMD_HEADER_OFFSET + 0xA4;
        let mut buf = vec![0u8; header_size + content_count as usize * WII_TMD_CONTENT_RECORD_SIZE];
        // Signature issuer at 0x140 (just put a string).
        let issuer = b"Root-CA00000001-CP00000004";
        buf[WII_TMD_HEADER_OFFSET..WII_TMD_HEADER_OFFSET + issuer.len()]
            .copy_from_slice(issuer);

        // Version
        buf[WII_TMD_HEADER_OFFSET + 0x40] = 0;
        // system_version at 0x184 = 0x0000000100000023 (IOS 35)
        (&mut buf[WII_TMD_HEADER_OFFSET + 0x44..WII_TMD_HEADER_OFFSET + 0x4C])
            .write_u64::<BE>(0x00000001_00000023)
            .unwrap();
        // title_id at 0x18C
        (&mut buf[WII_TMD_HEADER_OFFSET + 0x4C..WII_TMD_HEADER_OFFSET + 0x54])
            .write_u64::<BE>(title_id)
            .unwrap();
        // region at 0x19C = 1 (USA)
        (&mut buf[WII_TMD_HEADER_OFFSET + 0x5C..WII_TMD_HEADER_OFFSET + 0x5E])
            .write_u16::<BE>(1)
            .unwrap();

        // After consuming system_version/title_id/title_type/group_id/padding/region/
        // ratings/skip/access_rights the relative cursor sits at 88 bytes past 0x44;
        // title_version occupies the next two bytes.
        let title_ver_off = WII_TMD_HEADER_OFFSET + 0x44 + 88;
        (&mut buf[title_ver_off..title_ver_off + 2])
            .write_u16::<BE>(title_version)
            .unwrap();
        let content_count_off = title_ver_off + 2;
        (&mut buf[content_count_off..content_count_off + 2])
            .write_u16::<BE>(content_count)
            .unwrap();

        // Content record at WII_TMD_HEADER_OFFSET + 0xA4
        for i in 0..content_count as usize {
            let base = header_size + i * WII_TMD_CONTENT_RECORD_SIZE;
            (&mut buf[base..base + 4])
                .write_u32::<BE>(i as u32)
                .unwrap();
            (&mut buf[base + 4..base + 6])
                .write_u16::<BE>(i as u16)
                .unwrap();
            (&mut buf[base + 8..base + 16])
                .write_u64::<BE>(0x12345678)
                .unwrap();
        }
        buf
    }

    #[test]
    fn parses_header_and_contents() {
        let buf = build_tmd(0x0001000000010A00, 0x100, 3);
        let t = WiiTmd::parse(&buf).unwrap();
        assert_eq!(t.title_id, 0x0001000000010A00);
        assert_eq!(t.title_version, 0x100);
        assert_eq!(t.content_count, 3);
        assert_eq!(t.contents.len(), 3);
        assert_eq!(t.region, 1);
        assert_eq!(t.region_name(), "USA");
        assert_eq!(t.ios_slot(), Some(35));
    }

    #[test]
    fn rejects_truncated() {
        let buf = vec![0u8; 100];
        assert!(WiiTmd::parse(&buf).is_err());
    }
}
