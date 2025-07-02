use crate::nintendo::ctr::models::signature::SignatureData;
use binrw::{BinRead, BinWrite};

/// Title metadata is a format used to store information about a title (installed title, DLC, etc.) and all its installed contents, including which contents they consist of and their SHA256 hashes.
#[derive(Debug, Clone, BinRead, BinWrite)]
#[brw(big)]
pub struct TitleMetadata {
    /// Signature Data, The hash for the signature is calculated over the Title Metadata Data.
    pub signature_data: SignatureData,

    /// Title Metadata Header
    pub header: TitleMetadataHeader,

    /// Content Info Records; there are 64 of these records, usually only the first is used.
    #[br(count = 64)]
    pub content_info_records: Vec<ContentInfoRecord>,

    /// Content Chunk Records; there is one of these for each content contained in this title. (Determined by "Content Count" in the TMD Header).
    #[br(count = header.content_count)]
    pub content_chunk_records: Vec<ContentChunkRecord>,
}

#[derive(Debug, Clone, BinRead, BinWrite)]
#[brw(big)]
pub struct TitleMetadataHeader {
    /// Signature Issuer
    #[br(count = 0x40)]
    pub signature_issuer: Vec<u8>,

    /// Version
    pub version: u8,

    /// CaCrlVersion
    pub ca_crl_version: u8,

    /// signer_crl_version
    pub signer_crl_version: u8,

    /// Reserved
    pub reserved1: u8,

    /// System Version
    pub system_version: u64,

    /// Title ID
    pub title_id: u64,

    /// Title Type
    pub title_type: u32,

    /// Group ID
    pub group_id: u16,

    /// Save Data Size in Little Endian (Bytes) (Also SRL Public Save Data Size)
    #[brw(little)]
    pub save_data_size: u32,

    /// SRL Private Save Data Size in Little Endian (Bytes)
    #[brw(little)]
    pub srl_private_save_data_size: u32,

    /// Reserved
    pub reserved2: u32,

    /// SRL Flag
    pub srl_flag: u8,

    /// Reserved
    #[br(count = 0x31)]
    pub reserved3: Vec<u8>,

    /// Access Rights
    pub access_rights: u32,

    /// Title Version
    pub title_version: u16,

    /// Content Count
    pub content_count: u16,

    /// Boot Content
    pub boot_content: u16,

    /// Padding
    pub padding: u16,

    /// SHA-256 Hash of the Content Info Records
    #[br(count = 0x20)]
    pub content_info_records_hash: Vec<u8>,
}

#[derive(Debug, Clone, BinRead, BinWrite)]
#[brw(big)]
pub struct ContentInfoRecord {
    /// Content index offset
    pub content_index_offset: u16,

    /// Content command count [k]
    pub content_command_count: u16,

    /// SHA-256 hash of the next k content records that have not been hashed yet
    #[br(count = 0x20)]
    pub hash: Vec<u8>,
}

#[derive(Debug, Clone, BinRead, BinWrite)]
#[brw(big)]
pub struct ContentChunkRecord {
    /// Content id
    pub content_id: u32,

    /// Content index
    pub content_index: u16,

    /// Content type
    pub content_type: ContentType,

    /// Content size
    pub content_size: u64,

    /// SHA-256 hash
    #[br(count = 0x20)]
    pub hash: Vec<u8>,
}

/// Flags for the content chunk
#[derive(Debug, Clone, Copy, BinRead, BinWrite)]
#[brw(big)]
pub struct ContentType(pub u16);

#[allow(dead_code)]
impl ContentType {
    pub const ENCRYPTED: u16 = 0x0001;
    pub const DISC: u16 = 0x0002;
    pub const CFM: u16 = 0x0004;
    pub const OPTIONAL: u16 = 0x4000;
    pub const SHARED: u16 = 0x8000;

    pub fn is_encrypted(&self) -> bool {
        self.0 & Self::ENCRYPTED != 0
    }

    pub fn is_disc(&self) -> bool {
        self.0 & Self::DISC != 0
    }

    pub fn is_optional(&self) -> bool {
        self.0 & Self::OPTIONAL != 0
    }

    pub fn is_shared(&self) -> bool {
        self.0 & Self::SHARED != 0
    }

    pub fn set_encrypted(&mut self, encrypted: bool) {
        if encrypted {
            self.0 |= Self::ENCRYPTED;
        } else {
            self.0 &= !Self::ENCRYPTED;
        }
    }

    pub fn set_disc(&mut self, disc: bool) {
        if disc {
            self.0 |= Self::DISC;
        } else {
            self.0 &= !Self::DISC;
        }
    }

    pub fn set_optional(&mut self, optional: bool) {
        if optional {
            self.0 |= Self::OPTIONAL;
        } else {
            self.0 &= !Self::OPTIONAL;
        }
    }

