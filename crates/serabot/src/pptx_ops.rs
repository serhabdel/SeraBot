//! Bounded PPTX round trip: import an owner-supplied package, inspect it, edit
//! it, and export it again.
//!
//! The split with [`crate::deck_pptx`] is deliberate:
//!
//! - `deck_build` renders a *generated* deck from a validated `DeckSpec`. That
//!   package stays text-only — no media, links, notes, or embedded files.
//! - This module reads a package the *owner* supplied. Its images and media are
//!   the owner's content, not model output, so an edit round trip keeps them.
//!   Hyperlinks and external relationships are dropped on export.
//!
//! Every entry point takes an artifact hash or an inbox-relative path resolved
//! by [`crate::maker::ArtifactStore::ingest_inbox_file`]; a model never names an
//! arbitrary filesystem path. Packages are bounded before they are parsed: 32
//! MiB, 500 slides, 4096 zip parts, 128 MiB uncompressed, and 20 000 shapes. A
//! password-protected package and any package carrying a VBA project are
//! rejected outright instead of being sanitised.
//!
//! Text the model writes into an edit is bounded here and scanned by the
//! runtime against the blocklist before the next model round, exactly as for
//! every other tool argument.

use std::collections::{BTreeSet, HashSet};
use std::io::{Cursor, Read, Seek};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use deckcraft_model::text::{Paragraph, Run, TextBody};
use deckcraft_model::{Fill, Presentation, Shape, ShapeId, ShapeKind, Slide, SlideId, walk};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zip::ZipArchive;

use crate::domain::Language;
use crate::error::{Result, SeraError};
use crate::gateway::{Tool, ToolContext, ToolOutput, ToolSpec};
use crate::maker::{ArtifactMeta, ArtifactStore, StoredArtifact};
use crate::store::Store;
use crate::util::truncate_chars;

/// Hard ceiling for one input package.
pub const MAX_PPTX_BYTES: usize = 32 * 1024 * 1024;
/// Hard ceiling for the slide count of one package.
pub const MAX_SLIDES: usize = 500;
/// Hard ceiling for the number of zip parts in one package.
pub const MAX_ZIP_PARTS: usize = 4096;
/// Hard ceiling for the total uncompressed size, so a small package cannot
/// expand into an unbounded parse.
pub const MAX_UNCOMPRESSED_BYTES: u64 = 128 * 1024 * 1024;
/// Hard ceiling for shapes across the whole deck. Every walk afterwards is
/// bounded by this count.
pub const MAX_SHAPES: usize = 20_000;
/// Hard ceiling for one replacement string.
pub const MAX_TEXT_CHARS: usize = 4_000;
/// Hard ceiling for the number of text operations in one edit.
pub const MAX_EDIT_OPS: usize = 200;
/// A single XML part larger than this is refused while checking an export.
const MAX_XML_PART_BYTES: u64 = 8 * 1024 * 1024;
/// Titles are reported truncated; a slide's title is a label, not content.
const MAX_TITLE_CHARS: usize = 200;
/// Shape trees nest this deep before the walk stops (the model uses the same).
const MAX_SHAPE_DEPTH: usize = 64;
/// Shapes listed per slide by `pptx_inspect`. `shapes` still reports the full
/// count, and `set_text` still accepts any index in `1..=shapes`; the cap only
/// bounds the listing.
const MAX_LISTED_SHAPES: usize = 100;
/// Slides listed by `pptx_inspect`. `slide_count` still reports the full count.
const MAX_LISTED_SLIDES: usize = 200;
/// Shape text previews are capped; `chars` reports the full count.
const MAX_PREVIEW_CHARS: usize = 200;

fn invalid(message: impl Into<String>) -> SeraError {
    SeraError::Validation(message.into())
}

// ---------------------------------------------------------------------------
// Package preflight and parsing
// ---------------------------------------------------------------------------

/// `ppt/slides/slideN.xml`, the only part that counts as a slide.
fn is_slide_part(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("ppt/slides/") else {
        return false;
    };
    let Some(stem) = rest.strip_suffix(".xml") else {
        return false;
    };
    !stem.is_empty() && !stem.contains('/') && stem.starts_with("slide")
}

/// Whether `[Content_Types].xml` declares a macro-enabled presentation.
fn declares_macros<R: Read + Seek>(archive: &mut ZipArchive<R>) -> Result<bool> {
    let Ok(mut entry) = archive.by_name("[Content_Types].xml") else {
        return Ok(false);
    };
    if entry.size() > MAX_XML_PART_BYTES {
        return Err(invalid("[Content_Types].xml is too large"));
    }
    let mut xml = String::new();
    entry
        .read_to_string(&mut xml)
        .map_err(|error| invalid(format!("cannot read [Content_Types].xml: {error}")))?;
    Ok(xml.to_ascii_lowercase().contains("macroenabled"))
}

/// Bounded structural checks that run before the package is parsed.
///
/// Rejects oversized input, encrypted packages, macro-enabled packages, zip
/// bombs, path-traversal part names, and too many slides or parts.
fn preflight(bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() {
        return Err(invalid("PPTX input is empty"));
    }
    if bytes.len() > MAX_PPTX_BYTES {
        return Err(invalid(format!(
            "PPTX input is {} bytes; the limit is {MAX_PPTX_BYTES} bytes (32 MiB)",
            bytes.len()
        )));
    }
    if deckcraft_pptx::is_encrypted(bytes) {
        return Err(invalid(
            "PPTX is password-protected; decrypt it before importing it",
        ));
    }
    if !deckcraft_pptx::sniff(bytes) {
        return Err(invalid("not an OOXML presentation package"));
    }
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| invalid(format!("malformed PPTX package: {error}")))?;
    let parts = archive.len();
    if parts > MAX_ZIP_PARTS {
        return Err(invalid(format!(
            "package has {parts} parts; the limit is {MAX_ZIP_PARTS}"
        )));
    }
    let mut slides = 0usize;
    let mut uncompressed = 0u64;
    for index in 0..parts {
        let entry = archive
            .by_index_raw(index)
            .map_err(|error| invalid(format!("malformed PPTX part {index}: {error}")))?;
        let name = entry.name().to_ascii_lowercase();
        if name.contains("..") || name.starts_with('/') || name.contains('\\') {
            return Err(invalid(format!(
                "package part {name:?} is not a valid part name"
            )));
        }
        if name.ends_with("vbaproject.bin") {
            return Err(invalid(
                "PPTX contains a VBA project; macro-enabled presentations are rejected",
            ));
        }
        uncompressed = uncompressed.saturating_add(entry.size());
        if uncompressed > MAX_UNCOMPRESSED_BYTES {
            return Err(invalid(format!(
                "package expands to more than {MAX_UNCOMPRESSED_BYTES} bytes"
            )));
        }
        if is_slide_part(&name) {
            slides += 1;
        }
    }
    if slides == 0 {
        return Err(invalid("PPTX has no slides"));
    }
    if slides > MAX_SLIDES {
        return Err(invalid(format!(
            "PPTX has {slides} slides; the limit is {MAX_SLIDES}"
        )));
    }
    if declares_macros(&mut archive)? {
        return Err(invalid(
            "PPTX declares a macro-enabled content type; macro-enabled presentations are rejected",
        ));
    }
    Ok(())
}

/// Parse a preflighted package into the document model.
fn parse(bytes: &[u8]) -> Result<Presentation> {
    let presentation = catch_unwind(AssertUnwindSafe(|| deckcraft_pptx::import(bytes)))
        .map_err(|_| invalid("malformed PPTX: the importer aborted"))?
        .map_err(|error| invalid(format!("cannot read PPTX: {error}")))?;
    if presentation.slides.is_empty() {
        return Err(invalid("PPTX has no slides"));
    }
    if presentation.slides.len() > MAX_SLIDES {
        return Err(invalid(format!(
            "PPTX has {} slides; the limit is {MAX_SLIDES}",
            presentation.slides.len()
        )));
    }
    let problems = presentation.validate();
    if !problems.is_empty() {
        return Err(invalid(format!(
            "PPTX is inconsistent: {}",
            problems.join("; ")
        )));
    }
    Ok(presentation)
}

