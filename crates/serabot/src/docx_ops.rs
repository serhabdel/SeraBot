//! Bounded, deterministic DOCX block operations.
//!
//! Every entry point takes an artifact hash: documents arrive through the
//! [`ArtifactStore`], never as model-chosen paths, and results go back through
//! the store so an approval can bind the artifact hash.
//!
//! The address unit is a **block index**: the position of a paragraph or a
//! table in the document body, counted from zero in document order.
//! [`inspect`] reports that list with a text preview, and [`apply_ops`] only
//! accepts indices that exist in the document it was given.
//!
//! Packages carrying active or remote content are rejected outright instead of
//! being sanitised: a VBA project, an ActiveX part, or a relationship that
//! points at remote content (any external relationship that is not a
//! hyperlink). Encrypted packages are rejected rather than decrypted. A
//! partially rewritten document is not a trust decision this module can make.
//!
//! Sizes are bounded: 32 MiB of input, 4096 package parts, 256 MiB of declared
//! uncompressed content, 5000 blocks, 20 000 characters per block, 256
//! operations per edit, and a fixed ceiling on how many blocks an inspection
//! lists.
//!
//! Inline images are preserved. The document model keeps image bytes in
//! `Document::media`, so an edit that touches one paragraph rewrites the whole
//! package and still carries every picture, byte for byte. Replacing a
//! paragraph's text keeps any inline objects that paragraph held, appended
//! after the new text.

use std::io::Read as _;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wordcraft_doc::numbering::ListKind;
use wordcraft_doc::props::NumRef;
use wordcraft_doc::{
    Block, Cell, CharProps, Document, InlineObject, Paragraph, Run, StyleKind, Table, para_block,
};

use crate::domain::Language;
use crate::error::{Result, SeraError};
use crate::gateway::{Tool, ToolContext, ToolOutput, ToolSpec};
use crate::maker::{ArtifactMeta, ArtifactStore};
use crate::store::Store;

/// Hard ceiling for a single input document and for a written result.
pub const MAX_DOCX_BYTES: usize = 32 * 1024 * 1024;
/// Hard ceiling for the number of parts in one package.
pub const MAX_PACKAGE_PARTS: usize = 4096;
/// Hard ceiling for the sum of declared uncompressed part sizes.
pub const MAX_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
/// Hard ceiling for a `.rels` part we read to look for external references.
const MAX_RELS_PART_BYTES: u64 = 4 * 1024 * 1024;
/// Hard ceiling for the number of blocks in one document.
pub const MAX_BLOCKS: usize = 5000;
/// Hard ceiling for the characters in one block.
pub const MAX_BLOCK_CHARS: usize = 20_000;
/// Hard ceiling for the operations in one edit.
pub const MAX_OPS: usize = 256;
/// How many blocks an inspection lists (the count is always reported).
pub const MAX_LISTED_BLOCKS: usize = 400;
/// Characters of block text an inspection shows per block.
pub const MAX_PREVIEW_CHARS: usize = 160;
/// Hard ceiling for the blocks one `docx_read` call returns.
pub const MAX_READ_BLOCKS: usize = 50;
/// Hard ceiling for the blocks one build spec may produce.
pub const MAX_BUILD_BLOCKS: usize = 2000;
/// Hard ceiling for a table's columns.
pub const MAX_TABLE_COLUMNS: usize = 32;
/// Hard ceiling for a table's rows.
pub const MAX_TABLE_ROWS: usize = 500;
pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_SUBTITLE_CHARS: usize = 300;
pub const MAX_STYLE_CHARS: usize = 64;
pub const MAX_FILENAME_CHARS: usize = 120;
/// Recursion guard for walks over table cells.
const MAX_WALK_DEPTH: usize = 8;
/// Text width of a built table, in points (Letter with one-inch margins).
const TABLE_WIDTH_PT: f32 = 468.0;

fn invalid(message: impl Into<String>) -> SeraError {
    SeraError::Validation(message.into())
}

// ---------------------------------------------------------------------------
// Package validation
// ---------------------------------------------------------------------------

/// Names of the parts in a package, read from the zip central directory.
struct PackageIndex {
    names: Vec<String>,
}

fn open_archive(bytes: &[u8]) -> Result<zip::ZipArchive<std::io::Cursor<&[u8]>>> {
    zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|error| invalid(format!("not a valid .docx zip package: {error}")))
}

/// Read the central directory only: no part is inflated here.
fn index_package(bytes: &[u8]) -> Result<PackageIndex> {
    let mut archive = open_archive(bytes)?;
    if archive.len() > MAX_PACKAGE_PARTS {
        return Err(invalid(format!(
            "DOCX package declares {} parts; the limit is {MAX_PACKAGE_PARTS}",
            archive.len()
        )));
    }
    let mut names = Vec::with_capacity(archive.len());
    let mut uncompressed = 0u64;
    for position in 0..archive.len() {
        let entry = archive.by_index_raw(position).map_err(|error| {
            invalid(format!(
                "DOCX package has a malformed central directory: {error}"
            ))
        })?;
        names.push(entry.name().to_owned());
        uncompressed = uncompressed.saturating_add(entry.size());
    }
    if uncompressed > MAX_UNCOMPRESSED_BYTES {
        return Err(invalid(format!(
            "DOCX package declares {uncompressed} uncompressed bytes; the limit is {MAX_UNCOMPRESSED_BYTES}"
        )));
    }
    Ok(PackageIndex { names })
}

/// Reject parts that carry active content. The reader preserves unmodelled
/// parts, so anything dangerous must fail here rather than be written back out.
fn reject_active_parts(names: &[String]) -> Result<()> {
    for name in names {
        let lowered = name.to_ascii_lowercase();
        if lowered.contains("vbaproject") || lowered.contains("vbadata") {
            return Err(invalid(
                "DOCX contains a VBA project (macros); SeraBot does not accept macro-enabled documents",
            ));
        }
        if lowered.starts_with("word/activex/") || lowered.contains("activex") {
            return Err(invalid(
                "DOCX contains ActiveX controls; SeraBot does not accept documents with active content",
            ));
        }
    }
    Ok(())
}

/// Reject relationships that point at remote content.
///
/// A hyperlink is an external relationship too, and it is harmless: it is a
/// link the reader may click, not content the document loads. Everything else
/// external (an attached template, a linked image, a linked OLE object, a
/// sub-document) makes Word fetch something when the file is opened, so it is
/// refused rather than silently dropped.
fn reject_external_references(bytes: &[u8], index: &PackageIndex) -> Result<()> {
    let rels: Vec<&str> = index
        .names
        .iter()
        .filter(|name| name.to_ascii_lowercase().ends_with(".rels"))
        .map(String::as_str)
        .collect();
    if rels.is_empty() {
        return Ok(());
    }
    let mut archive = open_archive(bytes)?;
    for name in rels {
        let Some(xml) = read_part(&mut archive, name, MAX_RELS_PART_BYTES)? else {
            continue;
        };
        for relationship in xml.split("<Relationship").skip(1) {
            // XML allows whitespace around `=` and either quote character, so
            // the scan runs on a whitespace-free copy of the element.
            let compact: String = relationship
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            if !(compact.contains("TargetMode=\"External\"")
                || compact.contains("TargetMode='External'"))
            {
                continue;
            }
            let kind = attribute(&compact, "Type").unwrap_or_default();
            if kind.to_ascii_lowercase().contains("/hyperlink") {
                continue;
            }
            let target = attribute(&compact, "Target").unwrap_or_default();
            return Err(invalid(format!(
                "DOCX has an external reference ({} -> {}); SeraBot does not accept documents that point at remote content",
                clip(&kind, 120),
                clip(&target, 120)
            )));
        }
    }
    Ok(())
}

/// Read one part, refusing anything larger than `limit`.
fn read_part(
    archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
    name: &str,
    limit: u64,
) -> Result<Option<String>> {
    let mut entry = match archive.by_name(name) {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(error) => {
            return Err(invalid(format!(
                "DOCX part {name} could not be read: {error}"
            )));
        }
    };
    if entry.size() > limit {
        return Err(invalid(format!(
            "DOCX part {name} is {} bytes; the limit is {limit}",
            entry.size()
        )));
    }
    let mut buffer = Vec::with_capacity(entry.size().min(limit) as usize);
    entry
        .read_to_end(&mut buffer)
        .map_err(|error| invalid(format!("DOCX part {name} could not be read: {error}")))?;
    Ok(Some(String::from_utf8_lossy(&buffer).into_owned()))
}

/// The value of `name="..."` in an XML attribute list.
fn attribute(element: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = element.find(&needle)? + needle.len();
    let rest = element.get(start..)?;
    let end = rest.find('"')?;
    rest.get(..end).map(str::to_owned)
}

