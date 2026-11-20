//! Image editing tools (0.69): crop, rotate/flip, adjust, compress, images
//! to PDF, and the page images of a scanned PDF.
//!
//! Same rules as `image_ops`: inputs are an artifact the acting member owns or
//! a path in their own inbox (chat uploads from WhatsApp, Telegram or the
//! console live there), every result is a new artifact, and re-encoding drops
//! all metadata after applying the EXIF orientation.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use image::{DynamicImage, GenericImageView, ImageFormat};
use serde_json::{Value, json};

use crate::domain::Language;
use crate::error::{Result, SeraError};
use crate::gateway::{Tool, ToolContext, ToolOutput, ToolRegistry, ToolSpec};
use crate::image_ops::{
    MAX_IMAGE_BYTES, MAX_SIDE, Output, Sources, decode_oriented, derive_name, invalid, probe,
    reject_unknown, source_properties,
};
use crate::maker::{ArtifactMeta, ArtifactStore};
use crate::store::Store;

/// Images one PDF may hold.
pub const MAX_PDF_IMAGES: usize = 30;
/// Page images taken from one PDF.
pub const MAX_EXTRACTED_PAGES: usize = 30;
/// Largest PDF read for its page images.
const MAX_PDF_BYTES: usize = 50 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Pure operations (tested directly)
// ---------------------------------------------------------------------------

/// How to crop.
#[derive(Clone, Debug, PartialEq)]
pub enum Crop {
    /// x, y, width, height in pixels.
    Pixels(u32, u32, u32, u32),
    /// x, y, width, height in percent of the picture (0–100).
    Percent(f64, f64, f64, f64),
    /// The largest centred box with this width:height ratio.
    Aspect(f64),
    /// Cut away uniform borders (white or any flat colour).
    Trim,
}

/// Parse an aspect such as `1:1`, `4:3`, `16:9` or `a4` / `a4-landscape`.
pub fn parse_aspect(text: &str) -> Result<f64> {
    let lower = text.trim().to_ascii_lowercase();
    let a4 = 210.0 / 297.0;
    let ratio = match lower.as_str() {
        "a4" | "a4-portrait" => a4,
        "a4-landscape" => 1.0 / a4,
        "letter" => 8.5 / 11.0,
        "square" => 1.0,
        other => {
            let (w, h) = other
                .split_once(':')
                .ok_or_else(|| invalid("aspect must look like 4:3, 1:1, 16:9 or a4"))?;
            let w: f64 = w.trim().parse().map_err(|_| invalid("invalid aspect"))?;
            let h: f64 = h.trim().parse().map_err(|_| invalid("invalid aspect"))?;
            if !(w > 0.0 && h > 0.0 && w <= 100.0 && h <= 100.0) {
                return Err(invalid("aspect numbers must be between 0 and 100"));
            }
            w / h
        }
    };
    Ok(ratio)
}

/// The crop box `(x, y, width, height)` for a picture of `width` x `height`.
pub fn crop_box(
    width: u32,
    height: u32,
    crop: &Crop,
    image: Option<&DynamicImage>,
) -> Result<(u32, u32, u32, u32)> {
    let (x, y, w, h) = match *crop {
        Crop::Pixels(x, y, w, h) => (x, y, w, h),
        Crop::Percent(x, y, w, h) => {
            for value in [x, y, w, h] {
                if !(0.0..=100.0).contains(&value) {
                    return Err(invalid("percent values must be 0-100"));
                }
            }
            let px = |value: f64, side: u32| ((value / 100.0) * f64::from(side)).round() as u32;
            (px(x, width), px(y, height), px(w, width), px(h, height))
        }
        Crop::Aspect(ratio) => {
            let current = f64::from(width) / f64::from(height);
            if current > ratio {
                let w = (f64::from(height) * ratio).round() as u32;
                ((width - w.min(width)) / 2, 0, w, height)
            } else {
                let h = (f64::from(width) / ratio).round() as u32;
                (0, (height - h.min(height)) / 2, width, h)
            }
        }
        Crop::Trim => {
            let image = image.ok_or_else(|| invalid("trim needs the picture"))?;
            trim_box(image)
        }
    };
    if w == 0 || h == 0 {
        return Err(invalid("the crop box is empty"));
    }
    if x >= width || y >= height {
        return Err(invalid(format!(
            "the crop box starts outside the {width}x{height} picture"
        )));
    }
    // Clamp to the picture rather than failing on an off-by-a-few box.
    Ok((x, y, w.min(width - x), h.min(height - y)))
}

