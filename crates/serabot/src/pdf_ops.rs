//! Bounded, deterministic PDF page operations.
//!
//! Every entry point takes a byte slice: files arrive through the configured
//! inbox or the [`ArtifactStore`], never as model-chosen paths, and results go
//! back through the store so an approval can bind the artifact hash.
//!
//! Documents that carry active content are rejected outright instead of being
//! sanitised. A partially rewritten PDF is not a trust decision this module can
//! make, so `/JavaScript`, `/JS`, `/EmbeddedFile(s)`, `/Launch`, `/RichMedia`,
//! and `/AA` are detected in the raw bytes *and* in the parsed object graph
//! (which includes objects that were packed into compressed object streams) and
//! reported as a validation error naming what was found. Nothing is ever
//! executed or evaluated.
//!
//! Sizes are bounded: 64 MiB of input, 2000 pages, 32 merge inputs, and a
//! depth limit on every walk over the page tree or the object graph.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::domain::Language;
use crate::error::{Result, SeraError};
use crate::gateway::{Tool, ToolContext, ToolOutput, ToolSpec};
use crate::maker::{ArtifactMeta, ArtifactStore};
use crate::store::Store;

/// Hard ceiling for a single input document and for a merged input set.
pub const MAX_PDF_BYTES: usize = 64 * 1024 * 1024;
/// Hard ceiling for the page count of one document and of a merged result.
pub const MAX_PAGES: usize = 2000;
/// Hard ceiling for the number of documents in one merge.
pub const MAX_MERGE_DOCUMENTS: usize = 32;

/// Page tree nodes are allowed to nest this deep before we call the file broken.
const MAX_PAGE_TREE_DEPTH: usize = 64;
/// Recursion guard for object-graph walks and for the cross-document copier.
const MAX_OBJECT_DEPTH: usize = 64;

/// Active-content names. `/EmbeddedFiles` is the name tree that holds embedded
/// files, so it is treated as an embedded-file marker too.
const DANGEROUS_NAMES: [&str; 7] = [
    "/JavaScript",
    "/JS",
    "/EmbeddedFile",
    "/EmbeddedFiles",
    "/Launch",
    "/RichMedia",
    "/AA",
];

/// Page attributes that may be inherited from the page tree.
const INHERITABLE: [&[u8]; 4] = [b"MediaBox", b"CropBox", b"Resources", b"Rotate"];

/// Document information dictionary keys this module strips.
pub const METADATA_KEYS: [&str; 6] = [
    "Producer", "Creator", "Author", "Title", "Subject", "Keywords",
];

fn invalid(message: impl Into<String>) -> SeraError {
    SeraError::Validation(message.into())
}

/// One page as reported by [`inspect`].
///
/// `width_pt`/`height_pt` are the unrotated `MediaBox` extent in PostScript
/// points; `rotation` is the effective (inherited) rotation normalised to
/// 0/90/180/270. A page without a usable `MediaBox` reports 0x0 rather than
/// failing the whole inspection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PdfPageInfo {
    pub page: u32,
    pub width_pt: f32,
    pub height_pt: f32,
    pub rotation: i64,
}

/// Bounded structural summary of a document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PdfInfo {
    pub version: String,
    pub page_count: usize,
    pub pages: Vec<PdfPageInfo>,
    pub has_scripts: bool,
    pub has_embedded_files: bool,
    pub has_actions: bool,
    /// True when any active-content marker was found. Mutating operations
    /// refuse such documents; `inspect` only reports them.
    pub dangerous: bool,
    pub findings: Vec<String>,
}

/// Inclusive, 1-based page range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageRange {
    pub start: u32,
    pub end: u32,
}

impl PageRange {
    pub fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }
}

// ---------------------------------------------------------------------------
// Loading and validation
// ---------------------------------------------------------------------------

fn check_input_size(len: usize) -> Result<()> {
    if len == 0 {
        return Err(invalid("PDF input is empty"));
    }
    if len > MAX_PDF_BYTES {
        return Err(invalid(format!(
            "PDF input is {len} bytes; the limit is {MAX_PDF_BYTES} bytes (64 MiB)"
        )));
    }
    Ok(())
}

/// Parse bytes into a document. Rejects oversized, malformed, and encrypted
/// input; never mutates anything.
fn parse(bytes: &[u8]) -> Result<Document> {
    check_input_size(bytes.len())?;
    let mut document = catch_unwind(AssertUnwindSafe(|| Document::load_mem(bytes)))
        .map_err(|_| invalid("malformed PDF: the parser aborted; the file is not a safe PDF"))?
        .map_err(|error| invalid(format!("malformed PDF: {error}")))?;
    if catch_unwind(AssertUnwindSafe(|| document.is_encrypted())).unwrap_or(true) {
        return Err(invalid(
            "PDF is encrypted; decrypt it before inspecting or editing it",
        ));
    }
    // A rewritten file must not point at a previous cross-reference section.
    document.trailer.remove(b"Prev");
    document.trailer.remove(b"XRefStm");
    Ok(document)
}

/// Parse and fully validate a document before any mutation.
fn open(bytes: &[u8]) -> Result<(Document, Vec<ObjectId>)> {
    let document = parse(bytes)?;
    let findings = scan(bytes, &document);
    if !findings.is_empty() {
        return Err(invalid(format!(
            "PDF contains active content ({}); refusing to operate on it",
            findings.join(", ")
        )));
    }
    let pages = page_ids(&document)?;
    Ok((document, pages))
}

/// Page object ids in document order, bounded by [`MAX_PAGES`].
fn page_ids(document: &Document) -> Result<Vec<ObjectId>> {
    let pages: Vec<ObjectId> = catch_unwind(AssertUnwindSafe(|| {
        document.get_pages().into_values().collect()
    }))
    .map_err(|_| invalid("malformed PDF: page tree traversal aborted"))?;
    if pages.is_empty() {
        return Err(invalid("malformed PDF: no pages were found"));
    }
    if pages.len() > MAX_PAGES {
        return Err(invalid(format!(
            "PDF has {} pages; the limit is {MAX_PAGES}",
            pages.len()
        )));
    }
    Ok(pages)
}

// ---------------------------------------------------------------------------
// Active-content scan
// ---------------------------------------------------------------------------

/// Names found in the raw bytes and in the parsed object graph.
fn scan(bytes: &[u8], document: &Document) -> Vec<String> {
    let mut found: BTreeSet<&'static str> = BTreeSet::new();
    for name in DANGEROUS_NAMES {
        if contains_name(bytes, name.as_bytes()) {
            found.insert(name);
        }
    }
    for object in document.objects.values() {
        scan_object(object, &mut found, 0);
    }
    for (_, value) in document.trailer.iter() {
        scan_object(value, &mut found, 0);
    }
    found.into_iter().map(str::to_owned).collect()
}

fn contains_name(haystack: &[u8], token: &[u8]) -> bool {
    if token.is_empty() || haystack.len() < token.len() {
        return false;
    }
    haystack
        .windows(token.len())
        .enumerate()
        .any(|(offset, window)| {
            window[0] == b'/'
                && window.eq_ignore_ascii_case(token)
                && haystack
                    .get(offset + token.len())
                    .is_none_or(|byte| !is_name_byte(*byte))
        })
}

/// Regular characters inside a PDF name; anything else terminates the name.
fn is_name_byte(byte: u8) -> bool {
    !matches!(
        byte,
        0 | b'\t'
            | b'\n'
            | 0x0c
            | b'\r'
            | b' '
            | b'('
            | b')'
            | b'<'
            | b'>'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'/'
            | b'%'
    )
}

