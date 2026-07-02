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

impl Image {
    pub fn new(png_bytes: Vec<u8>, width: u32, height: u32) -> Self {
        Self {
            png_bytes,
            width,
            height,
        }
    }
}