/// Bounding box of everything that differs from the corner colour.
fn trim_box(image: &DynamicImage) -> (u32, u32, u32, u32) {
    let rgb = image.to_rgb8();
    let (width, height) = rgb.dimensions();
    let corner = *rgb.get_pixel(0, 0);
    let differs = |x: u32, y: u32| {
        let pixel = rgb.get_pixel(x, y);
        pixel
            .0
            .iter()
            .zip(corner.0.iter())
            .any(|(a, b)| a.abs_diff(*b) > 24)
    };
    let (mut left, mut top, mut right, mut bottom) = (width, height, 0u32, 0u32);
    for y in 0..height {
        for x in 0..width {
            if differs(x, y) {
                left = left.min(x);
                right = right.max(x);
                top = top.min(y);
                bottom = bottom.max(y);
            }
        }
    }
    if left > right || top > bottom {
        return (0, 0, width, height);
    }
    (left, top, right - left + 1, bottom - top + 1)
}

/// Brightness and contrast in -100..=100; grayscale; black-and-white.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Adjust {
    pub brightness: i32,
    pub contrast: i32,
    pub grayscale: bool,
    /// Threshold to pure black and white (a clean "scanned" look).
    pub black_white: bool,
}

pub fn adjust(image: DynamicImage, adjust: Adjust) -> DynamicImage {
    let mut image = image;
    if adjust.brightness != 0 {
        image = image.brighten((adjust.brightness.clamp(-100, 100) as f32 * 1.6) as i32);
    }
    if adjust.contrast != 0 {
        image = image.adjust_contrast(adjust.contrast.clamp(-100, 100) as f32);
    }
    if adjust.black_white {
        let gray = image.to_luma8();
        let threshold = otsu(&gray);
        let bw = image::GrayImage::from_fn(gray.width(), gray.height(), |x, y| {
            image::Luma([if gray.get_pixel(x, y).0[0] > threshold {
                255
            } else {
                0
            }])
        });
        return DynamicImage::ImageLuma8(bw);
    }
    if adjust.grayscale {
        image = DynamicImage::ImageLuma8(image.to_luma8());
    }
    image
}

/// Otsu's threshold for a grayscale picture.
fn otsu(gray: &image::GrayImage) -> u8 {
    let mut histogram = [0u64; 256];
    for pixel in gray.pixels() {
        histogram[usize::from(pixel.0[0])] += 1;
    }
    let total: u64 = histogram.iter().sum();
    if total == 0 {
        return 128;
    }
    let sum: f64 = histogram
        .iter()
        .enumerate()
        .map(|(level, count)| level as f64 * *count as f64)
        .sum();
    let (mut sum_back, mut weight_back, mut best, mut threshold) = (0.0, 0u64, 0.0, 128u8);
    for (level, count) in histogram.iter().enumerate() {
        weight_back += count;
        if weight_back == 0 {
            continue;
        }
        let weight_fore = total - weight_back;
        if weight_fore == 0 {
            break;
        }
        sum_back += level as f64 * *count as f64;
        let mean_back = sum_back / weight_back as f64;
        let mean_fore = (sum - sum_back) / weight_fore as f64;
        let between = weight_back as f64 * weight_fore as f64 * (mean_back - mean_fore).powi(2);
        if between > best {
            best = between;
            threshold = level as u8;
        }
    }
    threshold
}

/// Encode a picture as PNG, JPEG or WebP.
pub fn encode(image: &DynamicImage, output: Output, quality: u8) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    match output {
        Output::Png => image
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .map_err(|_| invalid("could not encode the PNG"))?,
        Output::Jpeg => {
            let rgb = DynamicImage::ImageRgb8(image.to_rgb8());
            let encoder =
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100));
            rgb.write_with_encoder(encoder)
                .map_err(|_| invalid("could not encode the JPEG"))?;
        }
        Output::WebP => DynamicImage::ImageRgba8(image.to_rgba8())
            .write_to(&mut Cursor::new(&mut out), ImageFormat::WebP)
            .map_err(|_| invalid("could not encode the WebP"))?,
    }
    Ok(out)
}

/// Re-encode (and if needed shrink) until the file fits in `max_bytes`.
/// Returns the bytes, final size and the JPEG quality used.
pub fn compress(
    image: DynamicImage,
    max_bytes: usize,
    output: Output,
) -> Result<(Vec<u8>, u32, u32, u8)> {
    let mut image = image;
    for _round in 0..8 {
        for quality in [90u8, 80, 70, 60, 50, 40] {
            let bytes = encode(&image, output, quality)?;
            if bytes.len() <= max_bytes {
                return Ok((bytes, image.width(), image.height(), quality));
            }
            if output == Output::Png {
                break;
            }
        }
        let (w, h) = (image.width(), image.height());
        if w <= 64 || h <= 64 {
            break;
        }
        image = image.resize(
            (f64::from(w) * 0.8) as u32,
            (f64::from(h) * 0.8) as u32,
            image::imageops::FilterType::Lanczos3,
        );
    }
    Err(invalid(format!(
        "could not get the image under {} KB; try a smaller size or JPEG",
        max_bytes / 1024
    )))
}

/// Page size in PDF points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PageSize {
    A4,
    Letter,
    /// Each page is the size of its picture (at 150 dpi).
    Fit,
}