fn clip(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn check_input_size(len: usize) -> Result<()> {
    if len == 0 {
        return Err(invalid("DOCX input is empty"));
    }
    if len > MAX_DOCX_BYTES {
        return Err(invalid(format!(
            "DOCX is {len} bytes; the limit is {MAX_DOCX_BYTES} bytes (32 MiB)"
        )));
    }
    Ok(())
}

/// Validate the package envelope before the reader sees it.
fn validate_package(bytes: &[u8]) -> Result<()> {
    check_input_size(bytes.len())?;
    if wordcraft_docx::is_encrypted(bytes) {
        return Err(invalid(
            "DOCX is password-protected; SeraBot does not decrypt documents",
        ));
    }
    let index = index_package(bytes)?;
    reject_active_parts(&index.names)?;
    reject_external_references(bytes, &index)?;
    Ok(())
}

fn read_error(error: wordcraft_docx::DocxError) -> SeraError {
    invalid(format!("invalid DOCX: {error}"))
}

fn write_error(error: wordcraft_docx::DocxError) -> SeraError {
    SeraError::Internal(format!("could not write DOCX: {error}"))
}

/// Parse and validate a document. Nothing is mutated here.
fn load(bytes: &[u8]) -> Result<Document> {
    validate_package(bytes)?;
    let document = wordcraft_docx::read(bytes).map_err(read_error)?;
    if document.body.len() > MAX_BLOCKS {
        return Err(invalid(format!(
            "DOCX has {} blocks; the limit is {MAX_BLOCKS}",
            document.body.len()
        )));
    }
    for (index, block) in document.body.iter().enumerate() {
        let mut longest = 0usize;
        for_each_paragraph(block, 0, &mut |paragraph| {
            longest = longest.max(paragraph.text.chars().count());
        });
        if longest > MAX_BLOCK_CHARS {
            return Err(invalid(format!(
                "DOCX block {index} holds {longest} characters; the limit is {MAX_BLOCK_CHARS} per block"
            )));
        }
    }
    Ok(document)
}

/// Serialise a document and bound the result.
fn save(document: &Document) -> Result<Vec<u8>> {
    let bytes = wordcraft_docx::write(document).map_err(write_error)?;
    if bytes.len() > MAX_DOCX_BYTES {
        return Err(invalid(format!(
            "the written DOCX is {} bytes; the limit is {MAX_DOCX_BYTES} bytes (32 MiB)",
            bytes.len()
        )));
    }
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// Block walking
// ---------------------------------------------------------------------------

fn for_each_paragraph<'a>(block: &'a Block, depth: usize, visit: &mut impl FnMut(&'a Paragraph)) {
    if depth > MAX_WALK_DEPTH {
        return;
    }
    match block {
        Block::Para(paragraph) => visit(paragraph),
        Block::Table(table) => {
            for row in &table.rows {
                for cell in &row.cells {
                    for nested in &cell.blocks {
                        for_each_paragraph(nested, depth + 1, visit);
                    }
                }
            }
        }
    }
}

fn block_has_image(block: &Block, depth: usize) -> bool {
    if depth > MAX_WALK_DEPTH {
        return false;
    }
    match block {
        Block::Para(paragraph) => paragraph
            .objects
            .iter()
            .any(|object| matches!(object, InlineObject::Image { .. })),
        Block::Table(table) => table.rows.iter().any(|row| {
            row.cells.iter().any(|cell| {
                cell.blocks
                    .iter()
                    .any(|nested| block_has_image(nested, depth + 1))
            })
        }),
    }
}

fn count_images(block: &Block, depth: usize) -> usize {
    if depth > MAX_WALK_DEPTH {
        return 0;
    }
    match block {
        Block::Para(paragraph) => paragraph
            .objects
            .iter()
            .filter(|object| matches!(object, InlineObject::Image { .. }))
            .count(),
        Block::Table(table) => table
            .rows
            .iter()
            .flat_map(|row| row.cells.iter())
            .flat_map(|cell| cell.blocks.iter())
            .map(|nested| count_images(nested, depth + 1))
            .sum(),
    }
}

fn table_text(table: &Table) -> String {
    table_cells(table)
        .into_iter()
        .map(|cells| cells.join(" | "))
        .collect::<Vec<_>>()
        .join(" ;; ")
}

/// A cell's text: its paragraphs joined, empty paragraphs dropped.
fn cell_text(cell: &Cell) -> String {
    let parts: Vec<String> = cell
        .blocks
        .iter()
        .filter_map(|block| block.as_para().map(|paragraph| paragraph.plain_text()))
        .filter(|text| !text.is_empty())
        .collect();
    parts.join(" / ")
}

/// Every cell's text, row by row.
fn table_cells(table: &Table) -> Vec<Vec<String>> {
    table
        .rows
        .iter()
        .map(|row| row.cells.iter().map(cell_text).collect())
        .collect()
}

// ---------------------------------------------------------------------------
// Inspection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocxBlockKind {
    Paragraph,
    Table,
}

/// One addressable block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocxBlockInfo {
    /// Zero-based position in the document body.
    pub index: usize,
    pub kind: DocxBlockKind,
    /// Paragraph style id, or the table style id.
    pub style: Option<String>,
    /// Text preview, capped at [`MAX_PREVIEW_CHARS`].
    pub text: String,
    /// Characters of text in the block (not capped).
    pub chars: usize,
    pub has_image: bool,
    /// List membership for a paragraph (`None` outside a list).
    pub list_level: Option<u8>,
    pub rows: Option<usize>,
    pub cols: Option<usize>,
}

/// Bounded structural summary of a document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocxInfo {
    pub block_count: usize,
    /// How many blocks [`DocxInfo::blocks`] holds.
    pub listed: usize,
    /// True when the document has more blocks than were listed.
    pub truncated: bool,
    pub images: usize,
    pub tables: usize,
    pub blocks: Vec<DocxBlockInfo>,
}

fn preview(text: &str) -> String {
    if text.chars().count() <= MAX_PREVIEW_CHARS {
        return text.to_owned();
    }
    let mut clipped: String = text.chars().take(MAX_PREVIEW_CHARS).collect();
    clipped.push_str("...");
    clipped
}

/// Structural summary with block addresses. Never mutates anything.
pub fn inspect(bytes: &[u8]) -> Result<DocxInfo> {
    let document = load(bytes)?;
    let block_count = document.body.len();
    let mut images = 0usize;
    let mut tables = 0usize;
    let mut blocks = Vec::with_capacity(block_count.min(MAX_LISTED_BLOCKS));
    for (index, block) in document.body.iter().enumerate() {
        images = images.saturating_add(count_images(block, 0));
        if matches!(block.as_ref(), Block::Table(_)) {
            tables = tables.saturating_add(1);
        }
        if index >= MAX_LISTED_BLOCKS {
            continue;
        }
        match block.as_ref() {
            Block::Para(paragraph) => {
                let text = paragraph.plain_text();
                blocks.push(DocxBlockInfo {
                    index,
                    kind: DocxBlockKind::Paragraph,
                    style: paragraph.props.style.clone(),
                    chars: text.chars().count(),
                    text: preview(&text),
                    has_image: block_has_image(block, 0),
                    list_level: paragraph
                        .props
                        .numbering
                        .as_ref()
                        .map(|numbering| numbering.level),
                    rows: None,
                    cols: None,
                });
            }
            Block::Table(table) => {
                let text = table_text(table);
                blocks.push(DocxBlockInfo {
                    index,
                    kind: DocxBlockKind::Table,
                    style: table.props.style.clone(),
                    chars: text.chars().count(),
                    text: preview(&text),
                    has_image: block_has_image(block, 0),
                    list_level: None,
                    rows: Some(table.rows.len()),
                    cols: Some(table.cols()),
                });
            }
        }
    }
    Ok(DocxInfo {
        block_count,
        listed: blocks.len(),
        truncated: block_count > MAX_LISTED_BLOCKS,
        images,
        tables,
        blocks,
    })
}

// ---------------------------------------------------------------------------
// Full block text
// ---------------------------------------------------------------------------

/// One block's complete text, for reading before an edit.
///
/// `text` is the whole block unless it is longer than [`MAX_BLOCK_CHARS`], in
/// which case `text_truncated` is set and `chars` still reports the real
/// length. A table also carries its cells, so no cell text is lost when the
/// joined form is clipped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocxBlockText {
    pub index: usize,
    pub kind: DocxBlockKind,
    pub style: Option<String>,
    /// The block's text, complete up to [`MAX_BLOCK_CHARS`].
    pub text: String,
    /// Characters of text in the block, before any clipping.
    pub chars: usize,
    pub has_image: bool,
    pub list_level: Option<u8>,
    pub rows: Option<usize>,
    pub cols: Option<usize>,
    /// Cell texts row by row, for a table block.
    pub cells: Option<Vec<Vec<String>>>,
    /// True when `text` (or a cell) holds fewer characters than `chars`.
    pub text_truncated: bool,
}

/// The blocks one read returned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocxText {
    pub block_count: usize,
    /// First block index included.
    pub from: usize,
    /// Last block index included, inclusive.
    pub to: usize,
    pub blocks: Vec<DocxBlockText>,
}

/// Clip text to [`MAX_BLOCK_CHARS`], reporting whether anything was dropped.
fn clip_to_block(text: &str) -> (String, bool) {
    if text.chars().count() <= MAX_BLOCK_CHARS {
        return (text.to_owned(), false);
    }
    (text.chars().take(MAX_BLOCK_CHARS).collect(), true)
}

/// The complete text of an inclusive block range. Never mutates anything.
///
/// The range is bounded: more than [`MAX_READ_BLOCKS`] blocks is an error that
/// names the range, never a silent truncation.
pub fn read_blocks(bytes: &[u8], from: usize, to: usize) -> Result<DocxText> {
    let document = load(bytes)?;
    let block_count = document.body.len();
    if from > to {
        return Err(invalid(format!(
            "from ({from}) must not be greater than to ({to})"
        )));
    }
    if to >= block_count {
        return Err(invalid(format!(
            "block {to} is out of range; the document has {block_count} block(s), so a block index is 0..{}",
            block_count.saturating_sub(1)
        )));
    }
    let requested = to - from + 1;
    if requested > MAX_READ_BLOCKS {
        return Err(invalid(format!(
            "the range {from}..={to} covers {requested} blocks; docx_read returns at most \
             {MAX_READ_BLOCKS} blocks per call — narrow the range"
        )));
    }

    let mut blocks = Vec::with_capacity(requested);
    for index in from..=to {
        let block = document
            .body
            .get(index)
            .ok_or_else(|| out_of_range(index, block_count))?;
        match block.as_ref() {
            Block::Para(paragraph) => {
                let full = paragraph.plain_text();
                let chars = full.chars().count();
                let (text, text_truncated) = clip_to_block(&full);
                blocks.push(DocxBlockText {
                    index,
                    kind: DocxBlockKind::Paragraph,
                    style: paragraph.props.style.clone(),
                    text,
                    chars,
                    has_image: block_has_image(block, 0),
                    list_level: paragraph
                        .props
                        .numbering
                        .as_ref()
                        .map(|numbering| numbering.level),
                    rows: None,
                    cols: None,
                    cells: None,
                    text_truncated,
                });
            }
            Block::Table(table) => {
                let full = table_text(table);
                let chars = full.chars().count();
                let (text, mut text_truncated) = clip_to_block(&full);
                let mut cells = Vec::with_capacity(table.rows.len());
                for row in &table.rows {
                    let mut row_cells = Vec::with_capacity(row.cells.len());
                    for cell in &row.cells {
                        let (cell_text, clipped) = clip_to_block(&cell_text(cell));
                        text_truncated = text_truncated || clipped;
                        row_cells.push(cell_text);
                    }
                    cells.push(row_cells);
                }
                blocks.push(DocxBlockText {
                    index,
                    kind: DocxBlockKind::Table,
                    style: table.props.style.clone(),
                    text,
                    chars,
                    has_image: block_has_image(block, 0),
                    list_level: None,
                    rows: Some(table.rows.len()),
                    cols: Some(table.cols()),
                    cells: Some(cells),
                    text_truncated,
                });
            }
        }
    }
    Ok(DocxText {
        block_count,
        from,
        to,
        blocks,
    })
}

// ---------------------------------------------------------------------------
// Text and style validation
// ---------------------------------------------------------------------------

fn is_bidi_or_zero_width(character: char) -> bool {
    matches!(
        character,
        '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'
            | '\u{2066}'..='\u{2069}'
            | '\u{00ad}'
            | '\u{feff}'
    )
}

