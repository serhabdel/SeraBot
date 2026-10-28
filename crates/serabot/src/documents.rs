//! Document ingestion and provenance-preserving extraction.
//!
//! Supported inputs are digital PDFs (per-page text extraction through
//! `lopdf`), plain text and markdown. OCR is intentionally deferred: scanned or
//! image-only PDFs are rejected with an actionable error instead of silently
//! producing an empty document.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use rusqlite::{ErrorCode, OptionalExtension, params};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use crate::config::DocumentsConfig;
use crate::domain::new_id;
use crate::embeddings::{Embedder, MAX_TEXT_CHARS};
use crate::error::{Result, SeraError};
use crate::store::Store;

/// Conservative embedding batch bound for a 2 CPU / 4 GB host.
const EMBED_BATCH_SIZE: usize = 16;
const MAX_TITLE_CHARS: usize = 512;
const MAX_ERROR_CHARS: usize = 500;

/// Outcome of a single [`ingest_file`] call.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IngestResult {
    pub document_id: String,
    pub sha256: String,
    pub pages: usize,
    pub chunks: usize,
    pub duplicate: bool,
}

/// One file that could not be ingested while indexing a directory.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DirectoryFailure {
    pub path: PathBuf,
    pub error: String,
}

/// Outcome of [`index_directory`].
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DirectoryIndexReport {
    pub ingested: Vec<IngestResult>,
    pub failures: Vec<DirectoryFailure>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocumentKind {
    Pdf,
    Text,
    Markdown,
}

impl DocumentKind {
    fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "pdf" => Some(Self::Pdf),
            "txt" => Some(Self::Text),
            "md" | "markdown" => Some(Self::Markdown),
            _ => None,
        }
    }

    fn mime(self) -> &'static str {
        match self {
            Self::Pdf => "application/pdf",
            Self::Text => "text/plain",
            Self::Markdown => "text/markdown",
        }
    }
}

#[derive(Debug)]
struct PageText {
    page: usize,
    text: String,
}

#[derive(Debug)]
struct PlannedChunk {
    page: usize,
    ordinal: usize,
    text: String,
}

#[derive(Debug)]
struct ChunkRecord {
    id: String,
    text: String,
}

#[derive(Debug)]
struct ExistingDocument {
    id: String,
    status: String,
}

