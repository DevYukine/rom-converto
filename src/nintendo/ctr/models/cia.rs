use crate::nintendo::ctr::models::certificate::Certificate;
use crate::nintendo::ctr::models::ticket::Ticket;
use crate::nintendo::ctr::models::title_metadata::TitleMetadata;
use crate::nintendo::ctr::util::{align_64, pad_to_align_64};
use binrw::{BinRead, BinResult, BinWrite, Endian};
use std::io::{Read, Seek, SeekFrom, Write};

pub const CIA_HEADER_SIZE: u32 = 0x2020;

#[derive(Debug, Clone, BinRead, BinWrite)]
#[brw(little)]
pub struct CiaHeader {
    pub header_size: u32,
    pub cia_type: u16,
    pub version: u16,
    pub cert_chain_size: u32,
    pub ticket_size: u32,
    pub tmd_size: u32,
    pub meta_size: u32,
    pub content_size: u64,
    #[br(count = 0x2000)]
    pub content_index: Vec<u8>,
}

impl CiaHeader {
    pub fn set_content_index(&mut self, content_index: usize) {
        let byte_index = content_index / 8;
        let bit_index = 7 - (content_index % 8);
        if byte_index < self.content_index.len() {
            self.content_index[byte_index] |= 1 << bit_index;
        }
    }
}

#[derive(Debug, Clone, BinRead, BinWrite)]
#[brw(little)]
pub struct MetaData {
    #[br(count = 0x180)]
    pub dependency_list: Vec<u8>,
    #[br(count = 0x180)]
    pub reserved1: Vec<u8>,
    pub core_version: u32,
    #[br(count = 0xFC)]
    pub reserved2: Vec<u8>,
    #[br(count = 0x36C0)]
    pub icon_data: Vec<u8>,
}

/// Complete CIA file structure
#[derive(Debug, Clone)]
pub struct CiaFile {
    pub header: CiaHeader,
    pub cert_chain: Vec<Certificate>,
    pub ticket: Ticket,
    pub tmd: TitleMetadata,
    pub content_data: Vec<u8>,
    pub meta_data: Option<MetaData>,
}

/// CIA file structure without content and metadata
#[derive(Debug, Clone)]
pub struct CiaFileWithoutContent {
    pub header: CiaHeader,
    pub cert_chain: Vec<Certificate>,
    pub ticket: Ticket,
    pub tmd: TitleMetadata,
}

impl CiaFile {
    pub fn apply_content_indexes(&mut self) {
        for (i, _) in self.tmd.content_chunk_records.iter().enumerate() {
            self.header.set_content_index(i);
        }
    }
}

impl BinRead for CiaFileWithoutContent {
    type Args<'a> = ();

    fn read_options<R: Read + Seek>(
        reader: &mut R,
        _endian: Endian,
        _args: Self::Args<'_>,
    ) -> BinResult<Self> {
        // Read header (little-endian)
        let header = CiaHeader::read_options(reader, Endian::Little, ())?;

        let header_end = reader.stream_position()?; // Header size is 0x2020, but we align to 64 bytes

        reader.seek(SeekFrom::Start(align_64(header_end)))?;

        let cert_start = reader.stream_position()?;
        let cert_end = cert_start + header.cert_chain_size as u64;

        let mut cert_chain = Vec::new();

        // Read certificates until we hit padding or reach the end
        while reader.stream_position()? < cert_end {
            // Peek at the next 4 bytes to check if it's a valid signature type
            let current_pos = reader.stream_position()?;
            let mut sig_type_bytes = [0u8; 4];
            reader.read_exact(&mut sig_type_bytes)?;
            reader.seek(SeekFrom::Start(current_pos))?;

            // Check if this looks like a valid signature type (big-endian u32)
            let sig_type_value = u32::from_be_bytes(sig_type_bytes);

            // Valid signature types are: 0x010000, 0x010001, 0x010002, 0x010003, 0x010004, 0x010005
            let is_valid_sig_type = matches!(sig_type_value, 0x010000..=0x010005);

            if !is_valid_sig_type {
                // We've hit padding, stop reading certificates
                break;
            }

            cert_chain.push(Certificate::read_options(reader, Endian::Big, ())?);
        }

        // Skip to the end of the certificate chain section
        reader.seek(SeekFrom::Start(cert_end))?;

        // Read ticket (big-endian, aligned to 64 bytes)
        reader.seek(SeekFrom::Start(align_64(cert_end)))?;
        let ticket = Ticket::read_options(reader, Endian::Big, ())?;

        // Read TMD (big-endian, aligned to 64 bytes)
        let tmd_start = align_64(reader.stream_position()?);
        reader.seek(SeekFrom::Start(tmd_start))?;
        let tmd = TitleMetadata::read_options(reader, Endian::Big, ())?;

        Ok(CiaFileWithoutContent {
            header,
            cert_chain,
            ticket,
            tmd,
        })
    }
}

