//! Pixel-format decoders and PNG encoder shared by per-console info extractors.
//!
//! Console icons live in unusual tiled / swizzled layouts. Decoders here turn
//! those raw bytes into linear RGBA8888 buffers; [`encode_png`] turns an RGBA8
//! buffer into a PNG byte vector suitable for embedding in [`crate::info`].

use anyhow::{Context, Result, anyhow};
use std::io::Cursor;

/// Decode RGB565 pixels stored in 8x8 Morton-tiled order.
///
/// This is the layout the 3DS GPU uses (and therefore what SMDH icons sit in).
/// Within each 8x8 tile pixel bytes follow a Z-order curve; tiles themselves
/// are row-major.
///
/// `width` and `height` must both be multiples of 8.
pub fn decode_rgb565_morton_tiled(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    if !width.is_multiple_of(8) || !height.is_multiple_of(8) {
        return Err(anyhow!(
            "rgb565 morton dimensions must be multiples of 8 (got {}x{})",
            width,
            height
        ));
    }
    let expected = (width as usize) * (height as usize) * 2;
    if data.len() < expected {
        return Err(anyhow!(
            "rgb565 morton input too short: {} bytes for {}x{}",
            data.len(),
            width,
            height
        ));
    }

    let tiles_x = width / 8;
    let mut out = vec![0u8; (width * height * 4) as usize];

    for y in 0..height {
        for x in 0..width {
            let tile_x = x / 8;
            let tile_y = y / 8;
            let lx = x & 7;
            let ly = y & 7;

            let tile_idx = tile_y * tiles_x + tile_x;
            let in_tile = morton_index_8x8(lx, ly);
            let byte_off = ((tile_idx * 64) + in_tile) * 2;

            let lo = data[byte_off as usize] as u16;
            let hi = data[byte_off as usize + 1] as u16;
            let pixel = lo | (hi << 8);

            let (r, g, b) = rgb565_to_rgb8(pixel);
            let out_off = ((y * width + x) * 4) as usize;
            out[out_off] = r;
            out[out_off + 1] = g;
            out[out_off + 2] = b;
            out[out_off + 3] = 0xFF;
        }
    }

    Ok(out)
}

/// Decode RGB5A3 pixels stored in 4x4-tiled order.
///
/// Layout used by GameCube `opening.bnr` (96x32) and Wii `banner.bin` /
/// `icon.bin` (192x64 / 48x48). Pixels are 16-bit big-endian; if the top
/// bit is set the remaining 15 bits are 5R/5G/5B opaque, otherwise they
/// are 3A/4R/4G/4B translucent.
///
/// `width` and `height` must both be multiples of 4.
pub fn decode_rgb5a3_tiled(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    if !width.is_multiple_of(4) || !height.is_multiple_of(4) {
        return Err(anyhow!(
            "rgb5a3 dimensions must be multiples of 4 (got {}x{})",
            width,
            height
        ));
    }
    let expected = (width as usize) * (height as usize) * 2;
    if data.len() < expected {
        return Err(anyhow!(
            "rgb5a3 input too short: {} bytes for {}x{}",
            data.len(),
            width,
            height
        ));
    }

    let tiles_x = width / 4;
    let mut out = vec![0u8; (width * height * 4) as usize];

    for y in 0..height {
        for x in 0..width {
            let tile_x = x / 4;
            let tile_y = y / 4;
            let lx = x & 3;
            let ly = y & 3;

            let tile_idx = tile_y * tiles_x + tile_x;
            let in_tile = ly * 4 + lx;
            let byte_off = ((tile_idx * 16) + in_tile) * 2;

            let hi = data[byte_off as usize] as u16;
            let lo = data[byte_off as usize + 1] as u16;
            let pixel = (hi << 8) | lo;

            let (r, g, b, a) = rgb5a3_to_rgba8(pixel);
            let out_off = ((y * width + x) * 4) as usize;
            out[out_off] = r;
            out[out_off + 1] = g;
            out[out_off + 2] = b;
            out[out_off + 3] = a;
        }
    }

    Ok(out)
}

