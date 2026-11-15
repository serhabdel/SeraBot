//! Server-rendered artifact previews (0.28.0, phase C3):
//! `GET /v1/artifacts/{hash}/preview` returns a JSON envelope with metadata
//! and a preview payload for the formats the server can render — pdf (page
//! text), pptx (slide text), xlsx (first rows as a JSON grid), docx (first
//! blocks), md (text). The artifact store is read only; ownership is gated
//! exactly like the download endpoint, so a foreign hash is a 404.

use std::panic::{AssertUnwindSafe, catch_unwind};

use super::*;

/// Characters of text a preview payload carries at most.
pub(super) const MAX_PREVIEW_CHARS: usize = 8_000;
/// Rows/columns of a spreadsheet grid preview.
const GRID_MAX_ROWS: usize = 50;
const GRID_MAX_COLS: usize = 26;
/// Blocks of a docx preview.
const DOCX_MAX_BLOCKS: usize = 50;

#[derive(Debug, Serialize)]
struct PreviewPayload {
    /// Payload media type: `text/plain; charset=utf-8` or `application/json`.
    content_type: &'static str,
    /// `text` payloads arrive as a string, `json` payloads as a value.
    encoding: &'static str,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    truncated: bool,
    preview: Value,
}

/// `GET /v1/artifacts/{hash}/preview`: the acting user's own artifact only.
pub(super) async fn artifact_preview(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(hash): Path<String>,
) -> Result<Json<Value>> {
    // Same gate as the download endpoint: another user's artifact is
    // indistinguishable from a missing one.
    if !state.store.user_owns_artifact(&user.id, &hash)? {
        return Err(SeraError::NotFound("artifact".into()));
    }
    let artifact = state.artifacts.load(&hash)?;
    let extension = std::path::Path::new(&artifact.filename)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let (kind, payload) = match extension.as_str() {
        "pdf" => (
            "pdf",
            pdf_preview(&artifact.bytes, &artifact.meta.title, &artifact.filename)?,
        ),
        "pptx" => ("pptx", pptx_preview(&artifact.bytes)?),
        "xlsx" | "xlsm" => ("xlsx", xlsx_preview(&artifact.bytes)?),
        "docx" => ("docx", docx_preview(&artifact.bytes)?),
        "md" | "markdown" => ("md", md_preview(&artifact.bytes)),
        _ => {
            return Err(SeraError::Validation(
                "no server preview for this artifact type".into(),
            ));
        }
    };
    let text = human_readable_text(kind, &payload);
    Ok(Json(json!({
        "hash": artifact.hash,
        "filename": artifact.filename,
        "title": artifact.meta.title,
        "kind": kind,
        "created_at": artifact.meta.created_at,
        "content_type": "text/plain; charset=utf-8",
        "text": text,
        "payload": payload,
    })))
}

fn text_payload(text: String, truncated: bool) -> PreviewPayload {
    PreviewPayload {
        content_type: "text/plain; charset=utf-8",
        encoding: "text",
        truncated,
        preview: Value::String(text),
    }
}

fn json_payload(value: Value, truncated: bool) -> PreviewPayload {
    PreviewPayload {
        content_type: "application/json",
        encoding: "json",
        truncated,
        preview: value,
    }
}

fn clip(text: &str, max: usize) -> (String, bool) {
    let mut chars = text.chars();
    let taken: String = chars.by_ref().take(max).collect();
    (taken, chars.next().is_some())
}