fn scan_object(object: &Object, found: &mut BTreeSet<&'static str>, depth: usize) {
    if depth > MAX_OBJECT_DEPTH {
        return;
    }
    match object {
        Object::Name(name) => note_name(name, found),
        Object::Array(values) => {
            for value in values {
                scan_object(value, found, depth + 1);
            }
        }
        Object::Dictionary(dict) => {
            for (key, value) in dict.iter() {
                note_name(key, found);
                scan_object(value, found, depth + 1);
            }
        }
        Object::Stream(stream) => {
            for (key, value) in stream.dict.iter() {
                note_name(key, found);
                scan_object(value, found, depth + 1);
            }
        }
        _ => {}
    }
}

fn note_name(name: &[u8], found: &mut BTreeSet<&'static str>) {
    for token in DANGEROUS_NAMES {
        if name.eq_ignore_ascii_case(token.as_bytes()) {
            found.insert(token);
        }
    }
}

// ---------------------------------------------------------------------------
// Page tree helpers
// ---------------------------------------------------------------------------

fn normalize_rotation(degrees: i64) -> i64 {
    degrees.rem_euclid(360)
}

/// Page attributes visible to `page_id`, walking `/Parent` at most
/// [`MAX_PAGE_TREE_DEPTH`] times. The page's own entries win.
fn inherited(document: &Document, page_id: ObjectId) -> Result<BTreeMap<Vec<u8>, Object>> {
    let mut found: BTreeMap<Vec<u8>, Object> = BTreeMap::new();
    let mut current = Some(page_id);
    let mut depth = 0usize;
    while let Some(id) = current {
        depth += 1;
        if depth > MAX_PAGE_TREE_DEPTH {
            return Err(invalid("malformed PDF: the page tree is too deep"));
        }
        let dict = document
            .get_dictionary(id)
            .map_err(|_| invalid("malformed PDF: a page tree node is not a dictionary"))?;
        for key in INHERITABLE {
            if found.contains_key(key) {
                continue;
            }
            if let Ok(value) = dict.get(key) {
                found.insert(key.to_vec(), value.clone());
            }
        }
        current = dict.get(b"Parent").ok().and_then(|v| v.as_reference().ok());
    }
    Ok(found)
}

fn rotation_of(document: &Document, page_id: ObjectId) -> Result<i64> {
    let attributes = inherited(document, page_id)?;
    Ok(match attributes.get(&b"Rotate"[..]) {
        Some(value) => {
            let value = dereference(document, value);
            normalize_rotation(
                value
                    .as_i64()
                    .or_else(|_| value.as_float().map(|number| number as i64))
                    .unwrap_or(0),
            )
        }
        None => 0,
    })
}

/// Follow at most a few reference hops; a page attribute is normally either
/// direct or a single indirect object.
fn dereference<'a>(document: &'a Document, object: &'a Object) -> &'a Object {
    let mut current = object;
    for _ in 0..8 {
        let Object::Reference(id) = current else {
            break;
        };
        match document.objects.get(id) {
            Some(value) => current = value,
            None => break,
        }
    }
    current
}

fn media_box(document: &Document, page_id: ObjectId) -> Result<(f32, f32)> {
    let attributes = inherited(document, page_id)?;
    let Some(value) = attributes.get(&b"MediaBox"[..]) else {
        return Ok((0.0, 0.0));
    };
    let Object::Array(values) = dereference(document, value) else {
        return Ok((0.0, 0.0));
    };
    if values.len() != 4 {
        return Ok((0.0, 0.0));
    }
    let mut numbers = [0.0f32; 4];
    for (index, value) in values.iter().enumerate() {
        numbers[index] = dereference(document, value).as_float().unwrap_or(0.0);
    }
    Ok((
        (numbers[2] - numbers[0]).abs(),
        (numbers[3] - numbers[1]).abs(),
    ))
}

/// The root `/Pages` node, creating an indirect object for it if the catalog
/// holds a direct dictionary.
fn pages_root(document: &mut Document) -> Result<ObjectId> {
    let catalog_id = document
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .map_err(|_| invalid("malformed PDF: the trailer has no document catalog"))?;
    let catalog = document
        .get_dictionary(catalog_id)
        .map_err(|_| invalid("malformed PDF: the document catalog is not a dictionary"))?;
    match catalog.get(b"Pages") {
        Ok(Object::Reference(id)) => Ok(*id),
        Ok(Object::Dictionary(dict)) => {
            let dict = dict.clone();
            let id = document.add_object(dict);
            document
                .get_dictionary_mut(catalog_id)
                .map_err(|_| invalid("malformed PDF: the document catalog is not a dictionary"))?
                .set("Pages", Object::Reference(id));
            Ok(id)
        }
        _ => Err(invalid("malformed PDF: the catalog has no page tree")),
    }
}

/// Rewrite the page tree so it contains exactly `order`, in that order.
///
/// Inherited page attributes are materialised onto each page first, because
/// intermediate `/Pages` nodes become unreachable once the tree is flattened.
fn rewrite_pages(document: &mut Document, order: &[ObjectId]) -> Result<()> {
    let root = pages_root(document)?;
    for page_id in order {
        let attributes = inherited(document, *page_id)?;
        let page = document
            .get_object_mut(*page_id)
            .map_err(|_| invalid("malformed PDF: a page object is missing"))?
            .as_dict_mut()
            .map_err(|_| invalid("malformed PDF: a page object is not a dictionary"))?;
        for (key, value) in attributes {
            if !page.has(&key) {
                page.set(key, value);
            }
        }
        page.set("Type", "Page");
        page.set("Parent", Object::Reference(root));
    }
    let kids: Vec<Object> = order.iter().map(|id| Object::Reference(*id)).collect();
    let root_dict = document
        .get_object_mut(root)
        .map_err(|_| invalid("malformed PDF: the page tree root is missing"))?
        .as_dict_mut()
        .map_err(|_| invalid("malformed PDF: the page tree root is not a dictionary"))?;
    root_dict.set("Type", "Pages");
    root_dict.set("Kids", Object::Array(kids));
    root_dict.set("Count", Object::Integer(order.len() as i64));
    root_dict.remove(b"Parent");
    Ok(())
}

/// Replace references to objects that no longer exist with `Null`, so deleting
/// pages cannot leave dangling destinations in outlines or form dictionaries.
fn clean_dangling_references(document: &mut Document) {
    let existing: HashSet<ObjectId> = document.objects.keys().copied().collect();
    for object in document.objects.values_mut() {
        replace_dangling(object, &existing, 0);
    }
    for (_, value) in document.trailer.iter_mut() {
        replace_dangling(value, &existing, 0);
    }
}

fn replace_dangling(object: &mut Object, existing: &HashSet<ObjectId>, depth: usize) {
    if depth > MAX_OBJECT_DEPTH {
        return;
    }
    match object {
        Object::Reference(id) => {
            if !existing.contains(id) {
                *object = Object::Null;
            }
        }
        Object::Array(values) => {
            for value in values {
                replace_dangling(value, existing, depth + 1);
            }
        }
        Object::Dictionary(dict) => {
            for (_, value) in dict.iter_mut() {
                replace_dangling(value, existing, depth + 1);
            }
        }
        Object::Stream(stream) => {
            for (_, value) in stream.dict.iter_mut() {
                replace_dangling(value, existing, depth + 1);
            }
        }
        _ => {}
    }
}