impl PageSize {
    pub fn parse(text: &str) -> Result<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "a4" => Ok(Self::A4),
            "letter" => Ok(Self::Letter),
            "fit" | "image" | "original" => Ok(Self::Fit),
            _ => Err(invalid("page must be a4, letter or fit")),
        }
    }
}

/// Build a PDF with one picture per page. Each picture is re-encoded as JPEG
/// (no metadata) and centred inside the margin, keeping its aspect ratio.
/// `orientation` is `auto`, `portrait` or `landscape`.
pub fn images_to_pdf(
    images: &[DynamicImage],
    page: PageSize,
    orientation: &str,
    margin_mm: f64,
) -> Result<Vec<u8>> {
    use lopdf::content::{Content, Operation};
    use lopdf::{Dictionary, Document, Object, Stream, dictionary};
    if images.is_empty() || images.len() > MAX_PDF_IMAGES {
        return Err(invalid(format!("give 1-{MAX_PDF_IMAGES} images")));
    }
    if !(0.0..=40.0).contains(&margin_mm) {
        return Err(invalid("margin_mm must be 0-40"));
    }
    let margin = margin_mm * 72.0 / 25.4;
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let mut kids = Vec::new();
    for image in images {
        // Large photos are shrunk so the PDF stays reasonable.
        let image = if image.width().max(image.height()) > 3000 {
            image.resize(3000, 3000, image::imageops::FilterType::Lanczos3)
        } else {
            image.clone()
        };
        let (iw, ih) = image.dimensions();
        let landscape_picture = iw > ih;
        let (mut pw, mut ph) = match page {
            PageSize::A4 => (595.28, 841.89),
            PageSize::Letter => (612.0, 792.0),
            PageSize::Fit => (
                f64::from(iw) * 72.0 / 150.0 + 2.0 * margin,
                f64::from(ih) * 72.0 / 150.0 + 2.0 * margin,
            ),
        };
        let landscape = match orientation {
            "landscape" => true,
            "portrait" => false,
            _ => landscape_picture,
        };
        if page != PageSize::Fit && landscape {
            std::mem::swap(&mut pw, &mut ph);
        }
        let (box_w, box_h) = ((pw - 2.0 * margin).max(1.0), (ph - 2.0 * margin).max(1.0));
        let scale = (box_w / f64::from(iw)).min(box_h / f64::from(ih));
        let (dw, dh) = (f64::from(iw) * scale, f64::from(ih) * scale);
        let (dx, dy) = ((pw - dw) / 2.0, (ph - dh) / 2.0);
        let jpeg = encode(&image, Output::Jpeg, 88)?;
        let gray = matches!(image, DynamicImage::ImageLuma8(_));
        let image_stream = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => i64::from(iw),
                "Height" => i64::from(ih),
                "ColorSpace" => if gray { "DeviceGray" } else { "DeviceRGB" },
                "BitsPerComponent" => 8,
                "Filter" => "DCTDecode",
            },
            if gray {
                encode_gray_jpeg(&image)?
            } else {
                jpeg
            },
        );
        let image_id = doc.add_object(image_stream);
        let content = Content {
            operations: vec![
                Operation::new("q", vec![]),
                Operation::new(
                    "cm",
                    vec![
                        Object::Real(dw as f32),
                        0.into(),
                        0.into(),
                        Object::Real(dh as f32),
                        Object::Real(dx as f32),
                        Object::Real(dy as f32),
                    ],
                ),
                Operation::new("Do", vec![Object::Name(b"Im0".to_vec())]),
                Operation::new("Q", vec![]),
            ],
        };
        let content_id = doc.add_object(Stream::new(
            Dictionary::new(),
            content
                .encode()
                .map_err(|_| invalid("could not build the PDF page"))?,
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), Object::Real(pw as f32), Object::Real(ph as f32)],
            "Contents" => content_id,
            "Resources" => dictionary! {
                "XObject" => dictionary! { "Im0" => image_id },
            },
        });
        kids.push(page_id.into());
    }
    let count = kids.len() as i64;
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => count,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);
    let mut out = Vec::new();
    doc.save_to(&mut out)
        .map_err(|_| invalid("could not write the PDF"))?;
    Ok(out)
}

fn encode_gray_jpeg(image: &DynamicImage) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88);
    DynamicImage::ImageLuma8(image.to_luma8())
        .write_with_encoder(encoder)
        .map_err(|_| invalid("could not encode the page"))?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

struct Saver {
    artifacts: Arc<ArtifactStore>,
    store: Store,
}

impl Saver {
    /// Save one result as a new artifact and audit it.
    fn save(
        &self,
        tool: &str,
        context: &ToolContext,
        bytes: &[u8],
        filename: &str,
        source: Option<String>,
        extra: Value,
    ) -> Result<(String, String)> {
        let stored = self.artifacts.save(
            bytes,
            filename,
            ArtifactMeta {
                title: filename.to_string(),
                filename: filename.to_string(),
                language: Language::En,
                source_artifact: source.clone(),
                created_at: Utc::now(),
            },
        )?;
        self.store.record_audit(
            &context.actor,
            "image.transformed",
            Some(tool),
            context.task_id.as_deref(),
            None,
            None,
            None,
            Some(&json!({
                "artifact_hash": stored.hash, "source_artifact": source, "details": extra,
            })),
        )?;
        Ok((stored.hash, stored.filename))
    }
}

