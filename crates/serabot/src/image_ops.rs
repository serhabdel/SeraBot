//! Bounded image tools: inspect, resize and convert PNG / JPEG / WebP.
//!
//! Inputs arrive the same way the document tools take them: an artifact hash
//! the acting user owns, or a path in that user's own inbox area (for example
//! the file `local_import` copied from their PC). Results are new artifacts,
//! so an approval or a later `local_export` binds the produced hash.
//!
//! Re-encoding drops all metadata (EXIF, GPS, thumbnails, ICC); the EXIF
//! orientation is applied to the pixels first so the picture does not turn.
//! `image_inspect` reports a few EXIF basics for JPEG and withholds GPS
//! coordinates unless `include_gps` is set.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};
use serde_json::{Value, json};

use crate::domain::Language;
use crate::error::{Result, SeraError};
use crate::gateway::{Tool, ToolContext, ToolOutput, ToolRegistry, ToolSpec};
use crate::maker::{ArtifactMeta, ArtifactStore};
use crate::store::Store;

/// Largest input file.
pub const MAX_IMAGE_BYTES: usize = 50 * 1024 * 1024;
/// Largest decoded picture, in pixels (100 megapixels).
pub const MAX_PIXELS: u64 = 100_000_000;
/// Largest output side, in pixels.
pub const MAX_SIDE: u32 = 8192;
const MAX_ALLOC: u64 = 1024 * 1024 * 1024;

fn invalid(message: impl Into<String>) -> SeraError {
    SeraError::Validation(message.into())
}

// ---------------------------------------------------------------------------
// Source resolution
// ---------------------------------------------------------------------------

struct Source {
    bytes: Vec<u8>,
    filename: String,
    artifact: Option<String>,
}

struct Sources {
    artifacts: Arc<ArtifactStore>,
    store: Store,
    inbox: PathBuf,
}

impl Sources {
    fn resolve(&self, args: &Value, user_id: &str) -> Result<Source> {
        let text = |key: &str| {
            args.get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        match (text("artifact"), text("path")) {
            (Some(hash), None) => {
                if !self.store.user_owns_artifact(user_id, hash)? {
                    return Err(SeraError::Forbidden(
                        "that artifact does not belong to this account".into(),
                    ));
                }
                let stored = self.artifacts.load(hash)?;
                if stored.bytes.len() > MAX_IMAGE_BYTES {
                    return Err(invalid("the image is larger than 50 MB"));
                }
                Ok(Source {
                    bytes: stored.bytes,
                    filename: stored.filename,
                    artifact: Some(stored.hash),
                })
            }
            (None, Some(path)) => {
                let bytes = self.read_inbox(path, user_id)?;
                let filename = path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned();
                Ok(Source {
                    bytes,
                    filename,
                    artifact: None,
                })
            }
            (Some(_), Some(_)) => Err(invalid("provide either artifact or path, not both")),
            (None, None) => Err(invalid("artifact or path is required")),
        }
    }

    /// Inbox paths must sit under the acting user's own directory.
    fn read_inbox(&self, value: &str, user_id: &str) -> Result<Vec<u8>> {
        let candidate = crate::user_inbox::resolve(&self.inbox, user_id, value)?;
        let extension = candidate
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "webp") {
            return Err(invalid("unsupported image type; use png, jpg or webp"));
        }
        let meta = std::fs::metadata(&candidate)?;
        if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_IMAGE_BYTES as u64 {
            return Err(invalid(
                "the image is empty, not a file, or larger than 50 MB",
            ));
        }
        Ok(std::fs::read(candidate)?)
    }
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

fn limits() -> Limits {
    let mut limits = Limits::no_limits();
    limits.max_image_width = Some(20_000);
    limits.max_image_height = Some(20_000);
    limits.max_alloc = Some(MAX_ALLOC);
    limits
}

fn format_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpeg",
        ImageFormat::WebP => "webp",
        _ => "other",
    }
}

/// Format and size without decoding the pixels.
fn probe(bytes: &[u8]) -> Result<(ImageFormat, u32, u32)> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| invalid("not a readable image"))?;
    let format = reader
        .format()
        .filter(|format| {
            matches!(
                format,
                ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP
            )
        })
        .ok_or_else(|| invalid("unsupported image; use PNG, JPEG or WebP"))?;
    let (width, height) = reader
        .into_dimensions()
        .map_err(|_| invalid("not a readable image"))?;
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(invalid("the image is larger than 100 megapixels"));
    }
    Ok((format, width, height))
}