fn count_shapes(presentation: &Presentation) -> usize {
    let mut total = 0usize;
    for slide in &presentation.slides {
        walk(&slide.shapes, &mut |_, _| total += 1);
    }
    total
}

/// Preflight, parse, and bound the shape count. Every later walk is bounded by
/// [`MAX_SHAPES`].
fn open(bytes: &[u8]) -> Result<Presentation> {
    preflight(bytes)?;
    let presentation = parse(bytes)?;
    let shapes = count_shapes(&presentation);
    if shapes > MAX_SHAPES {
        return Err(invalid(format!(
            "PPTX has {shapes} shapes; the limit is {MAX_SHAPES}"
        )));
    }
    Ok(presentation)
}

// ---------------------------------------------------------------------------
// Inspection
// ---------------------------------------------------------------------------

/// What a listed shape is, from the point of view of `set_text`.
///
/// [`PptxShapeKind::Text`] is exactly the set `set_text` can address: an
/// autoshape or a text box. A table is listed, but its cells are not editable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PptxShapeKind {
    Text,
    Picture,
    Table,
    Group,
    Other,
}

impl std::fmt::Display for PptxShapeKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::Text => "text",
            Self::Picture => "picture",
            Self::Table => "table",
            Self::Group => "group",
            Self::Other => "other",
        };
        formatter.write_str(label)
    }
}

/// One shape on a slide, addressed exactly the way `set_text` addresses it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PptxShapeInfo {
    /// 1-based depth-first index; pass this value as `set_text.shape`.
    pub index: usize,
    pub kind: PptxShapeKind,
    /// Text preview, at most 200 characters.
    pub text: String,
    /// Full character count of the shape's text; 0 when it has none.
    pub chars: usize,
    /// True when the shape is a picture or is filled with one.
    pub has_image: bool,
}

/// One slide as reported by [`inspect`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PptxSlideInfo {
    /// 1-based slide number.
    pub slide: usize,
    /// The title placeholder's text, or the first non-empty text line when the
    /// slide has no title placeholder. Truncated to 200 characters.
    pub title: String,
    /// Characters of slide text (shapes and table cells); notes are excluded.
    pub text_chars: usize,
    /// The full shape count. `shape_list` is capped, this is not.
    pub shapes: usize,
    pub pictures: usize,
    pub has_notes: bool,
    pub has_links: bool,
    /// The shapes on this slide in the order `set_text` addresses them, capped
    /// at 100 entries. `set_text` accepts any index in `1..=shapes`.
    pub shape_list: Vec<PptxShapeInfo>,
    /// True when `shape_list` was capped.
    pub truncated: bool,
}

/// Bounded structural summary of a package.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PptxInfo {
    pub slide_count: usize,
    pub slides: Vec<PptxSlideInfo>,
    pub text_chars: usize,
    pub shapes: usize,
    /// Embedded media parts that will be written back on export.
    pub media_parts: usize,
    /// Media items that are only an external link; export drops them.
    pub linked_media: usize,
    pub has_notes: bool,
    pub has_links: bool,
    /// True when the slide list or any per-slide shape list was capped. The
    /// counts above are always complete.
    pub truncated: bool,
}

/// The addressing `set_text` accepts: 1-based depth-first order over the
/// slide's shape tree.
///
/// `inspect` lists shapes through this same function, so a listed index is by
/// construction the index an edit takes. There is no second traversal to drift.
fn ordered_shapes(slide: &Slide) -> Vec<&Shape> {
    let mut shapes = Vec::new();
    walk(&slide.shapes, &mut |shape, _| shapes.push(shape));
    shapes
}

fn shape_kind(shape: &Shape) -> PptxShapeKind {
    match &shape.kind {
        ShapeKind::Shape => PptxShapeKind::Text,
        ShapeKind::Picture { .. } => PptxShapeKind::Picture,
        ShapeKind::Table(_) => PptxShapeKind::Table,
        ShapeKind::Group { .. } => PptxShapeKind::Group,
        _ => PptxShapeKind::Other,
    }
}

fn carries_image(shape: &Shape) -> bool {
    matches!(&shape.kind, ShapeKind::Picture { .. })
        || matches!(&shape.fill, Some(Fill::Picture(_)))
}

/// A one-line, control-character-free preview of a shape's text.
fn preview(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&flat, MAX_PREVIEW_CHARS)
}

fn shape_info(index: usize, shape: &Shape) -> PptxShapeInfo {
    PptxShapeInfo {
        index,
        kind: shape_kind(shape),
        text: shape
            .text
            .as_ref()
            .map(TextBody::text)
            .map(|text| preview(&text))
            .unwrap_or_default(),
        chars: shape.text.as_ref().map(TextBody::char_len).unwrap_or(0),
        has_image: carries_image(shape),
    }
}

struct SlideFacts {
    text_chars: usize,
    shapes: usize,
    pictures: usize,
    links: usize,
}

fn body_links(body: &TextBody) -> usize {
    body.paragraphs
        .iter()
        .flat_map(|paragraph| paragraph.runs.iter())
        .filter(|run| run.props.link.is_some() || run.props.hover.is_some())
        .count()
}

fn slide_facts(shapes: &[&Shape]) -> SlideFacts {
    let mut facts = SlideFacts {
        text_chars: 0,
        shapes: shapes.len(),
        pictures: 0,
        links: 0,
    };
    for shape in shapes {
        if matches!(&shape.kind, ShapeKind::Picture { .. }) {
            facts.pictures += 1;
        }
        if shape.click.is_some() || shape.hover.is_some() {
            facts.links += 1;
        }
        if let Some(body) = &shape.text {
            facts.text_chars += body.char_len();
            facts.links += body_links(body);
        }
        if let ShapeKind::Table(table) = &shape.kind {
            for row in &table.rows {
                for cell in &row.cells {
                    facts.text_chars += cell.text.char_len();
                    facts.links += body_links(&cell.text);
                }
            }
        }
    }
    facts
}

/// A slide's label: its title placeholder, else the first non-empty text line.
fn slide_title(slide: &Slide) -> String {
    let clean = |value: &str| -> String {
        let text: String = value.chars().filter(|c| !c.is_control()).collect();
        truncate_chars(text.trim(), MAX_TITLE_CHARS)
    };
    let title = clean(&slide.title());
    if !title.is_empty() {
        return title;
    }
    let mut found = String::new();
    walk(&slide.shapes, &mut |shape, _| {
        if !found.is_empty() {
            return;
        }
        if let Some(body) = &shape.text {
            let text = body.text();
            if let Some(line) = text.lines().find(|line| !line.trim().is_empty()) {
                found = clean(line);
            }
        }
    });
    found
}

fn inspect(presentation: &Presentation) -> PptxInfo {
    let mut info = PptxInfo {
        slide_count: presentation.slides.len(),
        slides: Vec::with_capacity(presentation.slides.len().min(MAX_LISTED_SLIDES)),
        text_chars: 0,
        shapes: 0,
        media_parts: 0,
        linked_media: 0,
        has_notes: false,
        has_links: false,
        truncated: false,
    };
    for (index, slide) in presentation.slides.iter().enumerate() {
        let ordered = ordered_shapes(slide);
        let facts = slide_facts(&ordered);
        let has_notes = !slide.notes.is_empty();
        let has_links = facts.links > 0;
        info.text_chars += facts.text_chars;
        info.shapes += facts.shapes;
        info.has_notes |= has_notes;
        info.has_links |= has_links;
        if index >= MAX_LISTED_SLIDES {
            info.truncated = true;
            continue;
        }
        let listed = ordered.len().min(MAX_LISTED_SHAPES);
        let shape_list = ordered
            .iter()
            .take(listed)
            .enumerate()
            .map(|(position, shape)| shape_info(position + 1, shape))
            .collect();
        let truncated = ordered.len() > listed;
        info.truncated |= truncated;
        info.slides.push(PptxSlideInfo {
            slide: index + 1,
            title: slide_title(slide),
            text_chars: facts.text_chars,
            shapes: ordered.len(),
            pictures: facts.pictures,
            has_notes,
            has_links,
            shape_list,
            truncated,
        });
    }
    for media in &presentation.media {
        if media.link.is_some() {
            info.linked_media += 1;
        } else if !media.data.is_empty() {
            info.media_parts += 1;
        }
    }
    info.has_links |= info.linked_media > 0;
    info
}

