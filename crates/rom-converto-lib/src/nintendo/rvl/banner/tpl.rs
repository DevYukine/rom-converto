//! TPL texture container parsing.
//!
//! A TPL holds one or more GX textures with an optional colour-index palette
//! each. Banner layouts only ever reference the first image of a file, so
//! [`decode_first`] is the whole public surface.

use crate::util::pixel::{
    decode_ci4_tiled, decode_ci8_tiled, decode_cmpr_tiled, decode_i4_tiled, decode_i8_tiled,
    decode_ia4_tiled, decode_ia8_tiled, decode_rgb5a3_tiled, decode_rgb565_gx_tiled,
    decode_rgba32_tiled, rgb5a3_to_rgba8, rgb565_to_rgb8,
};
use anyhow::{Result, anyhow};
use byteorder::{BE, ByteOrder};

const TPL_MAGIC: u32 = 0x0020_AF30;

/// A decoded RGBA8 texture.
pub(super) struct Texture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Texture {
    /// Returns the texel at `(x, y)`, which must be inside the texture.
    pub(super) fn texel(&self, x: usize, y: usize) -> [u8; 4] {
        let off = (y * self.width as usize + x) * 4;
        [
            self.rgba[off],
            self.rgba[off + 1],
            self.rgba[off + 2],
            self.rgba[off + 3],
        ]
    }
}

/// Decodes the first image of a TPL file into an RGBA8 texture.
pub(super) fn decode_first(tpl: &[u8]) -> Result<Texture> {
    if be_u32(tpl, 0)? != TPL_MAGIC {
        return Err(anyhow!("tpl: bad magic"));
    }
    if be_u32(tpl, 4)? == 0 {
        return Err(anyhow!("tpl: no images"));
    }
    let table = be_u32(tpl, 8)? as usize;
    let image_header = be_u32(tpl, table)? as usize;
    let palette_header = be_u32(tpl, table + 4)? as usize;

    let height = be_u16(tpl, image_header)? as u32;
    let width = be_u16(tpl, image_header + 2)? as u32;
    let format = be_u32(tpl, image_header + 4)?;
    let data_offset = be_u32(tpl, image_header + 8)? as usize;
    if width == 0 || height == 0 {
        return Err(anyhow!("tpl: zero-sized image {}x{}", width, height));
    }

    let (tile_w, tile_h, bits) = match format {
        0 => (8, 8, 4),
        1 => (8, 4, 8),
        2 => (8, 4, 8),
        3 => (4, 4, 16),
        4 => (4, 4, 16),
        5 => (4, 4, 16),
        6 => (4, 4, 32),
        8 => (8, 8, 4),
        9 => (8, 4, 8),
        0xE => (8, 8, 4),
        other => return Err(anyhow!("tpl: unsupported pixel format {}", other)),
    };
    let pw = width.next_multiple_of(tile_w);
    let ph = height.next_multiple_of(tile_h);
    let size = (pw as usize) * (ph as usize) * bits / 8;
    let pixels = tpl
        .get(data_offset..data_offset.saturating_add(size))
        .ok_or_else(|| anyhow!("tpl: pixel data past end of buffer"))?;

    let rgba = match format {
        0 => decode_i4_tiled(pixels, pw, ph)?,
        1 => decode_i8_tiled(pixels, pw, ph)?,
        2 => decode_ia4_tiled(pixels, pw, ph)?,
        3 => decode_ia8_tiled(pixels, pw, ph)?,
        4 => decode_rgb565_gx_tiled(pixels, pw, ph)?,
        5 => decode_rgb5a3_tiled(pixels, pw, ph)?,
        6 => decode_rgba32_tiled(pixels, pw, ph)?,
        8 => decode_ci4_tiled(pixels, pw, ph, &parse_palette(tpl, palette_header)?)?,
        9 => decode_ci8_tiled(pixels, pw, ph, &parse_palette(tpl, palette_header)?)?,
        _ => decode_cmpr_tiled(pixels, pw, ph)?,
    };

    Ok(Texture {
        width,
        height,
        rgba: crop(rgba, pw, ph, width, height),
    })
}