fn output_for(text: Option<&str>, source: &[u8]) -> Result<Output> {
    match text {
        Some(text) => Output::parse(text),
        None => Ok(probe(source).map(|(format, _, _)| Output::of(format))?),
    }
}

fn number(args: &Value, key: &str) -> Option<f64> {
    args.get(key).and_then(Value::as_f64)
}

fn result_output(hash: String, filename: String, lines: Vec<String>, data: Value) -> ToolOutput {
    let mut content = vec![format!("filename: {filename}"), format!("artifact: {hash}")];
    content.extend(lines);
    ToolOutput {
        content: content.join("\n"),
        data: Some(data),
        artifact_path: Some(format!("artifacts/{filename}")),
        artifact_hash: Some(hash),
        filename: Some(filename),
        destination: None,
        cards: Vec::new(),
    }
}

async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|error| SeraError::Internal(format!("image task failed: {error}")))?
}

fn spec(name: &str, description: &str, extra: Value, required: &[&str]) -> ToolSpec {
    let mut properties = source_properties();
    if let (Some(target), Some(more)) = (properties.as_object_mut(), extra.as_object()) {
        for (key, value) in more {
            target.insert(key.clone(), value.clone());
        }
    }
    ToolSpec {
        name: name.into(),
        description: description.into(),
        parameters: json!({
            "type": "object", "properties": properties,
            "required": required, "additionalProperties": false
        }),
        external_effect: false,
    }
}

const FORMAT_HELP: &str = "Output format png, jpg or webp (default: same as the input)";

pub struct ImageCropTool {
    sources: Sources,
    saver: Saver,
}

#[async_trait]
impl Tool for ImageCropTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "image_crop",
            "Crop an image (PNG, JPEG, WebP; chat photos included via their inbox path). Use \
             exactly one way: x/y/width/height in pixels; the same in percent with \
             unit=\"percent\"; aspect=\"1:1\"|\"4:3\"|\"16:9\"|\"a4\" for the largest centred box; \
             or trim=true to cut away flat borders. Produces a new artifact.",
            json!({
                "x": {"type": "number"}, "y": {"type": "number"},
                "width": {"type": "number"}, "height": {"type": "number"},
                "unit": {"type": "string", "enum": ["px", "percent"]},
                "aspect": {"type": "string"},
                "trim": {"type": "boolean"},
                "format": {"type": "string", "enum": ["png", "jpg", "webp"], "description": FORMAT_HELP},
            }),
            &[],
        )
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        reject_unknown(
            &args,
            &[
                "artifact", "path", "x", "y", "width", "height", "unit", "aspect", "trim", "format",
            ],
        )?;
        let crop = if args.get("trim").and_then(Value::as_bool) == Some(true) {
            Crop::Trim
        } else if let Some(aspect) = args.get("aspect").and_then(Value::as_str) {
            Crop::Aspect(parse_aspect(aspect)?)
        } else {
            let (Some(x), Some(y), Some(w), Some(h)) = (
                number(&args, "x").or(Some(0.0)),
                number(&args, "y").or(Some(0.0)),
                number(&args, "width"),
                number(&args, "height"),
            ) else {
                return Err(invalid(
                    "give width and height (with x/y), an aspect, or trim=true",
                ));
            };
            if [x, y, w, h].iter().any(|value| *value < 0.0) {
                return Err(invalid("crop values cannot be negative"));
            }
            if args.get("unit").and_then(Value::as_str) == Some("percent") {
                Crop::Percent(x, y, w, h)
            } else {
                Crop::Pixels(x as u32, y as u32, w as u32, h as u32)
            }
        };
        let source = self.sources.resolve(&args, &context.user_id)?;
        let output = output_for(args.get("format").and_then(Value::as_str), &source.bytes)?;
        let bytes = source.bytes;
        let (encoded, width, height, from) = blocking(move || {
            let image = decode_oriented(&bytes)?;
            let (iw, ih) = image.dimensions();
            let (x, y, w, h) = crop_box(iw, ih, &crop, Some(&image))?;
            let cropped = image.crop_imm(x, y, w, h);
            Ok((encode(&cropped, output, 90)?, w, h, (iw, ih)))
        })
        .await?;
        let filename = derive_name(&source.filename, "cropped", output.extension());
        let (hash, filename) = self.saver.save(
            "image_crop",
            context,
            &encoded,
            &filename,
            source.artifact.clone(),
            json!({"width": width, "height": height}),
        )?;
        Ok(result_output(
            hash.clone(),
            filename.clone(),
            vec![format!(
                "cropped from {}x{} to {width}x{height} px, {} bytes",
                from.0,
                from.1,
                encoded.len()
            )],
            json!({"artifact_hash": hash, "filename": filename, "width": width, "height": height}),
        ))
    }
}

