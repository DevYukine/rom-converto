use binrw::{BinRead, BinWrite};

pub const Z3DS_VERSION: u8 = 0x01;
pub const Z3DS_HEADER_SIZE: u16 = 0x20;
pub const Z3DS_METADATA_VERSION: u8 = 0x01;

pub const METADATA_TYPE_END: u8 = 0x00;
pub const METADATA_TYPE_BINARY: u8 = 0x01;

/// Underlying magic values for supported 3DS ROM types.
pub mod underlying_magic {
    pub const CIA: [u8; 4] = *b"CIA\0";
    pub const NCSD: [u8; 4] = *b"NCSD";
    pub const NCCH: [u8; 4] = *b"NCCH";
    pub const THREEDSX: [u8; 4] = *b"3DSX";
}

/// Z3DS file header — 0x20 bytes, little-endian.
#[derive(Debug, Clone, BinRead, BinWrite)]
#[brw(little, magic = b"Z3DS")]
pub struct Z3dsHeader {
    /// Magic of the original uncompressed ROM (e.g. "NCSD", "NCCH", "3DSX", "CIA\0").
    pub underlying_magic: [u8; 4],
    /// Format version — must be 0x01.
    pub version: u8,
    /// Reserved, must be zero.
    pub reserved: u8,
    /// Total size of this header in bytes (= 0x20).
    pub header_size: u16,
    /// Length of the optional metadata block following the header (0 if absent).
    /// Always 16-byte aligned.
    pub metadata_size: u32,
    /// Length of the compressed ROM data.
    pub compressed_size: u64,
    /// Original (uncompressed) size of the ROM.
    pub uncompressed_size: u64,
}

impl Z3dsHeader {
    pub fn new(
        underlying_magic: [u8; 4],
        metadata_size: u32,
        compressed_size: u64,
        uncompressed_size: u64,
    ) -> Self {
        Self {
            underlying_magic,
            version: Z3DS_VERSION,
            reserved: 0,
            header_size: Z3DS_HEADER_SIZE,
            metadata_size,
            compressed_size,
            uncompressed_size,
        }
    }
}

/// A single metadata item.
#[derive(Debug, Clone)]
pub struct Z3dsMetadataItem {
    pub name: String,
    pub data: Vec<u8>,
}

impl Z3dsMetadataItem {
    pub fn new(name: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.into(),
            data: data.into(),
        }
    }

    pub fn new_str(name: impl Into<String>, value: &str) -> Self {
        Self::new(name, value.as_bytes().to_vec())
    }
}

/// The optional metadata block that follows the Z3DS header.
#[derive(Debug, Clone, Default)]
pub struct Z3dsMetadata {
    pub items: Vec<Z3dsMetadataItem>,
}

impl Z3dsMetadata {
    pub fn new(items: Vec<Z3dsMetadataItem>) -> Self {
        Self { items }
    }

    /// Serialises the metadata block and pads it to a 16-byte boundary.
    /// Returns the padded bytes. Returns empty vec if there are no items.
    pub fn to_bytes(&self) -> std::io::Result<Vec<u8>> {
        if self.items.is_empty() {
            return Ok(vec![]);
        }

        let mut buf: Vec<u8> = Vec::new();

        // version byte
        buf.push(Z3DS_METADATA_VERSION);

        for item in &self.items {
            let name_bytes = item.name.as_bytes();
            let name_len = name_bytes.len() as u8;
            let data_len = item.data.len() as u16;

            buf.push(METADATA_TYPE_BINARY);
            buf.push(name_len);
            buf.extend_from_slice(&data_len.to_le_bytes());
            buf.extend_from_slice(name_bytes);
            buf.extend_from_slice(&item.data);
        }

        // TYPE_END terminator: type=0, name_len=0, data_len=0
        buf.extend_from_slice(&[METADATA_TYPE_END, 0x00, 0x00, 0x00]);

        // Pad to 16-byte boundary
        let remainder = buf.len() % 16;
        if remainder != 0 {
            buf.extend(std::iter::repeat(0u8).take(16 - remainder));
        }

        Ok(buf)
    }

