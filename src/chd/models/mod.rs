use binrw::{BinRead, BinWrite, binrw};

pub const CHD_V5_HEADER_SIZE: u32 = 124;
pub const CHD_METADATA_TAG_CD: [u8; 4] = *b"CHT2";
pub const CHD_METADATA_FLAG_HASHED: u8 = 0x01;
pub const CHD_METADATA_RESERVED_BYTES: usize = 8;
pub const SHA1_BYTES: usize = 20;

/// CHD file format version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, BinRead, BinWrite)]
#[brw(repr = u32)]
pub enum ChdVersion {
    V1 = 1,
    V2 = 2,
    V3 = 3,
    V4 = 4,
    V5 = 5,
}

/// CHD v5 file header (124 bytes, big-endian).
///
/// Contains compression methods, logical size, offsets for map and metadata,
/// and SHA1 hashes for integrity checks.
#[derive(Debug, BinRead, BinWrite)]
#[brw(big)]
#[brw(magic = b"MComprHD")]
pub struct ChdHeaderV5 {
    /// Header length in bytes (124 for V5).
    pub length: u32,
    /// CHD format version.
    pub version: ChdVersion,
    /// Compressor tags (4 slots, 4 bytes each).
    pub compressor_0: [u8; 4],
    pub compressor_1: [u8; 4],
    pub compressor_2: [u8; 4],
    pub compressor_3: [u8; 4],
    /// Logical size of the uncompressed data.
    pub logical_bytes: u64,
    /// File offset to the compressed map section.
    pub map_offset: u64,
    /// File offset to the metadata section.
    pub meta_offset: u64,
    /// Bytes per hunk (compression unit).
    pub hunk_bytes: u32,
    /// Bytes per unit within a hunk.
    pub unit_bytes: u32,
    /// SHA1 of the raw (uncompressed) data.
    pub raw_sha1: [u8; SHA1_BYTES],
    /// Overall SHA1 (raw data + metadata hashes).
    pub sha1: [u8; SHA1_BYTES],
    /// SHA1 of the parent CHD, or all zeros if standalone.
    pub parent_sha1: [u8; SHA1_BYTES],
}

#[binrw]
#[brw(big)]
#[derive(Debug)]
pub struct ChdMetadataHeader {
    pub tag: [u8; 4], // e.g. b"CHT2"
    pub flags: u8,    // 0x01

    #[bw(calc = {
        let len = data.len() as u32;
        [(len >> 16) as u8, (len >> 8) as u8, len as u8]
    })]
    #[br(temp)]
    length_raw: [u8; 3], // 24-bit big-endian length

    pub reserved: [u8; CHD_METADATA_RESERVED_BYTES], // 8 bytes of zeros

    #[br(count = ((length_raw[0] as u32) << 16) |
                  ((length_raw[1] as u32) << 8) |
                  (length_raw[2] as u32))]
    pub data: Vec<u8>, // The actual metadata string
}

impl ChdMetadataHeader {
    pub fn new_cd_metadata(metadata_string: String) -> Self {
        let mut data = metadata_string.into_bytes();
        data.push(0);

        Self {
            tag: CHD_METADATA_TAG_CD,
            flags: CHD_METADATA_FLAG_HASHED,
            reserved: [0; CHD_METADATA_RESERVED_BYTES],
            data,
        }
    }
}