fn decode_oriented(bytes: &[u8]) -> Result<DynamicImage> {
    probe(bytes)?;
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| invalid("not a readable image"))?;
    reader.limits(limits());
    let mut decoder = reader
        .into_decoder()
        .map_err(|_| invalid("not a readable image"))?;
    let orientation = decoder.orientation().ok();
    let mut image =
        DynamicImage::from_decoder(decoder).map_err(|_| invalid("not a readable image"))?;
    if let Some(orientation) = orientation {
        image.apply_orientation(orientation);
    }
    Ok(image)
}

// ---------------------------------------------------------------------------
// EXIF basics (JPEG)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, PartialEq)]
pub struct ExifBasics {
    pub make: Option<String>,
    pub model: Option<String>,
    pub taken: Option<String>,
    pub orientation: Option<u16>,
    pub has_gps: bool,
    /// Decimal degrees; only filled when asked for.
    pub gps: Option<(f64, f64)>,
}

struct Tiff<'a> {
    data: &'a [u8],
    little: bool,
}

impl Tiff<'_> {
    fn u16(&self, at: usize) -> Option<u16> {
        let b: [u8; 2] = self.data.get(at..at + 2)?.try_into().ok()?;
        Some(if self.little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let b: [u8; 4] = self.data.get(at..at + 4)?.try_into().ok()?;
        Some(if self.little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }

    /// `(tag, type, count, value_field_offset)` for every entry of an IFD.
    fn entries(&self, ifd: usize) -> Vec<(u16, u16, u32, usize)> {
        let Some(count) = self.u16(ifd) else {
            return Vec::new();
        };
        (0..usize::from(count).min(256))
            .filter_map(|i| {
                let at = ifd + 2 + i * 12;
                Some((self.u16(at)?, self.u16(at + 2)?, self.u32(at + 4)?, at + 8))
            })
            .collect()
    }

    fn ascii(&self, count: u32, field: usize) -> Option<String> {
        let count = count as usize;
        if count == 0 || count > 256 {
            return None;
        }
        let start = if count <= 4 {
            field
        } else {
            self.u32(field)? as usize
        };
        let raw = self.data.get(start..start + count)?;
        let text = String::from_utf8_lossy(raw);
        let text = text.trim_end_matches('\0').trim();
        (!text.is_empty()).then(|| text.chars().take(64).collect())
    }

    fn rationals(&self, count: u32, field: usize) -> Option<Vec<f64>> {
        if count == 0 || count > 3 {
            return None;
        }
        let start = self.u32(field)? as usize;
        (0..count as usize)
            .map(|i| {
                let num = f64::from(self.u32(start + i * 8)?);
                let den = f64::from(self.u32(start + i * 8 + 4)?);
                (den != 0.0).then_some(num / den)
            })
            .collect()
    }
}

fn dms(values: &[f64], negative: bool) -> Option<f64> {
    let degrees = values.first()?
        + values.get(1).copied().unwrap_or(0.0) / 60.0
        + values.get(2).copied().unwrap_or(0.0) / 3600.0;
    Some(if negative { -degrees } else { degrees })
}

/// Find the Exif TIFF block in a JPEG's APP1 segment.
fn jpeg_exif_tiff(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut at = 2usize;
    while at + 4 <= bytes.len() {
        if bytes[at] != 0xFF {
            return None;
        }
        let marker = bytes[at + 1];
        if marker == 0xDA || marker == 0xD9 {
            return None;
        }
        let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        if length < 2 {
            return None;
        }
        let body = bytes.get(at + 4..at + 2 + length)?;
        if marker == 0xE1 && body.starts_with(b"Exif\0\0") {
            return Some(&body[6..]);
        }
        at += 2 + length;
    }
    None
}

pub fn exif_basics(bytes: &[u8], include_gps: bool) -> Option<ExifBasics> {
    let tiff_bytes = jpeg_exif_tiff(bytes)?;
    let little = match tiff_bytes.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let tiff = Tiff {
        data: tiff_bytes,
        little,
    };
    if tiff.u16(2)? != 42 {
        return None;
    }
    let mut out = ExifBasics::default();
    let ifd0 = tiff.u32(4)? as usize;
    let mut exif_ifd = None;
    let mut gps_ifd = None;
    for (tag, kind, count, field) in tiff.entries(ifd0) {
        match tag {
            0x010F => out.make = tiff.ascii(count, field),
            0x0110 => out.model = tiff.ascii(count, field),
            0x0112 if kind == 3 => out.orientation = tiff.u16(field),
            0x0132 => out.taken = tiff.ascii(count, field),
            0x8769 => exif_ifd = tiff.u32(field).map(|v| v as usize),
            0x8825 => gps_ifd = tiff.u32(field).map(|v| v as usize),
            _ => {}
        }
    }
    if let Some(ifd) = exif_ifd {
        for (tag, _, count, field) in tiff.entries(ifd) {
            if tag == 0x9003 {
                out.taken = tiff.ascii(count, field);
            }
        }
    }
    if let Some(ifd) = gps_ifd {
        out.has_gps = true;
        if include_gps {
            let (mut lat, mut lat_ref, mut lon, mut lon_ref) = (None, None, None, None);
            for (tag, _, count, field) in tiff.entries(ifd) {
                match tag {
                    1 => lat_ref = tiff.ascii(count, field),
                    2 => lat = tiff.rationals(count, field),
                    3 => lon_ref = tiff.ascii(count, field),
                    4 => lon = tiff.rationals(count, field),
                    _ => {}
                }
            }
            if let (Some(lat), Some(lon)) = (lat, lon) {
                out.gps = dms(&lat, lat_ref.as_deref() == Some("S"))
                    .zip(dms(&lon, lon_ref.as_deref() == Some("W")));
            }
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Transform
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Output {
    Png,
    Jpeg,
    WebP,
}

impl Output {
    fn parse(text: &str) -> Result<Self> {
        match text.to_ascii_lowercase().as_str() {
            "png" => Ok(Self::Png),
            "jpg" | "jpeg" => Ok(Self::Jpeg),
            "webp" => Ok(Self::WebP),
            _ => Err(invalid("format must be png, jpg or webp")),
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::WebP => "webp",
        }
    }

    fn of(format: ImageFormat) -> Self {
        match format {
            ImageFormat::Jpeg => Self::Jpeg,
            ImageFormat::WebP => Self::WebP,
            _ => Self::Png,
        }
    }
}

/// Resize (keeping the aspect ratio, never upscaling past `MAX_SIDE`) and/or
/// re-encode. `width`/`height` are a bounding box; either may be omitted.
pub fn transform(
    bytes: &[u8],
    size: Option<(Option<u32>, Option<u32>)>,
    output: Output,
    quality: u8,
) -> Result<(Vec<u8>, u32, u32)> {
    let mut image = decode_oriented(bytes)?;
    if let Some((width, height)) = size {
        let (current_w, current_h) = (image.width(), image.height());
        let box_w = width.unwrap_or(u32::MAX);
        let box_h = height.unwrap_or(u32::MAX);
        if width.is_none() && height.is_none() {
            return Err(invalid("give a width, a height, or both"));
        }
        if width == Some(0)
            || height == Some(0)
            || width > Some(MAX_SIDE)
            || height > Some(MAX_SIDE)
        {
            return Err(invalid(format!("width and height must be 1-{MAX_SIDE}")));
        }
        let scale =
            (f64::from(box_w) / f64::from(current_w)).min(f64::from(box_h) / f64::from(current_h));
        let new_w = ((f64::from(current_w) * scale).round() as u32).clamp(1, MAX_SIDE);
        let new_h = ((f64::from(current_h) * scale).round() as u32).clamp(1, MAX_SIDE);
        image = image.resize_exact(new_w, new_h, image::imageops::FilterType::Lanczos3);
    }
    let (width, height) = (image.width(), image.height());
    let mut out = Vec::new();
    match output {
        Output::Png => image
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .map_err(|_| invalid("could not encode the PNG"))?,
        Output::Jpeg => {
            // JPEG has no alpha channel.
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
    Ok((out, width, height))
}

fn derive_name(source: &str, suffix: &str, extension: &str) -> String {
    let stem = Path::new(source)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("image");
    let stem: String = stem.chars().take(100).collect();
    format!("{stem}-{suffix}.{extension}")
}

fn optional_dimension(args: &Value, key: &str) -> Result<Option<u32>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|n| (1..=u64::from(MAX_SIDE)).contains(n))
            .map(|n| Some(n as u32))
            .ok_or_else(|| invalid(format!("{key} must be a whole number from 1 to {MAX_SIDE}"))),
    }
}

fn reject_unknown(args: &Value, allowed: &[&str]) -> Result<()> {
    let Some(object) = args.as_object() else {
        return Err(invalid("arguments must be a JSON object"));
    };
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(invalid(format!("unsupported argument: {key}")));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

pub struct ImageInspectTool {
    sources: Sources,
}

pub struct ImageTransformTool {
    sources: Sources,
    artifacts: Arc<ArtifactStore>,
    store: Store,
    /// `image_resize` or `image_convert`.
    name: &'static str,
}

fn source_properties() -> Value {
    json!({
        "artifact": {"type": "string", "description": "Artifact hash of the image (for example from local_import)"},
        "path": {"type": "string", "description": "Inbox path of the image (for example from local_import)"}
    })
}

#[async_trait]
impl Tool for ImageInspectTool {
    fn spec(&self) -> ToolSpec {
        let mut properties = source_properties();
        properties["include_gps"] = json!({"type": "boolean", "description": "Also report GPS coordinates (withheld by default)"});
        ToolSpec {
            name: "image_inspect".into(),
            description: "Inspect a PNG, JPEG or WebP image: format, dimensions, size, and EXIF \
                          basics (camera, date, orientation) for JPEG. GPS coordinates are \
                          withheld unless include_gps is true."
                .into(),
            parameters: json!({"type": "object", "properties": properties, "additionalProperties": false}),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        reject_unknown(&args, &["artifact", "path", "include_gps"])?;
        let include_gps = args
            .get("include_gps")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let source = self.sources.resolve(&args, &context.user_id)?;
        let size = source.bytes.len();
        let bytes = source.bytes;
        let (format, width, height, exif) = tokio::task::spawn_blocking(move || {
            let (format, width, height) = probe(&bytes)?;
            let exif = if format == ImageFormat::Jpeg {
                exif_basics(&bytes, include_gps)
            } else {
                None
            };
            Ok::<_, SeraError>((format, width, height, exif))
        })
        .await
        .map_err(|error| SeraError::Internal(format!("image task failed: {error}")))??;
        let mut lines = vec![
            format!("file: {}", source.filename),
            format!("format: {}", format_name(format)),
            format!("size: {width}x{height} px ({} bytes)", size),
        ];
        let mut data = json!({
            "filename": source.filename, "format": format_name(format),
            "width": width, "height": height, "bytes": size, "artifact": source.artifact,
        });
        match &exif {
            Some(exif) => {
                if let Some(make) = &exif.make {
                    lines.push(format!("camera make: {make}"));
                }
                if let Some(model) = &exif.model {
                    lines.push(format!("camera model: {model}"));
                }
                if let Some(taken) = &exif.taken {
                    lines.push(format!("taken: {taken}"));
                }
                if let Some(orientation) = exif.orientation {
                    lines.push(format!("orientation: {orientation}"));
                }
                match (exif.has_gps, exif.gps) {
                    (true, Some((lat, lon))) => {
                        lines.push(format!("gps: {lat:.5}, {lon:.5}"));
                    }
                    (true, None) => lines
                        .push("gps: present (withheld; pass include_gps=true to see it)".into()),
                    _ => lines.push("gps: none".into()),
                }
                data["exif"] = json!({
                    "make": exif.make, "model": exif.model, "taken": exif.taken,
                    "orientation": exif.orientation, "has_gps": exif.has_gps,
                    "gps": exif.gps.map(|(lat, lon)| json!({"lat": lat, "lon": lon})),
                });
            }
            None => lines.push(if format == ImageFormat::Jpeg {
                "exif: none".into()
            } else {
                "exif: not read for this format".into()
            }),
        }
        Ok(ToolOutput {
            content: lines.join("\n"),
            data: Some(data),
            ..ToolOutput::default()
        })
    }
}

#[async_trait]
impl Tool for ImageTransformTool {
    fn spec(&self) -> ToolSpec {
        let mut properties = source_properties();
        let (description, required): (&str, Vec<&str>) = if self.name == "image_resize" {
            properties["width"] =
                json!({"type": "integer", "description": "Maximum width in pixels, 1-8192"});
            properties["height"] =
                json!({"type": "integer", "description": "Maximum height in pixels, 1-8192"});
            properties["format"] = json!({"type": "string", "enum": ["png", "jpg", "webp"], "description": "Output format (default: same as the input)"});
            (
                "Resize a PNG, JPEG or WebP image to fit inside a width/height box, keeping the \
                 aspect ratio. Produces a new artifact; metadata such as EXIF and GPS is removed.",
                Vec::new(),
            )
        } else {
            properties["format"] = json!({"type": "string", "enum": ["png", "jpg", "webp"], "description": "Output format"});
            (
                "Convert a PNG, JPEG or WebP image to png, jpg or webp. Produces a new artifact; \
                 metadata such as EXIF and GPS is removed.",
                vec!["format"],
            )
        };
        properties["quality"] =
            json!({"type": "integer", "description": "JPEG quality 1-100 (default 85)"});
        ToolSpec {
            name: self.name.into(),
            description: description.into(),
            parameters: json!({
                "type": "object", "properties": properties,
                "required": required, "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let resize = self.name == "image_resize";
        reject_unknown(
            &args,
            if resize {
                &["artifact", "path", "width", "height", "format", "quality"]
            } else {
                &["artifact", "path", "format", "quality"]
            },
        )?;
        let source = self.sources.resolve(&args, &context.user_id)?;
        let size = if resize {
            Some((
                optional_dimension(&args, "width")?,
                optional_dimension(&args, "height")?,
            ))
        } else {
            None
        };
        if let Some((None, None)) = size {
            return Err(invalid("give a width, a height, or both"));
        }
        let quality = match args.get("quality") {
            None | Some(Value::Null) => 85u8,
            Some(value) => value
                .as_u64()
                .filter(|q| (1..=100).contains(q))
                .ok_or_else(|| invalid("quality must be 1-100"))? as u8,
        };
        let requested = args.get("format").and_then(Value::as_str);
        if !resize && requested.is_none() {
            return Err(invalid("format is required"));
        }
        let bytes = source.bytes;
        let requested = requested.map(Output::parse).transpose()?;
        let (encoded, width, height, output) = tokio::task::spawn_blocking(move || {
            let (format, _, _) = probe(&bytes)?;
            let output = requested.unwrap_or_else(|| Output::of(format));
            let (encoded, width, height) = transform(&bytes, size, output, quality)?;
            Ok::<_, SeraError>((encoded, width, height, output))
        })
        .await
        .map_err(|error| SeraError::Internal(format!("image task failed: {error}")))??;
        let suffix = if resize { "resized" } else { "converted" };
        let filename = derive_name(&source.filename, suffix, output.extension());
        let stored = self.artifacts.save(
            &encoded,
            &filename,
            ArtifactMeta {
                title: filename.clone(),
                filename: filename.clone(),
                language: Language::En,
                source_artifact: source.artifact.clone(),
                created_at: Utc::now(),
            },
        )?;
        self.store.record_audit(
            &context.actor,
            "image.transformed",
            Some(self.name),
            context.task_id.as_deref(),
            None,
            None,
            None,
            Some(&json!({
                "artifact_hash": stored.hash, "source_artifact": source.artifact,
                "width": width, "height": height,
            })),
        )?;
        let data = json!({
            "artifact_hash": stored.hash, "filename": stored.filename,
            "width": width, "height": height, "format": output.extension(),
            "bytes": encoded.len(), "source_artifact": source.artifact,
        });
        Ok(ToolOutput {
            content: format!(
                "filename: {}\nartifact: {}\nsize: {width}x{height} px, {} bytes\nformat: {}\nmetadata removed",
                stored.filename,
                stored.hash,
                encoded.len(),
                output.extension()
            ),
            data: Some(data),
            artifact_path: Some(format!("artifacts/{}", stored.filename)),
            artifact_hash: Some(stored.hash),
            filename: Some(stored.filename),
            destination: None,
            cards: Vec::new(),
        })
    }
}

/// Register `image_inspect`, `image_resize` and `image_convert`.
pub fn register_image_tools(
    registry: &mut ToolRegistry,
    artifacts: Arc<ArtifactStore>,
    store: Store,
    inbox: PathBuf,
) -> Result<()> {
    let sources = || Sources {
        artifacts: artifacts.clone(),
        store: store.clone(),
        inbox: inbox.clone(),
    };
    registry.register(ImageInspectTool { sources: sources() })?;
    for name in ["image_resize", "image_convert"] {
        registry.register(ImageTransformTool {
            sources: sources(),
            artifacts: artifacts.clone(),
            store: store.clone(),
            name,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    struct Fixture {
        artifacts: Arc<ArtifactStore>,
        store: Store,
        inbox: PathBuf,
        base: PathBuf,
        user: String,
    }

    fn fixture() -> Fixture {
        let base = std::env::temp_dir().join(format!("serabot-img-{}", crate::domain::new_id()));
        std::fs::create_dir_all(&base).unwrap();
        let store = Store::open_in_memory(384).unwrap();
        let user = store
            .create_user("subject-img", "img@example.example", "Synthetic")
            .unwrap()
            .id;
        store
            .create_conversation(&user, "dir:main", "direct", None)
            .unwrap();
        Fixture {
            artifacts: Arc::new(ArtifactStore::new(base.join("artifacts")).unwrap()),
            store,
            inbox: base.join("inbox"),
            base,
            user,
        }
    }

    fn context(user: &str) -> ToolContext {
        ToolContext {
            user_id: user.into(),
            actor: user.into(),
            bot: "main".into(),
            task_id: None,
            turn_id: None,
            conversation_id: Some("dir:main".into()),
            allowed_tools: HashSet::new(),
            require_approval: HashSet::new(),
        }
    }

    fn sample(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
        let image = image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([(x * 40) as u8, (y * 60) as u8, 128, 255])
        });
        let mut out = Vec::new();
        let image = DynamicImage::ImageRgba8(image);
        if format == ImageFormat::Jpeg {
            DynamicImage::ImageRgb8(image.to_rgb8())
                .write_to(&mut Cursor::new(&mut out), format)
                .unwrap();
        } else {
            image.write_to(&mut Cursor::new(&mut out), format).unwrap();
        }
        out
    }

    fn put_artifact(f: &Fixture, bytes: &[u8], name: &str) -> String {
        let stored = f
            .artifacts
            .save(
                bytes,
                name,
                ArtifactMeta {
                    title: name.into(),
                    filename: name.into(),
                    language: Language::En,
                    source_artifact: None,
                    created_at: Utc::now(),
                },
            )
            .unwrap();
        f.store
            .record_conversation_artifact(
                &f.user,
                "dir:main",
                None,
                "local_import",
                &stored.hash,
                name,
                None,
            )
            .unwrap();
        stored.hash
    }

    /// A JPEG carrying a hand-built little-endian Exif block: Make, Orientation
    /// 6 and a GPS IFD (48 deg 51 min N, 2 deg 21 min E).
    fn jpeg_with_exif(width: u32, height: u32) -> Vec<u8> {
        fn entry(tag: u16, kind: u16, count: u32, value: [u8; 4]) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend(tag.to_le_bytes());
            out.extend(kind.to_le_bytes());
            out.extend(count.to_le_bytes());
            out.extend(value);
            out
        }
        let mut tiff: Vec<u8> = b"II*\0".to_vec();
        tiff.extend(8u32.to_le_bytes());
        // IFD0 at 8: 3 entries -> ends at 8 + 2 + 36 + 4 = 50.
        tiff.extend(3u16.to_le_bytes());
        tiff.extend(entry(0x010F, 2, 8, 50u32.to_le_bytes()));
        tiff.extend(entry(0x0112, 3, 1, [6, 0, 0, 0]));
        tiff.extend(entry(0x8825, 4, 1, 58u32.to_le_bytes()));
        tiff.extend(0u32.to_le_bytes());
        tiff.extend(b"TestCam\0");
        // GPS IFD at 58: 4 entries -> ends at 58 + 2 + 48 + 4 = 112.
        tiff.extend(4u16.to_le_bytes());
        tiff.extend(entry(1, 2, 2, [b'N', 0, 0, 0]));
        tiff.extend(entry(2, 5, 3, 112u32.to_le_bytes()));
        tiff.extend(entry(3, 2, 2, [b'E', 0, 0, 0]));
        tiff.extend(entry(4, 5, 3, 136u32.to_le_bytes()));
        tiff.extend(0u32.to_le_bytes());
        for (degrees, minutes) in [(48u32, 51u32), (2, 21)] {
            for value in [degrees, minutes, 0] {
                tiff.extend(value.to_le_bytes());
                tiff.extend(1u32.to_le_bytes());
            }
        }
        let mut segment = b"Exif\0\0".to_vec();
        segment.extend(&tiff);
        let jpeg = sample(width, height, ImageFormat::Jpeg);
        let mut out = jpeg[..2].to_vec();
        out.extend([0xFF, 0xE1]);
        out.extend(((segment.len() + 2) as u16).to_be_bytes());
        out.extend(segment);
        out.extend(&jpeg[2..]);
        out
    }

    #[test]
    fn exif_basics_withhold_gps_unless_asked() {
        let jpeg = jpeg_with_exif(4, 3);
        let hidden = exif_basics(&jpeg, false).unwrap();
        assert_eq!(hidden.make.as_deref(), Some("TestCam"));
        assert_eq!(hidden.orientation, Some(6));
        assert!(hidden.has_gps && hidden.gps.is_none());
        let shown = exif_basics(&jpeg, true).unwrap();
        let (lat, lon) = shown.gps.unwrap();
        assert!(
            (lat - 48.85).abs() < 1e-6 && (lon - 2.35).abs() < 1e-6,
            "{lat} {lon}"
        );
        assert!(exif_basics(&sample(2, 2, ImageFormat::Jpeg), true).is_none());
        // Truncated or hostile blocks never panic.
        for cut in 0..jpeg.len().min(200) {
            let _ = exif_basics(&jpeg[..cut], true);
        }
        let mut hostile = jpeg.clone();
        for byte in hostile.iter_mut().skip(14).take(40) {
            *byte = 0xFF;
        }
        let _ = exif_basics(&hostile, true);
    }

    #[test]
    fn transform_resizes_converts_and_applies_orientation() {
        let png = sample(8, 4, ImageFormat::Png);
        let (small, w, h) = transform(&png, Some((Some(4), None)), Output::Png, 85).unwrap();
        assert_eq!((w, h), (4, 2));
        assert_eq!(probe(&small).unwrap().0, ImageFormat::Png);
        let (boxed, w, h) = transform(&png, Some((Some(2), Some(2))), Output::Jpeg, 80).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(probe(&boxed).unwrap().0, ImageFormat::Jpeg);
        let (webp, w, h) = transform(&png, None, Output::WebP, 85).unwrap();
        assert_eq!((w, h), (8, 4));
        assert_eq!(probe(&webp).unwrap().0, ImageFormat::WebP);
        // Orientation 6 turns the 4x3 picture to 3x4, and the metadata is gone.
        let (rotated, w, h) = transform(&jpeg_with_exif(4, 3), None, Output::Jpeg, 85).unwrap();
        assert_eq!((w, h), (3, 4));
        assert!(exif_basics(&rotated, true).is_none());
        assert!(!rotated.windows(4).any(|window| window == b"Exif"));
        for bad in [
            Some((Some(0), None)),
            Some((Some(9000), None)),
            Some((None, None)),
        ] {
            assert!(transform(&png, bad, Output::Png, 85).is_err());
        }
        assert!(transform(b"not an image", None, Output::Png, 85).is_err());
        assert!(transform(&png[..20], None, Output::Png, 85).is_err());
    }

    #[tokio::test]
    async fn tools_inspect_resize_and_convert_artifacts() {
        let f = fixture();
        let hash = put_artifact(&f, &jpeg_with_exif(4, 3), "photo.jpg");
        let mut registry = ToolRegistry::default();
        register_image_tools(
            &mut registry,
            f.artifacts.clone(),
            f.store.clone(),
            f.inbox.clone(),
        )
        .unwrap();
        for name in ["image_inspect", "image_resize", "image_convert"] {
            assert!(registry.is_registered(name), "{name}");
        }
        let inspect = ImageInspectTool {
            sources: Sources {
                artifacts: f.artifacts.clone(),
                store: f.store.clone(),
                inbox: f.inbox.clone(),
            },
        };
        let ctx = context(&f.user);
        let output = inspect
            .execute(json!({"artifact": hash}), &ctx)
            .await
            .unwrap();
        assert!(
            output.content.contains("format: jpeg") && output.content.contains("size: 4x3 px"),
            "{}",
            output.content
        );
        assert!(
            output.content.contains("camera make: TestCam")
                && output.content.contains("gps: present (withheld"),
            "{}",
            output.content
        );
        assert!(!output.content.contains("48.85"));
        let with_gps = inspect
            .execute(json!({"artifact": hash, "include_gps": true}), &ctx)
            .await
            .unwrap();
        assert!(
            with_gps.content.contains("gps: 48.85000, 2.35000"),
            "{}",
            with_gps.content
        );

        let tool = |name| ImageTransformTool {
            sources: Sources {
                artifacts: f.artifacts.clone(),
                store: f.store.clone(),
                inbox: f.inbox.clone(),
            },
            artifacts: f.artifacts.clone(),
            store: f.store.clone(),
            name,
        };
        let resized = tool("image_resize")
            .execute(
                json!({"artifact": hash, "width": 2, "format": "webp"}),
                &ctx,
            )
            .await
            .unwrap();
        let out_hash = resized.artifact_hash.clone().unwrap();
        assert_eq!(resized.filename.as_deref(), Some("photo-resized.webp"));
        let stored = f.artifacts.load(&out_hash).unwrap();
        assert_eq!(probe(&stored.bytes).unwrap(), (ImageFormat::WebP, 2, 3));
        let converted = tool("image_convert")
            .execute(json!({"artifact": out_hash, "format": "png"}), &ctx)
            .await;
        // The produced artifact is not recorded for the user yet (the gateway
        // does that), so ownership blocks chaining until it is.
        assert!(matches!(converted, Err(SeraError::Forbidden(_))));
        f.store
            .record_conversation_artifact(
                &f.user,
                "dir:main",
                None,
                "image_resize",
                &out_hash,
                "photo-resized.webp",
                None,
            )
            .unwrap();
        let converted = tool("image_convert")
            .execute(json!({"artifact": out_hash, "format": "png"}), &ctx)
            .await
            .unwrap();
        assert_eq!(
            converted.filename.as_deref(),
            Some("photo-resized-converted.png")
        );
        for bad in [
            json!({"artifact": hash}),
            json!({"artifact": hash, "format": "gif"}),
            json!({"artifact": hash, "format": "png", "junk": 1}),
            json!({"format": "png"}),
            json!({"artifact": hash, "path": "x/y.png", "format": "png"}),
        ] {
            assert!(
                tool("image_convert")
                    .execute(bad.clone(), &ctx)
                    .await
                    .is_err(),
                "{bad}"
            );
        }
        for bad in [
            json!({"artifact": hash}),
            json!({"artifact": hash, "width": 0}),
            json!({"artifact": hash, "width": 9000}),
            json!({"artifact": hash, "width": 2, "quality": 0}),
        ] {
            assert!(
                tool("image_resize")
                    .execute(bad.clone(), &ctx)
                    .await
                    .is_err(),
                "{bad}"
            );
        }
        std::fs::remove_dir_all(&f.base).ok();
    }

    #[tokio::test]
    async fn sources_are_scoped_to_the_acting_user() {
        let f = fixture();
        let other = f
            .store
            .create_user("subject-img2", "img2@example.example", "Other")
            .unwrap()
            .id;
        let hash = put_artifact(&f, &sample(2, 2, ImageFormat::Png), "mine.png");
        let own = f.inbox.join(&f.user).join("local");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::write(own.join("abc-pic.png"), sample(3, 3, ImageFormat::Png)).unwrap();
        std::fs::write(f.inbox.join("shared.png"), sample(3, 3, ImageFormat::Png)).unwrap();
        let inspect = ImageInspectTool {
            sources: Sources {
                artifacts: f.artifacts.clone(),
                store: f.store.clone(),
                inbox: f.inbox.clone(),
            },
        };
        let mine = inspect
            .execute(
                json!({"path": format!("{}/local/abc-pic.png", f.user)}),
                &context(&f.user),
            )
            .await
            .unwrap();
        assert!(mine.content.contains("size: 3x3 px"));
        // Another account cannot use my artifact or my inbox path.
        assert!(matches!(
            inspect
                .execute(json!({"artifact": hash}), &context(&other))
                .await,
            Err(SeraError::Forbidden(_))
        ));
        for path in [
            format!("{}/local/abc-pic.png", f.user),
            "shared.png".to_owned(),
            format!("{}/../shared.png", f.user),
            format!("{}/local/../../shared.png", f.user),
            "/etc/passwd".to_owned(),
            "C:\\x.png".to_owned(),
        ] {
            assert!(
                inspect
                    .execute(json!({"path": path}), &context(&other))
                    .await
                    .is_err(),
                "{path}"
            );
        }
        assert!(
            inspect
                .execute(json!({"path": "shared.png"}), &context(&f.user))
                .await
                .is_err()
        );
        std::fs::remove_dir_all(&f.base).ok();
    }

    #[test]
    fn limits_reject_huge_dimensions_without_decoding() {
        // A PNG header declaring 20000 x 20000 px (400 MP) is refused by probe.
        let mut png = sample(1, 1, ImageFormat::Png);
        png[16..20].copy_from_slice(&20_000u32.to_be_bytes());
        png[20..24].copy_from_slice(&20_000u32.to_be_bytes());
        assert!(probe(&png).is_err());
        assert!(decode_oriented(&png).is_err());
    }
}
