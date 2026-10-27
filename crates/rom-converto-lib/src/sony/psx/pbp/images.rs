//! The images a user gives for the boot screen and the XMB, fitted to the
//! sizes the PSP shows them at.
//!
//! A PNG already at the right size is kept byte for byte. Anything else
//! (another size, or a JPEG such as a cover scan) is scaled to fit inside
//! the frame, keeping its aspect ratio, and centred on the frame's
//! background.

use std::io::Cursor;
use std::path::Path;

use image::imageops::{self, FilterType};
use image::{ImageError, ImageFormat, Rgba, RgbaImage};

use super::error::{PsxPbpError, PsxPbpResult};

pub(crate) const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const IHDR_WIDTH: usize = 16;
const IHDR_HEIGHT: usize = 20;
const IHDR_END: usize = 24;

const TRANSPARENT: Rgba<u8> = Rgba([0, 0, 0, 0]);
const BLACK: Rgba<u8> = Rgba([0, 0, 0, 0xFF]);

/// The size the PSP shows an image at, and what fills the space a fitted
/// image leaves.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Frame {
    pub width: u32,
    pub height: u32,
    pub background: Rgba<u8>,
}

/// The XMB icon. Transparent around the image, as the XMB draws icons over
/// its own background.
pub(crate) const ICON0: Frame = Frame {
    width: 144,
    height: 80,
    background: TRANSPARENT,
};

/// The full-screen picture behind the game's XMB entry.
pub(crate) const PIC1: Frame = Frame {
    width: 480,
    height: 272,
    background: BLACK,
};

/// The full-screen image shown while the emulator starts.
pub(crate) const SPLASH: Frame = PIC1;

/// Reads the image at `path` as a PNG that fills `frame`.
///
/// # Errors
/// Returns [`PsxPbpError::InvalidImage`] when the file is not a PNG or JPEG
/// the decoder can read, and [`PsxPbpError::Io`] when it cannot be read.
pub(crate) fn fit(path: &Path, frame: Frame) -> PsxPbpResult<Vec<u8>> {
    let bytes = std::fs::read(path)?;
    if png_dimensions(&bytes) == Some((frame.width, frame.height)) {
        return Ok(bytes);
    }
    let invalid = |source: ImageError| PsxPbpError::InvalidImage {
        path: path.to_path_buf(),
        source,
    };
    let fitted = fit_image(
        &image::load_from_memory(&bytes)
            .map_err(invalid)?
            .into_rgba8(),
        frame,
    );
    let mut png = Vec::new();
    fitted
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .map_err(invalid)?;
    Ok(png)
}

/// `source` scaled to fit inside `frame` and centred on its background.
fn fit_image(source: &RgbaImage, frame: Frame) -> RgbaImage {
    let (width, height) = source.dimensions();
    let scale = f64::min(
        f64::from(frame.width) / f64::from(width),
        f64::from(frame.height) / f64::from(height),
    );
    let scaled_width = scaled(width, scale, frame.width);
    let scaled_height = scaled(height, scale, frame.height);
    let scaled = imageops::resize(source, scaled_width, scaled_height, FilterType::Lanczos3);

    let mut canvas = RgbaImage::from_pixel(frame.width, frame.height, frame.background);
    imageops::overlay(
        &mut canvas,
        &scaled,
        i64::from((frame.width - scaled_width) / 2),
        i64::from((frame.height - scaled_height) / 2),
    );
    canvas
}

/// `length * scale`, rounded, at least one pixel and at most `limit`.
fn scaled(length: u32, scale: f64, limit: u32) -> u32 {
    // The product is within 0..=limit, so the cast cannot truncate.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let pixels = (f64::from(length) * scale).round() as u32;
    pixels.clamp(1, limit)
}