impl BinWrite for CiaFileWithoutContent {
    type Args<'a> = ();

    fn write_options<W: Write + Seek>(
        &self,
        writer: &mut W,
        _endian: Endian,
        _args: Self::Args<'_>,
    ) -> BinResult<()> {
        // Write header (little-endian)
        self.header.write_options(writer, Endian::Little, ())?;

        let header_end = writer.stream_position()?;
        let cert_start = align_64(header_end);

        if cert_start > header_end {
            // Write padding to align to 64 bytes
            let padding_size = (cert_start - header_end) as usize;
            writer.write_all(&vec![0u8; padding_size])?;
        }

        // Write certificate chain (big-endian)
        for cert in &self.cert_chain {
            cert.write_options(writer, Endian::Big, ())?;
        }

        // IMPORTANT: Pad the certificate chain to match cert_chain_size
        let cert_written = writer.stream_position()? - cert_start;
        if cert_written < self.header.cert_chain_size as u64 {
            let padding_needed = self.header.cert_chain_size as u64 - cert_written;
            writer.write_all(&vec![0u8; padding_needed as usize])?;
        }

        // Write ticket (big-endian, aligned to 64 bytes)
        let ticket_start = align_64(writer.stream_position()?);
        pad_to_align_64(ticket_start, writer)?;
        self.ticket.write_options(writer, Endian::Big, ())?;

        // Write TMD (big-endian, aligned to 64 bytes)
        let tmd_start = align_64(writer.stream_position()?);
        pad_to_align_64(tmd_start, writer)?;
        self.tmd.write_options(writer, Endian::Big, ())?;

        // Align for content data
        let content_start = align_64(writer.stream_position()?);
        pad_to_align_64(content_start, writer)?;

        Ok(())
    }
}

impl BinRead for CiaFile {
    type Args<'a> = ();

    fn read_options<R: Read + Seek>(
        reader: &mut R,
        _endian: Endian,
        _args: Self::Args<'_>,
    ) -> BinResult<Self> {
        // Read header (little-endian)
        let header = CiaHeader::read_options(reader, Endian::Little, ())?;

        let header_end = reader.stream_position()?; // Header size is 0x2020, but we align to 64 bytes

        reader.seek(SeekFrom::Start(align_64(header_end)))?;

        let cert_start = reader.stream_position()?;
        let cert_end = cert_start + header.cert_chain_size as u64;

        let mut cert_chain = Vec::new();

        // Read certificates until we hit padding or reach the end
        while reader.stream_position()? < cert_end {
            // Peek at the next 4 bytes to check if it's a valid signature type
            let current_pos = reader.stream_position()?;
            let mut sig_type_bytes = [0u8; 4];
            reader.read_exact(&mut sig_type_bytes)?;
            reader.seek(SeekFrom::Start(current_pos))?;

            // Check if this looks like a valid signature type (big-endian u32)
            let sig_type_value = u32::from_be_bytes(sig_type_bytes);

            // Valid signature types are: 0x010000, 0x010001, 0x010002, 0x010003, 0x010004, 0x010005
            let is_valid_sig_type = matches!(sig_type_value, 0x010000..=0x010005);

            if !is_valid_sig_type {
                // We've hit padding, stop reading certificates
                break;
            }

            cert_chain.push(Certificate::read_options(reader, Endian::Big, ())?);
        }

        // Skip to the end of the certificate chain section
        reader.seek(SeekFrom::Start(cert_end))?;

        // Read ticket (big-endian, aligned to 64 bytes)
        reader.seek(SeekFrom::Start(align_64(cert_end)))?;
        let ticket = Ticket::read_options(reader, Endian::Big, ())?;

        // Read TMD (big-endian, aligned to 64 bytes)
        let tmd_start = align_64(reader.stream_position()?);
        reader.seek(SeekFrom::Start(tmd_start))?;
        let tmd = TitleMetadata::read_options(reader, Endian::Big, ())?;

        // Read content data (aligned to 64 bytes)
        let content_start = align_64(reader.stream_position()?);
        reader.seek(SeekFrom::Start(content_start))?;
        let mut content_data = vec![0u8; header.content_size as usize];
        reader.read_exact(&mut content_data)?;

        // Read meta data if present (little-endian, aligned to 64 bytes)
        let meta_data = if header.meta_size > 0 {
            let meta_start = align_64(reader.stream_position()?);
            reader.seek(SeekFrom::Start(meta_start))?;
            Some(MetaData::read_options(reader, Endian::Little, ())?)
        } else {
            None
        };

        Ok(CiaFile {
            header,
            cert_chain,
            ticket,
            tmd,
            content_data,
            meta_data,
        })
    }
}