/// Preflight, parse, and summarise a package.
fn summarize(bytes: &[u8]) -> Result<PptxInfo> {
    let presentation = open(bytes)?;
    Ok(inspect(&presentation))
}

/// Human-readable summary.
///
/// This is the text the model actually reads, so it carries the shape indices
/// `pptx_edit` consumes. The listing is capped three ways — slides, shapes per
/// slide, and total shape lines — so a large package cannot flood the context.
/// The complete listing stays in the tool's structured data, and the counts in
/// the header are always complete.
fn info_content(info: &PptxInfo) -> String {
    const MAX_CONTENT_SLIDES: usize = 60;
    const MAX_CONTENT_SHAPES: usize = 40;
    const MAX_CONTENT_SHAPE_LINES: usize = 300;

    let mut out = format!(
        "slides: {}\ntext characters: {}\nshapes: {}\nmedia parts: {}\nlinked media: {}\nnotes: {}\nlinks: {}\n",
        info.slide_count,
        info.text_chars,
        info.shapes,
        info.media_parts,
        info.linked_media,
        info.has_notes,
        info.has_links
    );
    if info.truncated {
        out.push_str(
            "truncated: true (the listing below is partial; the counts above are complete)\n",
        );
    }
    let mut lines = 0usize;
    for slide in info.slides.iter().take(MAX_CONTENT_SLIDES) {
        out.push_str(&format!(
            "slide {}: {} ({} chars, {} shapes, {} pictures, notes: {}, links: {})\n",
            slide.slide,
            slide.title,
            slide.text_chars,
            slide.shapes,
            slide.pictures,
            slide.has_notes,
            slide.has_links
        ));
        let mut printed = 0usize;
        for shape in slide.shape_list.iter().take(MAX_CONTENT_SHAPES) {
            if lines >= MAX_CONTENT_SHAPE_LINES {
                out.push_str("  ... shape listing stopped at the reply budget\n");
                return out;
            }
            out.push_str(&format!(
                "  shape {}: {} ({} chars)",
                shape.index, shape.kind, shape.chars
            ));
            if shape.has_image {
                out.push_str(" [image]");
            }
            if !shape.text.is_empty() {
                out.push_str(&format!(" \"{}\"", shape.text));
            }
            out.push('\n');
            printed += 1;
            lines += 1;
        }
        if printed < slide.shapes {
            out.push_str(&format!(
                "  ... {} more shape(s) not listed\n",
                slide.shapes - printed
            ));
        }
    }
    if info.slides.len() > MAX_CONTENT_SLIDES {
        out.push_str(&format!(
            "... {} more slide(s) not listed\n",
            info.slides.len() - MAX_CONTENT_SLIDES
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Edits
// ---------------------------------------------------------------------------

/// A bounded edit of an existing package. Slide and shape indices are 1-based
/// and always refer to the *input* deck.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PptxEdit {
    /// Replace the text of a shape, keeping its first run's formatting.
    #[serde(default)]
    pub set_text: Vec<SetText>,
    /// Slide numbers to remove.
    #[serde(default)]
    pub delete_slides: Vec<u32>,
    /// Complete order of the surviving slides; omitted keeps their order.
    #[serde(default)]
    pub reorder_slides: Option<Vec<u32>>,
}

/// One `set_text` operation.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetText {
    /// 1-based slide number.
    pub slide: u32,
    /// 1-based shape index in depth-first order on that slide. This is the
    /// `index` `pptx_inspect` lists for the shape.
    pub shape: u32,
    /// Replacement text, at most [`MAX_TEXT_CHARS`] characters.
    pub text: String,
}

struct EditSummary {
    set_text: usize,
    deleted: usize,
    reordered: bool,
    slides: usize,
}

fn check_slide(slide: u32, total: usize) -> Result<()> {
    if slide == 0 || slide as usize > total {
        return Err(invalid(format!("slide {slide} is outside 1..={total}")));
    }
    Ok(())
}

/// Replace a body's text while keeping the first paragraph's and first run's
/// formatting, so an edit does not reset the owner's styling.
fn replace_text(body: &mut TextBody, text: &str) {
    let normalised = text.replace("\r\n", "\n").replace('\r', "\n");
    let template = body.paragraphs.first();
    let props = template
        .map(|paragraph| paragraph.props.clone())
        .unwrap_or_default();
    let level = template.map(|paragraph| paragraph.level).unwrap_or(0);
    let run = template
        .and_then(|paragraph| paragraph.runs.first())
        .map(|run| run.props.clone())
        .unwrap_or_default();
    body.paragraphs = normalised
        .split('\n')
        .map(|line| Paragraph {
            level,
            props: props.clone(),
            runs: if line.is_empty() {
                Vec::new()
            } else {
                vec![Run::with(line, run.clone())]
            },
            ..Default::default()
        })
        .collect();
}

/// A validated `set_text` target: the slide position and the shape id.
struct TextTarget {
    slide: usize,
    shape: ShapeId,
    text: String,
}

/// Resolve one `set_text` operation, checking the index and that the shape can
/// hold text. Nothing is mutated.
///
/// The index is resolved through [`ordered_shapes`], the same function
/// `inspect` lists shapes with, so a listed index always addresses the shape it
/// was listed for.
fn text_target(presentation: &Presentation, operation: &SetText) -> Result<TextTarget> {
    let total = presentation.slides.len();
    check_slide(operation.slide, total)?;
    let index = operation.slide as usize - 1;
    let slide = presentation
        .slides
        .get(index)
        .ok_or_else(|| invalid(format!("slide {} is out of range", operation.slide)))?;
    let ordered = ordered_shapes(slide);
    let position = operation
        .shape
        .checked_sub(1)
        .ok_or_else(|| invalid("shape 0 is out of range"))? as usize;
    let count = ordered.len();
    let shape = *ordered.get(position).ok_or_else(|| {
        invalid(format!(
            "shape {} is outside 1..={count} on slide {}",
            operation.shape, operation.slide
        ))
    })?;
    let kind = shape_kind(shape);
    if kind != PptxShapeKind::Text {
        return Err(invalid(format!(
            "shape {} on slide {} is a {kind} and has no text body; set_text only addresses text shapes",
            operation.shape, operation.slide
        )));
    }
    Ok(TextTarget {
        slide: index,
        shape: shape.id,
        text: operation.text.clone(),
    })
}

fn apply_text_targets(presentation: &mut Presentation, targets: Vec<TextTarget>) -> Result<usize> {
    let mut applied = 0usize;
    for target in targets {
        let slide = presentation
            .slides
            .get_mut(target.slide)
            .ok_or_else(|| invalid("slide disappeared during the edit"))?;
        let slide = Arc::make_mut(slide);
        let shape = slide
            .shape_mut(target.shape)
            .ok_or_else(|| invalid("shape disappeared during the edit"))?;
        match shape.text.as_mut() {
            Some(body) => replace_text(body, &target.text),
            None => shape.text = Some(TextBody::from_text(&target.text)),
        }
        applied += 1;
    }
    Ok(applied)
}

/// Drop section and custom-show entries that name a removed slide.
fn prune_slide_references(presentation: &mut Presentation) {
    let ids: HashSet<SlideId> = presentation.slides.iter().map(|slide| slide.id).collect();
    for section in &mut presentation.sections {
        section.slides.retain(|id| ids.contains(id));
    }
    for show in &mut presentation.custom_shows {
        show.slides.retain(|id| ids.contains(id));
    }
}

/// Validate and apply one edit. Every index is checked before anything mutates.
fn apply_edit(presentation: &mut Presentation, edit: &PptxEdit) -> Result<EditSummary> {
    let total = presentation.slides.len();
    if total == 0 {
        return Err(invalid("PPTX has no slides"));
    }
    if edit.set_text.is_empty() && edit.delete_slides.is_empty() && edit.reorder_slides.is_none() {
        return Err(invalid("the edit spec is empty"));
    }
    if edit.set_text.len() > MAX_EDIT_OPS {
        return Err(invalid(format!(
            "set_text lists more than {MAX_EDIT_OPS} operations"
        )));
    }
    for operation in &edit.set_text {
        if operation.text.chars().count() > MAX_TEXT_CHARS {
            return Err(invalid(format!(
                "replacement text exceeds {MAX_TEXT_CHARS} characters"
            )));
        }
    }
    // Resolve every text target before anything mutates, so a bad index or a
    // shape that cannot hold text rejects the whole edit.
    let mut targets = Vec::with_capacity(edit.set_text.len());
    for operation in &edit.set_text {
        targets.push(text_target(presentation, operation)?);
    }

    let mut deleted = BTreeSet::new();
    for slide in &edit.delete_slides {
        check_slide(*slide, total)?;
        if !deleted.insert(*slide) {
            return Err(invalid(format!("slide {slide} is deleted twice")));
        }
    }
    let surviving: Vec<u32> = (1..=total as u32)
        .filter(|slide| !deleted.contains(slide))
        .collect();
    if surviving.is_empty() {
        return Err(invalid("the edit deletes every slide"));
    }

    let order = match &edit.reorder_slides {
        Some(order) => {
            if order.len() != surviving.len() {
                return Err(invalid(format!(
                    "reorder_slides must list all {} surviving slides",
                    surviving.len()
                )));
            }
            let mut seen = BTreeSet::new();
            for slide in order {
                if !surviving.contains(slide) {
                    return Err(invalid(format!(
                        "reorder_slides names slide {slide}, which is deleted or out of range"
                    )));
                }
                if !seen.insert(*slide) {
                    return Err(invalid(format!("reorder_slides names slide {slide} twice")));
                }
            }
            order.clone()
        }
        None => surviving.clone(),
    };

    let set = apply_text_targets(presentation, targets)?;

    // Delete and reorder in one pass over the original slide list.
    let mut slots: Vec<Option<Arc<Slide>>> = std::mem::take(&mut presentation.slides)
        .into_iter()
        .map(Some)
        .collect();
    let mut slides = Vec::with_capacity(order.len());
    for index in &order {
        if let Some(slide) = slots.get_mut(*index as usize - 1).and_then(Option::take) {
            slides.push(slide);
        }
    }
    presentation.slides = slides;
    prune_slide_references(presentation);

    Ok(EditSummary {
        set_text: set,
        deleted: deleted.len(),
        reordered: edit.reorder_slides.is_some(),
        slides: presentation.slides.len(),
    })
}

// ---------------------------------------------------------------------------
// Link stripping
// ---------------------------------------------------------------------------

fn strip_body(body: &mut TextBody, removed: &mut usize) {
    for paragraph in &mut body.paragraphs {
        for run in &mut paragraph.runs {
            if run.props.link.take().is_some() {
                *removed += 1;
            }
            if run.props.hover.take().is_some() {
                *removed += 1;
            }
        }
    }
}

fn strip_shapes(shapes: &mut [Shape], removed: &mut usize, depth: usize) {
    if depth > MAX_SHAPE_DEPTH {
        return;
    }
    for shape in shapes.iter_mut() {
        if shape.click.take().is_some() {
            *removed += 1;
        }
        if shape.hover.take().is_some() {
            *removed += 1;
        }
        if let Some(body) = shape.text.as_mut() {
            strip_body(body, removed);
        }
        match &mut shape.kind {
            ShapeKind::Group { children, .. } => strip_shapes(children, removed, depth + 1),
            ShapeKind::Table(table) => {
                for row in &mut table.rows {
                    for cell in &mut row.cells {
                        strip_body(&mut cell.text, removed);
                    }
                }
            }
            _ => {}
        }
    }
}

fn strip_master(master: &mut deckcraft_model::Master, removed: &mut usize) {
    strip_shapes(&mut master.shapes, removed, 0);
    for layout in &mut master.layouts {
        strip_shapes(&mut layout.shapes, removed, 0);
    }
}

/// Drop every hyperlink and external media link from the model.
///
/// The writer only emits an external relationship for a link it still holds, so
/// clearing them here is what keeps `TargetMode="External"` out of the package.
/// Internal relationships (a slide jump, an embedded clip) are unaffected.
fn strip_links(presentation: &mut Presentation) -> usize {
    let mut removed = 0usize;
    for slide in &mut presentation.slides {
        let slide = Arc::make_mut(slide);
        strip_shapes(&mut slide.shapes, &mut removed, 0);
        strip_body(&mut slide.notes, &mut removed);
    }
    for master in &mut presentation.masters {
        strip_master(Arc::make_mut(master), &mut removed);
    }
    for master in [
        presentation.notes_master.as_mut(),
        presentation.handout_master.as_mut(),
    ]
    .into_iter()
    .flatten()
    {
        strip_master(Arc::make_mut(master), &mut removed);
    }
    for media in &mut presentation.media {
        if media.link.take().is_some() {
            removed += 1;
        }
    }
    removed
}

/// Refuse to store an export that still carries an external relationship.
///
/// Only `.rels` parts are checked: `a:hlinkClick` alone is not proof of a link
/// out of the package (the writer emits one with an internal `ppaction://media`
/// action for every audio or video shape), while every external target is
/// written as `TargetMode="External"`.
fn verify_export(bytes: &[u8]) -> Result<()> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| SeraError::Internal(format!("exported package is unreadable: {error}")))?;
    if archive.len() > MAX_ZIP_PARTS {
        return Err(SeraError::Internal(
            "exported package has too many parts".into(),
        ));
    }
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| {
            SeraError::Internal(format!("exported part {index} is unreadable: {error}"))
        })?;
        let name = entry.name().to_ascii_lowercase();
        if !name.ends_with(".rels") {
            continue;
        }
        if entry.size() > MAX_XML_PART_BYTES {
            return Err(SeraError::Internal(format!(
                "exported part {name} is too large"
            )));
        }
        let mut xml = String::new();
        entry.read_to_string(&mut xml).map_err(|error| {
            SeraError::Internal(format!("cannot read exported part {name}: {error}"))
        })?;
        if xml.contains("TargetMode=\"External\"") {
            return Err(SeraError::Internal(format!(
                "exported part {name} still links outside the package"
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

fn reject_unknown_keys(args: &Value, allowed: &[&str]) -> Result<()> {
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

fn argument<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn is_pptx_artifact(stored: &StoredArtifact) -> bool {
    Path::new(&stored.filename)
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pptx"))
}

fn check_size(bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_PPTX_BYTES {
        return Err(invalid(format!(
            "PPTX is {} bytes; the limit is {MAX_PPTX_BYTES} bytes (32 MiB)",
            bytes.len()
        )));
    }
    Ok(())
}

fn artifact_meta(filename: &str, source_artifact: Option<String>) -> ArtifactMeta {
    ArtifactMeta {
        title: filename.to_owned(),
        filename: filename.to_owned(),
        language: Language::En,
        source_artifact,
        created_at: Utc::now(),
    }
}

fn edited_filename(source: &str) -> String {
    let stem = Path::new(source)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("deck");
    let stem: String = stem.chars().take(120).collect();
    format!("{stem}-edited.pptx")
}

/// Import an owner-supplied package into the artifact store.
pub struct PptxImportTool {
    artifacts: Arc<ArtifactStore>,
    store: Store,
    inbox: PathBuf,
}

impl PptxImportTool {
    pub fn new(
        artifacts: Arc<ArtifactStore>,
        store: Store,
        inbox: impl Into<PathBuf>,
    ) -> Result<Self> {
        let inbox = inbox.into();
        std::fs::create_dir_all(&inbox)?;
        let inbox = inbox.canonicalize()?;
        Ok(Self {
            artifacts,
            store,
            inbox,
        })
    }
}

#[async_trait]
impl Tool for PptxImportTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "pptx_import".into(),
            description:
                "Import a .pptx from the configured inbox (or re-check an existing artifact hash) \
                 into the artifact store. Returns the slide count, text size, media part count, and \
                 whether notes or links are present. Bounded to 32 MiB, 500 slides, and 4096 package \
                 parts; password-protected packages and packages carrying a VBA project are \
                 rejected. Images survive a later edit; hyperlinks are dropped on export."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path relative to the configured inbox"},
                    "artifact": {"type": "string", "description": "Artifact hash of an already imported package"}
                },
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        reject_unknown_keys(&args, &["path", "artifact"])?;
        let path = argument(&args, "path").map(str::to_owned);
        let artifact = argument(&args, "artifact").map(str::to_owned);
        let (hash, filename, info, source) = match (path, artifact) {
            (Some(path), None) => {
                let artifacts = self.artifacts.clone();
                let inbox = self.inbox.clone();
                let (stored, info) = tokio::task::spawn_blocking(move || {
                    artifacts.ingest_inbox_file(&inbox, &path, &["pptx"], MAX_PPTX_BYTES, summarize)
                })
                .await
                .map_err(|error| SeraError::Internal(format!("PPTX task failed: {error}")))??;
                (stored.hash, stored.filename, info, None)
            }
            (None, Some(hash)) => {
                let stored = self.artifacts.load(&hash)?;
                if !is_pptx_artifact(&stored) {
                    return Err(invalid("artifact is not a .pptx package"));
                }
                check_size(&stored.bytes)?;
                let bytes = stored.bytes;
                let info = tokio::task::spawn_blocking(move || summarize(&bytes))
                    .await
                    .map_err(|error| SeraError::Internal(format!("PPTX task failed: {error}")))??;
                (stored.hash, stored.filename, info, Some(hash))
            }
            (Some(_), Some(_)) => return Err(invalid("provide either path or artifact, not both")),
            (None, None) => return Err(invalid("path or artifact is required")),
        };

        self.store.record_audit(
            &context.actor,
            "pptx.imported",
            Some("pptx_import"),
            context.task_id.as_deref(),
            None,
            None,
            None,
            Some(&json!({
                "artifact_hash": hash.clone(),
                "source_artifact": source,
                "slides": info.slide_count,
                "media_parts": info.media_parts,
                "has_notes": info.has_notes,
                "has_links": info.has_links,
            })),
        )?;

        let result = json!({
            "artifact_hash": hash.clone(),
            "source_artifact": source,
            "filename": filename.clone(),
            "slides": info.slide_count,
            "text_chars": info.text_chars,
            "shapes": info.shapes,
            "media_parts": info.media_parts,
            "linked_media": info.linked_media,
            "has_notes": info.has_notes,
            "has_links": info.has_links,
        });
        Ok(ToolOutput {
            content: format!(
                "{}\nfilename: {}\nartifact: {}\n",
                info_content(&info),
                filename,
                hash
            ),
            data: Some(result),
            artifact_path: Some(format!("artifacts/{filename}")),
            artifact_hash: Some(hash),
            filename: Some(filename),
            destination: None,
        })
    }
}

/// Read-only summary of a stored package.
pub struct PptxInspectTool {
    artifacts: Arc<ArtifactStore>,
}

impl PptxInspectTool {
    pub fn new(artifacts: Arc<ArtifactStore>) -> Self {
        Self { artifacts }
    }
}

#[async_trait]
impl Tool for PptxInspectTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "pptx_inspect".into(),
            description:
                "Read-only summary of a stored .pptx: slide count, per-slide title text and text \
                 character count, image/media part count, whether notes or links are present, and \
                 the shapes on each slide with the 1-based index that pptx_edit set_text takes. Each \
                 listed shape reports its kind (text, picture, table, group, other), a 200-character \
                 text preview, its full character count, and whether it carries an image. The listing \
                 is capped at 100 shapes per slide and 200 slides with a truncated flag; the counts \
                 are always complete. It never renders or extracts the package."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "artifact": {"type": "string", "description": "Artifact hash of the package to inspect"}
                },
                "required": ["artifact"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, _context: &ToolContext) -> Result<ToolOutput> {
        reject_unknown_keys(&args, &["artifact"])?;
        let hash = argument(&args, "artifact")
            .ok_or_else(|| invalid("artifact is required"))?
            .to_owned();
        let stored = self.artifacts.load(&hash)?;
        if !is_pptx_artifact(&stored) {
            return Err(invalid("artifact is not a .pptx package"));
        }
        check_size(&stored.bytes)?;
        let bytes = stored.bytes;
        let info = tokio::task::spawn_blocking(move || summarize(&bytes))
            .await
            .map_err(|error| SeraError::Internal(format!("PPTX task failed: {error}")))??;
        Ok(ToolOutput {
            content: format!("{}\nartifact: {}\n", info_content(&info), stored.hash),
            data: Some(serde_json::to_value(&info)?),
            artifact_hash: Some(stored.hash),
            filename: Some(stored.filename),
            ..ToolOutput::default()
        })
    }
}