/// The width and height a PNG's header declares, or `None` for anything
/// that does not start like a PNG.
pub(crate) fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if !bytes.starts_with(PNG_SIGNATURE) || bytes.len() < IHDR_END {
        return None;
    }
    let word =
        |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    Some((word(IHDR_WIDTH), word(IHDR_HEIGHT)))
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    use std::io::Cursor;

    use image::{DynamicImage, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};

    /// A `width` by `height` PNG of one colour.
    pub fn png(width: u32, height: u32, colour: Rgba<u8>) -> Vec<u8> {
        encode(
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(width, height, colour)),
            ImageFormat::Png,
        )
    }

    /// A `width` by `height` JPEG of one colour.
    pub fn jpeg(width: u32, height: u32, colour: Rgb<u8>) -> Vec<u8> {
        encode(
            DynamicImage::ImageRgb8(RgbImage::from_pixel(width, height, colour)),
            ImageFormat::Jpeg,
        )
    }

    fn encode(image: DynamicImage, format: ImageFormat) -> Vec<u8> {
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), format)
            .unwrap();
        bytes
    }
}

#[cfg(test)]
mod tests {
    use image::Rgb;

    use super::test_fixtures::{jpeg, png};
    use super::*;

    const RED: Rgba<u8> = Rgba([0xFF, 0, 0, 0xFF]);

    fn fit_bytes(name: &str, bytes: &[u8], frame: Frame) -> RgbaImage {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        let fitted = fit(&path, frame).unwrap();
        assert_eq!(
            png_dimensions(&fitted),
            Some((frame.width, frame.height)),
            "{name}: fitted size"
        );
        image::load_from_memory(&fitted).unwrap().into_rgba8()
    }

    /// Channels after resampling, or a lossy JPEG round trip, land near the
    /// source rather than on it.
    fn near(actual: Rgba<u8>, expected: Rgba<u8>) -> bool {
        actual
            .0
            .iter()
            .zip(expected.0)
            .all(|(&a, e)| a.abs_diff(e) <= 8)
    }

    #[test]
    fn keeps_a_png_of_the_right_size_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icon.png");
        let original = png(144, 80, RED);
        std::fs::write(&path, &original).unwrap();
        assert_eq!(fit(&path, ICON0).unwrap(), original);
    }

    #[test]
    fn centres_a_square_cover_on_a_transparent_icon() {
        let icon = fit_bytes("cover.jpg", &jpeg(500, 500, Rgb([0xFF, 0, 0])), ICON0);
        // 500x500 scales to 80x80, leaving 32 transparent columns each side.
        assert!(
            near(*icon.get_pixel(72, 40), RED),
            "centre {:?}",
            icon.get_pixel(72, 40)
        );
        assert!(near(*icon.get_pixel(32, 0), RED), "left edge of the cover");
        assert_eq!(*icon.get_pixel(31, 40), TRANSPARENT, "left margin");
        assert_eq!(*icon.get_pixel(112, 40), TRANSPARENT, "right margin");
    }

    #[test]
    fn letterboxes_a_wide_background_in_black() {
        let background = fit_bytes("wide.png", &png(960, 272, RED), PIC1);
        // 960x272 scales to 480x136, leaving 68 black rows above and below.
        assert_eq!(*background.get_pixel(240, 67), BLACK, "top band");
        assert!(near(*background.get_pixel(240, 68), RED), "first image row");
        assert!(near(*background.get_pixel(240, 203), RED), "last image row");
        assert_eq!(*background.get_pixel(240, 204), BLACK, "bottom band");
    }

    #[test]
    fn scales_a_small_image_up() {
        let splash = fit_bytes("small.png", &png(48, 27, RED), SPLASH);
        // 48x27 scales ten times to 480x270: one black row above and below.
        assert_eq!(*splash.get_pixel(0, 0), BLACK, "top band");
        assert!(near(*splash.get_pixel(0, 1), RED), "first image row");
        assert!(near(*splash.get_pixel(479, 270), RED), "last image row");
        assert_eq!(*splash.get_pixel(479, 271), BLACK, "bottom band");
    }

    #[test]
    fn rejects_a_file_that_is_not_an_image() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cover.png");
        std::fs::write(&path, b"not an image").unwrap();
        let err = fit(&path, ICON0).unwrap_err();
        assert!(matches!(err, PsxPbpError::InvalidImage { .. }), "{err}");
    }

    #[test]
    fn reads_png_dimensions_from_the_header() {
        assert_eq!(png_dimensions(&png(144, 80, RED)), Some((144, 80)));
        assert_eq!(png_dimensions(&jpeg(8, 8, Rgb([0, 0, 0]))), None);
        assert_eq!(png_dimensions(PNG_SIGNATURE), None);
    }
}