/// Drop every object that is not reachable from the trailer.
///
/// `Document::prune_objects` does the same thing with a `Vec` membership test,
/// which is quadratic; this version is linear and does not clone stream bodies.
fn prune_unreachable(document: &mut Document) {
    let mut reachable: HashSet<ObjectId> = HashSet::new();
    let mut pending: Vec<ObjectId> = Vec::new();
    for (_, value) in document.trailer.iter() {
        collect_references(value, &mut pending);
    }
    while let Some(id) = pending.pop() {
        if !reachable.insert(id) {
            continue;
        }
        if let Some(object) = document.objects.get(&id) {
            collect_references(object, &mut pending);
        }
    }
    document.objects.retain(|id, _| reachable.contains(id));
}

fn collect_references(object: &Object, out: &mut Vec<ObjectId>) {
    let mut pending: Vec<&Object> = vec![object];
    while let Some(current) = pending.pop() {
        match current {
            Object::Reference(id) => out.push(*id),
            Object::Array(values) => pending.extend(values.iter()),
            Object::Dictionary(dict) => pending.extend(dict.iter().map(|(_, value)| value)),
            Object::Stream(stream) => {
                pending.extend(stream.dict.iter().map(|(_, value)| value));
            }
            _ => {}
        }
    }
}

fn save(document: &mut Document) -> Result<Vec<u8>> {
    let mut buffer = Vec::new();
    catch_unwind(AssertUnwindSafe(|| document.save_to(&mut buffer)))
        .map_err(|_| SeraError::Internal("the PDF writer aborted".into()))?
        .map_err(|error| SeraError::Internal(format!("the PDF writer failed: {error}")))?;
    if buffer.is_empty() || buffer.len() > MAX_PDF_BYTES {
        return Err(SeraError::Internal(
            "the rewritten PDF is not a valid size".into(),
        ));
    }
    Ok(buffer)
}

/// Shared tail for every in-place rewrite: clean, prune, verify, serialise.
fn finish(mut document: Document, expected_pages: usize) -> Result<Vec<u8>> {
    clean_dangling_references(&mut document);
    prune_unreachable(&mut document);
    let pages = catch_unwind(AssertUnwindSafe(|| document.get_pages()))
        .map_err(|_| SeraError::Internal("page tree verification aborted".into()))?;
    if pages.len() != expected_pages {
        return Err(SeraError::Internal(format!(
            "the page rewrite produced {} pages instead of {expected_pages}",
            pages.len()
        )));
    }
    save(&mut document)
}

fn validate_pages(pages: &[u32], count: usize, label: &str) -> Result<Vec<u32>> {
    if pages.is_empty() {
        return Err(invalid(format!("{label} must list at least one page")));
    }
    if pages.len() > count {
        return Err(invalid(format!(
            "{label} lists {} pages but the document has {count}",
            pages.len()
        )));
    }
    let mut seen = HashSet::new();
    for page in pages {
        if *page == 0 || *page as usize > count {
            return Err(invalid(format!(
                "{label} contains page {page}, but valid pages are 1..={count}"
            )));
        }
        if !seen.insert(*page) {
            return Err(invalid(format!("{label} lists page {page} more than once")));
        }
    }
    Ok(pages.to_vec())
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

/// Report page count, per-page size and rotation, and active-content markers.
///
/// This is read-only and never rejects a dangerous document; it reports it so
/// the owner can see why an edit would be refused.
pub fn inspect(bytes: &[u8]) -> Result<PdfInfo> {
    let document = parse(bytes)?;
    let findings = scan(bytes, &document);
    let pages = page_ids(&document)?;
    let mut page_info = Vec::with_capacity(pages.len());
    for (index, page_id) in pages.iter().enumerate() {
        let (width, height) = media_box(&document, *page_id)?;
        page_info.push(PdfPageInfo {
            page: (index + 1) as u32,
            width_pt: width,
            height_pt: height,
            rotation: rotation_of(&document, *page_id)?,
        });
    }
    let has_scripts = findings
        .iter()
        .any(|name| name == "/JavaScript" || name == "/JS");
    let has_embedded_files = findings
        .iter()
        .any(|name| name == "/EmbeddedFile" || name == "/EmbeddedFiles");
    let has_actions = findings
        .iter()
        .any(|name| name == "/Launch" || name == "/RichMedia" || name == "/AA");
    Ok(PdfInfo {
        version: document.version.clone(),
        page_count: page_info.len(),
        pages: page_info,
        has_scripts,
        has_embedded_files,
        has_actions,
        dangerous: !findings.is_empty(),
        findings,
    })
}

/// Add `degrees` to the rotation of each listed page.
///
/// `degrees` must be a non-zero multiple of 90; negative values are normalised.
pub fn rotate(bytes: &[u8], pages: &[u32], degrees: i64) -> Result<Vec<u8>> {
    let delta = normalize_rotation(degrees);
    if delta == 0 || delta % 90 != 0 {
        return Err(invalid(
            "rotation must be a non-zero multiple of 90 degrees",
        ));
    }
    let (mut document, ids) = open(bytes)?;
    let selected = validate_pages(pages, ids.len(), "pages")?;
    for page in selected {
        let page_id = ids[(page - 1) as usize];
        let next = normalize_rotation(rotation_of(&document, page_id)? + delta);
        document
            .get_object_mut(page_id)
            .map_err(|_| invalid("malformed PDF: a page object is missing"))?
            .as_dict_mut()
            .map_err(|_| invalid("malformed PDF: a page object is not a dictionary"))?
            .set("Rotate", Object::Integer(next));
    }
    finish(document, ids.len())
}

/// Remove the listed pages. At least one page must remain.
pub fn delete(bytes: &[u8], pages: &[u32]) -> Result<Vec<u8>> {
    let (mut document, ids) = open(bytes)?;
    let selected = validate_pages(pages, ids.len(), "pages")?;
    if selected.len() == ids.len() {
        return Err(invalid("deleting every page would leave an empty document"));
    }
    let removed: HashSet<u32> = selected.into_iter().collect();
    let order: Vec<ObjectId> = ids
        .iter()
        .enumerate()
        .filter(|(index, _)| !removed.contains(&(*index as u32 + 1)))
        .map(|(_, id)| *id)
        .collect();
    rewrite_pages(&mut document, &order)?;
    finish(document, order.len())
}

/// Rearrange all pages into `order`, which must be a permutation of `1..=n`.
pub fn reorder(bytes: &[u8], order: &[u32]) -> Result<Vec<u8>> {
    let (mut document, ids) = open(bytes)?;
    if order.len() != ids.len() {
        return Err(invalid(format!(
            "order must list all {} pages exactly once",
            ids.len()
        )));
    }
    let selected = validate_pages(order, ids.len(), "order")?;
    let ordered: Vec<ObjectId> = selected
        .iter()
        .map(|page| ids[(page - 1) as usize])
        .collect();
    rewrite_pages(&mut document, &ordered)?;
    finish(document, ordered.len())
}

/// Copy the inclusive page range into a new document.
pub fn extract(bytes: &[u8], range: PageRange) -> Result<Vec<u8>> {
    let (mut document, ids) = open(bytes)?;
    let count = ids.len();
    if range.start == 0 || range.end < range.start || range.end as usize > count {
        return Err(invalid(format!(
            "range {}-{} is not inside 1..={count}",
            range.start, range.end
        )));
    }
    let ordered: Vec<ObjectId> = ids[(range.start - 1) as usize..range.end as usize].to_vec();
    rewrite_pages(&mut document, &ordered)?;
    finish(document, ordered.len())
}

/// Remove the document information dictionary and any XMP metadata streams.
pub fn strip_metadata(bytes: &[u8]) -> Result<Vec<u8>> {
    let (mut document, ids) = open(bytes)?;
    document.trailer.remove(b"Info");
    if let Ok(catalog) = document.catalog_mut() {
        catalog.remove(b"Metadata");
        catalog.remove(b"PieceInfo");
    }
    for page_id in &ids {
        if let Ok(page) = document
            .get_object_mut(*page_id)
            .and_then(Object::as_dict_mut)
        {
            page.remove(b"Metadata");
            page.remove(b"PieceInfo");
        }
    }
    finish(document, ids.len())
}

fn pdf_minor(version: &str) -> u32 {
    version
        .split('.')
        .nth(1)
        .and_then(|minor| minor.trim().parse::<u32>().ok())
        .unwrap_or(4)
}

/// Concatenate documents in the given order.
///
/// Pages are imported into a fresh document, so resources, fonts, and content
/// streams are copied per source. Inherited page attributes are materialised
/// before copying. Interactive annotations are dropped: their destinations and
/// form fields reference page trees across documents, and rewriting them would
/// be a much larger, riskier transformation than the owner asked for.
pub fn merge(documents: &[&[u8]]) -> Result<Vec<u8>> {
    if documents.is_empty() {
        return Err(invalid("merge needs at least one document"));
    }
    if documents.len() > MAX_MERGE_DOCUMENTS {
        return Err(invalid(format!(
            "merge accepts at most {MAX_MERGE_DOCUMENTS} documents"
        )));
    }
    let mut total_bytes = 0usize;
    let mut sources = Vec::with_capacity(documents.len());
    for bytes in documents {
        check_input_size(bytes.len())?;
        total_bytes = total_bytes.saturating_add(bytes.len());
        if total_bytes > MAX_PDF_BYTES {
            return Err(invalid("the merged input exceeds 64 MiB"));
        }
        sources.push(open(bytes)?);
    }
    let page_total: usize = sources.iter().map(|(_, ids)| ids.len()).sum();
    if page_total > MAX_PAGES {
        return Err(invalid(format!(
            "the merged document would have {page_total} pages; the limit is {MAX_PAGES}"
        )));
    }

    let version = sources
        .iter()
        .map(|(document, _)| document.version.clone())
        .max_by_key(|version| pdf_minor(version))
        .unwrap_or_else(|| "1.4".to_owned());

    let mut target = Document::with_version(version);
    let pages_root = target.new_object_id();
    let catalog = target.add_object(Dictionary::new());
    target.trailer.set("Root", Object::Reference(catalog));

    let mut kids: Vec<Object> = Vec::with_capacity(page_total);
    for (source, ids) in &sources {
        let mut blocked: HashSet<ObjectId> = ids.iter().copied().collect();
        if let Ok(root) = source.trailer.get(b"Root").and_then(Object::as_reference) {
            blocked.insert(root);
            if let Ok(pages) = source
                .get_dictionary(root)
                .and_then(|catalog| catalog.get(b"Pages").and_then(Object::as_reference))
            {
                blocked.insert(pages);
            }
        }
        let mut copier = Copier {
            source,
            map: HashMap::new(),
            blocked,
        };
        for page_id in ids {
            let mut page = source
                .get_dictionary(*page_id)
                .map_err(|_| invalid("malformed PDF: a page object is not a dictionary"))?
                .clone();
            page.remove(b"Parent");
            page.remove(b"Annots");
            page.remove(b"B");
            for (key, value) in inherited(source, *page_id)? {
                if !page.has(&key) {
                    page.set(key, value);
                }
            }
            let mut copied = copier.copy_dictionary(&mut target, &page, 0)?;
            copied.set("Type", "Page");
            copied.set("Parent", Object::Reference(pages_root));
            kids.push(Object::Reference(target.add_object(copied)));
        }
    }

    let mut root = Dictionary::new();
    root.set("Type", "Pages");
    root.set("Kids", Object::Array(kids));
    root.set("Count", Object::Integer(page_total as i64));
    target.set_object(pages_root, root);

    let mut catalog_dict = Dictionary::new();
    catalog_dict.set("Type", "Catalog");
    catalog_dict.set("Pages", Object::Reference(pages_root));
    target.set_object(catalog, catalog_dict);

    finish(target, page_total)
}

/// Deep-copies one page's object graph from a source document into the target.
struct Copier<'a> {
    source: &'a Document,
    map: HashMap<ObjectId, ObjectId>,
    /// Objects that must not be pulled in: page objects, the catalog, and the
    /// source page tree root.
    blocked: HashSet<ObjectId>,
}

