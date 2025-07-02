use crate::nintendo::ctr::models::signature::SignatureData;
use binrw::{BinRead, BinWrite};

/// Tickets are a format used to store an encrypted titlekey (using 128-Bit AES-CBC). With 3DS, the Ticket format was updated (now v1) from Wii/DSi format (v0).
#[derive(Debug, Clone, BinRead, BinWrite)]
#[brw(big)]
pub struct Ticket {
    /// Signature Data, The hash for the signature is calculated over the Ticket Data.
    pub signature_data: SignatureData,

    /// Ticket Data
    pub ticket_data: TicketData,
}

#[derive(Debug, Clone, BinRead, BinWrite)]
#[brw(big)]
pub struct TicketData {
    /// Issuer
    #[br(count = 0x40)]
    pub issuer: Vec<u8>,

    /// ECC PublicKey
    #[br(count = 0x3C)]
    pub ecc_public_key: Vec<u8>,

    /// Version (For 3DS this is always 1)
    pub version: u8,

    /// CaCrlVersion
    pub ca_crl_version: u8,

    /// SignerCrlVersion
    pub signer_crl_version: u8,

    /// TitleKey (normal-key encrypted using one of the common keyYs; see below)
    #[br(count = 0x10)]
    pub title_key: Vec<u8>,

    /// Reserved
    pub reserved1: u8,

    /// TicketID
    pub ticket_id: u64,

    /// ConsoleID
    pub console_id: u32,

    /// TitleID
    pub title_id: u64,

    /// Reserved
    pub reserved2: u16,

    /// Ticket title version
    pub ticket_title_version: u16,

    /// Reserved
    pub reserved3: u64,

    /// License Type
    pub license_type: u8,

    /// Index to the common keyY used for this ticket, usually 0x1 for retail system titles;
    pub common_key_index: u8,

    /// Reserved
    #[br(count = 0x2A)]
    pub reserved4: Vec<u8>,

    /// eShop Account ID?
    pub eshop_account_id: u32,

    /// Reserved
    pub reserved5: u8,

    /// Audit
    pub audit: u8,

    /// Reserved
    #[br(count = 0x42)]
    pub reserved6: Vec<u8>,

    /// Limits
    #[br(count = 0x40)]
    pub limits: Vec<u8>,

    /// Content Index
    pub content_index: ContentIndex,
}

#[derive(Debug, Clone, BinRead, BinWrite)]
#[brw(big)]
pub struct ContentIndex {
    pub header_word: u32,

    /// Total size of this block, including the first two bytes
    pub total_size: u32,

    /// The data of the content index, which is usually at least 20 bytes long.
    #[br(count = total_size.checked_sub(8).expect("invalid size") as usize)]
    pub data: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::ctr::models::signature::SignatureType;
    use binrw::BinWrite;
    use std::io::Cursor;

    #[test]
    fn test_ticket_data() {
        let ticket_data = TicketData {
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
                total_size: 22,
                data: vec![0x00; 20],
            },
        };

        let mut buf = Vec::new();
        ticket_data.write(&mut Cursor::new(&mut buf)).unwrap();

        let mut cursor = Cursor::new(&buf);
        let read_ticket_data = TicketData::read(&mut cursor).unwrap();
        assert_eq!(ticket_data.version, read_ticket_data.version);
        assert_eq!(ticket_data.ticket_id, read_ticket_data.ticket_id);
        assert_eq!(ticket_data.title_id, read_ticket_data.title_id);
        assert_eq!(
            ticket_data.common_key_index,
            read_ticket_data.common_key_index
        );
    }

    #[test]
    fn test_full_ticket() {
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
                    total_size: 22,
                    data: vec![0x00; 20],
                },
            },
        };

        let mut buf = Vec::new();
        ticket.write(&mut Cursor::new(&mut buf)).unwrap();

        let mut cursor = Cursor::new(&buf);
        let read_ticket = Ticket::read(&mut cursor).unwrap();
        assert_eq!(
            ticket.signature_data.signature_type,
            read_ticket.signature_data.signature_type
        );
        assert_eq!(ticket.ticket_data.version, read_ticket.ticket_data.version);
        assert_eq!(
            ticket.ticket_data.title_id,
            read_ticket.ticket_data.title_id
        );
    }
}