/// Length- and control-character check for every string this module writes.
///
/// `multiline` allows the two characters the writer turns into a tab and a line
/// break (`\t`, `\n`); everything else in C0/C1 is refused rather than silently
/// dropped, and the inline-object placeholder is refused because the model has
/// no way to address it.
fn check_text(label: &str, value: &str, max_chars: usize, multiline: bool) -> Result<()> {
    if value.chars().count() > max_chars {
        return Err(invalid(format!(
            "{label} must be at most {max_chars} characters"
        )));
    }
    for character in value.chars() {
        if character == wordcraft_doc::para::OBJ {
            return Err(invalid(format!(
                "{label} contains the inline-object placeholder U+FFFC, which SeraBot cannot address"
            )));
        }
        if is_bidi_or_zero_width(character) {
            return Err(invalid(format!(
                "{label} contains invisible formatting characters"
            )));
        }
        if character.is_control() && !(multiline && matches!(character, '\n' | '\t')) {
            return Err(invalid(format!("{label} contains control characters")));
        }
    }
    Ok(())
}

fn check_block_text(label: &str, value: &str) -> Result<()> {
    check_text(label, value, MAX_BLOCK_CHARS, true)
}

fn check_style_name(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(invalid(format!("{label} must not be empty")));
    }
    check_text(label, value, MAX_STYLE_CHARS, false)
}

/// Paragraph styles a document offers, capped so an error message stays small.
fn style_list(document: &Document) -> String {
    const MAX_LISTED_STYLES: usize = 24;
    let names: Vec<&str> = document
        .styles
        .styles
        .iter()
        .filter(|style| style.kind == StyleKind::Paragraph)
        .map(|style| style.id.as_str())
        .take(MAX_LISTED_STYLES)
        .collect();
    if names.is_empty() {
        return "none".to_owned();
    }
    let mut joined = names.join(", ");
    let total = document
        .styles
        .styles
        .iter()
        .filter(|style| style.kind == StyleKind::Paragraph)
        .count();
    if total > names.len() {
        joined.push_str(", ...");
    }
    joined
}

/// Resolve a requested style to the id the document actually defines.
fn resolve_paragraph_style(document: &Document, requested: &str) -> Result<String> {
    check_style_name("style", requested)?;
    let Some(style) = document.styles.find(requested) else {
        return Err(invalid(format!(
            "unknown style `{requested}`; paragraph styles in this document are: {}",
            style_list(document)
        )));
    };
    if style.kind != StyleKind::Paragraph {
        return Err(invalid(format!(
            "style `{}` is not a paragraph style; paragraph styles in this document are: {}",
            style.id,
            style_list(document)
        )));
    }
    Ok(style.id.clone())
}

// ---------------------------------------------------------------------------
// Edits
// ---------------------------------------------------------------------------

/// One bounded edit operation.
///
/// The four operations share one JSON shape because serde cannot combine an
/// internally tagged enum with `deny_unknown_fields`; `op` selects which fields
/// are allowed, and a field that does not belong to the operation is rejected
/// rather than ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocxOp {
    /// `replace_text`, `insert_paragraph`, `delete_block`, or `apply_style`.
    pub op: String,
    #[serde(default)]
    pub index: Option<usize>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub style: Option<String>,
}

pub const OP_NAMES: [&str; 4] = [
    "replace_text",
    "insert_paragraph",
    "delete_block",
    "apply_style",
];

#[derive(Debug, Clone, PartialEq)]
enum Op {
    ReplaceText {
        index: usize,
        text: String,
    },
    InsertParagraph {
        index: usize,
        text: String,
        style: Option<String>,
    },
    DeleteBlock {
        index: usize,
    },
    ApplyStyle {
        index: usize,
        style: String,
    },
}

impl DocxOp {
    fn parse(&self, position: usize) -> Result<Op> {
        let name = self.op.trim().to_ascii_lowercase();
        let index = || {
            self.index.ok_or_else(|| {
                invalid(format!("ops[{position}]: `index` is required for `{name}`"))
            })
        };
        let forbid = |field: &str, present: bool| -> Result<()> {
            if present {
                return Err(invalid(format!(
                    "ops[{position}]: `{name}` does not take a `{field}` field"
                )));
            }
            Ok(())
        };
        match name.as_str() {
            "replace_text" => {
                forbid("style", self.style.is_some())?;
                let text = self.text.as_deref().ok_or_else(|| {
                    invalid(format!(
                        "ops[{position}]: `text` is required for `replace_text`"
                    ))
                })?;
                check_block_text(&format!("ops[{position}].text"), text)?;
                Ok(Op::ReplaceText {
                    index: index()?,
                    text: text.to_owned(),
                })
            }
            "insert_paragraph" => {
                let text = self.text.as_deref().ok_or_else(|| {
                    invalid(format!(
                        "ops[{position}]: `text` is required for `insert_paragraph`"
                    ))
                })?;
                check_block_text(&format!("ops[{position}].text"), text)?;
                if let Some(style) = &self.style {
                    check_style_name(&format!("ops[{position}].style"), style)?;
                }
                Ok(Op::InsertParagraph {
                    index: index()?,
                    text: text.to_owned(),
                    style: self.style.clone(),
                })
            }
            "delete_block" => {
                forbid("text", self.text.is_some())?;
                forbid("style", self.style.is_some())?;
                Ok(Op::DeleteBlock { index: index()? })
            }
            "apply_style" => {
                forbid("text", self.text.is_some())?;
                let style = self.style.as_deref().ok_or_else(|| {
                    invalid(format!(
                        "ops[{position}]: `style` is required for `apply_style`"
                    ))
                })?;
                check_style_name(&format!("ops[{position}].style"), style)?;
                Ok(Op::ApplyStyle {
                    index: index()?,
                    style: style.to_owned(),
                })
            }
            _ => Err(invalid(format!(
                "ops[{position}]: `{}` is not an operation; use one of {}",
                clip(&self.op, 40),
                OP_NAMES.join(", ")
            ))),
        }
    }
}

fn out_of_range(index: usize, block_count: usize) -> SeraError {
    invalid(format!(
        "block {index} is out of range; the document has {block_count} block(s), so a block index is 0..{}",
        block_count.saturating_sub(1)
    ))
}

/// Replace a paragraph's text, keeping its paragraph formatting and any inline
/// objects it held.
fn replace_paragraph_text(paragraph: &mut Paragraph, text: &str) -> Result<()> {
    let base = paragraph
        .runs
        .first()
        .map(|run| run.props.clone())
        .unwrap_or_else(|| paragraph.mark.clone());
    let props = paragraph.props.clone();
    let mark = paragraph.mark.clone();
    let section = paragraph.section.clone();
    let objects = std::mem::take(&mut paragraph.objects);

    let mut fresh = Paragraph::new();
    fresh.text = text.to_owned();
    if !text.is_empty() {
        fresh.runs.push(Run {
            len: text.len(),
            props: base,
        });
    }
    fresh.props = props;
    fresh.mark = mark.clone();
    fresh.section = section;

    for object in objects {
        let offset = fresh.text.len();
        fresh
            .insert_object(offset, object, &mark)
            .map_err(|error| {
                invalid(format!(
                    "could not keep an inline object while replacing the paragraph text: {error}"
                ))
            })?;
    }
    *paragraph = fresh;
    Ok(())
}

fn apply_op(document: &mut Document, op: &Op) -> Result<String> {
    match op {
        Op::ReplaceText { index, text } => {
            let block_count = document.body.len();
            let block = document
                .body
                .get_mut(*index)
                .ok_or_else(|| out_of_range(*index, block_count))?;
            match Arc::make_mut(block) {
                Block::Para(paragraph) => {
                    replace_paragraph_text(paragraph, text)?;
                    Ok(format!("replaced the text of block {index}"))
                }
                Block::Table(_) => Err(invalid(format!(
                    "block {index} is a table; `replace_text` only addresses paragraphs"
                ))),
            }
        }
        Op::InsertParagraph { index, text, style } => {
            let block_count = document.body.len();
            if *index > block_count {
                return Err(invalid(format!(
                    "block {index} is out of range; the document has {block_count} block(s), so an insert index is 0..={block_count}"
                )));
            }
            let mut paragraph = Paragraph::with_text(text, CharProps::default());
            if let Some(style) = style {
                let resolved = resolve_paragraph_style(document, style)?;
                paragraph.props.style = Some(resolved);
            }
            document.body.insert(*index, para_block(paragraph));
            Ok(format!("inserted a paragraph at block {index}"))
        }
        Op::DeleteBlock { index } => {
            let block_count = document.body.len();
            if block_count <= 1 {
                return Err(invalid(
                    "the document has a single block; deleting it would leave an empty document",
                ));
            }
            if *index >= block_count {
                return Err(out_of_range(*index, block_count));
            }
            document.body.remove(*index);
            Ok(format!("deleted block {index}"))
        }
        Op::ApplyStyle { index, style } => {
            let resolved = resolve_paragraph_style(document, style)?;
            let block_count = document.body.len();
            let block = document
                .body
                .get_mut(*index)
                .ok_or_else(|| out_of_range(*index, block_count))?;
            match Arc::make_mut(block) {
                Block::Para(paragraph) => {
                    paragraph.props.style = Some(resolved.clone());
                    Ok(format!("applied style `{resolved}` to block {index}"))
                }
                Block::Table(_) => Err(invalid(format!(
                    "block {index} is a table; `apply_style` only addresses paragraphs"
                ))),
            }
        }
    }
}

/// Apply every operation in order. Any failure leaves the document untouched in
/// the caller's hands: the caller re-reads the artifact rather than keeping a
/// half-edited document.
pub fn apply_ops(document: &mut Document, ops: &[DocxOp]) -> Result<Vec<String>> {
    if ops.is_empty() {
        return Err(invalid("ops must contain at least one operation"));
    }
    if ops.len() > MAX_OPS {
        return Err(invalid(format!(
            "ops contains {} operations; the limit is {MAX_OPS}",
            ops.len()
        )));
    }
    let mut parsed = Vec::with_capacity(ops.len());
    for (position, op) in ops.iter().enumerate() {
        parsed.push(op.parse(position)?);
    }
    let mut summaries = Vec::with_capacity(parsed.len());
    for op in &parsed {
        summaries.push(apply_op(document, op)?);
    }
    if document.body.len() > MAX_BLOCKS {
        return Err(invalid(format!(
            "the edit would leave {} blocks; the limit is {MAX_BLOCKS}",
            document.body.len()
        )));
    }
    Ok(summaries)
}

// ---------------------------------------------------------------------------
// Build
// ---------------------------------------------------------------------------

/// A bounded document specification: a title, an optional subtitle, and a list
/// of blocks. Text only — images and hyperlinks are not expressible here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocxSpec {
    pub title: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    /// Optional document language, recorded with the artifact.
    #[serde(default)]
    pub language: Option<Language>,
    pub blocks: Vec<DocxBlockSpec>,
}

