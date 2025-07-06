use std::io::{Read, Seek, Write};
// models.rs
use binrw::{BinRead, BinWrite, binrw};

/// Represents the version of the CHD file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, BinRead, BinWrite)]
#[brw(repr = u32)]
pub enum ChdVersion {
    V1 = 1,
    V2 = 2,
    V3 = 3,
    V4 = 4,
    V5 = 5,
}

/// Represents the header for CHD files, specifically version 5.
/// This header contains metadata about the CHD file, including compression methods,
/// logical size, offsets for map and metadata, and SHA1 hashes for integrity checks.
/// The header is 124 bytes long and uses big-endian byte order.
#[derive(Debug, BinRead, BinWrite)]
#[brw(big)]
#[brw(magic = b"MComprHD")]
pub struct ChdHeaderV5 {
    /// Length of the header in bytes (124 for V5)
    pub length: u32,

    /// Version of the CHD file format.
    pub version: ChdVersion,

    /// Compressor tags for the four compression methods used in the CHD file.
    pub compressor_0: [u8; 4],
    pub compressor_1: [u8; 4],
    pub compressor_2: [u8; 4],
    pub compressor_3: [u8; 4],

    /// Logical size of the data in bytes.
    pub logical_bytes: u64,

    /// Offset to the map section in the CHD file.
    pub map_offset: u64,

    /// Offset to the metadata section in the CHD file.
    pub meta_offset: u64,

    /// Number of hunks in the CHD file.
    pub hunk_bytes: u32,

    /// Number of bytes per unit within a hunk.
    pub unit_bytes: u32,

    /// SHA1 hash of the raw data.
    pub raw_sha1: [u8; 20],

    /// SHA1 hash of the compressed data.
    pub sha1: [u8; 20],

    /// SHA1 hash of the parent CHD file, otherwise all zeros.
    pub parent_sha1: [u8; 20],
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

    pub reserved: [u8; 8], // 8 bytes of zeros

    #[br(count = {
        let len = ((length_raw[0] as u32) << 16) |
                  ((length_raw[1] as u32) << 8) |
                  (length_raw[2] as u32);
        len
    })]
    pub data: Vec<u8>, // The actual metadata string
}

impl ChdMetadataHeader {
    pub fn new_cd_metadata(metadata_string: String) -> Self {
        let data = metadata_string.into_bytes();

        Self {
            tag: *b"CHT2",
            flags: 0x01,
            reserved: [0; 8],
            data,
        }
    }

    pub fn write_with_padding<W: Write + Seek>(
        &self,
        writer: &mut W,
    ) -> Result<usize, std::io::Error> {
        use byteorder::{BigEndian, WriteBytesExt};

        let start_pos = writer.stream_position()?;

        // Write tag
        writer.write_all(&self.tag)?;

        // Write flags
        writer.write_u8(self.flags)?;

        // Write 24-bit length
        let len = self.data.len() as u32;
        writer.write_u8((len >> 16) as u8)?;
        writer.write_u8((len >> 8) as u8)?;
        writer.write_u8(len as u8)?;

        // Write reserved
        writer.write_all(&self.reserved)?;

        // Write data
        writer.write_all(&self.data)?;

        // Calculate padding to 4-byte boundary
        let written = (writer.stream_position()? - start_pos) as usize;
        let padding = ((written + 3) & !3) - written;

        if padding > 0 {
            writer.write_all(&vec![0u8; padding])?;
        }

        Ok(written + padding)
    }
}