impl Copier<'_> {
    fn copy(&mut self, target: &mut Document, object: &Object, depth: usize) -> Result<Object> {
        if depth > MAX_OBJECT_DEPTH {
            return Err(invalid(
                "malformed PDF: the object graph is too deep to copy",
            ));
        }
        Ok(match object {
            Object::Array(values) => {
                let mut copied = Vec::with_capacity(values.len());
                for value in values {
                    copied.push(self.copy(target, value, depth + 1)?);
                }
                Object::Array(copied)
            }
            Object::Dictionary(dict) => {
                Object::Dictionary(self.copy_dictionary(target, dict, depth + 1)?)
            }
            Object::Stream(stream) => {
                let mut dict = Dictionary::new();
                for (key, value) in stream.dict.iter() {
                    // `Stream::new` recomputes /Length from the content.
                    if key.as_slice() == b"Length" {
                        continue;
                    }
                    dict.set(key.clone(), self.copy(target, value, depth + 1)?);
                }
                Object::Stream(Stream::new(dict, stream.content.clone()))
            }
            Object::Reference(id) => self.copy_indirect(target, *id, depth + 1)?,
            other => other.clone(),
        })
    }

    fn copy_dictionary(
        &mut self,
        target: &mut Document,
        dict: &Dictionary,
        depth: usize,
    ) -> Result<Dictionary> {
        let mut copied = Dictionary::new();
        for (key, value) in dict.iter() {
            copied.set(key.clone(), self.copy(target, value, depth + 1)?);
        }
        Ok(copied)
    }

    fn copy_indirect(
        &mut self,
        target: &mut Document,
        id: ObjectId,
        depth: usize,
    ) -> Result<Object> {
        if let Some(existing) = self.map.get(&id) {
            return Ok(Object::Reference(*existing));
        }
        if self.blocked.contains(&id) {
            return Ok(Object::Null);
        }
        let source = self.source;
        let Some(object) = source.objects.get(&id) else {
            return Ok(Object::Null);
        };
        let new_id = target.new_object_id();
        self.map.insert(id, new_id);
        let copied = self.copy(target, object, depth + 1)?;
        target.set_object(new_id, copied);
        Ok(Object::Reference(new_id))
    }
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

/// A PDF resolved from the artifact store or the configured inbox.
struct ResolvedSource {
    bytes: Vec<u8>,
    /// Store hash when the source was already an artifact.
    artifact: Option<String>,
    filename: String,
}

struct SourceResolver {
    artifacts: Arc<ArtifactStore>,
    inbox: PathBuf,
}

impl SourceResolver {
    fn new(artifacts: Arc<ArtifactStore>, inbox: impl Into<PathBuf>) -> Result<Self> {
        let inbox = inbox.into();
        std::fs::create_dir_all(&inbox)?;
        let inbox = inbox.canonicalize()?;
        Ok(Self { artifacts, inbox })
    }