    /// Parses metadata from a byte slice. Returns items found before TYPE_END.
    #[allow(dead_code)]
    pub fn from_bytes(data: &[u8]) -> Vec<Z3dsMetadataItem> {
        if data.is_empty() {
            return vec![];
        }

        let mut items = vec![];
        let mut pos = 1usize; // skip version byte

        while pos < data.len() {
            let item_type = data[pos];
            pos += 1;

            if item_type == METADATA_TYPE_END {
                break;
            }

            if pos + 3 > data.len() {
                break;
            }

            let name_len = data[pos] as usize;
            pos += 1;
            let data_len = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
            pos += 2;

            if pos + name_len + data_len > data.len() {
                break;
            }

            let name = String::from_utf8_lossy(&data[pos..pos + name_len]).into_owned();
            pos += name_len;
            let value = data[pos..pos + data_len].to_vec();
            pos += data_len;

            items.push(Z3dsMetadataItem { name, data: value });
        }

        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use binrw::{BinRead, BinWrite};
    use std::io::Cursor;

    // --- Z3dsHeader ---

    #[test]
    fn header_new_sets_correct_defaults() {
        let h = Z3dsHeader::new(*b"NCCH", 64, 1024, 4096);
        assert_eq!(h.underlying_magic, *b"NCCH");
        assert_eq!(h.version, Z3DS_VERSION);
        assert_eq!(h.reserved, 0);
        assert_eq!(h.header_size, Z3DS_HEADER_SIZE);
        assert_eq!(h.metadata_size, 64);
        assert_eq!(h.compressed_size, 1024);
        assert_eq!(h.uncompressed_size, 4096);
    }

    #[test]
    fn header_binrw_round_trip() {
        let original = Z3dsHeader::new(*b"NCSD", 32, 512, 2048);

        let mut buf = Cursor::new(Vec::new());
        original.write(&mut buf).unwrap();
        let bytes = buf.into_inner();

        // Must be exactly 0x20 bytes.
        assert_eq!(bytes.len(), 0x20);
        // File starts with the Z3DS magic.
        assert_eq!(&bytes[0..4], b"Z3DS");

        let mut read_cur = Cursor::new(&bytes);
        let parsed = Z3dsHeader::read(&mut read_cur).unwrap();

        assert_eq!(parsed.underlying_magic, *b"NCSD");
        assert_eq!(parsed.version, Z3DS_VERSION);
        assert_eq!(parsed.reserved, 0);
        assert_eq!(parsed.header_size, Z3DS_HEADER_SIZE);
        assert_eq!(parsed.metadata_size, 32);
        assert_eq!(parsed.compressed_size, 512);
        assert_eq!(parsed.uncompressed_size, 2048);
    }

    #[test]
    fn header_rejects_wrong_magic() {
        // Build a valid header then corrupt the magic.
        let h = Z3dsHeader::new(*b"NCCH", 0, 0, 0);
        let mut buf = Cursor::new(Vec::new());
        h.write(&mut buf).unwrap();
        let mut bytes = buf.into_inner();
        bytes[0] = b'X'; // corrupt "Z3DS" → "X3DS"

        let result = Z3dsHeader::read(&mut Cursor::new(&bytes));
        assert!(result.is_err());
    }

    // --- Z3dsMetadata ---

    #[test]
    fn metadata_empty_yields_empty_bytes() {
        let md = Z3dsMetadata::default();
        assert_eq!(md.to_bytes().unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn metadata_from_bytes_empty_slice_yields_no_items() {
        let items = Z3dsMetadata::from_bytes(&[]);
        assert!(items.is_empty());
    }

    #[test]
    fn metadata_to_bytes_version_byte_is_first() {
        let md = Z3dsMetadata::new(vec![Z3dsMetadataItem::new_str("k", "v")]);
        let bytes = md.to_bytes().unwrap();
        assert_eq!(bytes[0], Z3DS_METADATA_VERSION);
    }

    #[test]
    fn metadata_to_bytes_is_16_byte_aligned() {
        // Try several item sizes to hit different alignment cases.
        for name_len in [1usize, 5, 10, 15] {
            let name = "x".repeat(name_len);
            let md = Z3dsMetadata::new(vec![Z3dsMetadataItem::new_str(name, "value")]);
            let bytes = md.to_bytes().unwrap();
            assert_eq!(
                bytes.len() % 16,
                0,
                "metadata not 16-byte aligned for name_len={name_len}"
            );
        }
    }

    #[test]
    fn metadata_single_item_byte_layout() {
        let md = Z3dsMetadata::new(vec![Z3dsMetadataItem::new_str("key", "val")]);
        let bytes = md.to_bytes().unwrap();

        // version byte
        assert_eq!(bytes[0], Z3DS_METADATA_VERSION);
        // item type
        assert_eq!(bytes[1], METADATA_TYPE_BINARY);
        // name_len = 3
        assert_eq!(bytes[2], 3);
        // data_len = 3 (LE u16)
        assert_eq!(bytes[3], 3);
        assert_eq!(bytes[4], 0);
        // name = "key"
        assert_eq!(&bytes[5..8], b"key");
        // data = "val"
        assert_eq!(&bytes[8..11], b"val");
        // TYPE_END terminator
        assert_eq!(bytes[11], METADATA_TYPE_END);
    }

    #[test]
    fn metadata_round_trip_single_item() {
        let md = Z3dsMetadata::new(vec![Z3dsMetadataItem::new_str("compressor", "rom-converto")]);
        let bytes = md.to_bytes().unwrap();
        let items = Z3dsMetadata::from_bytes(&bytes);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "compressor");
        assert_eq!(items[0].data, b"rom-converto");
    }

    #[test]
    fn metadata_round_trip_multiple_items() {
        let md = Z3dsMetadata::new(vec![
            Z3dsMetadataItem::new_str("compressor", "rom-converto"),
            Z3dsMetadataItem::new_str("date", "2026-01-01T00:00:00Z"),
            Z3dsMetadataItem::new_str("maxframesize", "262144"),
        ]);
        let bytes = md.to_bytes().unwrap();
        let items = Z3dsMetadata::from_bytes(&bytes);

        assert_eq!(items.len(), 3);
        assert_eq!(items[0].name, "compressor");
        assert_eq!(items[1].name, "date");
        assert_eq!(items[2].name, "maxframesize");
        assert_eq!(items[2].data, b"262144");
    }

    #[test]
    fn metadata_from_bytes_stops_at_type_end() {
        let md = Z3dsMetadata::new(vec![Z3dsMetadataItem::new_str("a", "1")]);
        let mut bytes = md.to_bytes().unwrap();

        // Append extra garbage after the padding — the parser must stop at TYPE_END
        // and ignore everything after it.
        bytes.extend_from_slice(&[0x01, 0x05, 0x00, 0x00, b'e', b'x', b't', b'r', b'a']);

        let items = Z3dsMetadata::from_bytes(&bytes);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "a");
    }

    #[test]
    fn metadata_item_with_binary_data() {
        let payload = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let md = Z3dsMetadata::new(vec![Z3dsMetadataItem::new("bin", payload.clone())]);
        let bytes = md.to_bytes().unwrap();
        let items = Z3dsMetadata::from_bytes(&bytes);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "bin");
        assert_eq!(items[0].data, payload);
    }
}