/// Decode GameCube/Wii TPL format 6 (RGBA32) tiled pixel data into RGBA8.
///
/// Each 4x4 tile is 64 bytes split into two planes: bytes 0..32 hold
/// (A, R) pairs for the 16 pixels in raster order, bytes 32..64 hold
/// the matching (G, B) pairs.
pub fn decode_rgba32_tiled(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    if !width.is_multiple_of(4) || !height.is_multiple_of(4) {
        return Err(anyhow!(
            "decode_rgba32_tiled: dimensions must be multiples of 4 (got {}x{})",
            width,
            height
        ));
    }
    let expected = (width as usize) * (height as usize) * 4;
    if data.len() < expected {
        return Err(anyhow!(
            "decode_rgba32_tiled: buffer is {} bytes, expected at least {} ({}x{} RGBA32)",
            data.len(),
            expected,
            width,
            height
        ));
    }

    let tiles_x = (width / 4) as usize;
    let tiles_y = (height / 4) as usize;
    let mut out = vec![0u8; expected];

    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let tile_off = (ty * tiles_x + tx) * 64;
            for py in 0..4usize {
                for px in 0..4usize {
                    let in_tile = py * 4 + px;
                    let ar_off = tile_off + in_tile * 2;
                    let gb_off = tile_off + 32 + in_tile * 2;
                    let a = data[ar_off];
                    let r = data[ar_off + 1];
                    let g = data[gb_off];
                    let b = data[gb_off + 1];
                    let x = tx * 4 + px;
                    let y = ty * 4 + py;
                    let out_off = ((y * width as usize) + x) * 4;
                    out[out_off] = r;
                    out[out_off + 1] = g;
                    out[out_off + 2] = b;
                    out[out_off + 3] = a;
                }
            }
        }
    }
    Ok(out)
}

pub fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let expected = (width as usize) * (height as usize) * 4;
    if rgba.len() != expected {
        return Err(anyhow!(
            "encode_png: buffer is {} bytes, expected {} ({}x{} RGBA8)",
            rgba.len(),
            expected,
            width,
            height
        ));
    }

    let mut out = Vec::with_capacity(rgba.len() / 4);
    {
        let cursor = Cursor::new(&mut out);
        let mut encoder = png::Encoder::new(cursor, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().context("png: write header")?;
        writer.write_image_data(rgba).context("png: write data")?;
    }
    Ok(out)
}

#[inline]
fn morton_index_8x8(x: u32, y: u32) -> u32 {
    // 3-bit interleave for an 8x8 tile, 3DS convention. Bit layout
    // (lsb to msb): x0 y0 x1 y1 x2 y2.
    (x & 1) | ((y & 1) << 1) | ((x & 2) << 1) | ((y & 2) << 2) | ((x & 4) << 2) | ((y & 4) << 3)
}

#[inline]
fn rgb5a3_to_rgba8(pixel: u16) -> (u8, u8, u8, u8) {
    if pixel & 0x8000 != 0 {
        // Opaque branch: 0 _ rrrrr ggggg bbbbb
        let r5 = ((pixel >> 10) & 0x1F) as u8;
        let g5 = ((pixel >> 5) & 0x1F) as u8;
        let b5 = (pixel & 0x1F) as u8;
        let r = (r5 << 3) | (r5 >> 2);
        let g = (g5 << 3) | (g5 >> 2);
        let b = (b5 << 3) | (b5 >> 2);
        (r, g, b, 0xFF)
    } else {
        // Translucent branch: 0 aaa rrrr gggg bbbb
        let a3 = ((pixel >> 12) & 0x7) as u8;
        let r4 = ((pixel >> 8) & 0xF) as u8;
        let g4 = ((pixel >> 4) & 0xF) as u8;
        let b4 = (pixel & 0xF) as u8;
        let a = (a3 << 5) | (a3 << 2) | (a3 >> 1);
        let r = (r4 << 4) | r4;
        let g = (g4 << 4) | g4;
        let b = (b4 << 4) | b4;
        (r, g, b, a)
    }
}

