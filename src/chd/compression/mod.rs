use crate::chd::error::ChdResult;

pub mod cdfl;
pub mod cdlz;
pub mod cdzl;
pub mod cdzs;
pub mod flac;
pub mod lzma;
pub mod zlib;
pub mod zstd;

// Convert tag to FourCC bytes
pub const fn tag_to_bytes(tag: &str) -> [u8; 4] {
    let bytes = tag.as_bytes();
    assert!(bytes.len() == 4, "tag must be exactly 4 bytes");
    [bytes[0], bytes[1], bytes[2], bytes[3]]
}

// IMPORTANT: These values map to positions in the header, not codec IDs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChdCompression {
    Codec0 = 0, // First codec in header
    Codec1 = 1, // Second codec in header
    Codec2 = 2, // Third codec in header
    Codec3 = 3, // Fourth codec in header
    None = 4,   // Uncompressed
    Self_ = 5,  // Same as another hunk
    Parent = 6, // From parent CHD
}

pub trait ChdCompressor {
    fn name(&self) -> &'static str;
    fn tag_bytes(&self) -> [u8; 4];
    fn compress(&self, data: &[u8]) -> ChdResult<Vec<u8>>;
}