    pub fn set_shared(&mut self, shared: bool) {
        if shared {
            self.0 |= Self::SHARED;
        } else {
            self.0 &= !Self::SHARED;
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::nintendo::ctr::models::signature::SignatureType;
    use binrw::BinWrite;
    use std::io::Cursor;

    #[test]
    fn test_tmd_header() {
        let header = TitleMetadataHeader {
            signature_issuer: vec![0x00; 0x40],
            version: 1,
            ca_crl_version: 0,
            signer_crl_version: 0,
            reserved1: 0,
            system_version: 0,
            title_id: 0x0004000000030000,
            title_type: 0x00040010,
            group_id: 0,
            save_data_size: 0x00080000,
            srl_private_save_data_size: 0,
            reserved2: 0,
            srl_flag: 0,
            reserved3: vec![0x00; 0x31],
            access_rights: 0,
            title_version: 0x0100,
            content_count: 1,
            boot_content: 0,
            padding: 0,
            content_info_records_hash: vec![0x00; 0x20],
        };

        let mut buf = Vec::new();
        header.write(&mut Cursor::new(&mut buf)).unwrap();

        let mut cursor = Cursor::new(&buf);
        let read_header = TitleMetadataHeader::read(&mut cursor).unwrap();
        assert_eq!(header.version, read_header.version);
        assert_eq!(header.title_id, read_header.title_id);
        assert_eq!(header.save_data_size, read_header.save_data_size);
        assert_eq!(header.content_count, read_header.content_count);
    }

    #[test]
    fn test_content_chunk_record() {
        let record = ContentChunkRecord {
            content_id: 0,
            content_index: 0,
            content_type: ContentType(0x0001), // Encrypted
            content_size: 0x00400000,
            hash: vec![0xAB; 0x20],
        };

        let mut buf = Vec::new();
        record.write(&mut Cursor::new(&mut buf)).unwrap();

        let mut cursor = Cursor::new(&buf);
        let read_record = ContentChunkRecord::read(&mut cursor).unwrap();
        assert_eq!(record.content_id, read_record.content_id);
        assert_eq!(record.content_type.0, read_record.content_type.0);
        assert!(read_record.content_type.is_encrypted());
        assert!(!read_record.content_type.is_disc());
        assert!(!read_record.content_type.is_optional());
        assert!(!read_record.content_type.is_shared());
        assert_eq!(record.content_size, read_record.content_size);
    }

    #[test]
    fn test_full_tmd() {
        let tmd = TitleMetadata {
            signature_data: SignatureData {
                signature_type: SignatureType::Rsa2048Sha256,
                signature: vec![0xBB; 0x100],
                padding: vec![0x00; 0x3C],
            },
            header: TitleMetadataHeader {
                signature_issuer: vec![0x00; 0x40],
                version: 1,
                ca_crl_version: 0,
                signer_crl_version: 0,
                reserved1: 0,
                system_version: 0,
                title_id: 0x0004000000030000,
                title_type: 0x00040010,
                group_id: 0,
                save_data_size: 0x00080000,
                srl_private_save_data_size: 0,
                reserved2: 0,
                srl_flag: 0,
                reserved3: vec![0x00; 0x31],
                access_rights: 0,
                title_version: 0x0100,
                content_count: 2,
                boot_content: 0,
                padding: 0,
                content_info_records_hash: vec![0x00; 0x20],
            },
            content_info_records: vec![
                ContentInfoRecord {
                    content_index_offset: 0,
                    content_command_count: 2,
                    hash: vec![0x00; 0x20],
                };
                64
            ],
            content_chunk_records: vec![
                ContentChunkRecord {
                    content_id: 0,
                    content_index: 0,
                    content_type: ContentType(0x0001),
                    content_size: 0x00400000,
                    hash: vec![0xAB; 0x20],
                },
                ContentChunkRecord {
                    content_id: 1,
                    content_index: 1,
                    content_type: ContentType(0x0001),
                    content_size: 0x00080000,
                    hash: vec![0xCD; 0x20],
                },
            ],
        };

        let mut buf = Vec::new();
        tmd.write(&mut Cursor::new(&mut buf)).unwrap();

        let mut cursor = Cursor::new(&buf);
        let read_tmd = TitleMetadata::read(&mut cursor).unwrap();
        assert_eq!(tmd.header.content_count, read_tmd.header.content_count);
        assert_eq!(
            tmd.content_chunk_records.len(),
            read_tmd.content_chunk_records.len()
        );
        assert_eq!(
            tmd.content_chunk_records[0].content_id,
            read_tmd.content_chunk_records[0].content_id
        );
        assert_eq!(
            tmd.content_chunk_records[1].content_id,
            read_tmd.content_chunk_records[1].content_id
        );
    }
}
