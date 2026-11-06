//! Avatar images for bots and groups.
//!
//! An upload is untrusted bytes. It is never stored or served as received: the
//! server sniffs the format (png, jpeg or webp only), decodes it under hard
//! size limits, centre-crops and resizes it to a square, and re-encodes it as
//! a fresh PNG. Re-encoding drops every metadata block (EXIF, XMP, ICC,
//! comments) and any trailing payload, so what is stored is pixels only.

use std::io::Cursor;

use image::imageops::FilterType;
use image::{ImageFormat, ImageReader, Limits};

use crate::error::{Result, SeraError};

/// Largest upload accepted, in bytes (1 MiB).
pub const MAX_UPLOAD_BYTES: usize = 1024 * 1024;
/// Edge of the stored square image.
pub const AVATAR_EDGE: u32 = 256;
/// Content type of every stored avatar.
pub const AVATAR_MIME: &str = "image/png";

/// Widest or tallest source accepted. A 1 MiB file can still describe a huge
/// canvas, so the decoder is told to refuse it instead of allocating.
const MAX_SOURCE_EDGE: u32 = 8192;
/// Decoder allocation ceiling.
const MAX_DECODE_ALLOC: u64 = 96 * 1024 * 1024;

/// Decode `bytes` and return a 256x256 PNG with no metadata.
pub fn normalize_avatar(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.is_empty() {
        return Err(SeraError::Validation("the image is empty".into()));
    }
    if bytes.len() > MAX_UPLOAD_BYTES {
        return Err(SeraError::Validation(
            "the image must be at most 1 MiB".into(),
        ));
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| SeraError::Validation("the file is not a readable image".into()))?;
    match reader.format() {
        Some(ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP) => {}
        _ => {
            return Err(SeraError::Validation(
                "use a png, jpeg or webp image".into(),
            ));
        }
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_EDGE);
    limits.max_image_height = Some(MAX_SOURCE_EDGE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|_| SeraError::Validation("the image could not be decoded".into()))?;
    let square = decoded
        .resize_to_fill(AVATAR_EDGE, AVATAR_EDGE, FilterType::Lanczos3)
        .to_rgba8();
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(square)
        .write_to(&mut out, ImageFormat::Png)
        .map_err(|_| SeraError::Internal("the avatar could not be encoded".into()))?;
    Ok(out.into_inner())
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A synthetic non-square gradient, encoded as `format`.
    pub fn synthetic(format: ImageFormat, width: u32, height: u32) -> Vec<u8> {
        let mut canvas = image::RgbImage::new(width, height);
        for (x, y, pixel) in canvas.enumerate_pixels_mut() {
            *pixel = image::Rgb([(x % 251) as u8, (y % 241) as u8, 128]);
        }
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(canvas)
            .write_to(&mut out, format)
            .unwrap();
        out.into_inner()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::synthetic;
    use super::*;

    fn dimensions(png: &[u8]) -> (u32, u32) {
        let decoded = image::load_from_memory(png).unwrap();
        (decoded.width(), decoded.height())
    }

    #[test]
    fn png_jpeg_and_webp_become_a_square_png() {
        for format in [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::WebP] {
            let out = normalize_avatar(&synthetic(format, 640, 300)).unwrap();
            assert_eq!(dimensions(&out), (AVATAR_EDGE, AVATAR_EDGE));
            assert_eq!(&out[..8], b"\x89PNG\r\n\x1a\n");
        }
    }

    #[test]
    fn metadata_and_trailing_bytes_do_not_survive() {
        let mut upload = synthetic(ImageFormat::Png, 64, 64);
        upload.extend_from_slice(b"SYNTHETIC-TRAILING-SECRET");
        let out = normalize_avatar(&upload).unwrap();
        assert!(!out.windows(9).any(|w| w == b"SYNTHETIC"));
        // Only the chunks a pixel-only PNG needs remain.
        for chunk in [&b"eXIf"[..], b"tEXt", b"iTXt", b"zTXt", b"iCCP"] {
            assert!(!out.windows(4).any(|w| w == chunk));
        }
    }

    #[test]
    fn rejects_empty_oversized_and_non_image_input() {
        assert!(normalize_avatar(&[]).is_err());
        assert!(normalize_avatar(b"GIF89a not really").is_err());
        assert!(normalize_avatar(b"<svg xmlns='http://www.w3.org/2000/svg'/>").is_err());
        assert!(normalize_avatar(&vec![0u8; MAX_UPLOAD_BYTES + 1]).is_err());
        let truncated = synthetic(ImageFormat::Png, 64, 64);
        assert!(normalize_avatar(&truncated[..truncated.len() / 2]).is_err());
    }

    #[test]
    fn rejects_a_canvas_larger_than_the_decoder_limit() {
        // A tiny solid-colour PNG that declares a huge canvas compresses far
        // below 1 MiB, so only the decode limit can stop it.
        let canvas = image::GrayImage::new(9000, 20);
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma8(canvas)
            .write_to(&mut out, ImageFormat::Png)
            .unwrap();
        assert!(normalize_avatar(&out.into_inner()).is_err());
    }
}
