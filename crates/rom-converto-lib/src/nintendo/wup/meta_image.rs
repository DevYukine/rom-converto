//! Wii U `meta/*.tga` files are standard uncompressed type-2
//! Truevision TGAs (BGRA or BGR, bottom-up), not GX2-tiled BC3
//! textures. Cemu, decaf-emu, rom-properties, and the devkitPro
//! `wuhbtool` all treat them as plain TGA, so no GX2 deswizzler
//! is needed.

use anyhow::{Context, Result, anyhow};
use std::io::Cursor;

use crate::info::Image;
use crate::util::pixel::encode_png;

const TGA_HEADER_PREFIX: [u8; 12] = [0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const MIN_TGA_SIZE: usize = 18;
const MAX_TGA_SIZE: usize = 8 * 1024 * 1024;

pub fn decode_meta_tga(bytes: &[u8]) -> Result<Image> {
    validate_meta_tga_magic(bytes)?;
    let reader = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Tga);
    let decoded = reader
        .decode()
        .context("decode Wii U meta tga via image crate")?;
    let rgba = decoded.into_rgba8();
    let (width, height) = rgba.dimensions();
    let png_bytes = encode_png(&rgba.into_raw(), width, height)?;
    Ok(Image::new(png_bytes, width, height))
}

fn validate_meta_tga_magic(bytes: &[u8]) -> Result<()> {
    if bytes.len() < MIN_TGA_SIZE {
        return Err(anyhow!(
            "wup meta tga: buffer too small ({} bytes, need at least {})",
            bytes.len(),
            MIN_TGA_SIZE
        ));
    }
    if bytes.len() > MAX_TGA_SIZE {
        return Err(anyhow!(
            "wup meta tga: buffer too large ({} bytes, max {})",
            bytes.len(),
            MAX_TGA_SIZE
        ));
    }
    if bytes[..12] != TGA_HEADER_PREFIX {
        return Err(anyhow!(
            "wup meta tga: header magic does not match Wii U uncompressed TGA layout"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_minimal_bgra_tga(width: u16, height: u16) -> Vec<u8> {
        let pixel_count = width as usize * height as usize;
        let mut buf = Vec::with_capacity(18 + pixel_count * 4 + 26);
        buf.extend_from_slice(&TGA_HEADER_PREFIX);
        buf.extend_from_slice(&width.to_le_bytes());
        buf.extend_from_slice(&height.to_le_bytes());
        buf.push(32);
        buf.push(0x28);
        for _ in 0..pixel_count {
            buf.push(0xFF);
            buf.push(0x00);
            buf.push(0x00);
            buf.push(0xFF);
        }
        buf
    }

    #[test]
    fn decodes_minimal_bgra_tga_and_encodes_png() {
        let tga = build_minimal_bgra_tga(2, 2);
        let img = decode_meta_tga(&tga).unwrap();
        assert_eq!(img.width, 2);
        assert_eq!(img.height, 2);

        let decoder = png::Decoder::new(Cursor::new(&img.png_bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!(info.width, 2);
        assert_eq!(info.height, 2);
        assert_eq!(info.color_type, png::ColorType::Rgba);
        for chunk in buf[..info.buffer_size()].chunks_exact(4) {
            assert_eq!(chunk, &[0x00, 0x00, 0xFF, 0xFF]);
        }
    }

    #[test]
    fn rejects_invalid_magic() {
        let mut tga = build_minimal_bgra_tga(2, 2);
        tga[2] = 0x0A;
        let err = decode_meta_tga(&tga).unwrap_err();
        assert!(format!("{err}").contains("magic"));
    }

    #[test]
    fn rejects_too_small_buffer() {
        let err = decode_meta_tga(&[0u8; 4]).unwrap_err();
        assert!(format!("{err}").contains("too small"));
    }
}