#[inline]
fn rgb565_to_rgb8(pixel: u16) -> (u8, u8, u8) {
    let r5 = ((pixel >> 11) & 0x1F) as u8;
    let g6 = ((pixel >> 5) & 0x3F) as u8;
    let b5 = (pixel & 0x1F) as u8;
    let r = (r5 << 3) | (r5 >> 2);
    let g = (g6 << 2) | (g6 >> 4);
    let b = (b5 << 3) | (b5 >> 2);
    (r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn morton_index_walks_z_order() {
        // First 2x2 sub-block yields 0,1,2,3.
        assert_eq!(morton_index_8x8(0, 0), 0);
        assert_eq!(morton_index_8x8(1, 0), 1);
        assert_eq!(morton_index_8x8(0, 1), 2);
        assert_eq!(morton_index_8x8(1, 1), 3);
        // Next 2x2 sub-block (right of first) starts at 4.
        assert_eq!(morton_index_8x8(2, 0), 4);
        assert_eq!(morton_index_8x8(3, 0), 5);
        assert_eq!(morton_index_8x8(2, 1), 6);
        assert_eq!(morton_index_8x8(3, 1), 7);
        // Far corner of the 8x8 tile is 63.
        assert_eq!(morton_index_8x8(7, 7), 63);
    }

    #[test]
    fn rgb565_round_trips_known_pixels() {
        // RGB565 white = 0xFFFF
        assert_eq!(rgb565_to_rgb8(0xFFFF), (0xFF, 0xFF, 0xFF));
        // RGB565 black = 0x0000
        assert_eq!(rgb565_to_rgb8(0x0000), (0x00, 0x00, 0x00));
        // RGB565 pure red = 0xF800 -> (0xFF, 0x00, 0x00)
        assert_eq!(rgb565_to_rgb8(0xF800), (0xFF, 0x00, 0x00));
        // RGB565 pure green = 0x07E0 -> (0x00, 0xFF, 0x00)
        assert_eq!(rgb565_to_rgb8(0x07E0), (0x00, 0xFF, 0x00));
        // RGB565 pure blue = 0x001F -> (0x00, 0x00, 0xFF)
        assert_eq!(rgb565_to_rgb8(0x001F), (0x00, 0x00, 0xFF));
    }

    #[test]
    fn decode_rgb565_single_tile_solid_color() {
        // One 8x8 tile filled with pure red (0xF800 -> RGB 0xFF0000).
        let mut data = Vec::with_capacity(8 * 8 * 2);
        for _ in 0..64 {
            data.push(0x00);
            data.push(0xF8);
        }
        let rgba = decode_rgb565_morton_tiled(&data, 8, 8).unwrap();
        assert_eq!(rgba.len(), 8 * 8 * 4);
        for chunk in rgba.chunks_exact(4) {
            assert_eq!(chunk, &[0xFF, 0x00, 0x00, 0xFF]);
        }
    }

    #[test]
    fn decode_rgb565_morton_first_pixel_lookup() {
        // Position the bytes so that linear (x=0, y=0) reads back red and
        // (x=1, y=0) reads back green. In Morton-tiled order these are at
        // byte offsets 0 and 2 respectively, since morton_index(0,0)=0 and
        // morton_index(1,0)=1.
        let mut data = vec![0u8; 8 * 8 * 2];
        data[0] = 0x00;
        data[1] = 0xF8; // red at morton idx 0
        data[2] = 0xE0;
        data[3] = 0x07; // green at morton idx 1
        let rgba = decode_rgb565_morton_tiled(&data, 8, 8).unwrap();
        assert_eq!(&rgba[0..4], &[0xFF, 0x00, 0x00, 0xFF]);
        assert_eq!(&rgba[4..8], &[0x00, 0xFF, 0x00, 0xFF]);
    }

    #[test]
    fn decode_rgb565_rejects_non_multiple_of_8() {
        assert!(decode_rgb565_morton_tiled(&[0u8; 100], 7, 8).is_err());
        assert!(decode_rgb565_morton_tiled(&[0u8; 100], 8, 7).is_err());
    }

    #[test]
    fn decode_rgb565_rejects_short_input() {
        assert!(decode_rgb565_morton_tiled(&[0u8; 10], 8, 8).is_err());
    }

    #[test]
    fn encode_png_round_trips_via_decode() {
        // 2x2 RGBA -> PNG -> decode -> same RGBA
        let rgba = vec![
            0xFF, 0x00, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF,
            0xFF, 0xFF,
        ];
        let png_bytes = encode_png(&rgba, 2, 2).unwrap();

        let decoder = png::Decoder::new(Cursor::new(&png_bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!(info.width, 2);
        assert_eq!(info.height, 2);
        assert_eq!(info.color_type, png::ColorType::Rgba);
        assert_eq!(&buf[..info.buffer_size()], &rgba[..]);
    }

    #[test]
    fn encode_png_rejects_wrong_size_buffer() {
        assert!(encode_png(&[0u8; 10], 2, 2).is_err());
    }

    #[test]
    fn rgb5a3_opaque_known_pixels() {
        // 0xFFFF top bit set, 5R/5G/5B all 1 -> white opaque
        assert_eq!(rgb5a3_to_rgba8(0xFFFF), (0xFF, 0xFF, 0xFF, 0xFF));
        // 0x8000 top bit set, all colour bits zero -> black opaque
        assert_eq!(rgb5a3_to_rgba8(0x8000), (0x00, 0x00, 0x00, 0xFF));
        // Pure red opaque: 0_11111_00000_00000 = 0xFC00 | 0x8000 = 0xFC00
        assert_eq!(rgb5a3_to_rgba8(0xFC00), (0xFF, 0x00, 0x00, 0xFF));
    }

    #[test]
    fn rgb5a3_translucent_known_pixels() {
        // 0x0000 top bit clear, alpha 0 -> fully transparent
        assert_eq!(rgb5a3_to_rgba8(0x0000), (0x00, 0x00, 0x00, 0x00));
        // 0x7FFF top bit clear, alpha 7 (max), all colour 0xF -> white fully opaque-ish
        // alpha: 7 -> 0b111<<5 | 0b111<<2 | 0b111>>1 = 0xE0 | 0x1C | 0x3 = 0xFF
        // colour: 0xF -> (0xF << 4) | 0xF = 0xFF
        assert_eq!(rgb5a3_to_rgba8(0x7FFF), (0xFF, 0xFF, 0xFF, 0xFF));
    }

    #[test]
    fn decode_rgb5a3_solid_red_tile() {
        // One 4x4 tile filled with opaque red (0xFC00 big-endian = bytes [0xFC, 0x00]).
        let mut data = Vec::with_capacity(4 * 4 * 2);
        for _ in 0..16 {
            data.push(0xFC);
            data.push(0x00);
        }
        let rgba = decode_rgb5a3_tiled(&data, 4, 4).unwrap();
        assert_eq!(rgba.len(), 4 * 4 * 4);
        for chunk in rgba.chunks_exact(4) {
            assert_eq!(chunk, &[0xFF, 0x00, 0x00, 0xFF]);
        }
    }

    #[test]
    fn decode_rgb5a3_tiles_are_row_major() {
        // 8x4 image = 2 tiles horizontally, 1 vertically. First tile red, second tile blue.
        let mut data = Vec::with_capacity(8 * 4 * 2);
        for _ in 0..16 {
            data.push(0xFC);
            data.push(0x00);
        }
        for _ in 0..16 {
            data.push(0x80);
            data.push(0x1F);
        }
        let rgba = decode_rgb5a3_tiled(&data, 8, 4).unwrap();

        // Pixel (0,0) is in the first tile -> red
        assert_eq!(&rgba[0..4], &[0xFF, 0x00, 0x00, 0xFF]);
        // Pixel (4,0) is in the second tile -> blue
        let off = 4 * 4;
        assert_eq!(&rgba[off..off + 4], &[0x00, 0x00, 0xFF, 0xFF]);
    }

    #[test]
    fn decode_rgb5a3_rejects_non_multiple_of_4() {
        assert!(decode_rgb5a3_tiled(&[0u8; 200], 3, 4).is_err());
        assert!(decode_rgb5a3_tiled(&[0u8; 200], 4, 3).is_err());
    }

    #[test]
    fn decode_rgb5a3_rejects_short_input() {
        assert!(decode_rgb5a3_tiled(&[0u8; 10], 4, 4).is_err());
    }
}