/// Ingest one document.
///
/// The file is read once, bounded by `documents.max_bytes`, and hashed with
/// SHA-256. A document with the same hash that is already indexed is returned
/// as a duplicate without re-embedding. A previously failed document with the
/// same hash is replaced, which gives the operator a safe retry path.
///
/// Database writes happen in short transactions. No transaction or pooled
/// connection is held while the embedder is running.
pub async fn ingest_file(
    store: &Store,
    embedder: &dyn Embedder,
    path: &Path,
    config: &DocumentsConfig,
) -> Result<IngestResult> {
    validate_config(config)?;
    if embedder.dimension() != store.dimension() {
        return Err(SeraError::Config(format!(
            "embedder dimension {} does not match store dimension {}; reindex with matching configuration",
            embedder.dimension(),
            store.dimension()
        )));
    }

    let kind = DocumentKind::from_path(path).ok_or_else(|| {
        SeraError::Validation(format!(
            "unsupported document type for {}; supported types are .pdf, .txt, .md and .markdown",
            path.display()
        ))
    })?;

    let bytes = read_bounded(path, config.max_bytes).await?;
    let sha256 = sha256_hex(&bytes);

    let existing = find_by_sha(store, &sha256)?;
    if let Some(document) = &existing {
        if document.status != "failed" {
            return duplicate_result(store, &document.id, sha256);
        }
    }

    let pages = extract_pages(kind, bytes).await?;
    let page_count = pages.len();
    if page_count == 0 {
        return Err(SeraError::Validation(
            "document contains no pages; refusing to create an empty document".into(),
        ));
    }

    let planned = plan_chunks(pages, config.chunk_chars, config.chunk_overlap);
    if planned.is_empty() {
        return Err(empty_text_error());
    }
    let chunk_count = planned.len();

    // A failed attempt may have left chunks/vectors behind. Re-check the state
    // right before replacing it so a concurrent successful ingestion is never
    // purged by this one.
    if existing.is_some() {
        if let Some(current) = find_by_sha(store, &sha256)? {
            if current.status != "failed" {
                return duplicate_result(store, &current.id, sha256);
            }
            purge_document(store, &current.id)?;
        }
    }

    let source_path = tokio::fs::canonicalize(path)
        .await
        .unwrap_or_else(|_| path.to_path_buf());
    let title = document_title(path);
    let document_id = new_id();
    let now = now_string();

    let insert = {
        let connection = store.connection()?;
        connection.execute(
            "INSERT INTO documents(id, source_path, sha256, title, mime, pages, status, error, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'indexing', NULL, ?7, ?7)",
            params![
                document_id,
                source_path.to_string_lossy(),
                sha256,
                title,
                kind.mime(),
                page_count as i64,
                now,
            ],
        )
    };
    if let Err(error) = insert {
        if is_constraint_violation(&error) {
            // Another ingestion won the race for this hash.
            if let Some(document) = find_by_sha(store, &sha256)? {
                return duplicate_result(store, &document.id, sha256);
            }
        }
        return Err(SeraError::Database(error));
    }

    if let Err(error) = index_document(store, embedder, &document_id, planned, page_count).await {
        mark_failed(store, &document_id, &error);
        return Err(error);
    }

    Ok(IngestResult {
        document_id,
        sha256,
        pages: page_count,
        chunks: chunk_count,
        duplicate: false,
    })
}

