use crate::nintendo::ctr::models::certificate::{Certificate, KeyType, PublicKey};
use crate::nintendo::ctr::models::signature::{SignatureData, SignatureType};
use crate::nintendo::ctr::models::ticket::{ContentIndex, Ticket, TicketData};

fn padded(value: &[u8], size: usize) -> Vec<u8> {
    let mut v = value.to_vec();
    v.resize(size, 0);
    v
}

pub fn template_ticket() -> Ticket {
    Ticket {
        signature_data: SignatureData {
            signature_type: SignatureType::Rsa2048Sha256,
            signature: vec![0u8; 0x100],
            padding: vec![0u8; 0x3C],
        },
        ticket_data: TicketData {
            issuer: padded(b"Root-CA00000003-XS0000000c", 0x40),
            ecc_public_key: vec![0u8; 0x3C],
            version: 1,
            ca_crl_version: 0,
            signer_crl_version: 0,
            title_key: vec![0u8; 0x10],
            reserved1: 0,
            ticket_id: 0,
            console_id: 0,
            title_id: 0,
            reserved2: 0,
            ticket_title_version: 0,
            reserved3: 0,
            license_type: 0,
            common_key_index: 0,
            reserved4: vec![0u8; 0x2A],
            eshop_account_id: 0,
            reserved5: 0,
            audit: 0,
            reserved6: vec![0u8; 0x42],
            limits: vec![0u8; 0x40],
            content_index: ContentIndex {
                header_word: 0x00010014,
                total_size: 0xAC,
                data: vec![0u8; 0xA4],
            },
        },
    }
}

pub fn retail_cert_chain() -> Vec<Certificate> {
    vec![
        Certificate {
            signature_type: SignatureType::Rsa4096Sha256,
            signature: vec![0u8; 0x200],
            padding: vec![0u8; 0x3C],
            issuer: padded(b"Root", 0x40),
            key_type: KeyType::Rsa2048,
            name: padded(b"CA00000003", 0x40),
            expiration_time: 0,
            public_key: PublicKey::Rsa2048 {
                modulus: vec![0u8; 0x100],
                public_exponent: 0x10001,
                padding: vec![0u8; 0x34],
            },
        },
        Certificate {
            signature_type: SignatureType::Rsa2048Sha256,
            signature: vec![0u8; 0x100],
            padding: vec![0u8; 0x3C],
            issuer: padded(b"Root-CA00000003", 0x40),
            key_type: KeyType::Rsa2048,
            name: padded(b"XS0000000c", 0x40),
            expiration_time: 0,
            public_key: PublicKey::Rsa2048 {
                modulus: vec![0u8; 0x100],
                public_exponent: 0x10001,
                padding: vec![0u8; 0x34],
            },
        },
        Certificate {
            signature_type: SignatureType::Rsa2048Sha256,
            signature: vec![0u8; 0x100],
            padding: vec![0u8; 0x3C],
            issuer: padded(b"Root-CA00000003", 0x40),
            key_type: KeyType::Rsa2048,
            name: padded(b"CP0000000b", 0x40),
            expiration_time: 0,
            public_key: PublicKey::Rsa2048 {
                modulus: vec![0u8; 0x100],
                public_exponent: 0x10001,
                padding: vec![0u8; 0x34],
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use binrw::{BinRead, BinWrite, Endian};
    use std::io::Cursor;

    #[test]
    fn ticket_roundtrips() {
        let t = template_ticket();
        let mut buf = Vec::new();
        t.write_options(&mut Cursor::new(&mut buf), Endian::Big, ())
            .unwrap();
        let parsed = Ticket::read_options(&mut Cursor::new(&buf), Endian::Big, ()).unwrap();
        assert_eq!(parsed.ticket_data.version, 1);
        assert_eq!(parsed.ticket_data.common_key_index, 0);
    }

    #[test]
    fn cert_chain_sizes() {
        let chain = retail_cert_chain();
        assert_eq!(chain.len(), 3);
        let mut buf = Vec::new();
        let mut cursor = Cursor::new(&mut buf);
        for cert in &chain {
            cert.write_options(&mut cursor, Endian::Big, ()).unwrap();
        }
        // CA (RSA-4096 sig + RSA-2048 pubkey) = 0x400
        // XS, CP (RSA-2048 sig + RSA-2048 pubkey) = 0x300 each
        // Total = 0x400 + 0x300 + 0x300 = 0xA00, fits CIA_CERT_CHAIN_SIZE exactly.
        assert_eq!(buf.len(), 0xA00);
    }
}
