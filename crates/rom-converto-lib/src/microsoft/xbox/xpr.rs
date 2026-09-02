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

/// Decodes an XPR0 texture into a PNG-backed [`Image`].
///
/// `None` for anything unrecognized: a bad magic, a pixel format other than
/// DXT1 or swizzled A8R8G8B8, implausible dimensions, or a truncated payload.
pub(super) fn decode_xpr0(bytes: &[u8]) -> Option<Image> {
    if bytes.get(0..4)? != MAGIC {
        return None;
    }
    let data_offset = read_u32(bytes, DATA_OFFSET_OFFSET)? as usize;
    let format = read_u32(bytes, FORMAT_OFFSET)?;

    let width = 1u32 << ((format & USIZE_MASK) >> USIZE_SHIFT);
    let height = 1u32 << ((format & VSIZE_MASK) >> VSIZE_SHIFT);
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return None;
    }

    let data = bytes.get(data_offset..)?;
    let rgba = match (format & FORMAT_MASK) >> FORMAT_SHIFT {
        FMT_DXT1 => decode_dxt1(data, width, height).ok()?,
        FMT_A8R8G8B8 => decode_a8r8g8b8_swizzled(data, width, height).ok()?,
        _ => return None,
    };
    Some(Image::new(
        encode_png(&rgba, width, height).ok()?,
        width,
        height,
    ))
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