    /// Resolve exactly one of `artifact` (store hash) or `path` (inbox
    /// relative) into bytes.
    fn resolve(&self, args: &Value) -> Result<ResolvedSource> {
        let artifact = args
            .get("artifact")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        match (artifact, path) {
            (Some(hash), None) => {
                let stored = self.artifacts.load(hash)?;
                Ok(ResolvedSource {
                    bytes: stored.bytes,
                    artifact: Some(stored.hash),
                    filename: stored.filename,
                })
            }
            (None, Some(path)) => {
                let path = self.resolve_inbox(path)?;
                let bytes = std::fs::read(&path)?;
                check_input_size(bytes.len())?;
                let filename = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("document.pdf")
                    .to_owned();
                Ok(ResolvedSource {
                    bytes,
                    artifact: None,
                    filename,
                })
            }
            (Some(_), Some(_)) => Err(invalid("provide either artifact or path, not both")),
            (None, None) => Err(invalid("artifact or path is required")),
        }
    }

    fn resolve_inbox(&self, value: &str) -> Result<PathBuf> {
        if value.is_empty() || value.len() > 512 {
            return Err(invalid("invalid PDF path"));
        }
        let raw = Path::new(value);
        if raw.is_absolute() {
            return Err(SeraError::Forbidden(
                "PDF paths must be relative to the configured inbox".into(),
            ));
        }
        let candidate = self
            .inbox
            .join(raw)
            .canonicalize()
            .map_err(|_| SeraError::NotFound("PDF not found in the configured inbox".into()))?;
        if !candidate.starts_with(&self.inbox) {
            return Err(SeraError::Forbidden(
                "PDF path escapes the configured inbox".into(),
            ));
        }
        let extension = candidate
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if extension != "pdf" {
            return Err(invalid("unsupported document type; use pdf"));
        }
        Ok(candidate)
    }

    /// Provenance for an edit: inbox files are imported into the store first so
    /// the recorded `source_artifact` is a hash that actually resolves.
    fn record(&self, source: &ResolvedSource) -> Result<String> {
        if let Some(hash) = &source.artifact {
            return Ok(hash.clone());
        }
        let stored = self.artifacts.save(
            &source.bytes,
            &source.filename,
            ArtifactMeta {
                title: source.filename.clone(),
                filename: source.filename.clone(),
                language: Language::En,
                source_artifact: None,
                created_at: Utc::now(),
            },
        )?;
        Ok(stored.hash)
    }
}

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

fn pages_argument(args: &Value, key: &str) -> Result<Vec<u32>> {
    let values = args
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| invalid(format!("{key} is required")))?;
    if values.is_empty() {
        return Err(invalid(format!("{key} must list at least one page")));
    }
    if values.len() > MAX_PAGES {
        return Err(invalid(format!("{key} lists more than {MAX_PAGES} pages")));
    }
    let mut pages = Vec::with_capacity(values.len());
    for value in values {
        let page = value
            .as_u64()
            .ok_or_else(|| invalid(format!("{key} must contain positive integers")))?;
        if page == 0 || page > MAX_PAGES as u64 {
            return Err(invalid(format!(
                "{key} contains page {page}, which is outside 1..={MAX_PAGES}"
            )));
        }
        pages.push(page as u32);
    }
    Ok(pages)
}

fn derive_filename(source: &str, suffix: &str) -> String {
    let stem = Path::new(source)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("document");
    let stem: String = stem.chars().take(120).collect();
    format!("{stem}-{suffix}.pdf")
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

/// Human-readable page listing, capped so a 2000-page document cannot flood the
/// model's context. The complete list stays in the tool's structured data.
fn page_listing(info: &PdfInfo) -> String {
    const MAX_LISTED_PAGES: usize = 50;
    let mut listing = String::new();
    for page in info.pages.iter().take(MAX_LISTED_PAGES) {
        listing.push_str(&format!(
            "page {}: {}x{} pt, rotation {}\n",
            page.page, page.width_pt, page.height_pt, page.rotation
        ));
    }
    if info.pages.len() > MAX_LISTED_PAGES {
        listing.push_str(&format!(
            "... {} more page(s)\n",
            info.pages.len() - MAX_LISTED_PAGES
        ));
    }
    listing
}

pub struct PdfInspectTool {
    sources: SourceResolver,
}

impl PdfInspectTool {
    pub fn new(artifacts: Arc<ArtifactStore>, inbox: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            sources: SourceResolver::new(artifacts, inbox)?,
        })
    }
}

#[async_trait]
impl Tool for PdfInspectTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "pdf_inspect".into(),
            description:
                "Inspect a PDF from the artifact store or the configured inbox. Returns the page \
                 count, per-page size and rotation, and whether the file carries scripts, embedded \
                 files, or actions. It never executes or renders anything."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "artifact": {"type": "string", "description": "Artifact hash of the PDF (preferred)"},
                    "path": {"type": "string", "description": "Path relative to the configured inbox"}
                },
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, _context: &ToolContext) -> Result<ToolOutput> {
        reject_unknown_keys(&args, &["artifact", "path"])?;
        let source = self.sources.resolve(&args)?;
        let bytes = source.bytes;
        let info = tokio::task::spawn_blocking(move || inspect(&bytes))
            .await
            .map_err(|error| SeraError::Internal(format!("PDF task failed: {error}")))??;
        let data = serde_json::to_value(&info)?;
        let content = format!(
            "pages: {}\nversion: {}\nscripts: {}\nembedded files: {}\nactions: {}\nfindings: {}\n\n{}",
            info.page_count,
            info.version,
            info.has_scripts,
            info.has_embedded_files,
            info.has_actions,
            if info.findings.is_empty() {
                "none".to_owned()
            } else {
                info.findings.join(", ")
            },
            page_listing(&info)
        );
        Ok(ToolOutput {
            content,
            data: Some(data),
            ..ToolOutput::default()
        })
    }
}

enum EditKind {
    Rotate { pages: Vec<u32>, degrees: i64 },
    Delete { pages: Vec<u32> },
    Reorder { order: Vec<u32> },
}

impl EditKind {
    fn label(&self) -> &'static str {
        match self {
            Self::Rotate { .. } => "rotate",
            Self::Delete { .. } => "delete",
            Self::Reorder { .. } => "reorder",
        }
    }

    fn summary(&self) -> String {
        match self {
            Self::Rotate { pages, degrees } => {
                format!("rotated {} page(s) by {degrees} degrees", pages.len())
            }
            Self::Delete { pages } => format!("deleted {} page(s)", pages.len()),
            Self::Reorder { order } => format!("reordered {} page(s)", order.len()),
        }
    }
}

pub struct PdfEditTool {
    sources: SourceResolver,
    artifacts: Arc<ArtifactStore>,
    store: Store,
}

impl PdfEditTool {
    pub fn new(
        artifacts: Arc<ArtifactStore>,
        store: Store,
        inbox: impl Into<PathBuf>,
    ) -> Result<Self> {
        Ok(Self {
            sources: SourceResolver::new(artifacts.clone(), inbox)?,
            artifacts,
            store,
        })
    }
}