pub struct ImageRotateTool {
    sources: Sources,
    saver: Saver,
}

#[async_trait]
impl Tool for ImageRotateTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "image_rotate",
            "Rotate an image by 90, 180 or 270 degrees clockwise (negative turns \
             counter-clockwise), and/or flip it horizontally or vertically. Produces a new \
             artifact.",
            json!({
                "degrees": {"type": "integer", "enum": [-270, -180, -90, 0, 90, 180, 270]},
                "flip": {"type": "string", "enum": ["horizontal", "vertical"]},
                "format": {"type": "string", "enum": ["png", "jpg", "webp"], "description": FORMAT_HELP},
            }),
            &[],
        )
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        reject_unknown(&args, &["artifact", "path", "degrees", "flip", "format"])?;
        let degrees = args.get("degrees").and_then(Value::as_i64).unwrap_or(0);
        let degrees = degrees.rem_euclid(360);
        if degrees % 90 != 0 {
            return Err(invalid("degrees must be a multiple of 90"));
        }
        let flip = args.get("flip").and_then(Value::as_str).map(str::to_string);
        if degrees == 0 && flip.is_none() {
            return Err(invalid("give degrees, flip, or both"));
        }
        let source = self.sources.resolve(&args, &context.user_id)?;
        let output = output_for(args.get("format").and_then(Value::as_str), &source.bytes)?;
        let bytes = source.bytes;
        let flip_kind = flip.clone();
        let (encoded, width, height) = blocking(move || {
            let mut image = decode_oriented(&bytes)?;
            image = match degrees {
                90 => image.rotate90(),
                180 => image.rotate180(),
                270 => image.rotate270(),
                _ => image,
            };
            image = match flip_kind.as_deref() {
                Some("horizontal") => image.fliph(),
                Some("vertical") => image.flipv(),
                None => image,
                Some(_) => return Err(invalid("flip must be horizontal or vertical")),
            };
            Ok((encode(&image, output, 90)?, image.width(), image.height()))
        })
        .await?;
        let filename = derive_name(&source.filename, "rotated", output.extension());
        let (hash, filename) = self.saver.save(
            "image_rotate",
            context,
            &encoded,
            &filename,
            source.artifact.clone(),
            json!({"degrees": degrees, "flip": flip}),
        )?;
        Ok(result_output(
            hash.clone(),
            filename.clone(),
            vec![format!("now {width}x{height} px")],
            json!({"artifact_hash": hash, "filename": filename, "width": width, "height": height}),
        ))
    }
}

pub struct ImageAdjustTool {
    sources: Sources,
    saver: Saver,
}

#[async_trait]
impl Tool for ImageAdjustTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "image_adjust",
            "Adjust an image: brightness and contrast (-100 to 100), grayscale, or a clean \
             black-and-white \"scanned document\" look. Produces a new artifact.",
            json!({
                "brightness": {"type": "integer"},
                "contrast": {"type": "integer"},
                "grayscale": {"type": "boolean"},
                "black_white": {"type": "boolean"},
                "format": {"type": "string", "enum": ["png", "jpg", "webp"], "description": FORMAT_HELP},
            }),
            &[],
        )
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        reject_unknown(
            &args,
            &[
                "artifact",
                "path",
                "brightness",
                "contrast",
                "grayscale",
                "black_white",
                "format",
            ],
        )?;
        let int = |key: &str| -> Result<i32> {
            match args.get(key) {
                None | Some(Value::Null) => Ok(0),
                Some(value) => value
                    .as_i64()
                    .filter(|n| (-100..=100).contains(n))
                    .map(|n| n as i32)
                    .ok_or_else(|| invalid(format!("{key} must be -100 to 100"))),
            }
        };
        let settings = Adjust {
            brightness: int("brightness")?,
            contrast: int("contrast")?,
            grayscale: args.get("grayscale").and_then(Value::as_bool) == Some(true),
            black_white: args.get("black_white").and_then(Value::as_bool) == Some(true),
        };
        if settings == Adjust::default() {
            return Err(invalid("give at least one adjustment"));
        }
        let source = self.sources.resolve(&args, &context.user_id)?;
        let output = output_for(args.get("format").and_then(Value::as_str), &source.bytes)?;
        let bytes = source.bytes;
        let (encoded, width, height) = blocking(move || {
            let image = adjust(decode_oriented(&bytes)?, settings);
            Ok((encode(&image, output, 90)?, image.width(), image.height()))
        })
        .await?;
        let filename = derive_name(&source.filename, "adjusted", output.extension());
        let (hash, filename) = self.saver.save(
            "image_adjust",
            context,
            &encoded,
            &filename,
            source.artifact.clone(),
            json!({"width": width, "height": height}),
        )?;
        Ok(result_output(
            hash.clone(),
            filename.clone(),
            vec![format!("{width}x{height} px, {} bytes", encoded.len())],
            json!({"artifact_hash": hash, "filename": filename}),
        ))
    }
}

