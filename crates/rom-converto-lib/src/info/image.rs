//! Icon pixel decoding and PNG encoding for each platform's embedded
//! artwork. All extractors normalize to PNG so the GUI can render the bytes
//! through a single `data:image/png;base64,...` path without per-console
//! branching.

use serde::{Deserialize, Serialize};

/// `png_bytes` is a complete PNG file. Width and height describe the
/// decoded image so callers do not need to parse the PNG header to render.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Image {
    pub png_bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

const PNG_SIGNATURE: &[u8; 4] = &[0x89, 0x50, 0x4E, 0x47];
const PNG_IHDR_TAG_OFFSET: usize = 12;
const PNG_IHDR_WIDTH_OFFSET: usize = 16;
const PNG_IHDR_HEIGHT_OFFSET: usize = 20;

impl Image {
    /// Builds an [`Image`] from an encoded PNG and its decoded dimensions.
    pub fn new(png_bytes: Vec<u8>, width: u32, height: u32) -> Self {
        Self {
            png_bytes,
            width,
            height,
        }
    }

    /// Wraps PNG bytes, taking the dimensions from the IHDR chunk instead
    /// of decoding the image. `None` when the bytes are not a PNG whose
    /// first chunk is IHDR.
    pub fn from_png(png_bytes: Vec<u8>) -> Option<Self> {
        if !png_bytes.starts_with(PNG_SIGNATURE)
            || png_bytes.get(PNG_IHDR_TAG_OFFSET..PNG_IHDR_TAG_OFFSET + 4) != Some(b"IHDR")
        {
            return None;
        }
        let width = be_u32(&png_bytes, PNG_IHDR_WIDTH_OFFSET)?;
        let height = be_u32(&png_bytes, PNG_IHDR_HEIGHT_OFFSET)?;
        Some(Self::new(png_bytes, width, height))
    }
}

fn be_u32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}