fn format_cell_for_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn human_readable_text(kind: &str, payload: &PreviewPayload) -> String {
    let raw = match &payload.preview {
        Value::String(text) => text.clone(),
        Value::Object(obj) => match kind {
            "pdf" => obj
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            "xlsx" => {
                let Some(sheet) = obj.get("sheet").and_then(Value::as_str) else {
                    return "Empty workbook".to_owned();
                };
                let range = obj
                    .get("range")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                let mut out = if range.is_empty() {
                    format!("Sheet: {sheet}\n")
                } else {
                    format!("Sheet: {sheet} ({range})\n")
                };
                if let Some(rows) = obj.get("rows").and_then(Value::as_array) {
                    let lines: Vec<String> = rows
                        .iter()
                        .filter_map(|row| {
                            let cells = row.as_array()?;
                            let mut strings: Vec<String> =
                                cells.iter().map(format_cell_for_text).collect();
                            while strings.last().is_some_and(|s| s.trim().is_empty()) {
                                strings.pop();
                            }
                            Some(strings.join(" | "))
                        })
                        .collect();
                    out.push_str(&lines.join("\n"));
                }
                out
            }
            "docx" => obj
                .get("blocks")
                .and_then(Value::as_array)
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|b| {
                            b.as_str().or_else(|| b.get("text").and_then(Value::as_str))
                        })
                        .filter(|s| !s.trim().is_empty())
                        .collect::<Vec<_>>()
                        .join("\n\n")
                })
                .unwrap_or_default(),
            _ => obj
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        },
        _ => String::new(),
    };
    clip(&raw, MAX_PREVIEW_CHARS).0
}

