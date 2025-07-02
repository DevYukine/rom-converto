use crate::nintendo::ctr::models::signature::SignatureType;
use binrw::{BinRead, BinWrite};

#[derive(Debug, Clone, Copy, PartialEq, Eq, BinRead, BinWrite)]
#[brw(repr = u32)]
pub enum KeyType {
    /// This contains the Public Key(i.e. Modulus & Public Exponent) 4096 bits long, used for RSA signatures.
    Rsa4096 = 0x0,
    /// This contains the Public Key(i.e. Modulus & Public Exponent) 2048 bits long, used for RSA signatures.
    Rsa2048 = 0x1,
    /// This contains the ECC public key
    EllipticCurve = 0x2,
}

/// Certificates contain cryptography information for verifying Signatures. These certificates are also signed. The parent/child relationship between certificates, makes all the certificates effectively signed by 'Root', the public key for which is stored in NATIVE_FIRM.
#[derive(Debug, Clone, BinRead, BinWrite)]
pub struct Certificate {
    /// Signature Type
    #[brw(big)]
    pub signature_type: SignatureType,

    /// Signature
    #[br(count = signature_type.signature_size())]
    pub signature: Vec<u8>,

    /// Padding (aligning next data to 0x40 bytes)
    #[br(count = signature_type.padding_size())]
    pub padding: Vec<u8>,

    /// Issuer
    #[br(count = 0x40)]
    pub issuer: Vec<u8>,

    /// Key Type
    #[brw(big)]
    pub key_type: KeyType,

    /// Name
    #[br(count = 0x40)]
    pub name: Vec<u8>,

    /// Expiration time as UNIX Timestamp, used at least for CTCert
    #[brw(big)]
    pub expiration_time: u32,

    /// Public Key
    #[br(args(key_type))]
    pub public_key: PublicKey,
}

// Determining the type of public key stored, is done by checking the key type:
#[derive(Debug, Clone, BinRead, BinWrite)]
#[br(import(key_type: KeyType))]
pub enum PublicKey {
    #[br(pre_assert(key_type == KeyType::Rsa4096))]
    Rsa4096 {
        #[br(count = 0x200)]
        modulus: Vec<u8>,
        #[brw(big)]
        public_exponent: u32,
        #[br(count = 0x34)]
        padding: Vec<u8>,
    },
    #[br(pre_assert(key_type == KeyType::Rsa2048))]
    Rsa2048 {
        #[br(count = 0x100)]
        modulus: Vec<u8>,
        #[brw(big)]
        public_exponent: u32,
        #[br(count = 0x34)]
        padding: Vec<u8>,
    },
    #[br(pre_assert(key_type == KeyType::EllipticCurve))]
    EllipticCurve {
        #[br(count = 0x3C)]
        public_key: Vec<u8>,
        #[br(count = 0x3C)]
        padding: Vec<u8>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_certificate_rsa2048() {
        let cert = Certificate {
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

        let mut buf = Vec::new();
        cert.write_be(&mut Cursor::new(&mut buf)).unwrap();

        let mut cursor = Cursor::new(&buf);
        let read_cert = Certificate::read_be(&mut cursor).unwrap();
        assert_eq!(cert.signature_type, read_cert.signature_type);
        assert_eq!(cert.key_type, read_cert.key_type);
        match read_cert.public_key {
            PublicKey::Rsa2048 {
                public_exponent, ..
            } => {
                assert_eq!(public_exponent, 65537);
            }
            _ => panic!("Wrong public key type"),
        }
    }

    #[test]
    fn test_certificate_ecc() {
        let cert = Certificate {
            signature_type: SignatureType::EcdsaSha256,
            signature: vec![0xBB; 0x3C],
            padding: vec![0x00; 0x40],
            issuer: vec![0x00; 0x40],
            key_type: KeyType::EllipticCurve,
            name: vec![0x00; 0x40],
            expiration_time: 0x5F5E0F00,
            public_key: PublicKey::EllipticCurve {
                public_key: vec![0xCC; 0x3C],
                padding: vec![0x00; 0x3C],
            },
        };

        let mut buf = Vec::new();
        cert.write_be(&mut Cursor::new(&mut buf)).unwrap();

        let mut cursor = Cursor::new(&buf);
        let read_cert = Certificate::read_be(&mut cursor).unwrap();
        assert_eq!(cert.signature_type, read_cert.signature_type);
        assert_eq!(cert.key_type, read_cert.key_type);
        match read_cert.public_key {
            PublicKey::EllipticCurve { public_key, .. } => {
                assert_eq!(public_key.len(), 0x3C);
            }
            _ => panic!("Wrong public key type"),
        }
    }
}
