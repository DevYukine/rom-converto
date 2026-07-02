use crate::nintendo::ctr::models::certificate::Certificate;
use crate::nintendo::ctr::models::signature::{SignatureData, SignatureType};
use crate::nintendo::ctr::models::ticket::{ContentIndex, Ticket, TicketData};
use binrw::{BinRead, Endian};
use hex_literal::hex;
use std::io::Cursor;

/// Genuine retail certificate chain (CA00000003 -> XS0000000c / CP0000000b)
/// shipped in every retail title. AM verifies the ticket and TMD against the
/// public keys in these certificates before it will install a CIA, so the
/// chain has to carry the real moduli; a zeroed stand-in makes installs fail
/// on hardware with a cert/hash error even when signature checks are patched.
const RETAIL_CERT_CHAIN: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../resources/3ds_retail_certchain.bin"
));

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
                // The content-index "enabled content" block. The 0xFF run is the
                // bitmap that grants the ticket rights to the installed contents;
                // leaving it zeroed licenses nothing and AM refuses the install.
                data: {
                    let mut data = vec![0u8; 0xA4];
                    data[..0x44].copy_from_slice(&hex!(
                        "00000014 00010014 00000000 00000028 00000001 00000084 00000084 00030000 00000000 \
                         ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
                    ));
                    data
                },
            },
        },
    }
}

pub fn retail_cert_chain() -> Vec<Certificate> {
    let mut cursor = Cursor::new(RETAIL_CERT_CHAIN);
    let mut certs = Vec::new();
    while (cursor.position() as usize) < RETAIL_CERT_CHAIN.len() {
        certs.push(
            Certificate::read_options(&mut cursor, Endian::Big, ())
                .expect("embedded retail certificate chain must parse"),
        );
    }
    certs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::ctr::models::certificate::PublicKey;
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

    #[test]
    fn retail_cert_chain_carries_real_keys_and_round_trips() {
        let chain = retail_cert_chain();
        assert_eq!(chain.len(), 3);

        let name = |c: &Certificate| {
            String::from_utf8_lossy(&c.name)
                .trim_end_matches('\0')
                .to_string()
        };
        assert_eq!(name(&chain[0]), "CA00000003");
        assert_eq!(name(&chain[1]), "XS0000000c");
        assert_eq!(name(&chain[2]), "CP0000000b");

        for cert in &chain {
            match &cert.public_key {
                PublicKey::Rsa2048 { modulus, .. } => {
                    assert!(
                        modulus.iter().any(|&b| b != 0),
                        "modulus must not be zeroed"
                    );
                }
                other => panic!("unexpected key type in retail chain: {other:?}"),
            }
        }

        // Parsing then re-serializing must reproduce the embedded chain exactly,
        // otherwise the emitted CIA would carry a corrupted chain.
        let mut buf = Vec::new();
        let mut cursor = Cursor::new(&mut buf);
        for cert in &chain {
            cert.write_options(&mut cursor, Endian::Big, ()).unwrap();
        }
        assert_eq!(buf.as_slice(), RETAIL_CERT_CHAIN);
    }

    #[test]
    fn template_ticket_enables_content() {
        let t = template_ticket();
        let mut buf = Vec::new();
        t.write_options(&mut Cursor::new(&mut buf), Endian::Big, ())
            .unwrap();
        // Retail ticket size AM expects.
        assert_eq!(buf.len(), 0x350);

        let data = &t.ticket_data.content_index.data;
        assert_eq!(data.len(), 0xA4);
        // The 32-byte content-enable bitmap sits right after the 0x24-byte
        // structured header; it must be all 0xFF, not a zeroed (licenses-nothing) block.
        assert!(
            data[0x24..0x44].iter().all(|&b| b == 0xFF),
            "content enable bitmap must be set"
        );
    }
}