pub struct ImageCompressTool {
    sources: Sources,
    saver: Saver,
}

#[async_trait]
impl Tool for ImageCompressTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "image_compress",
            "Make an image smaller in file size, for WhatsApp, email or a web form: lowers the \
             quality and, if needed, the size until it fits in max_kb (and optionally within \
             max_side pixels). Produces a new artifact.",
            json!({
                "max_kb": {"type": "integer", "description": "Target size in KB, 10-20000"},
                "max_side": {"type": "integer", "description": "Optional longest side in pixels"},
                "format": {"type": "string", "enum": ["jpg", "webp", "png"], "description": "Default jpg"},
            }),
            &["max_kb"],
        )
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        reject_unknown(&args, &["artifact", "path", "max_kb", "max_side", "format"])?;
        let max_kb = args
            .get("max_kb")
            .and_then(Value::as_u64)
            .filter(|kb| (10..=20_000).contains(kb))
            .ok_or_else(|| invalid("max_kb must be 10-20000"))? as usize;
        let max_side = match args.get("max_side").and_then(Value::as_u64) {
            None => None,
            Some(side) if (16..=u64::from(MAX_SIDE)).contains(&side) => Some(side as u32),
            Some(_) => return Err(invalid(format!("max_side must be 16-{MAX_SIDE}"))),
        };
        let output = Output::parse(args.get("format").and_then(Value::as_str).unwrap_or("jpg"))?;
        let source = self.sources.resolve(&args, &context.user_id)?;
        let original = source.bytes.len();
        let bytes = source.bytes;
        let (encoded, width, height, quality) = blocking(move || {
            let mut image = decode_oriented(&bytes)?;
            if let Some(side) = max_side
                && image.width().max(image.height()) > side
            {
                image = image.resize(side, side, image::imageops::FilterType::Lanczos3);
            }
            compress(image, max_kb * 1024, output)
        })
        .await?;
        let filename = derive_name(&source.filename, "small", output.extension());
        let (hash, filename) = self.saver.save(
            "image_compress",
            context,
            &encoded,
            &filename,
            source.artifact.clone(),
            json!({"bytes": encoded.len(), "quality": quality}),
        )?;
        Ok(result_output(
            hash.clone(),
            filename.clone(),
            vec![format!(
                "{} KB -> {} KB, {width}x{height} px, quality {quality}",
                original / 1024,
                encoded.len() / 1024
            )],
            json!({"artifact_hash": hash, "filename": filename, "bytes": encoded.len()}),
        ))
    }
}

pub struct ImagesToPdfTool {
    sources: Sources,
    saver: Saver,
}

#[async_trait]
impl Tool for ImagesToPdfTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "images_to_pdf".into(),
            description: "Make one PDF from 1-30 images (photos of documents, receipts, \
                          scans), one image per page in the order given. page a4 (default), \
                          letter or fit (page = picture); orientation auto (default), portrait \
                          or landscape; margin_mm 0-40 (default 10). Produces a new PDF \
                          artifact. Crop or adjust the photos first when asked."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "images": {
                        "type": "array",
                        "items": {"type": "object", "properties": {
                            "artifact": {"type": "string"}, "path": {"type": "string"}
                        }, "additionalProperties": false},
                        "description": "Each item: {artifact} or {path} (inbox path of a chat photo)"
                    },
                    "page": {"type": "string", "enum": ["a4", "letter", "fit"]},
                    "orientation": {"type": "string", "enum": ["auto", "portrait", "landscape"]},
                    "margin_mm": {"type": "number"},
                    "filename": {"type": "string", "description": "Optional name ending in .pdf"}
                },
                "required": ["images"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        reject_unknown(
            &args,
            &["images", "page", "orientation", "margin_mm", "filename"],
        )?;
        let items = args
            .get("images")
            .and_then(Value::as_array)
            .filter(|items| !items.is_empty() && items.len() <= MAX_PDF_IMAGES)
            .ok_or_else(|| invalid(format!("images must list 1-{MAX_PDF_IMAGES} images")))?;
        let page = PageSize::parse(args.get("page").and_then(Value::as_str).unwrap_or("a4"))?;
        let orientation = args
            .get("orientation")
            .and_then(Value::as_str)
            .unwrap_or("auto")
            .to_string();
        if !matches!(orientation.as_str(), "auto" | "portrait" | "landscape") {
            return Err(invalid("orientation must be auto, portrait or landscape"));
        }
        let margin = args
            .get("margin_mm")
            .and_then(Value::as_f64)
            .unwrap_or(10.0);
        let mut sources = Vec::new();
        let mut first_name = None;
        let mut total = 0usize;
        for item in items {
            let source = self.sources.resolve(item, &context.user_id)?;
            total += source.bytes.len();
            if total > 4 * MAX_IMAGE_BYTES {
                return Err(invalid("the images add up to more than 200 MB"));
            }
            first_name.get_or_insert_with(|| source.filename.clone());
            sources.push(source.bytes);
        }
        let count = sources.len();
        let pdf = blocking(move || {
            let images = sources
                .iter()
                .map(|bytes| decode_oriented(bytes))
                .collect::<Result<Vec<_>>>()?;
            images_to_pdf(&images, page, &orientation, margin)
        })
        .await?;
        let filename = match args.get("filename").and_then(Value::as_str) {
            Some(name) if name.to_ascii_lowercase().ends_with(".pdf") && name.len() <= 120 => {
                crate::telegram::safe_upload_name(name)
            }
            Some(_) => return Err(invalid("filename must end in .pdf (max 120 characters)")),
            None => derive_name(first_name.as_deref().unwrap_or("images"), "scan", "pdf"),
        };
        let (hash, filename) = self.saver.save(
            "images_to_pdf",
            context,
            &pdf,
            &filename,
            None,
            json!({"pages": count}),
        )?;
        Ok(result_output(
            hash.clone(),
            filename.clone(),
            vec![format!("{count} pages, {} KB", pdf.len() / 1024)],
            json!({"artifact_hash": hash, "filename": filename, "pages": count}),
        ))
    }
}