/// One block of a build spec.
///
/// `kind` selects which fields apply: `heading` uses `level` and `text`,
/// `paragraph` uses `text`, `bullets` uses `items`, and `table` uses `header`
/// and `rows`. Fields that do not belong to the kind are rejected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocxBlockSpec {
    pub kind: String,
    #[serde(default)]
    pub level: Option<u8>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub items: Vec<String>,
    #[serde(default)]
    pub header: Vec<String>,
    #[serde(default)]
    pub rows: Vec<Vec<String>>,
}

pub const BLOCK_KINDS: [&str; 4] = ["heading", "paragraph", "bullets", "table"];

impl DocxBlockSpec {
    fn forbid(&self, label: &str, field: &str, present: bool) -> Result<()> {
        if present {
            return Err(invalid(format!(
                "{label}: `{}` does not take a `{field}` field",
                self.kind
            )));
        }
        Ok(())
    }

    fn text_field(&self, label: &str) -> Result<&str> {
        let text = self
            .text
            .as_deref()
            .ok_or_else(|| invalid(format!("{label}: `text` is required for `{}`", self.kind)))?;
        check_block_text(&format!("{label}.text"), text)?;
        Ok(text)
    }

    /// How many body blocks this spec entry produces.
    fn block_count(&self) -> usize {
        match self.kind.trim().to_ascii_lowercase().as_str() {
            "bullets" => self.items.len(),
            _ => 1,
        }
    }
}

fn validate_spec(spec: &DocxSpec) -> Result<()> {
    check_text("spec.title", &spec.title, MAX_TITLE_CHARS, false)?;
    if spec.title.trim().is_empty() {
        return Err(invalid("spec.title must not be empty"));
    }
    if let Some(subtitle) = &spec.subtitle {
        check_text("spec.subtitle", subtitle, MAX_SUBTITLE_CHARS, false)?;
    }
    if spec.blocks.is_empty() {
        return Err(invalid("spec.blocks must contain at least one block"));
    }
    if spec.blocks.len() > MAX_BUILD_BLOCKS {
        return Err(invalid(format!(
            "spec.blocks contains {} entries; the limit is {MAX_BUILD_BLOCKS}",
            spec.blocks.len()
        )));
    }

    let mut total = 1 + usize::from(spec.subtitle.is_some());
    for (position, block) in spec.blocks.iter().enumerate() {
        let label = format!("spec.blocks[{position}]");
        total = total.saturating_add(block.block_count());
        match block.kind.trim().to_ascii_lowercase().as_str() {
            "heading" => {
                block.forbid(&label, "items", !block.items.is_empty())?;
                block.forbid(&label, "header", !block.header.is_empty())?;
                block.forbid(&label, "rows", !block.rows.is_empty())?;
                let level = block.level.unwrap_or(1);
                if !(1..=4).contains(&level) {
                    return Err(invalid(format!("{label}.level must be 1, 2, 3, or 4")));
                }
                let text = block.text_field(&label)?;
                if text.trim().is_empty() {
                    return Err(invalid(format!("{label}.text must not be empty")));
                }
            }
            "paragraph" => {
                block.forbid(&label, "level", block.level.is_some())?;
                block.forbid(&label, "items", !block.items.is_empty())?;
                block.forbid(&label, "header", !block.header.is_empty())?;
                block.forbid(&label, "rows", !block.rows.is_empty())?;
                block.text_field(&label)?;
            }
            "bullets" => {
                block.forbid(&label, "level", block.level.is_some())?;
                block.forbid(&label, "text", block.text.is_some())?;
                block.forbid(&label, "header", !block.header.is_empty())?;
                block.forbid(&label, "rows", !block.rows.is_empty())?;
                if block.items.is_empty() {
                    return Err(invalid(format!(
                        "{label}.items must contain at least one bullet"
                    )));
                }
                for (item, text) in block.items.iter().enumerate() {
                    check_block_text(&format!("{label}.items[{item}]"), text)?;
                }
            }
            "table" => {
                block.forbid(&label, "level", block.level.is_some())?;
                block.forbid(&label, "text", block.text.is_some())?;
                block.forbid(&label, "items", !block.items.is_empty())?;
                if block.header.is_empty() && block.rows.is_empty() {
                    return Err(invalid(format!(
                        "{label} must have a header row, data rows, or both"
                    )));
                }
                if block.header.len() > MAX_TABLE_COLUMNS {
                    return Err(invalid(format!(
                        "{label}.header must contain at most {MAX_TABLE_COLUMNS} cells"
                    )));
                }
                if block.rows.len() > MAX_TABLE_ROWS {
                    return Err(invalid(format!(
                        "{label}.rows must contain at most {MAX_TABLE_ROWS} rows"
                    )));
                }
                for (column, text) in block.header.iter().enumerate() {
                    check_block_text(&format!("{label}.header[{column}]"), text)?;
                }
                for (row, cells) in block.rows.iter().enumerate() {
                    if cells.len() > MAX_TABLE_COLUMNS {
                        return Err(invalid(format!(
                            "{label}.rows[{row}] must contain at most {MAX_TABLE_COLUMNS} cells"
                        )));
                    }
                    for (column, text) in cells.iter().enumerate() {
                        check_block_text(&format!("{label}.rows[{row}][{column}]"), text)?;
                    }
                }
            }
            other => {
                return Err(invalid(format!(
                    "{label}.kind `{}` is not a block kind; use one of {}",
                    clip(other, 40),
                    BLOCK_KINDS.join(", ")
                )));
            }
        }
    }
    if total > MAX_BLOCKS {
        return Err(invalid(format!(
            "the spec would produce {total} blocks; the limit is {MAX_BLOCKS}"
        )));
    }
    Ok(())
}

fn text_cell(text: &str, bold: bool) -> Cell {
    let props = CharProps {
        bold: bold.then_some(true),
        ..CharProps::default()
    };
    let mut cell = Cell::empty();
    cell.blocks = vec![para_block(Paragraph::with_text(text, props))];
    cell
}

fn build_table(block: &DocxBlockSpec) -> Table {
    let columns = block
        .header
        .len()
        .max(block.rows.iter().map(Vec::len).max().unwrap_or(0))
        .clamp(1, MAX_TABLE_COLUMNS);
    let data_rows = block.rows.len();
    let header_rows = usize::from(!block.header.is_empty());
    let mut table = Table::new((header_rows + data_rows).max(1), columns, TABLE_WIDTH_PT);

    let mut row_index = 0usize;
    if !block.header.is_empty() {
        for (column, text) in block.header.iter().enumerate() {
            if let Some(cell) = table
                .rows
                .get_mut(row_index)
                .and_then(|row| row.cells.get_mut(column))
            {
                *cell = text_cell(text, true);
            }
        }
        row_index += 1;
    }
    for (offset, cells) in block.rows.iter().enumerate() {
        for (column, text) in cells.iter().enumerate() {
            if let Some(cell) = table
                .rows
                .get_mut(row_index + offset)
                .and_then(|row| row.cells.get_mut(column))
            {
                *cell = text_cell(text, false);
            }
        }
    }
    table
}

/// Build a `.docx` package from a bounded spec.
pub fn build(spec: &DocxSpec) -> Result<Vec<u8>> {
    validate_spec(spec)?;
    let mut document = Document::new();
    document.core.title = spec.title.clone();

    let mut body: Vec<Arc<Block>> = Vec::with_capacity(spec.blocks.len() + 2);
    body.push(para_block(
        Paragraph::with_text(&spec.title, CharProps::default()).styled("Title"),
    ));
    if let Some(subtitle) = &spec.subtitle {
        body.push(para_block(
            Paragraph::with_text(subtitle, CharProps::default()).styled("Subtitle"),
        ));
    }
    for block in &spec.blocks {
        match block.kind.trim().to_ascii_lowercase().as_str() {
            "heading" => {
                let level = block.level.unwrap_or(1);
                let style = format!("Heading{level}");
                let text = block.text.clone().unwrap_or_default();
                body.push(para_block(
                    Paragraph::with_text(&text, CharProps::default()).styled(&style),
                ));
            }
            "paragraph" => {
                let text = block.text.clone().unwrap_or_default();
                body.push(para_block(Paragraph::with_text(
                    &text,
                    CharProps::default(),
                )));
            }
            "bullets" => {
                let num = document.numbering.add_list(ListKind::Bullet);
                for item in &block.items {
                    let mut paragraph = Paragraph::with_text(item, CharProps::default());
                    paragraph.props.numbering = Some(NumRef { num, level: 0 });
                    body.push(para_block(paragraph));
                }
            }
            _ => body.push(Arc::new(Block::Table(build_table(block)))),
        }
    }
    // A body cannot end with a table: Word (and this reader) keeps an empty
    // paragraph after one, so the block list a build reports is the block list
    // an inspection reports.
    if matches!(body.last().map(AsRef::as_ref), Some(Block::Table(_))) {
        body.push(para_block(Paragraph::new()));
    }
    document.body = body;
    save(&document)
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

fn required_artifact(args: &Value) -> Result<String> {
    let hash = args
        .get("artifact")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid("artifact is required"))?;
    if hash.chars().count() > 128 {
        return Err(invalid("artifact is not a valid artifact hash"));
    }
    Ok(hash.to_owned())
}

fn check_filename(name: &str) -> Result<()> {
    if name.chars().count() > MAX_FILENAME_CHARS {
        return Err(invalid(format!(
            "filename must be at most {MAX_FILENAME_CHARS} characters"
        )));
    }
    if name.chars().any(|character| {
        character.is_control()
            || is_bidi_or_zero_width(character)
            || matches!(
                character,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
            )
    }) {
        return Err(invalid(
            "filename must not contain path separators or reserved characters",
        ));
    }
    if !name.to_ascii_lowercase().ends_with(".docx") {
        return Err(invalid("filename must end in .docx"));
    }
    Ok(())
}

/// A deterministic, ASCII-only artifact filename derived from the title.
fn default_filename(title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    for character in title.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug: String = slug.chars().take(MAX_FILENAME_CHARS - 5).collect();
    if slug.is_empty() {
        "document.docx".to_owned()
    } else {
        format!("{slug}.docx")
    }
}

fn optional_filename(args: &Value, fallback: impl FnOnce() -> String) -> Result<String> {
    match args
        .get("filename")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(name) => {
            check_filename(name)?;
            Ok(name.to_owned())
        }
        None => Ok(fallback()),
    }
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