fn parse_palette(tpl: &[u8], header: usize) -> Result<Vec<[u8; 4]>> {
    if header == 0 {
        return Err(anyhow!("tpl: colour-indexed image without a palette"));
    }
    let count = be_u16(tpl, header)? as usize;
    let format = be_u32(tpl, header + 4)?;
    let data_offset = be_u32(tpl, header + 8)? as usize;
    let entries = tpl
        .get(data_offset..data_offset.saturating_add(count * 2))
        .ok_or_else(|| anyhow!("tpl: palette data past end of buffer"))?;

    entries
        .as_chunks::<2>()
        .0
        .iter()
        .map(|e| {
            let v = u16::from_be_bytes(*e);
            match format {
                0 => Ok([e[1], e[1], e[1], e[0]]),
                1 => {
                    let (r, g, b) = rgb565_to_rgb8(v);
                    Ok([r, g, b, 0xFF])
                }
                2 => {
                    let (r, g, b, a) = rgb5a3_to_rgba8(v);
                    Ok([r, g, b, a])
                }
                other => Err(anyhow!("tpl: unsupported palette format {}", other)),
            }
        })
        .collect()
}

fn crop(rgba: Vec<u8>, pw: u32, ph: u32, width: u32, height: u32) -> Vec<u8> {
    if pw == width && ph == height {
        return rgba;
    }
    let (w, pw) = (width as usize, pw as usize);
    let mut out = Vec::with_capacity(w * height as usize * 4);
    for y in 0..height as usize {
        let row = y * pw * 4;
        out.extend_from_slice(&rgba[row..row + w * 4]);
    }
    out
}

fn be_u16(data: &[u8], off: usize) -> Result<u16> {
    data.get(off..off + 2)
        .map(BE::read_u16)
        .ok_or_else(|| anyhow!("tpl: read past end at 0x{:X}", off))
}

fn be_u32(data: &[u8], off: usize) -> Result<u32> {
    data.get(off..off + 4)
        .map(BE::read_u32)
        .ok_or_else(|| anyhow!("tpl: read past end at 0x{:X}", off))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::rvl::banner::test_fixtures::{build_tpl, build_tpl_ci8};

    #[test]
    fn decodes_rgb5a3_image() {
        // 0xFC00 is RGB5A3 opaque red.
        let tpl = build_tpl(4, 4, 5, &[0xFC, 0x00].repeat(16), None);
        let tex = decode_first(&tpl).expect("rgb5a3 tpl must decode");
        assert_eq!((tex.width, tex.height), (4, 4));
        assert!(
            tex.rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [0xFF, 0, 0, 0xFF])
        );
    }

    #[test]
    fn decodes_ci8_image_through_its_palette() {
        // Palette entry 0 = opaque red, entry 1 = opaque blue (RGB5A3).
        let palette = [0xFCu8, 0x00, 0x80, 0x1F];
        let mut pixels = vec![0u8; 32];
        pixels[1] = 1;
        let tpl = build_tpl_ci8(8, 4, &pixels, &palette);
        let tex = decode_first(&tpl).expect("ci8 tpl must decode");
        assert_eq!(tex.texel(0, 0), [0xFF, 0, 0, 0xFF]);
        assert_eq!(tex.texel(1, 0), [0, 0, 0xFF, 0xFF]);
    }

    #[test]
    fn crops_i4_image_back_to_its_declared_size() {
        // 5x3 pads to 8x8; the decoder must still report 5x3.
        let tpl = build_tpl(5, 3, 0, &[0xFFu8; 8 * 8 / 2], None);
        let tex = decode_first(&tpl).expect("padded i4 tpl must decode");
        assert_eq!((tex.width, tex.height), (5, 3));
        assert_eq!(tex.rgba.len(), 5 * 3 * 4);
        assert!(tex.rgba.iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn rejects_unknown_format() {
        let tpl = build_tpl(4, 4, 7, &[0u8; 64], None);
        assert!(decode_first(&tpl).is_err());
    }
}