/// List one level of `dir`, skipping hidden entries, directories, symlinks and
/// unsupported extensions. Results are sorted for deterministic ingestion.
pub fn indexable_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() || !file_type.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if is_hidden(&entry, name.as_ref()) {
            continue;
        }
        let path = entry.path();
        if DocumentKind::from_path(&path).is_some() {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Ingest every supported, non-hidden file directly inside `dir`.
///
/// This never recurses and never follows symlinks. Individual file failures are
/// reported instead of aborting the whole directory scan.
pub async fn index_directory(
    store: &Store,
    embedder: &dyn Embedder,
    dir: &Path,
    config: &DocumentsConfig,
) -> Result<DirectoryIndexReport> {
    let files = indexable_files(dir)?;
    let mut report = DirectoryIndexReport::default();
    for path in files {
        match ingest_file(store, embedder, &path, config).await {
            Ok(result) => report.ingested.push(result),
            Err(error) => report.failures.push(DirectoryFailure {
                path,
                error: error.to_string(),
            }),
        }
    }
    Ok(report)
}

fn validate_config(config: &DocumentsConfig) -> Result<()> {
    if config.max_bytes == 0 {
        return Err(SeraError::Config(
            "documents.max_bytes must be greater than zero".into(),
        ));
    }
    if config.chunk_chars == 0 {
        return Err(SeraError::Config(
            "documents.chunk_chars must be greater than zero".into(),
        ));
    }
    if config.chunk_overlap >= config.chunk_chars {
        return Err(SeraError::Config(
            "documents.chunk_overlap must be smaller than documents.chunk_chars".into(),
        ));
    }
    if config.chunk_chars > MAX_TEXT_CHARS {
        return Err(SeraError::Config(format!(
            "documents.chunk_chars must not exceed {MAX_TEXT_CHARS}"
        )));
    }
    Ok(())
}

async fn read_bounded(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    let file = tokio::fs::File::open(path).await.map_err(|error| {
        SeraError::Validation(format!("cannot open {}: {error}", path.display()))
    })?;
    let metadata = file.metadata().await?;
    if !metadata.is_file() {
        return Err(SeraError::Validation(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    if metadata.len() > max_bytes {
        return Err(SeraError::Validation(format!(
            "document is {} bytes, exceeding documents.max_bytes ({} bytes)",
            metadata.len(),
            max_bytes
        )));
    }

    let capacity = usize::try_from(metadata.len())
        .unwrap_or(0)
        .min(16 * 1024 * 1024);
    let mut buffer = Vec::with_capacity(capacity);
    let mut reader = file.take(max_bytes.saturating_add(1));
    reader.read_to_end(&mut buffer).await?;
    if buffer.len() as u64 > max_bytes {
        return Err(SeraError::Validation(format!(
            "document exceeds documents.max_bytes ({} bytes)",
            max_bytes
        )));
    }
    Ok(buffer)
}

async fn extract_pages(kind: DocumentKind, bytes: Vec<u8>) -> Result<Vec<PageText>> {
    match kind {
        DocumentKind::Pdf => {
            let pages = tokio::task::spawn_blocking(move || extract_pdf_pages(bytes))
                .await
                .map_err(|error| {
                    SeraError::Internal(format!("PDF extraction task failed: {error}"))
                })??;
            Ok(pages)
        }
        DocumentKind::Text | DocumentKind::Markdown => {
            let text = String::from_utf8(bytes).map_err(|_| {
                SeraError::Validation(
                    "text documents must be valid UTF-8; convert the file to UTF-8 first".into(),
                )
            })?;
            if text.trim().is_empty() {
                return Err(empty_text_error());
            }
            Ok(vec![PageText { page: 1, text }])
        }
    }
}

fn extract_pdf_pages(bytes: Vec<u8>) -> Result<Vec<PageText>> {
    let document = catch_unwind(AssertUnwindSafe(|| lopdf::Document::load_mem(&bytes)))
        .map_err(|_| {
            SeraError::Validation(
                "malformed PDF: the parser aborted; the file is not a safe digital PDF".into(),
            )
        })?
        .map_err(|error| SeraError::Validation(format!("malformed PDF: {error}")))?;

    let encrypted = catch_unwind(AssertUnwindSafe(|| document.is_encrypted()))
        .map_err(|_| SeraError::Validation("malformed PDF: encryption check aborted".into()))?;
    if encrypted {
        return Err(SeraError::Validation(
            "PDF is encrypted; decrypt it before ingestion (encrypted PDFs are not supported)"
                .into(),
        ));
    }

    let page_numbers: Vec<u32> = catch_unwind(AssertUnwindSafe(|| document.get_pages()))
        .map_err(|_| SeraError::Validation("malformed PDF: page tree traversal aborted".into()))?
        .keys()
        .copied()
        .collect();
    if page_numbers.is_empty() {
        return Err(SeraError::Validation(
            "malformed PDF: no pages were found".into(),
        ));
    }

    let mut pages = Vec::with_capacity(page_numbers.len());
    for page_number in page_numbers {
        let text = catch_unwind(AssertUnwindSafe(|| document.extract_text(&[page_number])))
            .map_err(|_| {
                SeraError::Validation(format!(
                    "malformed PDF: extracting page {page_number} aborted"
                ))
            })?
            .map_err(|error| {
                SeraError::Validation(format!("PDF page {page_number} could not be read: {error}"))
            })?;
        pages.push(PageText {
            page: page_number as usize,
            text,
        });
    }

    if pages.iter().all(|page| page.text.trim().is_empty()) {
        return Err(SeraError::Validation(
            "PDF contains no extractable text; it may be scanned or image-only. OCR is not supported; provide a text-based PDF."
                .into(),
        ));
    }
    Ok(pages)
}

fn plan_chunks(pages: Vec<PageText>, chunk_chars: usize, overlap: usize) -> Vec<PlannedChunk> {
    let mut chunks = Vec::new();
    let mut ordinal = 0usize;
    for page in pages {
        for (start, end) in chunk_ranges(&page.text, chunk_chars, overlap) {
            let text = page.text[start..end].trim();
            if text.is_empty() {
                continue;
            }
            chunks.push(PlannedChunk {
                page: page.page,
                ordinal,
                text: text.to_owned(),
            });
            ordinal += 1;
        }
    }
    chunks
}

/// Deterministic character-based ranges over UTF-8 text.
///
/// Chunks never exceed `chunk_chars` characters and consecutive chunks overlap
/// by at most `overlap` characters. Byte offsets are always char boundaries.
fn chunk_ranges(text: &str, chunk_chars: usize, overlap: usize) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    if text.is_empty() || chunk_chars == 0 {
        return ranges;
    }
    let overlap = overlap.min(chunk_chars.saturating_sub(1));
    let mut start = 0usize;
    while start < text.len() {
        let mut end = start;
        for (count, (offset, character)) in text[start..].char_indices().enumerate() {
            if count == chunk_chars {
                break;
            }
            end = start + offset + character.len_utf8();
        }
        if end == start {
            break;
        }
        ranges.push((start, end));
        if end >= text.len() {
            break;
        }

        let mut next = end;
        for (backed_off, (offset, _)) in text[start..end].char_indices().rev().enumerate() {
            if backed_off >= overlap {
                break;
            }
            next = start + offset;
        }
        if next <= start {
            // Guarantee forward progress when the overlap covers the window.
            next = text[start..]
                .chars()
                .next()
                .map(|character| start + character.len_utf8())
                .unwrap_or(end);
        }
        start = next;
    }
    ranges
}

async fn index_document(
    store: &Store,
    embedder: &dyn Embedder,
    document_id: &str,
    chunks: Vec<PlannedChunk>,
    pages: usize,
) -> Result<()> {
    let chunk_count = chunks.len();
    if chunk_count == 0 {
        return Err(empty_text_error());
    }
    let created_at = now_string();

    // 1. Persist chunks with page and document-wide ordinal in one transaction.
    let records = {
        let connection = store.connection()?;
        let transaction = connection.unchecked_transaction()?;
        let mut records = Vec::with_capacity(chunk_count);
        {
            let mut statement = transaction.prepare(
                "INSERT INTO chunks(id, document_id, page, ordinal, text, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for chunk in chunks {
                let id = new_id();
                statement.execute(params![
                    id,
                    document_id,
                    chunk.page as i64,
                    chunk.ordinal as i64,
                    chunk.text,
                    created_at,
                ])?;
                records.push(ChunkRecord {
                    id,
                    text: chunk.text,
                });
            }
        }
        transaction.commit()?;
        records
    };

    // 2. Embed bounded batches, then write each batch's vectors in its own
    //    transaction. No connection is held across the embedder await.
    let dimension = store.dimension();
    let mut embedded = 0usize;
    for batch in records.chunks(EMBED_BATCH_SIZE) {
        let texts: Vec<String> = batch.iter().map(|record| record.text.clone()).collect();
        let vectors = embedder.embed(texts).await?;
        if vectors.len() != batch.len() {
            return Err(SeraError::Internal(format!(
                "embedder returned {} vectors for {} chunks",
                vectors.len(),
                batch.len()
            )));
        }
        for vector in &vectors {
            if vector.len() != dimension {
                return Err(SeraError::Internal(format!(
                    "embedder returned dimension {} but the store is configured for {dimension}",
                    vector.len()
                )));
            }
            if vector.iter().any(|value| !value.is_finite()) {
                return Err(SeraError::Internal(
                    "embedder returned non-finite values".into(),
                ));
            }
        }

        let connection = store.connection()?;
        let transaction = connection.unchecked_transaction()?;
        {
            let mut statement = transaction
                .prepare("INSERT INTO chunk_vectors(chunk_id, embedding) VALUES (?1, ?2)")?;
            for (record, vector) in batch.iter().zip(vectors.iter()) {
                statement.execute(params![record.id, encode_vector(vector)])?;
            }
        }
        transaction.commit()?;
        embedded += batch.len();
    }
    if embedded != chunk_count {
        return Err(SeraError::Internal(format!(
            "embedded {embedded} of {chunk_count} chunks"
        )));
    }

    // 3. Only a fully indexed document is visible to retrieval.
    let connection = store.connection()?;
    let changed = connection.execute(
        "UPDATE documents SET status='ready', error=NULL, pages=?1, updated_at=?2 WHERE id=?3",
        params![pages as i64, now_string(), document_id],
    )?;
    if changed != 1 {
        return Err(SeraError::Internal(
            "document disappeared while it was being indexed".into(),
        ));
    }
    Ok(())
}

fn find_by_sha(store: &Store, sha256: &str) -> Result<Option<ExistingDocument>> {
    let connection = store.connection()?;
    Ok(connection
        .query_row(
            "SELECT id, status FROM documents WHERE sha256 = ?1",
            params![sha256],
            |row| {
                Ok(ExistingDocument {
                    id: row.get(0)?,
                    status: row.get(1)?,
                })
            },
        )
        .optional()?)
}

fn duplicate_result(store: &Store, document_id: &str, sha256: String) -> Result<IngestResult> {
    let connection = store.connection()?;
    let (pages, chunks): (i64, i64) = connection.query_row(
        "SELECT COALESCE(d.pages, 0),
                (SELECT COUNT(*) FROM chunks c WHERE c.document_id = d.id)
         FROM documents d WHERE d.id = ?1",
        params![document_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(IngestResult {
        document_id: document_id.to_owned(),
        sha256,
        pages: usize::try_from(pages).unwrap_or(0),
        chunks: usize::try_from(chunks).unwrap_or(0),
        duplicate: true,
    })
}

fn purge_document(store: &Store, document_id: &str) -> Result<()> {
    let connection = store.connection()?;
    let transaction = connection.unchecked_transaction()?;
    transaction.execute(
        "DELETE FROM chunk_vectors WHERE chunk_id IN (SELECT id FROM chunks WHERE document_id = ?1)",
        params![document_id],
    )?;
    transaction.execute(
        "DELETE FROM chunks WHERE document_id = ?1",
        params![document_id],
    )?;
    transaction.execute("DELETE FROM documents WHERE id = ?1", params![document_id])?;
    transaction.commit()?;
    Ok(())
}

fn mark_failed(store: &Store, document_id: &str, error: &SeraError) {
    let message = truncate_chars(&error.to_string(), MAX_ERROR_CHARS);
    let result = store.connection().and_then(|connection| {
        connection.execute(
            "UPDATE documents SET status='failed', error=?1, updated_at=?2 WHERE id=?3",
            params![message, now_string(), document_id],
        )?;
        Ok(())
    });
    if let Err(mark_error) = result {
        tracing::warn!(
            document_id = document_id,
            error = %mark_error,
            "could not mark document as failed"
        );
    }
}

fn is_constraint_violation(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(inner, _) if inner.code == ErrorCode::ConstraintViolation
    )
}

fn empty_text_error() -> SeraError {
    SeraError::Validation(
        "document contains no extractable text; scanned or image-only files require OCR, which is not supported"
            .into(),
    )
}

fn document_title(path: &Path) -> String {
    let title = path
        .file_stem()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("document");
    truncate_chars(title, MAX_TITLE_CHARS)
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn encode_vector(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn now_string() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn truncate_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

fn is_hidden(entry: &std::fs::DirEntry, name: &str) -> bool {
    if name.starts_with('.') || name.starts_with("~$") {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if let Ok(metadata) = entry.metadata() {
            const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
            if metadata.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0 {
                return true;
            }
        }
    }
    #[cfg(not(windows))]
    let _ = entry;
    false
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use printpdf::{BuiltinFont, Mm, PdfDocument};
    use tempfile::tempdir;

    use super::*;
    use crate::embeddings::DeterministicTestEmbedder;
    use crate::retrieval::{Retriever, SqliteRetriever};

    fn make_pdf(pages: &[&str]) -> Vec<u8> {
        let (document, first_page, first_layer) =
            PdfDocument::new("SeraBot ingestion test", Mm(210.0), Mm(297.0), "Layer 1");
        let font = document
            .add_builtin_font(BuiltinFont::Helvetica)
            .expect("builtin font");
        document
            .get_page(first_page)
            .get_layer(first_layer)
            .use_text(pages[0], 12.0, Mm(20.0), Mm(260.0), &font);
        for text in &pages[1..] {
            let (page, layer) = document.add_page(Mm(210.0), Mm(297.0), "Layer 1");
            document.get_page(page).get_layer(layer).use_text(
                *text,
                12.0,
                Mm(20.0),
                Mm(260.0),
                &font,
            );
        }
        document.save_to_bytes().expect("PDF bytes")
    }

    fn make_blank_pdf() -> Vec<u8> {
        let (document, _, _) = PdfDocument::new("blank", Mm(210.0), Mm(297.0), "Layer 1");
        document.save_to_bytes().expect("blank PDF bytes")
    }

    fn mark_encrypted(pdf: Vec<u8>) -> Vec<u8> {
        let mut document = lopdf::Document::load_mem(&pdf).expect("generated PDF parses");
        let mut dictionary = lopdf::Dictionary::new();
        dictionary.set("Filter", lopdf::Object::Name(b"Standard".to_vec()));
        dictionary.set("V", lopdf::Object::Integer(1));
        dictionary.set("R", lopdf::Object::Integer(2));
        dictionary.set(
            "O",
            lopdf::Object::String(vec![0u8; 32], lopdf::StringFormat::Literal),
        );
        dictionary.set(
            "U",
            lopdf::Object::String(vec![0u8; 32], lopdf::StringFormat::Literal),
        );
        dictionary.set("P", lopdf::Object::Integer(-4));
        let encrypt_id = document.add_object(lopdf::Object::Dictionary(dictionary));
        document
            .trailer
            .set("Encrypt", lopdf::Object::Reference(encrypt_id));
        let mut output = Vec::new();
        document
            .save_to(&mut output)
            .expect("encrypted marker PDF bytes");
        output
    }

    fn test_config() -> DocumentsConfig {
        DocumentsConfig {
            max_bytes: 4 * 1024 * 1024,
            chunk_chars: 200,
            chunk_overlap: 20,
            ..DocumentsConfig::default()
        }
    }

    struct FailingEmbedder;

    #[async_trait::async_trait]
    impl Embedder for FailingEmbedder {
        async fn embed(&self, _texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
            Err(SeraError::External("synthetic embedding failure".into()))
        }

        fn dimension(&self) -> usize {
            384
        }

        fn model_id(&self) -> &str {
            "test-failing"
        }
    }

    fn vector_count(store: &Store) -> i64 {
        store
            .connection()
            .expect("connection")
            .query_row("SELECT COUNT(*) FROM chunk_vectors", [], |row| row.get(0))
            .expect("vector count")
    }

    #[tokio::test]
    async fn ingests_digital_pdf_with_pages_and_deduplicates_by_sha() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("sample.pdf");
        std::fs::write(
            &path,
            make_pdf(&["alpha project brief", "omega closing notes"]),
        )
        .expect("write PDF");

        let store = Store::open_in_memory(384).expect("store");
        let embedder = DeterministicTestEmbedder::new(384);

        let first = ingest_file(&store, &embedder, &path, &test_config())
            .await
            .expect("first ingestion");
        assert!(!first.duplicate);
        assert_eq!(first.pages, 2);
        assert_eq!(first.chunks, 2);
        assert_eq!(first.sha256.len(), 64);
        assert_eq!(vector_count(&store), 2);

        let second = ingest_file(&store, &embedder, &path, &test_config())
            .await
            .expect("duplicate ingestion");
        assert!(second.duplicate);
        assert_eq!(second.document_id, first.document_id);
        assert_eq!(second.sha256, first.sha256);
        assert_eq!(second.pages, 2);
        assert_eq!(second.chunks, 2);
        // Deduplication must not re-embed.
        assert_eq!(vector_count(&store), 2);

        let connection = store.connection().expect("connection");
        let pages: Vec<i64> = {
            let mut statement = connection
                .prepare("SELECT page FROM chunks WHERE document_id = ?1 ORDER BY ordinal")
                .expect("prepare");
            statement
                .query_map(params![first.document_id], |row| row.get(0))
                .expect("query")
                .collect::<std::result::Result<_, _>>()
                .expect("pages")
        };
        assert_eq!(pages, vec![1, 2]);
    }

    #[tokio::test]
    async fn retrieval_returns_exact_page_provenance() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("evidence.pdf");
        std::fs::write(
            &path,
            make_pdf(&["alpha project brief", "omega closing notes"]),
        )
        .expect("write PDF");

        let store = Store::open_in_memory(384).expect("store");
        let embedder = DeterministicTestEmbedder::new(384);
        let ingested = ingest_file(&store, &embedder, &path, &test_config())
            .await
            .expect("ingestion");

        let retriever = SqliteRetriever::new(
            store.clone(),
            Arc::new(DeterministicTestEmbedder::new(384)),
            8,
        );
        let chunks = retriever.retrieve("alpha", 4).await.expect("retrieval");
        assert!(!chunks.is_empty());
        let hit = chunks
            .iter()
            .find(|chunk| chunk.text.contains("alpha"))
            .expect("alpha chunk");
        assert_eq!(hit.document_id, ingested.document_id);
        assert_eq!(hit.document_title, "evidence");
        assert_eq!(hit.page, 1);
        assert!(hit.score.is_finite());
        assert!(hit.score >= 0.0);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.page == 1 || chunk.page == 2)
        );
    }

    #[tokio::test]
    async fn rejects_scanned_malformed_encrypted_and_oversized_documents() {
        let directory = tempdir().expect("tempdir");
        let store = Store::open_in_memory(384).expect("store");
        let embedder = DeterministicTestEmbedder::new(384);
        let config = test_config();

        let scanned = directory.path().join("scanned.pdf");
        std::fs::write(&scanned, make_blank_pdf()).expect("blank PDF");
        let error = ingest_file(&store, &embedder, &scanned, &config)
            .await
            .expect_err("scanned PDF rejected");
        assert!(error.to_string().contains("no extractable text"));

        let malformed = directory.path().join("malformed.pdf");
        std::fs::write(&malformed, b"%PDF-1.7\nthis is not a real PDF").expect("malformed PDF");
        assert!(
            ingest_file(&store, &embedder, &malformed, &config)
                .await
                .is_err()
        );

        let encrypted = directory.path().join("encrypted.pdf");
        std::fs::write(&encrypted, mark_encrypted(make_pdf(&["secret alpha text"])))
            .expect("encrypted PDF");
        let error = ingest_file(&store, &embedder, &encrypted, &config)
            .await
            .expect_err("encrypted PDF rejected");
        assert!(error.to_string().to_lowercase().contains("encrypted"));

        let oversized = directory.path().join("oversized.txt");
        std::fs::write(&oversized, "x".repeat(1024)).expect("oversized text");
        let mut small = config.clone();
        small.max_bytes = 64;
        assert!(
            ingest_file(&store, &embedder, &oversized, &small)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn marks_document_failed_when_embedding_fails() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("failure.txt");
        std::fs::write(&path, "alpha content that cannot be embedded").expect("write");

        let store = Store::open_in_memory(384).expect("store");
        let error = ingest_file(&store, &FailingEmbedder, &path, &test_config())
            .await
            .expect_err("embedding failure propagates");
        assert!(error.to_string().contains("synthetic embedding failure"));

        let connection = store.connection().expect("connection");
        let (status, stored_error): (String, Option<String>) = connection
            .query_row(
                "SELECT status, error FROM documents WHERE source_path LIKE '%failure.txt'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("failed document row");
        assert_eq!(status, "failed");
        assert!(
            stored_error
                .unwrap_or_default()
                .contains("synthetic embedding failure")
        );
    }

    #[test]
    fn chunker_stays_on_utf8_boundaries_and_respects_bounds() {
        let text = format!("{}{}{}", "é".repeat(17), "漢字かな".repeat(23), "🙂");
        let ranges = chunk_ranges(&text, 11, 3);
        assert!(!ranges.is_empty());
        assert_eq!(ranges.first().map(|range| range.0), Some(0));
        assert_eq!(ranges.last().map(|range| range.1), Some(text.len()));
        for (start, end) in ranges {
            assert!(text.is_char_boundary(start));
            assert!(text.is_char_boundary(end));
            assert!(end > start);
            assert!(text[start..end].chars().count() <= 11);
        }
    }

    #[tokio::test]
    async fn ingests_utf8_markdown_without_splitting_characters() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("notes.md");
        let content = format!("# Città\n\n{}", "perché 🙂 ".repeat(60));
        std::fs::write(&path, &content).expect("write markdown");

        let store = Store::open_in_memory(384).expect("store");
        let embedder = DeterministicTestEmbedder::new(384);
        let config = DocumentsConfig {
            chunk_chars: 40,
            chunk_overlap: 7,
            ..DocumentsConfig::default()
        };
        let result = ingest_file(&store, &embedder, &path, &config)
            .await
            .expect("markdown ingestion");
        assert_eq!(result.pages, 1);
        assert!(result.chunks > 1);

        let connection = store.connection().expect("connection");
        let texts: Vec<String> = {
            let mut statement = connection
                .prepare("SELECT text FROM chunks WHERE document_id = ?1 ORDER BY ordinal")
                .expect("prepare");
            statement
                .query_map(params![result.document_id], |row| row.get(0))
                .expect("query")
                .collect::<std::result::Result<_, _>>()
                .expect("texts")
        };
        assert!(texts.iter().all(|text| text.chars().count() <= 40));
        assert!(texts.iter().any(|text| text.contains('🙂')));
    }

    #[tokio::test]
    async fn index_directory_is_one_level_and_skips_hidden_or_unsupported() {
        let directory = tempdir().expect("tempdir");
        std::fs::write(directory.path().join("one.txt"), "alpha one").expect("write");
        std::fs::write(directory.path().join("two.md"), "omega two").expect("write");
        std::fs::write(directory.path().join(".hidden.txt"), "hidden").expect("write");
        std::fs::write(directory.path().join("table.csv"), "a,b").expect("write");
        std::fs::create_dir(directory.path().join("nested")).expect("nested dir");
        std::fs::write(directory.path().join("nested").join("three.txt"), "nested")
            .expect("write nested");

        let store = Store::open_in_memory(384).expect("store");
        let embedder = DeterministicTestEmbedder::new(384);
        let report = index_directory(&store, &embedder, directory.path(), &test_config())
            .await
            .expect("directory ingestion");
        assert_eq!(report.ingested.len(), 2);
        assert!(report.failures.is_empty());
        let count: i64 = store
            .connection()
            .expect("connection")
            .query_row("SELECT COUNT(*) FROM documents", [], |row| row.get(0))
            .expect("document count");
        assert_eq!(count, 2);
    }
}