impl BinWrite for CiaFile {
    type Args<'a> = ();

    fn write_options<W: Write + Seek>(
        &self,
        writer: &mut W,
        _endian: Endian,
        _args: Self::Args<'_>,
    ) -> BinResult<()> {
        // Write header (little-endian)
        self.header.write_options(writer, Endian::Little, ())?;

        let header_end = writer.stream_position()?;
        let cert_start = align_64(header_end);

        if cert_start > header_end {
            // Write padding to align to 64 bytes
            let padding_size = (cert_start - header_end) as usize;
            writer.write_all(&vec![0u8; padding_size])?;
        }

        // Write certificate chain (big-endian)
        for cert in &self.cert_chain {
            cert.write_options(writer, Endian::Big, ())?;
        }

        // IMPORTANT: Pad the certificate chain to match cert_chain_size
        let cert_written = writer.stream_position()? - cert_start;
        if cert_written < self.header.cert_chain_size as u64 {
            let padding_needed = self.header.cert_chain_size as u64 - cert_written;
            writer.write_all(&vec![0u8; padding_needed as usize])?;
        }

        // Write ticket (big-endian, aligned to 64 bytes)
        let ticket_start = align_64(writer.stream_position()?);
        pad_to_align_64(ticket_start, writer)?;
        self.ticket.write_options(writer, Endian::Big, ())?;

        // Write TMD (big-endian, aligned to 64 bytes)
        let tmd_start = align_64(writer.stream_position()?);
        pad_to_align_64(tmd_start, writer)?;
        self.tmd.write_options(writer, Endian::Big, ())?;

        // Write content data (aligned to 64 bytes)
        let content_start = align_64(writer.stream_position()?);
        pad_to_align_64(content_start, writer)?;
        writer.write_all(&self.content_data)?;

        // Write meta data if present (little-endian, aligned to 64 bytes)
        if let Some(ref meta) = self.meta_data {
            let meta_start = align_64(writer.stream_position()?);
            pad_to_align_64(meta_start, writer)?;
            meta.write_options(writer, Endian::Little, ())?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::ctr::models::certificate::{KeyType, PublicKey};
    use crate::nintendo::ctr::models::signature::{SignatureData, SignatureType};
    use crate::nintendo::ctr::models::ticket::{ContentIndex, TicketData};
    use crate::nintendo::ctr::models::title_metadata::{
        ContentChunkRecord, ContentInfoRecord, ContentType, TitleMetadataHeader,
    };
    use binrw::{BinRead, BinWrite};
    use std::io::Cursor;

    #[test]
    fn test_cia_header() {
        let header = CiaHeader {
            header_size: CIA_HEADER_SIZE,
            cia_type: 0,
            version: 0,
            cert_chain_size: 0x0A00,
            ticket_size: 0x0350,
            tmd_size: 0x0B34,
            meta_size: 0,
            content_size: 0x00400000,
            content_index: vec![0x00; 0x2000],
        };

        let mut buf = Vec::new();
        header.write(&mut Cursor::new(&mut buf)).unwrap();

        assert_eq!(buf.len(), header.header_size as usize);

        let mut cursor = Cursor::new(&buf);
        let read_header = CiaHeader::read(&mut cursor).unwrap();
        assert_eq!(header.header_size, read_header.header_size);
        assert_eq!(header.cert_chain_size, read_header.cert_chain_size);
        assert_eq!(header.content_size, read_header.content_size);
    }

    #[test]
    fn test_certificate_sizes() {
        // Test RSA-4096 certificate (CA cert)
        let cert_rsa4096 = Certificate {
            signature_type: SignatureType::Rsa4096Sha256,
            signature: vec![0xAA; 0x200],
            padding: vec![0x00; 0x3C],
            issuer: vec![0x00; 0x40],
            key_type: KeyType::Rsa2048,
            name: vec![0x00; 0x40],
            expiration_time: 0x5F5E0F00,
            public_key: PublicKey::Rsa2048 {
                modulus: vec![0xFF; 0x100],
                public_exponent: 65537,
                padding: vec![0x00; 0x34],
            },
        };

        let mut buf = Vec::new();
        cert_rsa4096
            .write_options(&mut Cursor::new(&mut buf), Endian::Big, ())
            .unwrap();

        println!("CA Certificate size (RSA-4096 sig): 0x{:X}", buf.len());
        // Recalculating: 4 + 0x200 + 0x3C + 0x40 + 4 + 0x40 + 4 + 0x100 + 4 + 0x34 = 0x400
        assert_eq!(buf.len(), 0x400);

        // Test RSA-2048 certificate
        let cert_rsa2048 = Certificate {
            signature_type: SignatureType::Rsa2048Sha256,
            signature: vec![0xAA; 0x100],
            padding: vec![0x00; 0x3C],
            issuer: vec![0x00; 0x40],
            key_type: KeyType::Rsa2048,
            name: vec![0x00; 0x40],
            expiration_time: 0x5F5E0F00,
            public_key: PublicKey::Rsa2048 {
                modulus: vec![0xFF; 0x100],
                public_exponent: 65537,
                padding: vec![0x00; 0x34],
            },
        };

        let mut buf2 = Vec::new();
        cert_rsa2048
            .write_options(&mut Cursor::new(&mut buf2), Endian::Big, ())
            .unwrap();

        println!(
            "TMD/Ticket Certificate size (RSA-2048 sig): 0x{:X}",
            buf2.len()
        );
        // Recalculating: 4 + 0x100 + 0x3C + 0x40 + 4 + 0x40 + 4 + 0x100 + 4 + 0x34 = 0x300
        assert_eq!(buf2.len(), 0x300);
    }

    #[test]
    fn test_ticket_size() {
        let ticket = Ticket {
            signature_data: SignatureData {
                signature_type: SignatureType::Rsa2048Sha256,
                signature: vec![0xAA; 0x100],
                padding: vec![0x00; 0x3C],
            },
            ticket_data: TicketData {
                issuer: vec![0x00; 0x40],
                ecc_public_key: vec![0x00; 0x3C],
                version: 1,
                ca_crl_version: 0,
                signer_crl_version: 0,
                title_key: vec![0xFF; 0x10],
                reserved1: 0,
                ticket_id: 0x0123456789ABCDEF,
                console_id: 0x12345678,
                title_id: 0xFEDCBA9876543210,
                reserved2: 0,
                ticket_title_version: 0x0100,
                reserved3: 0,
                license_type: 0,
                common_key_index: 1,
                reserved4: vec![0x00; 0x2A],
                eshop_account_id: 0,
                reserved5: 0,
                audit: 0,
                reserved6: vec![0x00; 0x42],
                limits: vec![0x00; 0x40],
                content_index: ContentIndex {
                    header_word: 0,
                    total_size: 0,
                    data: vec![0x00; 20],
                },
            },
        };

        let mut buf = Vec::new();
        ticket
            .write_options(&mut Cursor::new(&mut buf), Endian::Big, ())
            .unwrap();

        println!("Actual ticket size: 0x{:X}", buf.len());
        // Typical ticket size is around 0x2B8
    }

    #[test]
    fn test_simple_cia_file() {
        // Calculate how many certificates we need for 0xA00 bytes
        // Each RSA-2048 cert is 0x300 bytes, so we need 3 certificates + padding
        // 3 * 0x300 = 0x900, then 0x100 bytes of padding to reach 0xA00

        let cia_file = CiaFile {
            header: CiaHeader {
                header_size: CIA_HEADER_SIZE,
                cia_type: 0,
                version: 0,
                cert_chain_size: 0x0A00,
                ticket_size: 0x0350,
                tmd_size: 0x0B34,
                meta_size: 0,
                content_size: 0x1000,
                content_index: vec![0x00; 0x2000],
            },
            cert_chain: vec![
                // CA Certificate
                Certificate {
                    signature_type: SignatureType::Rsa2048Sha256,
                    signature: vec![0xAA; 0x100],
                    padding: vec![0x00; 0x3C],
                    issuer: {
                        let mut issuer = b"Root".to_vec();
                        issuer.resize(0x40, 0);
                        issuer
                    },
                    key_type: KeyType::Rsa2048,
                    name: {
                        let mut name = b"CA00000003".to_vec();
                        name.resize(0x40, 0);
                        name
                    },
                    expiration_time: 0x5F5E0F00,
                    public_key: PublicKey::Rsa2048 {
                        modulus: vec![0xFF; 0x100],
                        public_exponent: 65537,
                        padding: vec![0x00; 0x34],
                    },
                },
                // TMD Certificate
                Certificate {
                    signature_type: SignatureType::Rsa2048Sha256,
                    signature: vec![0xBB; 0x100],
                    padding: vec![0x00; 0x3C],
                    issuer: {
                        let mut issuer = b"Root-CA00000003".to_vec();
                        issuer.resize(0x40, 0);
                        issuer
                    },
                    key_type: KeyType::Rsa2048,
                    name: {
                        let mut name = b"CP0000000b".to_vec();
                        name.resize(0x40, 0);
                        name
                    },
                    expiration_time: 0x5F5E0F00,
                    public_key: PublicKey::Rsa2048 {
                        modulus: vec![0xEE; 0x100],
                        public_exponent: 65537,
                        padding: vec![0x00; 0x34],
                    },
                },
                // Ticket Certificate
                Certificate {
                    signature_type: SignatureType::Rsa2048Sha256,
                    signature: vec![0xCC; 0x100],
                    padding: vec![0x00; 0x3C],
                    issuer: {
                        let mut issuer = b"Root-CA00000003".to_vec();
                        issuer.resize(0x40, 0);
                        issuer
                    },
                    key_type: KeyType::Rsa2048,
                    name: {
                        let mut name = b"XS0000000c".to_vec();
                        name.resize(0x40, 0);
                        name
                    },
                    expiration_time: 0x5F5E0F00,
                    public_key: PublicKey::Rsa2048 {
                        modulus: vec![0xDD; 0x100],
                        public_exponent: 65537,
                        padding: vec![0x00; 0x34],
                    },
                },
            ],
            ticket: Ticket {
                signature_data: SignatureData {
                    signature_type: SignatureType::Rsa2048Sha256,
                    signature: vec![0xBB; 0x100],
                    padding: vec![0x00; 0x3C],
                },
                ticket_data: TicketData {
                    issuer: {
                        let mut issuer = b"Root-CA00000003-XS0000000c".to_vec();
                        issuer.resize(0x40, 0);
                        issuer
                    },
                    ecc_public_key: vec![0x00; 0x3C],
                    version: 1,
                    ca_crl_version: 0,
                    signer_crl_version: 0,
                    title_key: vec![0xFF; 0x10],
                    reserved1: 0,
                    ticket_id: 0x0123456789ABCDEF,
                    console_id: 0x12345678,
                    title_id: 0xFEDCBA9876543210,
                    reserved2: 0,
                    ticket_title_version: 0x0100,
                    reserved3: 0,
                    license_type: 0,
                    common_key_index: 1,
                    reserved4: vec![0x00; 0x2A],
                    eshop_account_id: 0,
                    reserved5: 0,
                    audit: 0,
                    reserved6: vec![0x00; 0x42],
                    limits: vec![0x00; 0x40],
                    content_index: ContentIndex {
                        header_word: 0,
                        total_size: 22,
                        data: vec![0x00; 20],
                    },
                },
            },
            tmd: TitleMetadata {
                signature_data: SignatureData {
                    signature_type: SignatureType::Rsa2048Sha256,
                    signature: vec![0xCC; 0x100],
                    padding: vec![0x00; 0x3C],
                },
                header: TitleMetadataHeader {
                    signature_issuer: {
                        let mut issuer = b"Root-CA00000003-CP0000000b".to_vec();
                        issuer.resize(0x40, 0);
                        issuer
                    },
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
                },
                content_info_records: vec![
                    ContentInfoRecord {
                        content_index_offset: 0,
                        content_command_count: 1,
                        hash: vec![0x00; 0x20],
                    };
                    64
                ],
                content_chunk_records: vec![ContentChunkRecord {
                    content_id: 0,
                    content_index: 0,
                    content_type: ContentType(0x0001),
                    content_size: 0x1000,
                    hash: vec![0xAB; 0x20],
                }],
            },
            content_data: vec![0x00; 0x1000],
            meta_data: None,
        };

        // Write using the standard BinWrite implementation
        let mut buf = Vec::new();
        cia_file
            .write_options(&mut Cursor::new(&mut buf), Endian::Little, ())
            .unwrap();

        // Read back
        let mut read_cursor = Cursor::new(&buf);
        let read_cia = CiaFile::read_options(&mut read_cursor, Endian::Little, ()).unwrap();

        // Verify key fields
        assert_eq!(cia_file.header.header_size, read_cia.header.header_size);
        assert_eq!(cia_file.header.content_size, read_cia.header.content_size);
        assert_eq!(cia_file.cert_chain.len(), read_cia.cert_chain.len());
        assert_eq!(
            cia_file.ticket.ticket_data.title_id,
            read_cia.ticket.ticket_data.title_id
        );
        assert_eq!(cia_file.tmd.header.title_id, read_cia.tmd.header.title_id);
        assert_eq!(cia_file.content_data.len(), read_cia.content_data.len());
    }
}