/// Bounded edit of a stored package; the result is a new artifact.
pub struct PptxEditTool {
    artifacts: Arc<ArtifactStore>,
    store: Store,
}

impl PptxEditTool {
    pub fn new(artifacts: Arc<ArtifactStore>, store: Store) -> Self {
        Self { artifacts, store }
    }
}

#[async_trait]
impl Tool for PptxEditTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "pptx_edit".into(),
            description:
                "Apply a bounded edit to a stored .pptx and store the result as a new artifact whose \
                 source_artifact is the input. Operations: set_text (replace one shape's text), \
                 delete_slides, and reorder_slides. Slide and shape indices are 1-based and refer to \
                 the input deck; take `shape` from the index pptx_inspect lists for that slide. Every \
                 index is validated before anything is written, and an index outside the slide's \
                 shapes is rejected. Images and other media parts are preserved; hyperlinks and \
                 external links are dropped."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "artifact": {"type": "string", "description": "Artifact hash of the package to edit"},
                    "edit": {
                        "type": "object",
                        "description": "Bounded edit spec; unknown fields are rejected",
                        "properties": {
                            "set_text": {
                                "type": "array",
                                "maxItems": MAX_EDIT_OPS,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "slide": {"type": "integer", "description": "1-based slide number"},
                                        "shape": {"type": "integer", "description": "1-based shape index, taken from pptx_inspect"},
                                        "text": {"type": "string", "description": "Replacement text, max 4000 characters"}
                                    },
                                    "required": ["slide", "shape", "text"],
                                    "additionalProperties": false
                                }
                            },
                            "delete_slides": {
                                "type": "array",
                                "items": {"type": "integer"},
                                "description": "1-based slide numbers to delete"
                            },
                            "reorder_slides": {
                                "type": "array",
                                "items": {"type": "integer"},
                                "description": "Complete 1-based order of the surviving slides"
                            }
                        },
                        "additionalProperties": false
                    }
                },
                "required": ["artifact", "edit"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        reject_unknown_keys(&args, &["artifact", "edit"])?;
        let hash = argument(&args, "artifact")
            .ok_or_else(|| invalid("artifact is required"))?
            .to_owned();
        let edit: PptxEdit = serde_json::from_value(
            args.get("edit")
                .cloned()
                .ok_or_else(|| invalid("edit is required"))?,
        )
        .map_err(|error| invalid(format!("invalid edit spec: {error}")))?;
        let source = self.artifacts.load(&hash)?;
        if !is_pptx_artifact(&source) {
            return Err(invalid("artifact is not a .pptx package"));
        }
        check_size(&source.bytes)?;
        let source_artifact = source.hash.clone();
        let filename = edited_filename(&source.filename);
        let input = source.bytes;
        let (bytes, summary, info, stripped) = tokio::task::spawn_blocking(move || {
            let mut presentation = open(&input)?;
            let summary = apply_edit(&mut presentation, &edit)?;
            let stripped = strip_links(&mut presentation);
            let bytes = deckcraft_pptx::export(&presentation)
                .map_err(|error| SeraError::Internal(format!("cannot serialize pptx: {error}")))?;
            verify_export(&bytes)?;
            let info = inspect(&presentation);
            Ok::<_, SeraError>((bytes, summary, info, stripped))
        })
        .await
        .map_err(|error| SeraError::Internal(format!("PPTX task failed: {error}")))??;

        let stored = self.artifacts.save(
            &bytes,
            &filename,
            artifact_meta(&filename, Some(source_artifact.clone())),
        )?;
        self.store.record_audit(
            &context.actor,
            "pptx.edited",
            Some("pptx_edit"),
            context.task_id.as_deref(),
            None,
            None,
            None,
            Some(&json!({
                "artifact_hash": stored.hash.clone(),
                "source_artifact": source_artifact,
                "slides": summary.slides,
                "set_text": summary.set_text,
                "deleted_slides": summary.deleted,
                "reordered": summary.reordered,
                "links_dropped": stripped,
                "media_parts": info.media_parts,
            })),
        )?;

        let result = json!({
            "artifact_hash": stored.hash.clone(),
            "source_artifact": source_artifact,
            "filename": stored.filename.clone(),
            "slides": summary.slides,
            "set_text": summary.set_text,
            "deleted_slides": summary.deleted,
            "reordered": summary.reordered,
            "links_dropped": stripped,
            "media_parts": info.media_parts,
        });
        Ok(ToolOutput {
            content: format!(
                "slides: {}\nset text: {}\ndeleted slides: {}\nreordered: {}\nlinks dropped: {}\nmedia parts: {}\nfilename: {}\nartifact: {}\nsource_artifact: {}\n",
                summary.slides,
                summary.set_text,
                summary.deleted,
                summary.reordered,
                stripped,
                info.media_parts,
                stored.filename,
                stored.hash,
                source_artifact
            ),
            data: Some(result),
            artifact_path: Some(format!("artifacts/{}", stored.filename)),
            artifact_hash: Some(stored.hash),
            filename: Some(stored.filename),
            destination: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use deckcraft_model::style::PictureFill;
    use deckcraft_model::text::{Action, Hyperlink, RunProps};
    use deckcraft_model::{LayoutType, Xfrm, defaults};

    use crate::gateway::ToolRegistry;

    /// A synthetic 1×1 RGBA PNG, so the fixture carries a real image and no
    /// borrowed asset.
    const PNG_HEX: &str = "89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d4944415478da63fccfc0f01f00050001ffabce36890000000049454e44ae426082";

    fn png() -> Vec<u8> {
        hex::decode(PNG_HEX).unwrap()
    }

    fn link(url: &str) -> Hyperlink {
        Hyperlink {
            action: Action::Url { url: url.into() },
            tooltip: "Synthetic link".into(),
            highlight_click: false,
            sound: None,
            stop_sound: false,
            zoom_return: None,
        }
    }

    fn text_shape(presentation: &mut Presentation, name: &str, text: &str) -> Shape {
        Shape {
            id: ShapeId(presentation.alloc_id().unwrap()),
            name: name.into(),
            xfrm: Some(Xfrm::new(80.0, 80.0, 500.0, 80.0)),
            text: Some(TextBody::from_text(text)),
            text_box: true,
            ..Default::default()
        }
    }

    /// Three slides: a titled cover with notes, a picture slide with a
    /// hyperlinked run, and a plain text slide.
    fn fixture() -> Presentation {
        let mut presentation = defaults::new_presentation(None);
        let image = presentation
            .add_media("mark.png", "image/png", png())
            .unwrap();
        {
            let id = presentation.slides[0].id;
            let slide = presentation.slide_mut(id).unwrap();
            if let Some(title) = slide.shapes.first_mut() {
                title.text = Some(TextBody::from_text("Original title"));
            }
            slide.notes = TextBody::from_text("Speaker note");
        }

        let blank = defaults::layout_of_kind(&presentation, LayoutType::Blank).unwrap();
        let mut picture_slide = defaults::new_slide(&mut presentation, blank).unwrap();
        let mut picture = Shape {
            id: ShapeId(presentation.alloc_id().unwrap()),
            name: "Mark".into(),
            xfrm: Some(Xfrm::new(80.0, 200.0, 120.0, 120.0)),
            ..Default::default()
        };
        picture.kind = ShapeKind::Picture {
            fill: PictureFill {
                media: image,
                ..Default::default()
            },
        };
        picture_slide.shapes.push(picture);
        let mut linked = text_shape(&mut presentation, "Linked", "North report");
        if let Some(body) = linked.text.as_mut() {
            body.paragraphs = vec![Paragraph {
                runs: vec![Run::with(
                    "North report",
                    RunProps {
                        link: Some(link("https://example.invalid/north")),
                        ..Default::default()
                    },
                )],
                ..Default::default()
            }];
        }
        picture_slide.shapes.push(linked);
        presentation.slides.push(Arc::new(picture_slide));

        let mut third = defaults::new_slide(&mut presentation, blank).unwrap();
        for (name, text) in [
            ("Third", "Third slide"),
            ("Alpha", "Alpha body"),
            ("Beta", "Beta body"),
        ] {
            let shape = text_shape(&mut presentation, name, text);
            third.shapes.push(shape);
        }
        presentation.slides.push(Arc::new(third));

        presentation
    }

    fn exported() -> Vec<u8> {
        deckcraft_pptx::export(&fixture()).unwrap()
    }

    fn parts(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut out = Vec::new();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).unwrap();
            let name = entry.name().to_owned();
            let mut data = Vec::new();
            entry.read_to_end(&mut data).unwrap();
            out.push((name, data));
        }
        out
    }

    fn context() -> ToolContext {
        ToolContext {
            actor: "owner".into(),
            bot: "maker".into(),
            task_id: None,
            turn_id: None,
            conversation_id: None,
            allowed_tools: ["pptx_import", "pptx_inspect", "pptx_edit"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            require_approval: HashSet::new(),
        }
    }

    struct Harness {
        dir: tempfile::TempDir,
        inbox: PathBuf,
        artifacts: Arc<ArtifactStore>,
        store: Store,
    }

    fn harness() -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let artifacts = Arc::new(ArtifactStore::new(dir.path().join("artifacts")).unwrap());
        let store = Store::open_in_memory(384).unwrap();
        Harness {
            dir,
            inbox,
            artifacts,
            store,
        }
    }

    fn import_tool(harness: &Harness) -> PptxImportTool {
        PptxImportTool::new(
            harness.artifacts.clone(),
            harness.store.clone(),
            &harness.inbox,
        )
        .unwrap()
    }

    async fn imported(harness: &Harness, name: &str, bytes: &[u8]) -> String {
        std::fs::write(harness.inbox.join(name), bytes).unwrap();
        let output = import_tool(harness)
            .execute(json!({"path": name}), &context())
            .await
            .unwrap();
        output.artifact_hash.unwrap()
    }

    fn slide_text(presentation: &Presentation) -> String {
        presentation
            .slides
            .iter()
            .flat_map(|slide| slide.shapes.iter())
            .filter_map(|shape| shape.text.as_ref())
            .map(|body| body.text())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The text of every shape on one slide (1-based), in shape order.
    fn texts_on(presentation: &Presentation, slide: usize) -> Vec<String> {
        slide
            .checked_sub(1)
            .and_then(|index| presentation.slides.get(index))
            .map(|slide| {
                slide
                    .shapes
                    .iter()
                    .filter_map(|shape| shape.text.as_ref())
                    .map(|body| body.text())
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn inspect_data(harness: &Harness, source: &str) -> serde_json::Value {
        PptxInspectTool::new(harness.artifacts.clone())
            .execute(json!({"artifact": source}), &context())
            .await
            .unwrap()
            .data
            .unwrap()
    }

    fn stored_count(harness: &Harness) -> usize {
        std::fs::read_dir(harness.dir.path().join("artifacts"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .count()
    }

    /// The defect this guards: `inspect` must publish the addressing `set_text`
    /// consumes, so an edit is never a guess.
    #[tokio::test]
    async fn inspect_indices_address_the_shapes_edit_changes() {
        let harness = harness();
        let source = imported(&harness, "deck.pptx", &exported()).await;

        let report = PptxInspectTool::new(harness.artifacts.clone())
            .execute(json!({"artifact": source}), &context())
            .await
            .unwrap();
        let content = report.content.clone();
        let data = report.data.unwrap();

        let slide = &data["slides"][2];
        assert_eq!(slide["slide"], 3);
        assert_eq!(slide["shapes"], 3);
        assert_eq!(slide["truncated"], false);
        let listed = slide["shape_list"].as_array().unwrap();
        assert_eq!(listed.len(), 3);

        // Find the shape by its text, exactly as a model would, and take the
        // index the listing reports for it.
        let target = listed
            .iter()
            .find(|shape| shape["text"] == "Beta body")
            .expect("Beta body is listed");
        assert_eq!(target["kind"], "text");
        assert_eq!(target["chars"], 9);
        assert_eq!(target["has_image"], false);
        let index = target["index"].as_u64().unwrap();
        assert_eq!(index, 3, "depth-first order: Third, Alpha, Beta");

        // The content string the model reads carries the same index and text.
        assert!(
            content.contains("shape 3: text (9 chars) \"Beta body\""),
            "{content}"
        );

        let edit = PptxEditTool::new(harness.artifacts.clone(), harness.store.clone());
        let edited = edit
            .execute(
                json!({
                    "artifact": source,
                    "edit": {"set_text": [{"slide": 3, "shape": index, "text": "Beta body edited"}]}
                }),
                &context(),
            )
            .await
            .unwrap();

        let stored = harness
            .artifacts
            .load(edited.artifact_hash.as_ref().unwrap())
            .unwrap();
        let reopened = deckcraft_pptx::import(&stored.bytes).unwrap();
        assert_eq!(
            texts_on(&reopened, 3),
            vec!["Third slide", "Alpha body", "Beta body edited"],
            "only the addressed shape changed"
        );
        // Nothing on the other slides moved.
        assert_eq!(
            texts_on(&reopened, 1).first().map(String::as_str),
            Some("Original title")
        );
        assert_eq!(texts_on(&reopened, 2), vec!["North report"]);
    }

    #[tokio::test]
    async fn rejects_a_shape_index_beyond_the_slide() {
        let harness = harness();
        let source = imported(&harness, "deck.pptx", &exported()).await;
        let data = inspect_data(&harness, &source).await;
        let count = data["slides"][2]["shapes"].as_u64().unwrap();
        assert_eq!(count, 3);

        let edit = PptxEditTool::new(harness.artifacts.clone(), harness.store.clone());
        for shape in [0u64, count + 1, count + 99] {
            let error = edit
                .execute(
                    json!({
                        "artifact": source,
                        "edit": {"set_text": [{"slide": 3, "shape": shape, "text": "x"}]}
                    }),
                    &context(),
                )
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains("shape"), "shape {shape}: {error}");
        }
        // A rejected edit stores nothing.
        assert_eq!(stored_count(&harness), 1);
    }

    /// The listing is capped; the addressing is not. A shape past the cap is
    /// still addressable by its real index, and the flag says the list is short.
    #[tokio::test]
    async fn a_capped_shape_list_still_addresses_every_shape() {
        let mut presentation = defaults::new_presentation(None);
        let blank = defaults::layout_of_kind(&presentation, LayoutType::Blank).unwrap();
        let mut slide = defaults::new_slide(&mut presentation, blank).unwrap();
        for index in 1..=MAX_LISTED_SHAPES + 1 {
            let shape = text_shape(
                &mut presentation,
                &format!("S{index}"),
                &format!("Shape {index}"),
            );
            slide.shapes.push(shape);
        }
        presentation.slides.push(Arc::new(slide));
        let bytes = deckcraft_pptx::export(&presentation).unwrap();

        let harness = harness();
        let source = imported(&harness, "big.pptx", &bytes).await;
        let data = inspect_data(&harness, &source).await;
        let slide_info = &data["slides"][1];
        assert_eq!(slide_info["shapes"], MAX_LISTED_SHAPES + 1);
        assert_eq!(
            slide_info["shape_list"].as_array().unwrap().len(),
            MAX_LISTED_SHAPES
        );
        assert_eq!(slide_info["truncated"], true);
        assert_eq!(data["truncated"], true);

        let report = PptxInspectTool::new(harness.artifacts.clone())
            .execute(json!({"artifact": source}), &context())
            .await
            .unwrap();
        assert!(
            report.content.contains("more shape(s) not listed"),
            "{}",
            report.content
        );

        let edit = PptxEditTool::new(harness.artifacts.clone(), harness.store.clone());
        let edited = edit
            .execute(
                json!({
                    "artifact": source,
                    "edit": {"set_text": [{"slide": 2, "shape": MAX_LISTED_SHAPES + 1, "text": "Last shape edited"}]}
                }),
                &context(),
            )
            .await
            .unwrap();
        let stored = harness
            .artifacts
            .load(edited.artifact_hash.as_ref().unwrap())
            .unwrap();
        let reopened = deckcraft_pptx::import(&stored.bytes).unwrap();
        let texts = texts_on(&reopened, 2);
        assert_eq!(texts.len(), MAX_LISTED_SHAPES + 1);
        assert_eq!(texts.last().map(String::as_str), Some("Last shape edited"));
        assert_eq!(texts.first().map(String::as_str), Some("Shape 1"));
    }

    #[tokio::test]
    async fn imported_deck_keeps_its_image_across_an_edit() {
        let harness = harness();
        let source = imported(&harness, "deck.pptx", &exported()).await;

        let inspect_tool = PptxInspectTool::new(harness.artifacts.clone());
        let report = inspect_tool
            .execute(json!({"artifact": source}), &context())
            .await
            .unwrap();
        let data = report.data.unwrap();
        assert_eq!(data["slide_count"], 3);
        assert_eq!(data["media_parts"], 1);
        assert_eq!(data["has_notes"], true);
        assert_eq!(data["has_links"], true);
        assert_eq!(data["slides"][0]["title"], "Original title");

        let edit = PptxEditTool::new(harness.artifacts.clone(), harness.store.clone());
        let edited = edit
            .execute(
                json!({
                    "artifact": source,
                    "edit": {"set_text": [{"slide": 1, "shape": 1, "text": "Changed title"}]}
                }),
                &context(),
            )
            .await
            .unwrap();
        let edited_hash = edited.artifact_hash.clone().unwrap();
        assert_ne!(edited_hash, source);
        assert_eq!(edited.data.as_ref().unwrap()["media_parts"], 1);

        let stored = harness.artifacts.load(&edited_hash).unwrap();
        assert_eq!(
            stored.meta.source_artifact.as_deref(),
            Some(source.as_str()),
            "an edit records the artifact it came from"
        );

        // The owner's image part survived byte for byte.
        let parts = parts(&stored.bytes);
        let image = parts
            .iter()
            .find(|(name, _)| name.starts_with("ppt/media/") && name.ends_with(".png"))
            .expect("image part survived the edit");
        assert_eq!(image.1, png());

        // Text changed, the picture is still a picture, and a second import
        // still finds the image.
        let reopened = deckcraft_pptx::import(&stored.bytes).unwrap();
        let text = slide_text(&reopened);
        assert!(text.contains("Changed title"), "{text}");
        assert!(!text.contains("Original title"), "{text}");
        assert!(
            reopened
                .media
                .iter()
                .any(|media| media.data.as_slice() == png().as_slice())
        );
        let pictures = reopened
            .slides
            .iter()
            .flat_map(|slide| slide.shapes.iter())
            .filter(|shape| matches!(&shape.kind, ShapeKind::Picture { .. }))
            .count();
        assert_eq!(pictures, 1);
        assert!(
            reopened.slides.iter().any(|slide| !slide.notes.is_empty()),
            "speaker notes survived the edit"
        );
    }

    #[tokio::test]
    async fn edit_drops_hyperlinks_and_external_relationships() {
        let harness = harness();
        let source = imported(&harness, "deck.pptx", &exported()).await;
        let edit = PptxEditTool::new(harness.artifacts.clone(), harness.store.clone());
        let edited = edit
            .execute(
                json!({
                    "artifact": source,
                    "edit": {"set_text": [{"slide": 2, "shape": 2, "text": "Plain report"}]}
                }),
                &context(),
            )
            .await
            .unwrap();
        assert_eq!(edited.data.as_ref().unwrap()["links_dropped"], 1);

        let stored = harness
            .artifacts
            .load(edited.artifact_hash.as_ref().unwrap())
            .unwrap();
        for (name, data) in parts(&stored.bytes) {
            let xml = String::from_utf8_lossy(&data);
            for forbidden in [
                "hlinkClick",
                "hlinkHover",
                "TargetMode=\"External\"",
                "example.invalid",
            ] {
                assert!(!xml.contains(forbidden), "{name} contains {forbidden}");
            }
        }
        let reopened = deckcraft_pptx::import(&stored.bytes).unwrap();
        let links = reopened
            .slides
            .iter()
            .flat_map(|slide| slide.shapes.iter())
            .flat_map(|shape| shape.text.iter())
            .flat_map(|body| body.paragraphs.iter())
            .flat_map(|paragraph| paragraph.runs.iter())
            .filter(|run| run.props.link.is_some() || run.props.hover.is_some())
            .count();
        assert_eq!(links, 0);
        assert!(slide_text(&reopened).contains("Plain report"));
    }

    #[tokio::test]
    async fn deletes_and_reorders_slides() {
        let harness = harness();
        let source = imported(&harness, "deck.pptx", &exported()).await;
        let edit = PptxEditTool::new(harness.artifacts.clone(), harness.store.clone());
        let edited = edit
            .execute(
                json!({
                    "artifact": source,
                    "edit": {"delete_slides": [3], "reorder_slides": [2, 1]}
                }),
                &context(),
            )
            .await
            .unwrap();
        let data = edited.data.unwrap();
        assert_eq!(data["slides"], 2);
        assert_eq!(data["deleted_slides"], 1);
        assert_eq!(data["reordered"], true);

        let stored = harness
            .artifacts
            .load(edited.artifact_hash.as_ref().unwrap())
            .unwrap();
        let reopened = deckcraft_pptx::import(&stored.bytes).unwrap();
        assert_eq!(reopened.slides.len(), 2);
        assert!(
            matches!(
                &reopened.slides[0].shapes[0].kind,
                ShapeKind::Picture { .. }
            ),
            "the picture slide moved first"
        );
        assert!(slide_text(&reopened).contains("Original title"));
        assert!(!slide_text(&reopened).contains("Third slide"));
    }

    #[tokio::test]
    async fn rejects_invalid_edits() {
        let harness = harness();
        let source = imported(&harness, "deck.pptx", &exported()).await;
        let edit = PptxEditTool::new(harness.artifacts.clone(), harness.store.clone());
        let cases = [
            json!({"edit": {}}),
            json!({"edit": {"set_text": [{"slide": 9, "shape": 1, "text": "x"}]}}),
            json!({"edit": {"set_text": [{"slide": 1, "shape": 99, "text": "x"}]}}),
            json!({"edit": {"set_text": [{"slide": 2, "shape": 1, "text": "x"}]}}),
            json!({"edit": {"set_text": [{"slide": 0, "shape": 1, "text": "x"}]}}),
            json!({"edit": {"set_text": [{"slide": 1, "shape": 1, "text": "x".repeat(MAX_TEXT_CHARS + 1)}]}}),
            json!({"edit": {"delete_slides": [1, 2, 3]}}),
            json!({"edit": {"delete_slides": [1, 1]}}),
            json!({"edit": {"reorder_slides": [1]}}),
            json!({"edit": {"reorder_slides": [1, 1, 2]}}),
            json!({"edit": {"reorder_slides": [1, 2, 3, 4]}}),
            json!({"edit": {"set_text": [], "unknown": true}}),
            json!({"edit": {"set_text": [{"slide": 1, "shape": 1, "text": "x", "extra": 1}]}}),
        ];
        for case in cases {
            let mut args = case;
            args["artifact"] = json!(source.clone());
            assert!(
                edit.execute(args.clone(), &context()).await.is_err(),
                "accepted {args}"
            );
        }
        // A rejected edit stores nothing: the store still holds only the import.
        let stored = std::fs::read_dir(harness.dir.path().join("artifacts"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .count();
        assert_eq!(stored, 1);
    }

    #[test]
    fn rejects_encrypted_macro_and_oversized_packages() {
        // A compound file carrying an encryption stream.
        let mut encrypted = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
        encrypted.resize(512, 0);
        for unit in "EncryptedPackage".encode_utf16() {
            encrypted.extend_from_slice(&unit.to_le_bytes());
        }
        assert!(deckcraft_pptx::is_encrypted(&encrypted));
        assert!(preflight(&encrypted).is_err());

        // The same package with a VBA project added.
        let source = exported();
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, data) in parts(&source) {
            writer.start_file(name, options).unwrap();
            writer.write_all(&data).unwrap();
        }
        writer.start_file("ppt/vbaProject.bin", options).unwrap();
        writer.write_all(b"synthetic macro bytes").unwrap();
        let with_macro = writer.finish().unwrap().into_inner();
        assert!(deckcraft_pptx::sniff(&with_macro));
        let error = preflight(&with_macro).unwrap_err().to_string();
        assert!(error.contains("VBA"), "{error}");

        // An oversized input is refused before anything is opened.
        assert!(preflight(&vec![0u8; MAX_PPTX_BYTES + 1]).is_err());

        // Too many slides.
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer.start_file("[Content_Types].xml", options).unwrap();
        writer.write_all(b"<Types/>").unwrap();
        for index in 1..=MAX_SLIDES + 1 {
            writer
                .start_file(format!("ppt/slides/slide{index}.xml"), options)
                .unwrap();
            writer.write_all(b"<p:sld/>").unwrap();
        }
        let too_many = writer.finish().unwrap().into_inner();
        let error = preflight(&too_many).unwrap_err().to_string();
        assert!(error.contains("slides"), "{error}");
    }

    #[tokio::test]
    async fn rejects_paths_outside_the_inbox() {
        let harness = harness();
        let tool = import_tool(&harness);
        std::fs::write(harness.inbox.join("deck.pptx"), exported()).unwrap();
        std::fs::write(harness.inbox.join("deck.txt"), b"not a deck").unwrap();
        // A real package one level above the inbox, reachable by traversal.
        let outside = harness.inbox.parent().unwrap().join("outside.pptx");
        std::fs::write(&outside, exported()).unwrap();

        for args in [
            json!({"path": "../outside.pptx"}),
            json!({"path": outside.to_string_lossy().to_string()}),
            json!({"path": "C:outside.pptx"}),
            json!({"path": "deck.txt"}),
            json!({"path": "missing.pptx"}),
            json!({"path": "deck.pptx", "artifact": "0".repeat(64)}),
            json!({}),
            json!({"path": "deck.pptx", "extra": true}),
        ] {
            assert!(
                tool.execute(args.clone(), &context()).await.is_err(),
                "accepted {args}"
            );
        }
        // The same file inside the inbox is accepted.
        assert!(
            tool.execute(json!({"path": "deck.pptx"}), &context())
                .await
                .is_ok()
        );
    }

    #[test]
    fn registers_its_three_tools() {
        let harness = harness();
        let mut registry = ToolRegistry::default();
        crate::tools::register_pptx_tools(
            &mut registry,
            harness.artifacts.clone(),
            harness.store.clone(),
            harness.inbox.clone(),
        )
        .unwrap();
        for name in ["pptx_import", "pptx_inspect", "pptx_edit"] {
            assert!(registry.is_registered(name), "{name} is not registered");
        }
    }

    #[test]
    fn export_check_refuses_an_external_relationship() {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("_rels/.rels", options).unwrap();
        writer
            .write_all(
                br#"<Relationships><Relationship Id="rId1" Type="hyperlink" Target="https://example.invalid" TargetMode="External"/></Relationships>"#,
            )
            .unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        assert!(verify_export(&bytes).is_err());
    }
}
