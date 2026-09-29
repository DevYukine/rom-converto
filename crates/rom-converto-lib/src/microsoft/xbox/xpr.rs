//! XPR0 (Xbox Packed Resource) decoding for the title image an XBE carries
//! in its `$$XTIMAGE` section.
//!
//! An XPR0 file is a 12-byte container header (magic, total size, offset to
//! the pixel data) followed by the `D3DPixelContainer` the resource was
//! created from. Its `Format` dword packs the pixel format and the log2 of
//! each axis, so no separate dimension fields exist.

use super::xbe::read_u32;
use crate::info::Image;
use crate::util::pixel::{decode_a8r8g8b8_swizzled, decode_dxt1, encode_png};

const MAGIC: &[u8; 4] = b"XPR0";
const DATA_OFFSET_OFFSET: usize = 0x08;
/// `Format` is the fourth dword of the `D3DPixelContainer` at 0x0C.
const FORMAT_OFFSET: usize = 0x18;

const FORMAT_MASK: u32 = 0x0000_FF00;
const FORMAT_SHIFT: u32 = 8;
const USIZE_MASK: u32 = 0x00F0_0000;
const USIZE_SHIFT: u32 = 20;
const VSIZE_MASK: u32 = 0x0F00_0000;
const VSIZE_SHIFT: u32 = 24;

const FMT_A8R8G8B8: u32 = 0x06;
const FMT_DXT1: u32 = 0x0C;

/// Largest icon accepted. Retail title images are 128x128; anything much
/// larger is a misread format dword rather than an icon.
const MAX_DIMENSION: u32 = 512;

/// Everything [`decode_xpr0_parts`] needs from the 0x20-byte XPR0 header.
pub(super) struct Xpr0Layout {
    pub(super) data_offset: usize,
    pub(super) pixels_len: usize,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) format: u32,
}

pub(super) fn xpr0_layout(header: &[u8], section_size: usize) -> Option<Xpr0Layout> {
    if header.get(0..4)? != MAGIC {
        return None;
    }
    let data_offset = read_u32(header, DATA_OFFSET_OFFSET)? as usize;
    let format = read_u32(header, FORMAT_OFFSET)?;
    let width = 1u32.checked_shl((format & USIZE_MASK) >> USIZE_SHIFT)?;
    let height = 1u32.checked_shl((format & VSIZE_MASK) >> VSIZE_SHIFT)?;
    if width > MAX_DIMENSION || height > MAX_DIMENSION || data_offset < 0x20 {
        return None;
    }
    let pixels_len = match (format & FORMAT_MASK) >> FORMAT_SHIFT {
        FMT_DXT1 => width
            .div_ceil(4)
            .checked_mul(height.div_ceil(4))?
            .checked_mul(8)?,
        FMT_A8R8G8B8 => width.checked_mul(height)?.checked_mul(4)?,
        _ => return None,
    } as usize;
    let end = data_offset.checked_add(pixels_len)?;
    if end > section_size {
        return None;
    }
    Some(Xpr0Layout {
        data_offset,
        pixels_len,
        width,
        height,
        format,
    })
}

pub(super) fn decode_xpr0_parts(layout: Xpr0Layout, pixels: &[u8]) -> Option<Image> {
    let rgba = match (layout.format & FORMAT_MASK) >> FORMAT_SHIFT {
        FMT_DXT1 => decode_dxt1(pixels, layout.width, layout.height).ok()?,
        FMT_A8R8G8B8 => decode_a8r8g8b8_swizzled(pixels, layout.width, layout.height).ok()?,
        _ => return None,
    };
    Some(Image::new(
        encode_png(&rgba, layout.width, layout.height).ok()?,
        layout.width,
        layout.height,
    ))
}

/// Decodes an XPR0 texture into a PNG-backed [`Image`].
#[cfg(test)]
fn decode_xpr0(bytes: &[u8]) -> Option<Image> {
    let header = bytes.get(..0x20)?;
    let layout = xpr0_layout(header, bytes.len())?;
    let pixels =
        bytes.get(layout.data_offset..layout.data_offset.checked_add(layout.pixels_len)?)?;
    decode_xpr0_parts(layout, pixels)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// XPR0 holding a single solid-blue 4x4 DXT1 block.
    pub fn build_xpr0_dxt1_4x4() -> Vec<u8> {
        build_xpr0(FMT_DXT1, 2, 2, &solid_dxt1_block())
    }

    fn solid_dxt1_block() -> Vec<u8> {
        let mut block = Vec::new();
        block.extend_from_slice(&0x001Fu16.to_le_bytes());
        block.extend_from_slice(&0x001Fu16.to_le_bytes());
        block.extend_from_slice(&[0u8; 4]);
        block
    }

    fn build_xpr0(format: u32, log2_width: u32, log2_height: u32, pixels: &[u8]) -> Vec<u8> {
        let data_offset = 0x20u32;
        let mut buf = vec![0u8; data_offset as usize];
        buf[0..4].copy_from_slice(MAGIC);
        let total = data_offset as usize + pixels.len();
        buf[4..8].copy_from_slice(&(total as u32).to_le_bytes());
        buf[DATA_OFFSET_OFFSET..DATA_OFFSET_OFFSET + 4].copy_from_slice(&data_offset.to_le_bytes());
        let format_dword =
            (format << FORMAT_SHIFT) | (log2_width << USIZE_SHIFT) | (log2_height << VSIZE_SHIFT);
        buf[FORMAT_OFFSET..FORMAT_OFFSET + 4].copy_from_slice(&format_dword.to_le_bytes());
        buf.extend_from_slice(pixels);
        buf
    }

    #[test]
    fn decodes_a_dxt1_texture() {
        let image = decode_xpr0(&build_xpr0_dxt1_4x4()).expect("icon decoded");
        assert_eq!((image.width, image.height), (4, 4));
        assert_eq!(&image.png_bytes[..4], &[0x89, b'P', b'N', b'G']);
    }

    #[test]
    fn decodes_a_swizzled_a8r8g8b8_texture() {
        let pixels = vec![0xAAu8; 4 * 4 * 4];
        let image = decode_xpr0(&build_xpr0(FMT_A8R8G8B8, 2, 2, &pixels)).expect("icon decoded");
        assert_eq!((image.width, image.height), (4, 4));
    }

    #[test]
    fn bad_magic_returns_none() {
        let mut buf = build_xpr0_dxt1_4x4();
        buf[0] = b'Y';
        assert!(decode_xpr0(&buf).is_none());
    }

    #[test]
    fn unsupported_format_returns_none() {
        // 0x05 is R5G6B5, which this decoder deliberately does not handle.
        assert!(decode_xpr0(&build_xpr0(0x05, 2, 2, &[0u8; 32])).is_none());
    }

    #[test]
    fn implausible_dimensions_return_none() {
        assert!(decode_xpr0(&build_xpr0(FMT_DXT1, 12, 12, &[0u8; 32])).is_none());
    }

    #[test]
    fn truncated_pixel_data_returns_none() {
        assert!(decode_xpr0(&build_xpr0(FMT_DXT1, 2, 2, &[0u8; 4])).is_none());
    }

    #[test]
    fn data_offset_past_eof_returns_none() {
        let mut buf = build_xpr0_dxt1_4x4();
        let past = buf.len() as u32 + 0x100;
        buf[DATA_OFFSET_OFFSET..DATA_OFFSET_OFFSET + 4].copy_from_slice(&past.to_le_bytes());
        assert!(decode_xpr0(&buf).is_none());
    }
}