/// Best-effort fallback when `extract_pdf_text` yields no text: walk page
/// content streams with `lopdf` and extract printable literal strings from
/// `Tj` / `TJ` / `'` / `"` text-showing operators.
fn extract_pdf_literal_strings(bytes: &[u8]) -> Option<String> {
    let document = catch_unwind(AssertUnwindSafe(|| lopdf::Document::load_mem(bytes)))
        .ok()?
        .ok()?;
    if catch_unwind(AssertUnwindSafe(|| document.is_encrypted())).unwrap_or(true) {
        return None;
    }
    let pages = catch_unwind(AssertUnwindSafe(|| document.get_pages())).ok()?;
    let mut out = Vec::new();
    let mut total_chars = 0usize;

    let push_bytes = |raw: &[u8], page_parts: &mut Vec<String>, total: &mut usize| {
        if *total >= MAX_PREVIEW_CHARS {
            return;
        }
        let decoded = std::str::from_utf8(raw)
            .map(str::to_owned)
            .unwrap_or_else(|_| raw.iter().map(|&b| char::from(b)).collect());
        let cleaned: String = decoded
            .chars()
            .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
            .collect();
        let trimmed = cleaned.trim();
        if !trimmed.is_empty() && trimmed.chars().any(|c| c.is_alphanumeric()) {
            *total += trimmed.chars().count() + 1;
            page_parts.push(trimmed.to_owned());
        }
    };

    for (_, page_id) in pages {
        if total_chars >= MAX_PREVIEW_CHARS {
            break;
        }
        let Ok(content_bytes) =
            catch_unwind(AssertUnwindSafe(|| document.get_page_content(page_id)))
                .unwrap_or(Err(lopdf::Error::ContentDecode))
        else {
            continue;
        };
        let Ok(content) = lopdf::content::Content::decode(&content_bytes) else {
            continue;
        };
        let mut page_parts = Vec::new();
        for op in content.operations {
            match op.operator.as_str() {
                "Tj" | "'" | "\"" => {
                    for operand in &op.operands {
                        if let lopdf::Object::String(raw, _) = operand {
                            push_bytes(raw, &mut page_parts, &mut total_chars);
                        }
                    }
                }
                "TJ" => {
                    if let Some(lopdf::Object::Array(items)) = op.operands.first() {
                        for item in items {
                            if let lopdf::Object::String(raw, _) = item {
                                push_bytes(raw, &mut page_parts, &mut total_chars);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if !page_parts.is_empty() {
            out.push(page_parts.join(" "));
        }
    }
    let joined = out.join("\n");
    (!joined.trim().is_empty()).then_some(joined)
}

/// PDF: page count from the structural inspect, page-1-first text bounded to
/// [`MAX_PREVIEW_CHARS`]. (No rasterizer is linked, so the text form is the
/// preview the server can produce.)
fn pdf_preview(bytes: &[u8], title: &str, filename: &str) -> Result<PreviewPayload> {
    let info = crate::pdf_ops::inspect(bytes)?;
    let mut text = crate::documents::extract_pdf_text(bytes.to_vec()).unwrap_or_default();
    if text.trim().is_empty()
        && let Some(literals) = extract_pdf_literal_strings(bytes)
    {
        text = literals;
    }
    if text.trim().is_empty() {
        let label = [title.trim(), filename.trim()]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or("PDF document");
        text = format!(
            "{label}\nPDF document · {} page{}",
            info.page_count,
            if info.page_count == 1 { "" } else { "s" }
        );
    }
    let (text, truncated) = clip(&text, MAX_PREVIEW_CHARS);
    Ok(json_payload(
        json!({"page_count": info.page_count, "text": text}),
        truncated,
    ))
}

/// PPTX: slide text starting at slide 1, bounded.
#[cfg(feature = "pptx")]
fn pptx_preview(bytes: &[u8]) -> Result<PreviewPayload> {
    let text = crate::pptx_ops::slide_plain_text(bytes, MAX_PREVIEW_CHARS)?;
    let (text, truncated) = clip(&text, MAX_PREVIEW_CHARS);
    Ok(text_payload(text, truncated))
}

#[cfg(not(feature = "pptx"))]
fn pptx_preview(_bytes: &[u8]) -> Result<PreviewPayload> {
    Err(SeraError::Validation(
        "pptx support is not compiled in".into(),
    ))
}

/// XLSX: the first sheet's leading rows as a JSON grid.
fn xlsx_preview(bytes: &[u8]) -> Result<PreviewPayload> {
    let workbook = crate::workbooks::import(bytes)?;
    let Some(sheet) = workbook.sheet_names().first().copied() else {
        return Ok(json_payload(
            json!({"sheet": Value::Null, "rows": [], "truncated": false}),
            false,
        ));
    };
    let read = match crate::workbooks::read_range(bytes, sheet) {
        Ok(read) => read,
        // A used range over the read cap still previews as bounded text.
        Err(_) => {
            let text = crate::workbooks::plain_text(bytes, MAX_PREVIEW_CHARS)?;
            let (text, truncated) = clip(&text, MAX_PREVIEW_CHARS);
            return Ok(text_payload(text, truncated));
        }
    };
    let mut rows: Vec<Vec<Value>> = Vec::new();
    let mut truncated = read.truncated_values > 0;
    for cell in &read.cells {
        let Some((col, row)) = a1_to_col_row(&cell.address) else {
            continue;
        };
        if row > GRID_MAX_ROWS || col > GRID_MAX_COLS {
            truncated = true;
            continue;
        }
        while rows.len() < row {
            rows.push(Vec::new());
        }
        let target = &mut rows[row - 1];
        while target.len() < col {
            target.push(Value::Null);
        }
        target[col - 1] = cell
            .value
            .clone()
            .or_else(|| cell.cached.clone())
            .unwrap_or(Value::Null);
    }
    Ok(json_payload(
        json!({"sheet": read.sheet, "range": read.range, "rows": rows, "truncated": truncated}),
        truncated,
    ))
}

/// `A1` → 1-based (column, row).
fn a1_to_col_row(address: &str) -> Option<(usize, usize)> {
    let mut col = 0usize;
    let mut row = 0usize;
    let mut seen_col = false;
    for c in address.chars() {
        if c.is_ascii_alphabetic() {
            col = col * 26 + (c.to_ascii_uppercase() as usize - 'A' as usize + 1);
            seen_col = true;
        } else if c.is_ascii_digit() {
            row = row * 10 + (c as usize - '0' as usize);
        } else {
            return None;
        }
    }
    (seen_col && row > 0).then_some((col, row))
}

/// DOCX: the first blocks, structured.
#[cfg(feature = "docx")]
fn docx_preview(bytes: &[u8]) -> Result<PreviewPayload> {
    let info = crate::docx_ops::inspect(bytes)?;
    if info.block_count == 0 {
        return Ok(json_payload(json!({"block_count": 0, "blocks": []}), false));
    }
    let last = (info.block_count - 1).min(DOCX_MAX_BLOCKS - 1);
    let text = crate::docx_ops::read_blocks(bytes, 0, last)?;
    Ok(json_payload(
        json!({"block_count": text.block_count, "blocks": text.blocks}),
        info.block_count > DOCX_MAX_BLOCKS,
    ))
}

#[cfg(not(feature = "docx"))]
fn docx_preview(_bytes: &[u8]) -> Result<PreviewPayload> {
    Err(SeraError::Validation(
        "docx support is not compiled in".into(),
    ))
}

/// Markdown: the rendered text itself.
fn md_preview(bytes: &[u8]) -> PreviewPayload {
    let text = String::from_utf8_lossy(bytes);
    let (text, truncated) = clip(&text, MAX_PREVIEW_CHARS);
    text_payload(text, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{Gateway, ToolRegistry};
    use crate::llm::LlmClient;
    use crate::maker::ArtifactMeta;
    use crate::policy::Policy;
    use crate::runtime::NullNotifier;
    use tower::ServiceExt;

    fn build_pdf(content_stream: &[u8], pages_count: usize) -> Vec<u8> {
        use lopdf::{Dictionary, Document, Object, Stream};
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

        let mut kids = Vec::new();
        for _ in 0..pages_count {
            let content_id =
                document.add_object(Stream::new(Dictionary::new(), content_stream.to_vec()));
            let mut page = Dictionary::new();
            page.set("Type", "Page");
            page.set("Parent", Object::Reference(pages_id));
            page.set("Contents", Object::Reference(content_id));
            page.set("Resources", resources.clone());
            let page_id = document.add_object(page);
            kids.push(Object::Reference(page_id));
        }

        let mut pages = Dictionary::new();
        pages.set("Type", "Pages");
        pages.set("Kids", Object::Array(kids));
        pages.set("Count", Object::Integer(pages_count as i64));
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
        let mut catalog = Dictionary::new();
        catalog.set("Type", "Catalog");
        catalog.set("Pages", Object::Reference(pages_id));
        let catalog_id = document.add_object(catalog);
        document.trailer.set("Root", Object::Reference(catalog_id));
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    fn preview_test_app() -> (Router, Store, Arc<crate::maker::ArtifactStore>, String) {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.keep();
        let mut config = Config::default();
        config.api.enabled = true;
        config.api.token_hash = sha256_hex("test-token-that-is-long-enough-123");
        config.llm.model = "test".into();
        config.llm.base_url = "https://example.com/v1".into();
        config.base_dir = base.clone();
        let db_path = base.join("serabot.db");
        config.database.path = db_path.to_string_lossy().into_owned();
        let config = Arc::new(config);
        let store = Store::open(&db_path, 384, 5000).unwrap();
        let gateway = Gateway::new(
            store.clone(),
            Arc::new(Policy::empty()),
            ToolRegistry::default(),
            1,
            false,
        );
        let llm = LlmClient::new("https://example.com/v1", "test", 5, 50)
            .unwrap()
            .with_model("test");
        let runtime = Runtime::new(
            config.clone(),
            store.clone(),
            gateway,
            crate::model_router::ModelRouter::new(
                &config.llm,
                BTreeMap::from([("default".into(), llm)]),
            )
            .unwrap(),
            vec![crate::config::LoadedBot {
                config: BotConfig {
                    title: None,
                    description: None,
                    name: "main".into(),
                    trigger: "command".into(),
                    model: None,
                    system_prompt: "prompts/main.txt".into(),
                    skills: vec![],
                    tools: vec![],
                    require_approval: vec![],
                    enabled: true,
                    temperature: None,
                    thinking: None,
                    role: BotRole::Orchestrator,
                    delegates: vec![],
                },
                prompt: "test prompt".into(),
            }],
            Arc::new(NullNotifier),
            tokio_util::sync::CancellationToken::new(),
        )
        .unwrap();
        let artifacts = Arc::new(crate::maker::ArtifactStore::new(base.join("artifacts")).unwrap());
        let secrets =
            SharedSecrets::new(crate::secrets::SecretStore::from_map(BTreeMap::from([(
                config.llm.api_key_secret.clone(),
                "synthetic-llm".into(),
            )])));
        let users = Arc::new(UserSecrets::with_master_key(store.clone(), [8u8; 32]).unwrap());
        let state = ApiState::new(
            runtime,
            store.clone(),
            config,
            artifacts.clone(),
            secrets,
            users,
            None,
        )
        .unwrap();
        let user_id = store.admin_user_id().unwrap();
        store
            .create_conversation(&user_id, "dir:main", "direct", None)
            .unwrap();
        (crate::api::router(state), store, artifacts, user_id)
    }

    async fn fetch_preview(
        app: &Router,
        store: &Store,
        artifacts: &Arc<crate::maker::ArtifactStore>,
        user_id: &str,
        bytes: Vec<u8>,
        filename: &str,
        title: &str,
    ) -> Value {
        let meta = ArtifactMeta {
            title: title.to_owned(),
            filename: filename.to_owned(),
            language: crate::domain::Language::En,
            source_artifact: None,
            created_at: Utc::now(),
        };
        let stored = artifacts.save(&bytes, filename, meta).unwrap();
        store
            .record_conversation_artifact(
                user_id,
                "dir:main",
                None,
                "test_tool",
                &stored.hash,
                filename,
                None,
            )
            .unwrap();
        let token = store
            .mint_session(user_id, chrono::Duration::hours(1))
            .unwrap();
        let resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri(format!("/v1/artifacts/{}/preview", stored.hash))
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{filename}");
        let body = axum::body::to_bytes(resp.into_body(), 256 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[test]
    fn pdf_preview_synthesizes_summary_when_pdf_has_no_text_layer() {
        // Pure vector path (like outline-rendered deck PDFs): no Tj/TJ text.
        let vector_pdf = build_pdf(b"0 0 100 100 re f", 2);
        let payload = pdf_preview(&vector_pdf, "Q4 Strategy Deck", "deck.pdf").unwrap();
        assert_eq!(payload.content_type, "application/json");
        assert_eq!(payload.preview["page_count"], 2);
        assert_eq!(
            payload.preview["text"],
            "Q4 Strategy Deck\nPDF document · 2 pages"
        );

        let one_page_pdf = build_pdf(b"0 0 50 50 re f", 1);
        let one_payload = pdf_preview(&one_page_pdf, "", "single.pdf").unwrap();
        assert_eq!(
            one_payload.preview["text"],
            "single.pdf\nPDF document · 1 page"
        );
    }

    #[tokio::test]
    async fn artifact_preview_includes_payload_and_top_level_content_type_and_text_for_all_types() {
        let (app, store, artifacts, user_id) = preview_test_app();

        // 1. PDF with text
        let pdf_text = build_pdf(b"BT /F1 12 Tf 20 100 Td (Hello PDF) Tj ET", 1);
        let pdf_json = fetch_preview(
            &app,
            &store,
            &artifacts,
            &user_id,
            pdf_text,
            "report.pdf",
            "Report",
        )
        .await;
        assert_eq!(pdf_json["kind"], "pdf");
        assert_eq!(pdf_json["content_type"], "text/plain; charset=utf-8");
        assert!(pdf_json["text"].as_str().unwrap().contains("Hello PDF"));
        assert!(
            pdf_json["payload"]["preview"]["text"]
                .as_str()
                .unwrap()
                .contains("Hello PDF")
        );

        // 2. PDF without text layer (vector/outline fallback)
        let pdf_vector = build_pdf(b"0 0 80 80 re S", 3);
        let pdf_vec_json = fetch_preview(
            &app,
            &store,
            &artifacts,
            &user_id,
            pdf_vector,
            "deck.pdf",
            "Outline Deck",
        )
        .await;
        assert_eq!(pdf_vec_json["kind"], "pdf");
        assert_eq!(pdf_vec_json["content_type"], "text/plain; charset=utf-8");
        assert_eq!(pdf_vec_json["text"], "Outline Deck\nPDF document · 3 pages");
        assert_eq!(
            pdf_vec_json["payload"]["preview"]["text"],
            "Outline Deck\nPDF document · 3 pages"
        );

        // 3. PPTX
        let pptx_bytes = {
            use crate::deck::{Brand, BrandPalette, DeckSection, DeckSpec};
            let brand = Brand {
                name: "Test".into(),
                palette: BrandPalette {
                    background: "#FFFFFF".into(),
                    foreground: "#111111".into(),
                    accent: "#2F6F6A".into(),
                    muted: "#6B7280".into(),
                },
                monogram_text: "T".into(),
                footer: None,
                font_family: Some("helvetica".into()),
            };
            let spec = DeckSpec {
                title: "Test Deck".into(),
                subtitle: None,
                language: crate::domain::Language::En,
                date_label: None,
                author: None,
                sections: vec![DeckSection {
                    part_title: None,
                    title: "Section One".into(),
                    cards: vec![],
                    bullets: vec!["Bullet one".into()],
                }],
                footer: None,
            };
            crate::deck_pptx::render_deck_pptx(&spec, &brand, 10)
                .unwrap()
                .bytes
        };
        let pptx_json = fetch_preview(
            &app,
            &store,
            &artifacts,
            &user_id,
            pptx_bytes,
            "slides.pptx",
            "Slides",
        )
        .await;
        assert_eq!(pptx_json["kind"], "pptx");
        assert_eq!(pptx_json["content_type"], "text/plain; charset=utf-8");
        assert!(
            pptx_json["text"]
                .as_str()
                .unwrap()
                .contains("--- slide 1 ---")
        );
        assert!(
            pptx_json["payload"]["preview"]
                .as_str()
                .unwrap()
                .contains("--- slide 1 ---")
        );

        // 4. XLSX
        let xlsx_bytes = {
            use crate::workbooks::{CellValue, SheetSpec, WorkbookSpec};
            crate::workbooks::build(&WorkbookSpec {
                title: "Sheet".into(),
                language: None,
                sheets: vec![SheetSpec {
                    name: "Data".into(),
                    header: vec!["item".into(), "qty".into()],
                    rows: vec![vec![
                        CellValue::Text("widgets".into()),
                        CellValue::Number(5.0),
                    ]],
                }],
            })
            .unwrap()
        };
        let xlsx_json = fetch_preview(
            &app,
            &store,
            &artifacts,
            &user_id,
            xlsx_bytes,
            "sheet.xlsx",
            "Sheet",
        )
        .await;
        assert_eq!(xlsx_json["kind"], "xlsx");
        assert_eq!(xlsx_json["content_type"], "text/plain; charset=utf-8");
        let xlsx_text = xlsx_json["text"].as_str().unwrap();
        assert!(xlsx_text.contains("Sheet: Data"), "{xlsx_text}");
        assert!(xlsx_text.contains("widgets | 5"), "{xlsx_text}");
        assert_eq!(xlsx_json["payload"]["preview"]["sheet"], "Data");

        // 5. DOCX
        let docx_bytes = {
            use crate::docx_ops::{DocxBlockSpec, DocxSpec};
            crate::docx_ops::build(&DocxSpec {
                title: "Doc".into(),
                subtitle: None,
                language: None,
                blocks: vec![DocxBlockSpec {
                    kind: "paragraph".into(),
                    level: None,
                    text: Some("Docx body text.".into()),
                    items: vec![],
                    header: vec![],
                    rows: vec![],
                }],
            })
            .unwrap()
        };
        let docx_json = fetch_preview(
            &app, &store, &artifacts, &user_id, docx_bytes, "doc.docx", "Doc",
        )
        .await;
        assert_eq!(docx_json["kind"], "docx");
        assert_eq!(docx_json["content_type"], "text/plain; charset=utf-8");
        assert!(
            docx_json["text"]
                .as_str()
                .unwrap()
                .contains("Docx body text.")
        );
        assert!(docx_json["payload"]["preview"]["blocks"].is_array());

        // 6. MD
        let md_json = fetch_preview(
            &app,
            &store,
            &artifacts,
            &user_id,
            b"# Markdown Heading\nBody".to_vec(),
            "notes.md",
            "Notes",
        )
        .await;
        assert_eq!(md_json["kind"], "md");
        assert_eq!(md_json["content_type"], "text/plain; charset=utf-8");
        assert_eq!(md_json["text"], "# Markdown Heading\nBody");
        assert_eq!(md_json["payload"]["preview"], "# Markdown Heading\nBody");
    }
}