#[async_trait]
impl Tool for PdfEditTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "pdf_edit".into(),
            description:
                "Apply one bounded page operation to a PDF: rotate, delete, or reorder. The result \
                 is stored as a new artifact whose source_artifact is the input. Documents carrying \
                 scripts, embedded files, or actions are rejected, and every page index is \
                 validated before anything is written."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "artifact": {"type": "string", "description": "Artifact hash of the source PDF (preferred)"},
                    "path": {"type": "string", "description": "Path relative to the configured inbox"},
                    "operation": {"type": "string", "enum": ["rotate", "delete", "reorder"]},
                    "pages": {
                        "type": "array",
                        "items": {"type": "integer"},
                        "description": "1-based page numbers for rotate and delete"
                    },
                    "degrees": {"type": "integer", "description": "Rotation delta; a non-zero multiple of 90"},
                    "order": {
                        "type": "array",
                        "items": {"type": "integer"},
                        "description": "Complete 1-based page order for reorder"
                    }
                },
                "required": ["operation"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        reject_unknown_keys(
            &args,
            &["artifact", "path", "operation", "pages", "degrees", "order"],
        )?;
        let operation = args
            .get("operation")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let kind = match operation.as_str() {
            "rotate" => {
                let degrees = args
                    .get("degrees")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| invalid("degrees is required for rotate"))?;
                EditKind::Rotate {
                    pages: pages_argument(&args, "pages")?,
                    degrees,
                }
            }
            "delete" => EditKind::Delete {
                pages: pages_argument(&args, "pages")?,
            },
            "reorder" => EditKind::Reorder {
                order: pages_argument(&args, "order")?,
            },
            _ => {
                return Err(invalid(
                    "operation must be one of rotate, delete, or reorder",
                ));
            }
        };
        let source = self.sources.resolve(&args)?;
        let source_artifact = self.sources.record(&source)?;
        let label = kind.label();
        let summary = kind.summary();
        let filename = derive_filename(&source.filename, label);
        let input = source.bytes;
        let (bytes, info) = tokio::task::spawn_blocking(move || {
            let bytes = match &kind {
                EditKind::Rotate { pages, degrees } => rotate(&input, pages, *degrees)?,
                EditKind::Delete { pages } => delete(&input, pages)?,
                EditKind::Reorder { order } => reorder(&input, order)?,
            };
            let info = inspect(&bytes)?;
            Ok::<_, SeraError>((bytes, info))
        })
        .await
        .map_err(|error| SeraError::Internal(format!("PDF task failed: {error}")))??;

        let stored = self.artifacts.save(
            &bytes,
            &filename,
            artifact_meta(&filename, Some(source_artifact.clone())),
        )?;
        self.store.record_audit(
            &context.actor,
            "pdf.edited",
            Some("pdf_edit"),
            context.task_id.as_deref(),
            None,
            None,
            None,
            Some(&json!({
                "operation": label,
                "artifact_hash": stored.hash,
                "source_artifact": source_artifact,
                "pages": info.page_count,
            })),
        )?;
        let result = json!({
            "operation": label,
            "artifact_hash": stored.hash,
            "source_artifact": source_artifact,
            "filename": stored.filename,
            "pages": info.page_count,
        });
        Ok(ToolOutput {
            content: format!(
                "operation: {}\n{}\nfilename: {}\nartifact: {}\nsource_artifact: {}\npages: {}\n",
                label, summary, stored.filename, stored.hash, source_artifact, info.page_count
            ),
            data: Some(result),
            artifact_path: Some(format!("artifacts/{}", stored.filename)),
            artifact_hash: Some(stored.hash),
            filename: Some(stored.filename),
            destination: None,
        })
    }
}

pub struct PdfMergeTool {
    sources: SourceResolver,
    artifacts: Arc<ArtifactStore>,
    store: Store,
}

impl PdfMergeTool {
    pub fn new(
        artifacts: Arc<ArtifactStore>,
        store: Store,
        inbox: impl Into<PathBuf>,
    ) -> Result<Self> {
        Ok(Self {
            sources: SourceResolver::new(artifacts.clone(), inbox)?,
            artifacts,
            store,
        })
    }
}