pub struct PdfToImagesTool {
    sources: Sources,
    saver: Saver,
    inbox: PathBuf,
}

#[async_trait]
impl Tool for PdfToImagesTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "pdf_to_images",
            "Take the page pictures out of a scanned or photographed PDF (one image per page, \
             up to 30), as new image artifacts you can crop, adjust or send. A PDF made of \
             text has no page pictures; read it with pdf_inspect instead.",
            json!({
                "format": {"type": "string", "enum": ["png", "jpg"], "description": "Default jpg"},
            }),
            &[],
        )
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        reject_unknown(&args, &["artifact", "path", "format"])?;
        let output = Output::parse(args.get("format").and_then(Value::as_str).unwrap_or("jpg"))?;
        let (bytes, name, source_hash) = self.load_pdf(&args, &context.user_id)?;
        let pages = blocking(move || {
            let found =
                crate::documents::extract_pdf_images(&bytes, MAX_EXTRACTED_PAGES, MAX_IMAGE_BYTES);
            let mut out = Vec::new();
            for (_, raw) in found {
                // Re-encode so every result is a plain image without metadata.
                if let Ok(image) = decode_oriented(&raw) {
                    out.push(encode(&image, output, 90)?);
                }
            }
            Ok(out)
        })
        .await?;
        if pages.is_empty() {
            return Err(invalid(
                "this PDF has no page pictures (it is probably text); read it with pdf_inspect",
            ));
        }
        let mut lines = Vec::new();
        let mut results = Vec::new();
        let stem = Path::new(&name)
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("page")
            .to_string();
        for (index, page) in pages.iter().enumerate() {
            let filename = derive_name(&stem, &format!("page-{}", index + 1), output.extension());
            let (hash, filename) = self.saver.save(
                "pdf_to_images",
                context,
                page,
                &filename,
                source_hash.clone(),
                json!({"page": index + 1}),
            )?;
            lines.push(format!("page {}: {filename} artifact {hash}", index + 1));
            results.push(json!({"page": index + 1, "artifact_hash": hash, "filename": filename}));
        }
        let first = results[0].clone();
        Ok(ToolOutput {
            content: format!("{} page images\n{}", pages.len(), lines.join("\n")),
            data: Some(json!({"pages": results})),
            artifact_hash: first["artifact_hash"].as_str().map(str::to_string),
            filename: first["filename"].as_str().map(str::to_string),
            ..ToolOutput::default()
        })
    }
}