/// Human-readable block listing, capped so a 5000-block document cannot flood
/// the model's context. The complete list stays in the tool's structured data.
fn block_listing(info: &DocxInfo) -> String {
    let mut listing = String::new();
    for block in &info.blocks {
        let kind = match block.kind {
            DocxBlockKind::Paragraph => "paragraph",
            DocxBlockKind::Table => "table",
        };
        let style = block.style.as_deref().unwrap_or("-");
        let mut flags = Vec::new();
        if block.has_image {
            flags.push("image".to_owned());
        }
        if let Some(level) = block.list_level {
            flags.push(format!("list level {level}"));
        }
        if let (Some(rows), Some(cols)) = (block.rows, block.cols) {
            flags.push(format!("{rows}x{cols}"));
        }
        let flags = if flags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", flags.join(", "))
        };
        listing.push_str(&format!(
            "[{}] {kind} style={style}{flags} {:?}\n",
            block.index, block.text
        ));
    }
    if info.truncated {
        listing.push_str(&format!(
            "... {} more block(s) not listed\n",
            info.block_count - info.listed
        ));
    }
    listing
}

pub struct DocxInspectTool {
    artifacts: Arc<ArtifactStore>,
}

impl DocxInspectTool {
    pub fn new(artifacts: Arc<ArtifactStore>) -> Self {
        Self { artifacts }
    }
}

#[async_trait]
impl Tool for DocxInspectTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "docx_inspect".into(),
            description:
                "Inspect a .docx artifact and return its addressable blocks. A block is a \
                 paragraph or a table, numbered from 0 in document order; each entry carries the \
                 index, kind, style, a text preview, `chars` (the block's real character count), \
                 and whether it holds an image. A preview shorter than `chars` means the block was \
                 cut: call `docx_read` for the full text before editing it. Packages with macros, \
                 ActiveX parts, remote references, or a password are rejected."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "artifact": {"type": "string", "description": "Artifact hash of the .docx"}
                },
                "required": ["artifact"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, _context: &ToolContext) -> Result<ToolOutput> {
        reject_unknown_keys(&args, &["artifact"])?;
        let hash = required_artifact(&args)?;
        let stored = self.artifacts.load(&hash)?;
        let bytes = stored.bytes;
        let info = tokio::task::spawn_blocking(move || inspect(&bytes))
            .await
            .map_err(|error| SeraError::Internal(format!("DOCX task failed: {error}")))??;
        let data = serde_json::to_value(&info)?;
        let content = format!(
            "blocks: {}\nimages: {}\ntables: {}\n\n{}",
            info.block_count,
            info.images,
            info.tables,
            block_listing(&info)
        );
        Ok(ToolOutput {
            content,
            data: Some(data),
            ..ToolOutput::default()
        })
    }
}

pub struct DocxReadTool {
    artifacts: Arc<ArtifactStore>,
}

impl DocxReadTool {
    pub fn new(artifacts: Arc<ArtifactStore>) -> Self {
        Self { artifacts }
    }
}

/// Which blocks a read addresses.
enum Selection {
    Single(usize),
    Range(usize, usize),
}

fn index_argument(args: &Value, key: &str) -> Result<Option<usize>> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    let number = value
        .as_u64()
        .ok_or_else(|| invalid(format!("{key} must be a non-negative integer")))?;
    let index =
        usize::try_from(number).map_err(|_| invalid(format!("{key} is too large for an index")))?;
    Ok(Some(index))
}

fn read_selection(args: &Value) -> Result<Selection> {
    let block = index_argument(args, "block")?;
    let from = index_argument(args, "from")?;
    let to = index_argument(args, "to")?;
    match (block, from, to) {
        (Some(index), None, None) => Ok(Selection::Single(index)),
        (None, Some(from), Some(to)) => Ok(Selection::Range(from, to)),
        (None, None, None) => Err(invalid("provide `block`, or `from` and `to`")),
        (None, Some(_), None) => Err(invalid("`from` needs `to` as well")),
        (None, None, Some(_)) => Err(invalid("`to` needs `from` as well")),
        (Some(_), _, _) => Err(invalid(
            "provide either `block` or `from` and `to`, not both",
        )),
    }
}

/// Human-readable dump of a read range. Tables are shown row by row so the
/// model can see every cell, not a joined rendering it has to re-split.
fn block_text_listing(text: &DocxText) -> String {
    let mut listing = format!(
        "blocks: {}\nreading: {}..={} ({} block(s))\n\n",
        text.block_count,
        text.from,
        text.to,
        text.blocks.len()
    );
    for block in &text.blocks {
        let kind = match block.kind {
            DocxBlockKind::Paragraph => "paragraph",
            DocxBlockKind::Table => "table",
        };
        let style = block.style.as_deref().unwrap_or("-");
        let mut flags = Vec::new();
        if block.has_image {
            flags.push("image".to_owned());
        }
        if let Some(level) = block.list_level {
            flags.push(format!("list level {level}"));
        }
        if let (Some(rows), Some(cols)) = (block.rows, block.cols) {
            flags.push(format!("{rows}x{cols}"));
        }
        let flags = if flags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", flags.join(", "))
        };
        listing.push_str(&format!(
            "[{}] {kind} style={style} chars={}{flags}\n",
            block.index, block.chars
        ));
        match &block.cells {
            Some(cells) => {
                for (row, row_cells) in cells.iter().enumerate() {
                    listing.push_str(&format!("  row {row}: {}\n", row_cells.join(" | ")));
                }
            }
            None => {
                listing.push_str(&block.text);
                listing.push('\n');
            }
        }
        if block.text_truncated {
            listing.push_str(&format!(
                "  (this block holds {} characters; only the first {MAX_BLOCK_CHARS} are shown)\n",
                block.chars
            ));
        }
    }
    listing
}

#[async_trait]
impl Tool for DocxReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "docx_read".into(),
            description:
                "Read the complete text of one .docx block, or of an inclusive range of blocks. Use \
                 this before `docx_edit`'s replace_text, which replaces the whole block: the \
                 preview `docx_inspect` returns is capped, so editing from it would destroy text \
                 you never saw. Returns per block the index, kind, style, full text, character \
                 count, and whether it holds an image; a table also returns its row and column \
                 counts and every cell's text. Read-only: nothing is written and no artifact is \
                 produced. At most 50 blocks per call; a wider range is an error, not a truncation."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "artifact": {"type": "string", "description": "Artifact hash of the .docx"},
                    "block": {"type": "integer", "description": "A single zero-based block index"},
                    "from": {"type": "integer", "description": "First block index of an inclusive range"},
                    "to": {"type": "integer", "description": "Last block index of an inclusive range"}
                },
                "required": ["artifact"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, _context: &ToolContext) -> Result<ToolOutput> {
        reject_unknown_keys(&args, &["artifact", "block", "from", "to"])?;
        let hash = required_artifact(&args)?;
        let (from, to) = match read_selection(&args)? {
            Selection::Single(index) => (index, index),
            Selection::Range(from, to) => (from, to),
        };
        let stored = self.artifacts.load(&hash)?;
        let bytes = stored.bytes;
        let text = tokio::task::spawn_blocking(move || read_blocks(&bytes, from, to))
            .await
            .map_err(|error| SeraError::Internal(format!("DOCX task failed: {error}")))??;
        let data = serde_json::to_value(&text)?;
        Ok(ToolOutput {
            content: block_text_listing(&text),
            data: Some(data),
            ..ToolOutput::default()
        })
    }
}

pub struct DocxEditTool {
    artifacts: Arc<ArtifactStore>,
    store: Store,
}

impl DocxEditTool {
    pub fn new(artifacts: Arc<ArtifactStore>, store: Store) -> Self {
        Self { artifacts, store }
    }
}

#[async_trait]
impl Tool for DocxEditTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "docx_edit".into(),
            description:
                "Apply bounded block operations to a .docx artifact and store the result as a new \
                 artifact that records its source. Operations are replace_text {index, text}, \
                 insert_paragraph {index, text, optional style}, delete_block {index}, and \
                 apply_style {index, style}. replace_text replaces the block's ENTIRE text, so call \
                 docx_read first and supply the full replacement text. Every index is validated \
                 against the document before anything is written, and inline images are preserved."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "artifact": {"type": "string", "description": "Artifact hash of the source .docx"},
                    "ops": {
                        "type": "array",
                        "description": "Operations in order, at most 256",
                        "items": {
                            "type": "object",
                            "properties": {
                                "op": {
                                    "type": "string",
                                    "enum": ["replace_text", "insert_paragraph", "delete_block", "apply_style"]
                                },
                                "index": {"type": "integer", "description": "Zero-based block index"},
                                "text": {"type": "string"},
                                "style": {"type": "string", "description": "Paragraph style id or name"}
                            },
                            "required": ["op"],
                            "additionalProperties": false
                        }
                    },
                    "filename": {"type": "string", "description": "Optional artifact filename ending in .docx"}
                },
                "required": ["artifact", "ops"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        reject_unknown_keys(&args, &["artifact", "ops", "filename"])?;
        let hash = required_artifact(&args)?;
        let ops_value = args.get("ops").ok_or_else(|| invalid("ops is required"))?;
        let ops: Vec<DocxOp> = serde_json::from_value(ops_value.clone())
            .map_err(|error| invalid(format!("ops is not a valid operation list: {error}")))?;
        if ops.is_empty() {
            return Err(invalid("ops must contain at least one operation"));
        }
        if ops.len() > MAX_OPS {
            return Err(invalid(format!(
                "ops contains {} operations; the limit is {MAX_OPS}",
                ops.len()
            )));
        }
        let source = self.artifacts.load(&hash)?;
        let filename = optional_filename(&args, || {
            let stem = std::path::Path::new(&source.filename)
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("document");
            let stem: String = stem.chars().take(MAX_FILENAME_CHARS - 12).collect();
            format!("{stem}-edited.docx")
        })?;

        let input = source.bytes;
        let (bytes, info, summaries) = tokio::task::spawn_blocking(move || {
            let mut document = load(&input)?;
            let summaries = apply_ops(&mut document, &ops)?;
            let bytes = save(&document)?;
            let info = inspect(&bytes)?;
            Ok::<_, SeraError>((bytes, info, summaries))
        })
        .await
        .map_err(|error| SeraError::Internal(format!("DOCX task failed: {error}")))??;

        let stored = self.artifacts.save(
            &bytes,
            &filename,
            artifact_meta(&filename, Some(hash.clone())),
        )?;
        self.store.record_audit(
            &context.actor,
            "docx.edited",
            Some("docx_edit"),
            context.task_id.as_deref(),
            None,
            None,
            None,
            Some(&json!({
                "artifact_hash": stored.hash,
                "source_artifact": hash,
                "operations": summaries.len(),
                "blocks": info.block_count,
                "images": info.images,
            })),
        )?;

        let result = json!({
            "artifact_hash": stored.hash,
            "source_artifact": hash,
            "filename": stored.filename,
            "blocks": info.block_count,
            "images": info.images,
            "operations": summaries,
        });
        Ok(ToolOutput {
            content: format!(
                "{}\nfilename: {}\nartifact: {}\nsource_artifact: {}\nblocks: {}\nimages: {}\n",
                summaries.join("\n"),
                stored.filename,
                stored.hash,
                hash,
                info.block_count,
                info.images
            ),
            data: Some(result),
            artifact_path: Some(format!("artifacts/{}", stored.filename)),
            artifact_hash: Some(stored.hash),
            filename: Some(stored.filename),
            destination: None,
        })
    }
}