#[async_trait]
impl Tool for PdfMergeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "pdf_merge".into(),
            description:
                "Concatenate PDFs in the given order into one artifact. Each source is an artifact \
                 hash or an inbox-relative path. Documents carrying scripts, embedded files, or \
                 actions are rejected; the merged result is bounded to 2000 pages."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "sources": {
                        "type": "array",
                        "description": "Documents in merge order, each {artifact} or {path}",
                        "items": {
                            "type": "object",
                            "properties": {
                                "artifact": {"type": "string"},
                                "path": {"type": "string"}
                            },
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["sources"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        reject_unknown_keys(&args, &["sources"])?;
        let entries = args
            .get("sources")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("sources is required"))?;
        if entries.is_empty() {
            return Err(invalid("sources must list at least one document"));
        }
        if entries.len() > MAX_MERGE_DOCUMENTS {
            return Err(invalid(format!(
                "sources lists more than {MAX_MERGE_DOCUMENTS} documents"
            )));
        }
        let mut inputs = Vec::with_capacity(entries.len());
        let mut source_artifacts = Vec::with_capacity(entries.len());
        let mut first_filename = None;
        for entry in entries {
            let object = entry
                .as_object()
                .ok_or_else(|| invalid("each source must be an object with artifact or path"))?;
            reject_unknown_keys(&Value::Object(object.clone()), &["artifact", "path"])?;
            let source = self.sources.resolve(&Value::Object(object.clone()))?;
            source_artifacts.push(self.sources.record(&source)?);
            if first_filename.is_none() {
                first_filename = Some(source.filename.clone());
            }
            inputs.push(source.bytes);
        }
        let filename = derive_filename(first_filename.as_deref().unwrap_or("document"), "merged");
        let (bytes, info) = tokio::task::spawn_blocking(move || {
            let references: Vec<&[u8]> = inputs.iter().map(Vec::as_slice).collect();
            let bytes = merge(&references)?;
            let info = inspect(&bytes)?;
            Ok::<_, SeraError>((bytes, info))
        })
        .await
        .map_err(|error| SeraError::Internal(format!("PDF task failed: {error}")))??;

        // `ArtifactMeta` carries a single source hash; the first input is
        // recorded there and every input is listed in the audit event.
        let primary = source_artifacts.first().cloned();
        let stored =
            self.artifacts
                .save(&bytes, &filename, artifact_meta(&filename, primary.clone()))?;
        self.store.record_audit(
            &context.actor,
            "pdf.merged",
            Some("pdf_merge"),
            context.task_id.as_deref(),
            None,
            None,
            None,
            Some(&json!({
                "artifact_hash": stored.hash,
                "source_artifacts": source_artifacts,
                "pages": info.page_count,
            })),
        )?;
        let result = json!({
            "artifact_hash": stored.hash,
            "source_artifacts": source_artifacts,
            "filename": stored.filename,
            "pages": info.page_count,
        });
        Ok(ToolOutput {
            content: format!(
                "merged {} document(s)\nfilename: {}\nartifact: {}\npages: {}\n",
                source_artifacts.len(),
                stored.filename,
                stored.hash,
                info.page_count
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
    use super::*;

    /// Synthetic multi-page PDF: every page draws "Page N", resources and the
    /// media box live on the `/Pages` node so inheritance is exercised.
    fn sample_document(count: usize) -> Document {
        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();

        let mut font = Dictionary::new();
        font.set("Type", "Font");
        font.set("Subtype", "Type1");
        font.set("BaseFont", "Helvetica");
        let font_id = document.add_object(font);
        let mut fonts = Dictionary::new();
        fonts.set("F1", Object::Reference(font_id));
        let mut resources = Dictionary::new();
        resources.set("Font", fonts);

        let mut kids = Vec::with_capacity(count);
        for index in 1..=count {
            let content = format!("BT /F1 12 Tf 20 100 Td (Page {index}) Tj ET");
            let content_id =
                document.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
            let mut page = Dictionary::new();
            page.set("Type", "Page");
            page.set("Parent", Object::Reference(pages_id));
            page.set("Contents", Object::Reference(content_id));
            kids.push(Object::Reference(document.add_object(page)));
        }

        let mut pages = Dictionary::new();
        pages.set("Type", "Pages");
        pages.set("Kids", Object::Array(kids));
        pages.set("Count", Object::Integer(count as i64));
        pages.set(
            "MediaBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(200),
                Object::Integer(300),
            ]),
        );
        pages.set("Resources", resources);
        document.set_object(pages_id, pages);

        let mut info = Dictionary::new();
        info.set("Title", Object::string_literal("Synthetic report"));
        info.set("Author", Object::string_literal("Synthetic Author"));
        info.set("Producer", Object::string_literal("Synthetic Producer"));
        info.set("Creator", Object::string_literal("Synthetic Creator"));
        info.set("Subject", Object::string_literal("Synthetic subject"));
        info.set("Keywords", Object::string_literal("synthetic, fixture"));
        let info_id = document.add_object(info);

        let mut catalog = Dictionary::new();
        catalog.set("Type", "Catalog");
        catalog.set("Pages", Object::Reference(pages_id));
        let catalog_id = document.add_object(catalog);

        document.trailer.set("Root", Object::Reference(catalog_id));
        document.trailer.set("Info", Object::Reference(info_id));
        document
    }

    fn sample_pdf(count: usize) -> Vec<u8> {
        let mut document = sample_document(count);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).expect("synthetic PDF saves");
        bytes
    }

    fn page_texts(bytes: &[u8]) -> Vec<String> {
        let document = Document::load_mem(bytes).expect("output parses");
        let numbers: Vec<u32> = document.get_pages().keys().copied().collect();
        numbers
            .iter()
            .map(|number| document.extract_text(&[*number]).unwrap_or_default())
            .collect()
    }

    fn rotations(bytes: &[u8]) -> Vec<i64> {
        let document = Document::load_mem(bytes).expect("output parses");
        document
            .get_pages()
            .values()
            .map(|id| rotation_of(&document, *id).unwrap())
            .collect()
    }

    fn validation_error<T: std::fmt::Debug>(result: Result<T>) -> String {
        match result {
            Err(SeraError::Validation(message)) => message,
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[test]
    fn inspect_reports_pages_sizes_and_rotation() {
        let info = inspect(&sample_pdf(3)).unwrap();
        assert_eq!(info.page_count, 3);
        assert_eq!(info.pages.len(), 3);
        assert_eq!(info.version, "1.5");
        for page in &info.pages {
            assert_eq!(page.width_pt, 200.0);
            assert_eq!(page.height_pt, 300.0);
            assert_eq!(page.rotation, 0);
        }
        assert!(!info.dangerous);
        assert!(info.findings.is_empty());
        assert!(!info.has_scripts);
        assert!(!info.has_embedded_files);
        assert!(!info.has_actions);
    }

    #[test]
    fn rotate_updates_the_rotate_entry_and_accumulates() {
        let source = sample_pdf(2);
        let once = rotate(&source, &[1], 90).unwrap();
        assert_eq!(rotations(&once), vec![90, 0]);

        let twice = rotate(&once, &[1], 90).unwrap();
        assert_eq!(rotations(&twice), vec![180, 0]);

        let negative = rotate(&source, &[2], -90).unwrap();
        assert_eq!(rotations(&negative), vec![0, 270]);
    }

    #[test]
    fn rotate_rejects_invalid_angles_and_pages() {
        let source = sample_pdf(2);
        assert!(validation_error(rotate(&source, &[1], 45)).contains("multiple of 90"));
        assert!(validation_error(rotate(&source, &[1], 0)).contains("multiple of 90"));
        assert!(validation_error(rotate(&source, &[3], 90)).contains("1..=2"));
        assert!(validation_error(rotate(&source, &[0], 90)).contains("1..=2"));
        assert!(validation_error(rotate(&source, &[1, 1], 90)).contains("more than once"));
        assert!(validation_error(rotate(&source, &[], 90)).contains("at least one page"));
    }

    #[test]
    fn delete_reduces_the_page_count_and_keeps_the_rest() {
        let source = sample_pdf(3);
        let output = delete(&source, &[2]).unwrap();
        let info = inspect(&output).unwrap();
        assert_eq!(info.page_count, 2);
        let texts = page_texts(&output);
        assert!(texts[0].contains("Page 1"));
        assert!(texts[1].contains("Page 3"));
    }

    #[test]
    fn delete_refuses_to_empty_the_document() {
        let source = sample_pdf(2);
        assert!(validation_error(delete(&source, &[1, 2])).contains("empty document"));
        assert!(validation_error(delete(&source, &[5])).contains("1..=2"));
    }

    #[test]
    fn reorder_permutes_page_content() {
        let source = sample_pdf(3);
        let output = reorder(&source, &[3, 1, 2]).unwrap();
        let texts = page_texts(&output);
        assert_eq!(texts.len(), 3);
        assert!(texts[0].contains("Page 3"));
        assert!(texts[1].contains("Page 1"));
        assert!(texts[2].contains("Page 2"));
    }

    #[test]
    fn reorder_requires_a_complete_permutation() {
        let source = sample_pdf(3);
        assert!(validation_error(reorder(&source, &[1, 2])).contains("exactly once"));
        assert!(validation_error(reorder(&source, &[1, 2, 2])).contains("more than once"));
        assert!(validation_error(reorder(&source, &[1, 2, 4])).contains("1..=3"));
    }

    #[test]
    fn extract_returns_only_the_requested_range() {
        let source = sample_pdf(4);
        let output = extract(&source, PageRange::new(2, 3)).unwrap();
        let texts = page_texts(&output);
        assert_eq!(texts.len(), 2);
        assert!(texts[0].contains("Page 2"));
        assert!(texts[1].contains("Page 3"));
        assert_eq!(inspect(&output).unwrap().page_count, 2);
    }

    #[test]
    fn extract_rejects_out_of_range_requests() {
        let source = sample_pdf(3);
        assert!(validation_error(extract(&source, PageRange::new(0, 2))).contains("1..=3"));
        assert!(validation_error(extract(&source, PageRange::new(2, 1))).contains("1..=3"));
        assert!(validation_error(extract(&source, PageRange::new(1, 4))).contains("1..=3"));
    }

    #[test]
    fn merge_concatenates_in_order() {
        let first = sample_pdf(2);
        let second = sample_pdf(3);
        let output = merge(&[first.as_slice(), second.as_slice()]).unwrap();
        let texts = page_texts(&output);
        assert_eq!(texts.len(), 5);
        assert!(texts[0].contains("Page 1"));
        assert!(texts[1].contains("Page 2"));
        assert!(texts[2].contains("Page 1"));
        assert!(texts[3].contains("Page 2"));
        assert!(texts[4].contains("Page 3"));
        assert_eq!(inspect(&output).unwrap().page_count, 5);
    }

    #[test]
    fn merge_rejects_empty_and_oversized_input_sets() {
        assert!(validation_error(merge(&[])).contains("at least one document"));
        let source = sample_pdf(1);
        let many: Vec<&[u8]> = (0..MAX_MERGE_DOCUMENTS + 1)
            .map(|_| source.as_slice())
            .collect();
        assert!(validation_error(merge(&many)).contains("at most"));
    }

    #[test]
    fn merge_rejects_a_dangerous_source() {
        let source = sample_pdf(1);
        let dangerous = javascript_document();
        let result = merge(&[source.as_slice(), dangerous.as_slice()]);
        assert!(validation_error(result).contains("/JavaScript"));
    }

    #[test]
    fn strip_metadata_removes_the_information_dictionary() {
        let source = sample_pdf(2);
        let output = strip_metadata(&source).unwrap();
        let document = Document::load_mem(&output).unwrap();
        assert!(document.trailer.get(b"Info").is_err());
        let text = String::from_utf8_lossy(&output);
        for key in METADATA_KEYS {
            assert!(!text.contains(key), "{key} is still present");
        }
        assert_eq!(inspect(&output).unwrap().page_count, 2);
    }

    fn javascript_document() -> Vec<u8> {
        let mut document = sample_document(1);
        let mut action = Dictionary::new();
        action.set("S", "JavaScript");
        action.set("JS", Object::string_literal("app.alert('synthetic')"));
        let action_id = document.add_object(action);
        let catalog = document
            .trailer
            .get(b"Root")
            .and_then(Object::as_reference)
            .unwrap();
        document
            .get_dictionary_mut(catalog)
            .unwrap()
            .set("OpenAction", Object::Reference(action_id));
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn inspect_reports_scripts_without_executing_them() {
        let info = inspect(&javascript_document()).unwrap();
        assert!(info.dangerous);
        assert!(info.has_scripts);
        assert!(info.findings.iter().any(|name| name == "/JavaScript"));
    }

    #[test]
    fn every_mutating_operation_rejects_javascript() {
        let source = javascript_document();
        assert!(validation_error(rotate(&source, &[1], 90)).contains("/JavaScript"));
        assert!(validation_error(delete(&source, &[1])).contains("/JavaScript"));
        assert!(validation_error(reorder(&source, &[1])).contains("/JavaScript"));
        assert!(validation_error(extract(&source, PageRange::new(1, 1))).contains("/JavaScript"));
        assert!(validation_error(strip_metadata(&source)).contains("/JavaScript"));
    }

    #[test]
    fn embedded_files_are_detected_and_rejected() {
        let mut document = sample_document(1);
        let mut tree = Dictionary::new();
        tree.set("Names", Object::Array(Vec::new()));
        let tree_id = document.add_object(tree);
        let mut names = Dictionary::new();
        names.set("EmbeddedFiles", Object::Reference(tree_id));
        let catalog = document
            .trailer
            .get(b"Root")
            .and_then(Object::as_reference)
            .unwrap();
        document
            .get_dictionary_mut(catalog)
            .unwrap()
            .set("Names", names);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();

        let info = inspect(&bytes).unwrap();
        assert!(info.has_embedded_files);
        assert!(info.findings.iter().any(|name| name == "/EmbeddedFiles"));
        assert!(validation_error(delete(&bytes, &[1])).contains("/EmbeddedFiles"));
    }

    #[test]
    fn encrypted_documents_are_rejected() {
        let mut document = sample_document(1);
        let mut encrypt = Dictionary::new();
        encrypt.set("Filter", "Standard");
        encrypt.set("V", Object::Integer(1));
        encrypt.set("R", Object::Integer(2));
        let encrypt_id = document.add_object(encrypt);
        document
            .trailer
            .set("Encrypt", Object::Reference(encrypt_id));
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        assert!(validation_error(delete(&bytes, &[1])).contains("encrypted"));
        assert!(inspect(&bytes).is_err());
    }

    #[test]
    fn oversized_and_empty_inputs_are_rejected() {
        assert!(validation_error(check_input_size(0)).contains("empty"));
        assert!(validation_error(check_input_size(MAX_PDF_BYTES + 1)).contains("64 MiB"));
        assert!(check_input_size(MAX_PDF_BYTES).is_ok());
        assert!(validation_error(inspect(&[])).contains("empty"));
        assert!(validation_error(inspect(b"not a pdf at all")).contains("malformed"));
    }

    #[test]
    fn documents_over_the_page_limit_are_rejected() {
        let source = sample_pdf(MAX_PAGES + 1);
        assert!(validation_error(delete(&source, &[1])).contains("limit is 2000"));
    }

    #[test]
    fn inherited_page_attributes_survive_a_rewrite() {
        // MediaBox and Resources live on the /Pages node in the fixture.
        let output = reorder(&sample_pdf(2), &[2, 1]).unwrap();
        let document = Document::load_mem(&output).unwrap();
        for page_id in document.get_pages().values() {
            let page = document.get_dictionary(*page_id).unwrap();
            assert!(page.has(b"MediaBox"), "MediaBox was not materialised");
            assert!(page.has(b"Resources"), "Resources were not materialised");
        }
        let info = inspect(&output).unwrap();
        assert_eq!(info.pages[0].width_pt, 200.0);
        assert_eq!(info.pages[0].height_pt, 300.0);
    }

    #[test]
    fn deleted_pages_are_pruned_from_the_file() {
        let output = delete(&sample_pdf(3), &[2]).unwrap();
        let text = String::from_utf8_lossy(&output);
        assert!(!text.contains("(Page 2)"), "the deleted page survived");
        assert!(text.contains("(Page 1)"));
        assert!(text.contains("(Page 3)"));
    }

    #[test]
    fn tools_edit_an_artifact_and_record_its_source() {
        let directory = tempfile::tempdir().unwrap();
        let artifacts = Arc::new(ArtifactStore::new(directory.path()).unwrap());
        let store = Store::open_in_memory(384).unwrap();
        let tool = PdfEditTool::new(artifacts.clone(), store, directory.path()).unwrap();

        let source = artifacts
            .save(
                &sample_pdf(3),
                "synthetic.pdf",
                artifact_meta("synthetic.pdf", None),
            )
            .unwrap();
        let context = ToolContext {
            actor: "owner".into(),
            bot: "maker".into(),
            task_id: None,
            turn_id: None,
            conversation_id: None,
            allowed_tools: ["pdf_edit".to_string()].into_iter().collect(),
            require_approval: HashSet::new(),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let output = runtime
            .block_on(tool.execute(
                json!({"artifact": source.hash, "operation": "delete", "pages": [2]}),
                &context,
            ))
            .unwrap();

        let hash = output.artifact_hash.clone().expect("artifact hash");
        let stored = artifacts.load(&hash).unwrap();
        assert_eq!(
            stored.meta.source_artifact.as_deref(),
            Some(source.hash.as_str())
        );
        assert_eq!(stored.filename, "synthetic-delete.pdf");
        assert_eq!(inspect(&stored.bytes).unwrap().page_count, 2);
    }

    #[test]
    fn tools_reject_unknown_arguments_and_absolute_paths() {
        let directory = tempfile::tempdir().unwrap();
        let artifacts = Arc::new(ArtifactStore::new(directory.path()).unwrap());
        let store = Store::open_in_memory(384).unwrap();
        let tool = PdfInspectTool::new(artifacts, directory.path()).unwrap();
        let context = ToolContext {
            actor: "owner".into(),
            bot: "maker".into(),
            task_id: None,
            turn_id: None,
            conversation_id: None,
            allowed_tools: ["pdf_inspect".to_string()].into_iter().collect(),
            require_approval: HashSet::new(),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(
            runtime
                .block_on(tool.execute(json!({"path": "a.pdf", "extra": 1}), &context))
                .is_err()
        );
        assert!(
            runtime
                .block_on(tool.execute(json!({"path": "C:/Windows/system.ini"}), &context))
                .is_err()
        );
        let _ = store;
    }
}