impl PdfToImagesTool {
    fn load_pdf(&self, args: &Value, user_id: &str) -> Result<(Vec<u8>, String, Option<String>)> {
        let text = |key: &str| {
            args.get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        match (text("artifact"), text("path")) {
            (Some(hash), None) => {
                if !self.sources.store.user_owns_artifact(user_id, hash)? {
                    return Err(SeraError::Forbidden(
                        "that artifact does not belong to this account".into(),
                    ));
                }
                let stored = self.sources.artifacts.load(hash)?;
                if stored.bytes.len() > MAX_PDF_BYTES {
                    return Err(invalid("the PDF is larger than 50 MB"));
                }
                Ok((stored.bytes, stored.filename, Some(stored.hash)))
            }
            (None, Some(path)) => {
                let candidate = crate::user_inbox::resolve(&self.inbox, user_id, path)?;
                if !candidate
                    .extension()
                    .and_then(|value| value.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
                {
                    return Err(invalid("that is not a PDF"));
                }
                let meta = std::fs::metadata(&candidate)?;
                if !meta.is_file() || meta.len() as usize > MAX_PDF_BYTES {
                    return Err(invalid("the PDF is missing or larger than 50 MB"));
                }
                let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_string();
                Ok((std::fs::read(candidate)?, name, None))
            }
            (Some(_), Some(_)) => Err(invalid("provide either artifact or path, not both")),
            (None, None) => Err(invalid("artifact or path is required")),
        }
    }
}

/// The tools this module adds, for allowlists and tests.
pub const IMAGE_EDIT_TOOLS: &[&str] = &[
    "image_crop",
    "image_rotate",
    "image_adjust",
    "image_compress",
    "images_to_pdf",
    "pdf_to_images",
];

pub fn register_image_edit_tools(
    registry: &mut ToolRegistry,
    artifacts: &Arc<ArtifactStore>,
    store: &Store,
    inbox: &Path,
) -> Result<()> {
    let sources = || Sources {
        artifacts: artifacts.clone(),
        store: store.clone(),
        inbox: inbox.to_path_buf(),
    };
    let saver = || Saver {
        artifacts: artifacts.clone(),
        store: store.clone(),
    };
    registry.register(ImageCropTool {
        sources: sources(),
        saver: saver(),
    })?;
    registry.register(ImageRotateTool {
        sources: sources(),
        saver: saver(),
    })?;
    registry.register(ImageAdjustTool {
        sources: sources(),
        saver: saver(),
    })?;
    registry.register(ImageCompressTool {
        sources: sources(),
        saver: saver(),
    })?;
    registry.register(ImagesToPdfTool {
        sources: sources(),
        saver: saver(),
    })?;
    registry.register(PdfToImagesTool {
        sources: sources(),
        saver: saver(),
        inbox: inbox.to_path_buf(),
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(width: u32, height: u32) -> DynamicImage {
        DynamicImage::ImageRgba8(image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, 90, 255])
        }))
    }

    #[test]
    fn crop_boxes_cover_pixels_percent_aspect_and_clamp() {
        assert_eq!(
            crop_box(100, 50, &Crop::Pixels(10, 5, 30, 20), None).unwrap(),
            (10, 5, 30, 20)
        );
        assert_eq!(
            crop_box(100, 50, &Crop::Percent(0.0, 0.0, 50.0, 50.0), None).unwrap(),
            (0, 0, 50, 25)
        );
        assert_eq!(
            crop_box(200, 100, &Crop::Aspect(1.0), None).unwrap(),
            (50, 0, 100, 100)
        );
        // A box that runs past the edge is clamped, not refused.
        assert_eq!(
            crop_box(100, 50, &Crop::Pixels(90, 40, 30, 30), None).unwrap(),
            (90, 40, 10, 10)
        );
        assert!(crop_box(100, 50, &Crop::Pixels(100, 0, 5, 5), None).is_err());
        assert!(crop_box(100, 50, &Crop::Percent(0.0, 0.0, 120.0, 10.0), None).is_err());
        assert!((parse_aspect("16:9").unwrap() - 16.0 / 9.0).abs() < 1e-9);
        assert!(parse_aspect("a4").unwrap() < 1.0);
        assert!(parse_aspect("banana").is_err());
    }

    #[test]
    fn trim_finds_the_content_inside_a_flat_border() {
        let mut canvas = image::RgbaImage::from_pixel(60, 40, image::Rgba([255, 255, 255, 255]));
        for y in 10..30 {
            for x in 20..45 {
                canvas.put_pixel(x, y, image::Rgba([10, 10, 10, 255]));
            }
        }
        let image = DynamicImage::ImageRgba8(canvas);
        assert_eq!(
            crop_box(60, 40, &Crop::Trim, Some(&image)).unwrap(),
            (20, 10, 25, 20)
        );
    }

    #[test]
    fn black_white_gives_only_two_levels() {
        let image = adjust(
            picture(32, 32),
            Adjust {
                black_white: true,
                ..Adjust::default()
            },
        );
        let gray = image.to_luma8();
        assert!(
            gray.pixels()
                .all(|pixel| pixel.0[0] == 0 || pixel.0[0] == 255)
        );
    }

    #[test]
    fn compress_fits_the_budget() {
        let image = picture(1200, 900);
        let (bytes, width, _, _) = compress(image, 40 * 1024, Output::Jpeg).unwrap();
        assert!(bytes.len() <= 40 * 1024);
        assert!(width <= 1200);
    }

    #[test]
    fn images_become_one_pdf_page_each() {
        let pdf = images_to_pdf(
            &[
                picture(400, 300),
                picture(300, 400),
                DynamicImage::ImageLuma8(picture(50, 50).to_luma8()),
            ],
            PageSize::A4,
            "auto",
            10.0,
        )
        .unwrap();
        let document = lopdf::Document::load_mem(&pdf).unwrap();
        assert_eq!(document.get_pages().len(), 3);
        // The page pictures come back out.
        let found = crate::documents::extract_pdf_images(&pdf, 10, MAX_IMAGE_BYTES);
        assert_eq!(found.len(), 3);
        assert!(images_to_pdf(&[], PageSize::A4, "auto", 10.0).is_err());
        assert!(images_to_pdf(&[picture(10, 10)], PageSize::Fit, "auto", 99.0).is_err());
    }
}