pub struct DocxBuildTool {
    artifacts: Arc<ArtifactStore>,
    store: Store,
}

impl DocxBuildTool {
    pub fn new(artifacts: Arc<ArtifactStore>, store: Store) -> Self {
        Self { artifacts, store }
    }
}

#[async_trait]
impl Tool for DocxBuildTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "docx_build".into(),
            description:
                "Build a deterministic .docx from a bounded spec: a title, an optional subtitle, \
                 and blocks of kind heading {level, text}, paragraph {text}, bullets {items}, or \
                 table {header, rows}. Text only; images and hyperlinks are not expressible here. \
                 A spec that ends with a table gets one empty paragraph after it, as Word keeps."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "spec": {
                        "type": "object",
                        "description": "DocxSpec: title, optional subtitle, optional language (it|fr|en), blocks[{kind, level?, text?, items?, header?, rows?}]"
                    },
                    "filename": {"type": "string", "description": "Optional artifact filename ending in .docx"}
                },
                "required": ["spec"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        reject_unknown_keys(&args, &["spec", "filename"])?;
        let spec_value = args
            .get("spec")
            .ok_or_else(|| invalid("spec is required"))?;
        let spec: DocxSpec = serde_json::from_value(spec_value.clone())
            .map_err(|error| invalid(format!("spec is not a valid document spec: {error}")))?;
        let filename = optional_filename(&args, || default_filename(&spec.title))?;
        let built = spec.clone();
        let (bytes, info) = tokio::task::spawn_blocking(move || {
            let bytes = build(&built)?;
            let info = inspect(&bytes)?;
            Ok::<_, SeraError>((bytes, info))
        })
        .await
        .map_err(|error| SeraError::Internal(format!("DOCX task failed: {error}")))??;

        let language = spec.language.unwrap_or(Language::En);
        let meta = ArtifactMeta {
            title: spec.title.clone(),
            filename: filename.clone(),
            language,
            source_artifact: None,
            created_at: Utc::now(),
        };
        let stored = self.artifacts.save(&bytes, &filename, meta)?;
        self.store.record_audit(
            &context.actor,
            "docx.built",
            Some("docx_build"),
            context.task_id.as_deref(),
            None,
            None,
            None,
            Some(&json!({
                "artifact_hash": stored.hash,
                "blocks": info.block_count,
                "source_artifact": Value::Null,
            })),
        )?;

        let result = json!({
            "artifact_hash": stored.hash,
            "filename": stored.filename,
            "title": spec.title,
            "blocks": info.block_count,
            "tables": info.tables,
            "language": language.code(),
        });
        Ok(ToolOutput {
            content: format!(
                "title: {}\nblocks: {}\ntables: {}\nfilename: {}\nartifact: {}",
                spec.title, info.block_count, info.tables, stored.filename, stored.hash
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
    use std::collections::HashSet;
    use std::io::Write as _;

    use base64::Engine as _;
    use wordcraft_doc::props::ParaProps;

    use super::*;
    use crate::gateway::ToolRegistry;

    /// A 1x1 PNG. Synthetic; no owner asset.
    const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    fn png() -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode(PNG_1X1)
            .expect("the fixture is valid base64")
    }

    fn context(tool: &str) -> ToolContext {
        ToolContext {
            actor: "owner".into(),
            bot: "maker".into(),
            task_id: None,
            turn_id: None,
            conversation_id: None,
            allowed_tools: [tool.to_string()].into_iter().collect(),
            require_approval: HashSet::new(),
        }
    }

    fn spec() -> DocxSpec {
        DocxSpec {
            title: "Synthetic Report".into(),
            subtitle: Some("A fixture".into()),
            language: None,
            blocks: vec![
                DocxBlockSpec {
                    kind: "heading".into(),
                    level: Some(1),
                    text: Some("Findings".into()),
                    items: Vec::new(),
                    header: Vec::new(),
                    rows: Vec::new(),
                },
                DocxBlockSpec {
                    kind: "paragraph".into(),
                    level: None,
                    text: Some("Plain body paragraph.".into()),
                    items: Vec::new(),
                    header: Vec::new(),
                    rows: Vec::new(),
                },
                DocxBlockSpec {
                    kind: "bullets".into(),
                    level: None,
                    text: None,
                    items: vec!["First bullet".into(), "Second bullet".into()],
                    header: Vec::new(),
                    rows: Vec::new(),
                },
                DocxBlockSpec {
                    kind: "table".into(),
                    level: None,
                    text: None,
                    items: Vec::new(),
                    header: vec!["Header A".into(), "Header B".into()],
                    rows: vec![vec!["Value 1".into(), "Value 2".into()]],
                },
            ],
        }
    }

    fn ops(value: Value) -> Vec<DocxOp> {
        serde_json::from_value(value).expect("the fixture ops deserialise")
    }

    fn paragraph_block(text: &str) -> DocxBlockSpec {
        DocxBlockSpec {
            kind: "paragraph".into(),
            level: None,
            text: Some(text.into()),
            items: Vec::new(),
            header: Vec::new(),
            rows: Vec::new(),
        }
    }

    fn table_block() -> DocxBlockSpec {
        DocxBlockSpec {
            kind: "table".into(),
            level: None,
            text: None,
            items: Vec::new(),
            header: vec!["Header A".into(), "Header B".into()],
            rows: vec![vec!["Value 1".into(), "Value 2".into()]],
        }
    }

    /// A paragraph well past [`MAX_PREVIEW_CHARS`], with numbered words so a
    /// single changed word is detectable.
    fn long_text() -> String {
        let mut text = String::new();
        let mut index = 0usize;
        while text.chars().count() < 2000 {
            text.push_str(&format!("word{index} "));
            index += 1;
        }
        text
    }

    /// title, one long paragraph, one short paragraph, one table, and the empty
    /// paragraph a body keeps after a trailing table.
    fn long_spec(long: &str) -> DocxSpec {
        DocxSpec {
            title: "Long Fixture".into(),
            subtitle: None,
            language: None,
            blocks: vec![
                paragraph_block(long),
                paragraph_block("Short paragraph."),
                table_block(),
            ],
        }
    }

    /// A document with one image paragraph and one text paragraph.
    fn document_with_image() -> Document {
        let mut document = Document::new();
        let mut picture = Paragraph::new();
        picture
            .insert_object(
                0,
                InlineObject::Image {
                    media: "image1.png".into(),
                    w: 64.0,
                    h: 64.0,
                    alt: "synthetic pixel".into(),
                    float: Default::default(),
                    crop: [0.0; 4],
                    ole: None,
                },
                &CharProps::default(),
            )
            .expect("the fixture offset is valid");
        document.body = vec![
            para_block(Paragraph::with_text("Caption", CharProps::default())),
            para_block(picture),
        ];
        document.media.insert("image1.png".into(), Arc::new(png()));
        document
    }

    fn rewrite_package(
        bytes: &[u8],
        replaced: &[(&str, &[u8])],
        added: &[(&str, &[u8])],
    ) -> Vec<u8> {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .expect("the fixture is a zip package");
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for position in 0..archive.len() {
            let mut entry = archive.by_index(position).expect("a readable entry");
            let name = entry.name().to_owned();
            let mut buffer = Vec::new();
            entry.read_to_end(&mut buffer).expect("a readable part");
            if let Some((_, data)) = replaced.iter().find(|(target, _)| *target == name) {
                buffer = data.to_vec();
            }
            writer.start_file(name, options).expect("a writable entry");
            writer.write_all(&buffer).expect("a writable part");
        }
        for (name, data) in added {
            writer.start_file(*name, options).expect("a writable entry");
            writer.write_all(data).expect("a writable part");
        }
        writer
            .finish()
            .expect("the package is written")
            .into_inner()
    }

    #[test]
    fn build_reports_addressable_blocks() {
        let bytes = build(&spec()).expect("the fixture spec builds");
        let info = inspect(&bytes).expect("the built document inspects");
        // title, subtitle, heading, paragraph, two bullets, one table, and the
        // empty paragraph a body keeps after a trailing table.
        assert_eq!(info.block_count, 8);
        assert_eq!(info.tables, 1);
        assert_eq!(info.images, 0);
        assert!(!info.truncated);
        assert_eq!(info.blocks[0].style.as_deref(), Some("Title"));
        assert_eq!(info.blocks[2].style.as_deref(), Some("Heading1"));
        assert_eq!(info.blocks[3].text, "Plain body paragraph.");
        assert_eq!(info.blocks[4].list_level, Some(0));
        assert_eq!(info.blocks[6].kind, DocxBlockKind::Table);
        assert_eq!(info.blocks[6].rows, Some(2));
        assert_eq!(info.blocks[6].cols, Some(2));
        assert!(info.blocks[6].text.contains("Header A"));
        assert_eq!(info.blocks[7].text, "");
    }

    #[test]
    fn read_returns_a_long_block_in_full() {
        let long = long_text();
        let bytes = build(&long_spec(&long)).expect("the fixture spec builds");

        // The inspection preview is genuinely partial, and `chars` says by how much.
        let info = inspect(&bytes).expect("the built document inspects");
        assert_eq!(info.blocks[1].chars, long.chars().count());
        assert!(info.blocks[1].chars > MAX_PREVIEW_CHARS);
        assert!(
            info.blocks[1].text.chars().count() < info.blocks[1].chars,
            "the preview must be shorter than the block"
        );

        let text = read_blocks(&bytes, 1, 1).expect("the long block reads");
        assert_eq!(text.blocks.len(), 1);
        assert_eq!(text.blocks[0].index, 1);
        assert_eq!(text.blocks[0].kind, DocxBlockKind::Paragraph);
        assert_eq!(text.blocks[0].text, long, "the full text must come back");
        assert_eq!(text.blocks[0].chars, long.chars().count());
        assert!(!text.blocks[0].text_truncated);
    }

    #[test]
    fn read_returns_a_range_in_order_with_table_cells() {
        let long = long_text();
        let bytes = build(&long_spec(&long)).expect("the fixture spec builds");
        let text = read_blocks(&bytes, 1, 3).expect("the range reads");

        assert_eq!(text.from, 1);
        assert_eq!(text.to, 3);
        assert_eq!(text.block_count, 5);
        assert_eq!(text.blocks.len(), 3);
        assert_eq!(
            text.blocks
                .iter()
                .map(|block| block.index)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(text.blocks[0].text, long);
        assert_eq!(text.blocks[1].text, "Short paragraph.");
        assert_eq!(text.blocks[2].kind, DocxBlockKind::Table);
        assert_eq!(text.blocks[2].rows, Some(2));
        assert_eq!(text.blocks[2].cols, Some(2));
        let cells = text.blocks[2].cells.as_ref().expect("a table has cells");
        assert_eq!(cells[0], vec!["Header A", "Header B"]);
        assert_eq!(cells[1], vec!["Value 1", "Value 2"]);
        assert!(text.blocks[2].text.contains("Header A"));
    }

    #[test]
    fn read_rejects_a_range_over_the_block_cap_instead_of_truncating() {
        let many = DocxSpec {
            title: "Many Blocks".into(),
            subtitle: None,
            language: None,
            blocks: (0..60)
                .map(|index| paragraph_block(&format!("paragraph {index}")))
                .collect(),
        };
        let bytes = build(&many).expect("the fixture spec builds");
        // 61 blocks: the title plus 60 paragraphs.
        assert_eq!(inspect(&bytes).expect("inspect").block_count, 61);

        let error = read_blocks(&bytes, 0, 60).expect_err("a range over the cap is rejected");
        let message = error.to_string();
        assert!(message.contains("narrow the range"), "{message}");
        assert!(message.contains(&MAX_READ_BLOCKS.to_string()), "{message}");

        // Exactly the cap is allowed, and nothing is dropped from it.
        let text = read_blocks(&bytes, 0, MAX_READ_BLOCKS - 1).expect("a capped range reads");
        assert_eq!(text.blocks.len(), MAX_READ_BLOCKS);
        assert_eq!(text.blocks[0].text, "Many Blocks");
        assert_eq!(
            text.blocks[MAX_READ_BLOCKS - 1].text,
            format!("paragraph {}", MAX_READ_BLOCKS - 2)
        );
    }

    #[test]
    fn read_flags_a_block_that_exceeds_the_per_block_cap() {
        // A table's joined text can pass MAX_BLOCK_CHARS even when every cell is
        // under it. The read must say so instead of clipping silently, and the
        // cells must still carry their full text.
        let cell = "c".repeat(700);
        let spec = DocxSpec {
            title: "Wide Table".into(),
            subtitle: None,
            language: None,
            blocks: vec![DocxBlockSpec {
                kind: "table".into(),
                level: None,
                text: None,
                items: Vec::new(),
                header: (0..MAX_TABLE_COLUMNS).map(|_| cell.clone()).collect(),
                rows: Vec::new(),
            }],
        };
        let bytes = build(&spec).expect("the fixture spec builds");
        let info = inspect(&bytes).expect("the built document inspects");
        let table_index = info
            .blocks
            .iter()
            .position(|block| block.kind == DocxBlockKind::Table)
            .expect("the fixture has a table");

        let text = read_blocks(&bytes, table_index, table_index).expect("the table reads");
        let block = &text.blocks[0];
        assert!(
            block.chars > MAX_BLOCK_CHARS,
            "the joined text is over the cap: {}",
            block.chars
        );
        assert!(block.text_truncated, "clipping must be reported");
        assert_eq!(block.text.chars().count(), MAX_BLOCK_CHARS);
        let cells = block.cells.as_ref().expect("a table has cells");
        assert_eq!(cells[0].len(), MAX_TABLE_COLUMNS);
        assert!(
            cells[0].iter().all(|text| text.chars().count() == 700),
            "every cell keeps its full text"
        );
    }

    #[test]
    fn read_then_replace_keeps_every_other_character() {
        let long = long_text();
        let bytes = build(&long_spec(&long)).expect("the fixture spec builds");

        let read = read_blocks(&bytes, 1, 1).expect("the long block reads");
        let full = read.blocks[0].text.clone();
        let expected = full.replacen("word7 ", "WORD7 ", 1);
        assert_ne!(expected, full, "the fixture word must exist");
        // Same length, so a loss anywhere else would show up as a shorter block.
        assert_eq!(expected.chars().count(), full.chars().count());

        let mut document = load(&bytes).expect("the built document loads");
        apply_ops(
            &mut document,
            &ops(json!([{"op": "replace_text", "index": 1, "text": expected.clone()}])),
        )
        .expect("the replacement applies");
        let written = save(&document).expect("the edited document writes");

        let again = read_blocks(&written, 1, 1).expect("the edited block reads");
        assert_eq!(again.blocks[0].text, expected);
        assert_eq!(again.blocks[0].chars, expected.chars().count());

        // Nothing outside the edited block moved.
        let before = inspect(&bytes).expect("the source inspects");
        let after = inspect(&written).expect("the result inspects");
        assert_eq!(after.block_count, before.block_count);
        for index in [0usize, 2, 3, 4] {
            assert_eq!(
                after.blocks[index].text, before.blocks[index].text,
                "block {index} must be untouched"
            );
            assert_eq!(after.blocks[index].chars, before.blocks[index].chars);
        }
    }

    #[test]
    fn read_rejects_bad_selections() {
        let bytes = build(&spec()).expect("the fixture spec builds");
        assert!(read_blocks(&bytes, 3, 2).is_err(), "from > to");
        assert!(read_blocks(&bytes, 0, 999).is_err(), "out of range");
        assert!(read_blocks(&bytes, 999, 999).is_err(), "out of range");
    }

    #[tokio::test]
    async fn read_tool_is_read_only_and_validates_its_selection() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let artifacts = Arc::new(ArtifactStore::new(directory.path()).expect("an artifact store"));
        let long = long_text();
        let built = build(&long_spec(&long)).expect("the fixture spec builds");
        let hash = artifacts
            .save(
                &built,
                "long.docx",
                ArtifactMeta {
                    title: "Long Fixture".into(),
                    filename: "long.docx".into(),
                    language: Language::En,
                    source_artifact: None,
                    created_at: Utc::now(),
                },
            )
            .expect("the fixture is stored")
            .hash;
        let files_before = std::fs::read_dir(directory.path())
            .expect("the store is readable")
            .count();
        let tool = DocxReadTool::new(artifacts.clone());

        let output = tool
            .execute(json!({"artifact": hash, "block": 1}), &context("docx_read"))
            .await
            .expect("the tool reads a block");
        assert!(output.content.contains(&long), "the full text is returned");
        assert!(output.artifact_hash.is_none(), "a read writes no artifact");
        assert!(output.artifact_path.is_none());
        assert!(output.filename.is_none());
        assert!(output.destination.is_none());
        let data = output.data.expect("structured data");
        assert_eq!(data["blocks"][0]["chars"], json!(long.chars().count()));

        let files_after = std::fs::read_dir(directory.path())
            .expect("the store is readable")
            .count();
        assert_eq!(files_after, files_before, "a read stores nothing");

        // `block` and `from`/`to` are mutually exclusive.
        assert!(
            tool.execute(
                json!({"artifact": hash, "block": 1, "from": 1, "to": 2}),
                &context("docx_read"),
            )
            .await
            .is_err()
        );
        assert!(
            tool.execute(json!({"artifact": hash, "from": 1}), &context("docx_read"))
                .await
                .is_err()
        );
        assert!(
            tool.execute(json!({"artifact": hash}), &context("docx_read"))
                .await
                .is_err()
        );
        assert!(
            tool.execute(
                json!({"artifact": hash, "block": 1, "colour": "red"}),
                &context("docx_read"),
            )
            .await
            .is_err()
        );

        let range = tool
            .execute(
                json!({"artifact": hash, "from": 1, "to": 3}),
                &context("docx_read"),
            )
            .await
            .expect("the tool reads a range");
        assert!(range.content.contains("row 1: Value 1 | Value 2"));
    }

    #[test]
    fn edit_replaces_inserts_deletes_and_styles() {
        let bytes = build(&spec()).expect("the fixture spec builds");
        let mut document = load(&bytes).expect("the built document loads");
        let summaries = apply_ops(
            &mut document,
            &ops(json!([
                {"op": "replace_text", "index": 3, "text": "Edited body paragraph."},
                {"op": "insert_paragraph", "index": 4, "text": "Inserted line.", "style": "Heading2"},
                {"op": "apply_style", "index": 3, "style": "Quote"},
                {"op": "delete_block", "index": 5}
            ])),
        )
        .expect("the operations apply");
        assert_eq!(summaries.len(), 4);
        let written = save(&document).expect("the edited document writes");
        let info = inspect(&written).expect("the edited document inspects");
        assert_eq!(info.block_count, 8);
        assert_eq!(info.blocks[3].text, "Edited body paragraph.");
        assert_eq!(info.blocks[3].style.as_deref(), Some("Quote"));
        assert_eq!(info.blocks[4].text, "Inserted line.");
        assert_eq!(info.blocks[4].style.as_deref(), Some("Heading2"));
        // The first bullet was deleted; the second one remains.
        assert_eq!(info.blocks[5].text, "Second bullet");
    }

    #[test]
    fn edit_rejects_out_of_range_indices_and_unknown_fields() {
        let bytes = build(&spec()).expect("the fixture spec builds");
        let mut document = load(&bytes).expect("the built document loads");
        let error = apply_ops(
            &mut document,
            &ops(json!([{"op": "delete_block", "index": 99}])),
        )
        .expect_err("an out-of-range index is rejected");
        assert!(error.to_string().contains("out of range"), "{error}");

        let error = apply_ops(
            &mut document,
            &ops(json!([{"op": "insert_paragraph", "index": 99, "text": "x"}])),
        )
        .expect_err("an out-of-range insert is rejected");
        assert!(error.to_string().contains("out of range"), "{error}");

        let error = apply_ops(
            &mut document,
            &ops(json!([{"op": "replace_text", "index": 3}])),
        )
        .expect_err("a missing text is rejected");
        assert!(error.to_string().contains("`text` is required"), "{error}");

        let error = apply_ops(
            &mut document,
            &ops(json!([{"op": "delete_block", "index": 3, "text": "x"}])),
        )
        .expect_err("a field that does not belong is rejected");
        assert!(
            error.to_string().contains("does not take a `text` field"),
            "{error}"
        );

        assert!(
            serde_json::from_value::<Vec<DocxOp>>(json!([
                {"op": "delete_block", "index": 3, "colour": "red"}
            ]))
            .is_err(),
            "unknown operation fields must be rejected by serde"
        );

        let error = apply_ops(
            &mut document,
            &ops(json!([{"op": "apply_style", "index": 3, "style": "NoSuchStyle"}])),
        )
        .expect_err("an unknown style is rejected");
        assert!(error.to_string().contains("unknown style"), "{error}");

        let error = apply_ops(
            &mut document,
            &ops(json!([{"op": "replace_text", "index": 6, "text": "x"}])),
        )
        .expect_err("a table is not a paragraph");
        assert!(error.to_string().contains("is a table"), "{error}");
    }

    #[test]
    fn edit_keeps_inline_images_byte_for_byte() {
        let bytes = save(&document_with_image()).expect("the fixture writes");
        let mut document = load(&bytes).expect("the fixture loads");
        assert_eq!(inspect(&bytes).expect("inspect").images, 1);

        // Replace the caption, then replace the text of the image paragraph too:
        // the picture must survive both.
        apply_ops(
            &mut document,
            &ops(json!([
                {"op": "replace_text", "index": 0, "text": "New caption"},
                {"op": "replace_text", "index": 1, "text": "Text beside the picture"}
            ])),
        )
        .expect("the operations apply");
        let written = save(&document).expect("the edited document writes");

        let info = inspect(&written).expect("the edited document inspects");
        assert_eq!(info.images, 1, "the image must survive the edit");
        assert!(info.blocks[1].has_image);
        let reloaded = load(&written).expect("the edited document reloads");
        let media = reloaded
            .media
            .get("image1.png")
            .expect("the media part is kept");
        assert_eq!(media.as_slice(), png().as_slice());
    }

    #[test]
    fn packages_with_macros_or_remote_references_are_rejected() {
        let bytes = build(&spec()).expect("the fixture spec builds");

        let macro_package = rewrite_package(&bytes, &[], &[("word/vbaProject.bin", b"macro")]);
        let error = inspect(&macro_package).expect_err("a macro project is rejected");
        assert!(error.to_string().contains("VBA project"), "{error}");

        let activex = rewrite_package(&bytes, &[], &[("word/activeX/activeX1.xml", b"<x/>")]);
        let error = inspect(&activex).expect_err("an ActiveX part is rejected");
        assert!(error.to_string().contains("ActiveX"), "{error}");

        let remote = rewrite_package(
            &bytes,
            &[(
                "word/_rels/document.xml.rels",
                br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId99" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/attachedTemplate" Target="https://example.invalid/t.dotx" TargetMode="External"/>
</Relationships>"#,
            )],
            &[],
        );
        let error = inspect(&remote).expect_err("a remote template is rejected");
        assert!(error.to_string().contains("external reference"), "{error}");

        // The same relationship with single quotes and padding is still caught.
        let remote_spaced = rewrite_package(
            &bytes,
            &[(
                "word/_rels/document.xml.rels",
                br#"<Relationships><Relationship Id='rId98' Type='http://schemas.openxmlformats.org/officeDocument/2006/relationships/oleObject' Target = 'https://example.invalid/x.bin' TargetMode = 'External'/></Relationships>"#,
            )],
            &[],
        );
        let error =
            inspect(&remote_spaced).expect_err("a spaced external relationship is rejected");
        assert!(error.to_string().contains("external reference"), "{error}");

        // A hyperlink is an external relationship too, and it must stay usable.
        let linked = rewrite_package(
            &bytes,
            &[(
                "word/_rels/document.xml.rels",
                br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId99" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com/report" TargetMode="External"/>
</Relationships>"#,
            )],
            &[],
        );
        assert!(
            inspect(&linked).is_ok(),
            "a hyperlink must not be treated as remote content"
        );
    }

    #[test]
    fn encrypted_packages_are_rejected() {
        let bytes = build(&spec()).expect("the fixture spec builds");
        let encrypted =
            wordcraft_docx::encrypt(&bytes, "synthetic-password").expect("the fixture encrypts");
        let error = inspect(&encrypted).expect_err("an encrypted package is rejected");
        assert!(error.to_string().contains("password-protected"), "{error}");
    }

    #[test]
    fn oversized_and_malformed_input_is_rejected() {
        assert!(inspect(&[]).is_err());
        assert!(inspect(b"not a zip package").is_err());
        assert!(inspect(&vec![0u8; MAX_DOCX_BYTES + 1]).is_err());

        let mut document = Document::new();
        document.body = vec![para_block(Paragraph::with_text(
            &"x".repeat(MAX_BLOCK_CHARS + 1),
            CharProps::default(),
        ))];
        let bytes = save(&document).expect("the fixture writes");
        let error = inspect(&bytes).expect_err("an oversized block is rejected");
        assert!(error.to_string().contains("characters"), "{error}");
    }

    #[test]
    fn text_checks_reject_control_characters_and_the_object_placeholder() {
        assert!(check_block_text("text", "plain").is_ok());
        assert!(check_block_text("text", "two\nlines\tand a tab").is_ok());
        assert!(check_block_text("text", "bell\u{7}").is_err());
        assert!(check_block_text("text", "zero\u{200b}width").is_err());
        assert!(check_block_text("text", "object\u{fffc}").is_err());
        assert!(check_block_text("text", &"x".repeat(MAX_BLOCK_CHARS + 1)).is_err());
        assert!(check_style_name("style", "Heading 1").is_ok());
        assert!(check_style_name("style", "bad\nstyle").is_err());
    }

    #[test]
    fn build_rejects_bad_specs() {
        let mut bad = spec();
        bad.title = String::new();
        assert!(build(&bad).is_err());

        let mut bad = spec();
        bad.blocks.clear();
        assert!(build(&bad).is_err());

        let mut bad = spec();
        bad.blocks[0].kind = "image".into();
        assert!(build(&bad).is_err());

        let mut bad = spec();
        bad.blocks[0].level = Some(9);
        assert!(build(&bad).is_err());

        let mut bad = spec();
        bad.blocks[1].items = vec!["not allowed here".into()];
        assert!(build(&bad).is_err());

        let mut bad = spec();
        bad.blocks[3].rows = vec![vec!["x".into(); MAX_TABLE_COLUMNS + 1]];
        assert!(build(&bad).is_err());

        let mut bad = spec();
        bad.blocks[1].text = Some("bell\u{7}".into());
        assert!(build(&bad).is_err());

        let mut bad = spec();
        bad.blocks = (0..MAX_BUILD_BLOCKS + 1)
            .map(|index| DocxBlockSpec {
                kind: "paragraph".into(),
                level: None,
                text: Some(format!("paragraph {index}")),
                items: Vec::new(),
                header: Vec::new(),
                rows: Vec::new(),
            })
            .collect();
        assert!(build(&bad).is_err());

        assert!(
            serde_json::from_value::<DocxSpec>(json!({
                "title": "x",
                "blocks": [],
                "attachments": ["a.bin"]
            }))
            .is_err(),
            "unknown spec fields must be rejected by serde"
        );
    }

    #[test]
    fn filenames_are_bounded_and_typed() {
        assert_eq!(
            default_filename("Synthetic Report"),
            "synthetic-report.docx"
        );
        assert_eq!(default_filename("***"), "document.docx");
        assert!(check_filename("report.docx").is_ok());
        assert!(check_filename("report.pdf").is_err());
        assert!(check_filename("../report.docx").is_err());
        assert!(check_filename("a\\b.docx").is_err());
        assert!(check_filename(&format!("{}.docx", "x".repeat(MAX_FILENAME_CHARS))).is_err());
    }

    #[tokio::test]
    async fn tools_build_inspect_and_edit_through_the_artifact_store() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let artifacts = Arc::new(ArtifactStore::new(directory.path()).expect("an artifact store"));
        let store = Store::open_in_memory(384).expect("an in-memory store");
        let mut registry = ToolRegistry::default();
        registry
            .register(DocxBuildTool::new(artifacts.clone(), store.clone()))
            .expect("the build tool registers");
        registry
            .register(DocxInspectTool::new(artifacts.clone()))
            .expect("the inspect tool registers");
        registry
            .register(DocxEditTool::new(artifacts.clone(), store))
            .expect("the edit tool registers");
        assert!(registry.is_registered("docx_build"));
        assert!(registry.is_registered("docx_inspect"));
        assert!(registry.is_registered("docx_edit"));

        let built = registry
            .get("docx_build")
            .expect("the build tool")
            .execute(json!({"spec": spec()}), &context("docx_build"))
            .await
            .expect("the fixture builds through the tool");
        let built_hash = built.artifact_hash.expect("a built artifact hash");
        assert_eq!(
            built.filename.as_deref(),
            Some("synthetic-report.docx"),
            "the filename derives from the title"
        );

        let inspected = registry
            .get("docx_inspect")
            .expect("the inspect tool")
            .execute(json!({"artifact": built_hash}), &context("docx_inspect"))
            .await
            .expect("the built document inspects through the tool");
        assert!(inspected.content.contains("blocks: 8"));
        assert!(inspected.content.contains("[3] paragraph"));

        let edited = registry
            .get("docx_edit")
            .expect("the edit tool")
            .execute(
                json!({
                    "artifact": built_hash,
                    "ops": [{"op": "replace_text", "index": 3, "text": "Edited through the tool."}]
                }),
                &context("docx_edit"),
            )
            .await
            .expect("the edit runs through the tool");
        let edited_hash = edited.artifact_hash.expect("an edited artifact hash");
        assert_ne!(edited_hash, built_hash);
        let stored = artifacts.load(&edited_hash).expect("the edited artifact");
        assert_eq!(
            stored.meta.source_artifact.as_deref(),
            Some(built_hash.as_str()),
            "an edit records its source artifact"
        );
        let info = inspect(&stored.bytes).expect("the edited document inspects");
        assert_eq!(info.blocks[3].text, "Edited through the tool.");
    }

    #[tokio::test]
    async fn tools_reject_unknown_arguments() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let artifacts = Arc::new(ArtifactStore::new(directory.path()).expect("an artifact store"));
        let store = Store::open_in_memory(384).expect("an in-memory store");
        let tool = DocxBuildTool::new(artifacts, store);
        let args = json!({"spec": spec(), "template": "x.docx"});
        assert!(tool.execute(args, &context("docx_build")).await.is_err());
    }

    #[test]
    fn a_paragraph_keeps_its_own_formatting_when_its_text_is_replaced() {
        let mut paragraph = Paragraph::with_text(
            "bold text",
            CharProps {
                bold: Some(true),
                ..CharProps::default()
            },
        );
        paragraph.props = ParaProps {
            style: Some("Heading1".into()),
            ..ParaProps::default()
        };
        replace_paragraph_text(&mut paragraph, "new text").expect("the text is replaced");
        assert_eq!(paragraph.plain_text(), "new text");
        assert_eq!(paragraph.props.style.as_deref(), Some("Heading1"));
        assert_eq!(paragraph.runs.len(), 1);
        assert_eq!(paragraph.runs[0].props.bold, Some(true));
    }
}
