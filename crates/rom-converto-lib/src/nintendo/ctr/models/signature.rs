use binrw::{BinRead, BinWrite};

/// The signature method used to sign the certificate can be determined by checking the Signature Type:
#[derive(Debug, Clone, Copy, PartialEq, Eq, BinRead, BinWrite)]
#[brw(repr = u32)]
pub enum SignatureType {
    /// RSA_4096 SHA1 (Unused for 3DS)
    Rsa4096Sha1 = 0x010000,

    /// RSA_2048 SHA1 (Unused for 3DS)
    Rsa2048Sha1 = 0x010001,

    /// Elliptic Curve with SHA1 (Unused for 3DS)
    EllipticCurveSha1 = 0x010002,

    /// RSA_4096 SHA256
    Rsa4096Sha256 = 0x010003,

    /// RSA_2048 SHA256
    Rsa2048Sha256 = 0x010004,

    /// ECDSA with SHA256
    EcdsaSha256 = 0x010005,
}

impl SignatureType {
    pub fn signature_size(&self) -> usize {
        match self {
            Self::Rsa4096Sha1 | Self::Rsa4096Sha256 => 0x200,
            Self::Rsa2048Sha1 | Self::Rsa2048Sha256 => 0x100,
            Self::EllipticCurveSha1 | Self::EcdsaSha256 => 0x3C,
        }
    }

    pub fn padding_size(&self) -> usize {
        // According to the documentation:
        // RSA signatures have 0x3C padding
        // ECC signatures have 0x40 padding
        match self {
            Self::Rsa4096Sha1 | Self::Rsa4096Sha256 => 0x3C,
            Self::Rsa2048Sha1 | Self::Rsa2048Sha256 => 0x3C,
            Self::EllipticCurveSha1 | Self::EcdsaSha256 => 0x40,
        }
    }
}

/// Generic signature data structure
#[derive(Debug, Clone, BinRead, BinWrite)]
pub struct SignatureData {
    #[brw(big)]
    pub signature_type: SignatureType,
    #[br(count = signature_type.signature_size())]
    pub signature: Vec<u8>,
    #[br(count = signature_type.padding_size())]
    pub padding: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_signature_type() {
        let sig_type = SignatureType::Rsa2048Sha256;
        let mut buf = Vec::new();
        sig_type.write_be(&mut Cursor::new(&mut buf)).unwrap();

        let mut cursor = Cursor::new(&buf);
        let read_sig_type = SignatureType::read_be(&mut cursor).unwrap();
        assert_eq!(sig_type, read_sig_type);
        assert_eq!(sig_type.signature_size(), 0x100);
        assert_eq!(sig_type.padding_size(), 0x3C);
    }

    #[test]
    fn test_signature_padding() {
        // According to documentation: RSA has 0x3C padding, ECC has 0x40 padding
        assert_eq!(SignatureType::Rsa4096Sha256.padding_size(), 0x3C);
        assert_eq!(SignatureType::Rsa2048Sha256.padding_size(), 0x3C);
        assert_eq!(SignatureType::EcdsaSha256.padding_size(), 0x40);
    }

    #[test]
    fn test_signature_data() {
        let sig_data = SignatureData {
            signature_type: SignatureType::EcdsaSha256,
            signature: vec![0xAA; 0x3C],
            padding: vec![0x00; 0x40], // ECC has 0x40 padding
        };

        let mut buf = Vec::new();
        sig_data.write_be(&mut Cursor::new(&mut buf)).unwrap();
        assert_eq!(buf.len(), 0x80); // 4 + 0x3C + 0x40

        let mut cursor = Cursor::new(&buf);
        let read_sig_data = SignatureData::read_be(&mut cursor).unwrap();
        assert_eq!(sig_data.signature_type, read_sig_data.signature_type);
        assert_eq!(sig_data.signature, read_sig_data.signature);
        assert_eq!(sig_data.padding, read_sig_data.padding);
    }
}
