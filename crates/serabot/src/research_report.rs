//! Research reports (0.70): `research_report_build`.
//!
//! The researcher bot does its own searching and reading, then hands this tool
//! a bounded spec: a title, the question, a summary, sections whose text cites
//! sources as `[n]`, and the numbered sources. The tool validates the spec
//! (every cited number must exist, URLs are http(s) only, counts are bounded),
//! builds a Word document with the deterministic DOCX builder, and optionally
//! renders it to PDF. The tool fetches nothing: URLs are written as text.
//!
//! The gateway scans the arguments and the result against the blocklist, like
//! every other document builder.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::docx_ops::{DocxBlockSpec, DocxSpec, build, inspect, load};
use crate::domain::Language;
use crate::error::{Result, SeraError};
use crate::gateway::{Tool, ToolContext, ToolOutput, ToolSpec};
use crate::maker::{ArtifactMeta, ArtifactStore};
use crate::store::Store;

pub const MAX_SOURCES: usize = 25;
pub const MAX_SECTIONS: usize = 20;
pub const MAX_PARAGRAPHS_PER_SECTION: usize = 12;
pub const MAX_PARAGRAPH_CHARS: usize = 4000;
pub const MAX_SUMMARY_CHARS: usize = 3000;
pub const MAX_QUESTION_CHARS: usize = 500;
pub const MAX_QUOTE_CHARS: usize = 300;
pub const MAX_URL_CHARS: usize = 2000;
const MAX_FIELD_CHARS: usize = 200;

fn invalid(message: impl Into<String>) -> SeraError {
    SeraError::Validation(message.into())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportSection {
    pub heading: String,
    pub paragraphs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportSource {
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub publisher: Option<String>,
    /// Date the page was read, `YYYY-MM-DD`.
    pub accessed: String,
    #[serde(default)]
    pub quote: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportSpec {
    pub title: String,
    pub question: String,
    pub summary: String,
    pub sections: Vec<ReportSection>,
    pub sources: Vec<ReportSource>,
    #[serde(default)]
    pub language: Option<Language>,
}

/// Citation numbers written as `[3]` or `[1, 2]` in a text.
fn citations(text: &str) -> Vec<usize> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else { break };
        let inner = &after[..close];
        if !inner.is_empty()
            && inner.len() <= 24
            && inner
                .chars()
                .all(|c| c.is_ascii_digit() || c == ',' || c == ' ')
        {
            for part in inner.split(',') {
                if let Ok(n) = part.trim().parse::<usize>() {
                    found.push(n);
                }
            }
        }
        rest = &after[close + 1..];
    }
    found
}

fn check_url(label: &str, url: &str) -> Result<()> {
    if url.is_empty() || url.chars().count() > MAX_URL_CHARS {
        return Err(invalid(format!(
            "{label} must be 1-{MAX_URL_CHARS} characters"
        )));
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid(format!("{label} must not contain spaces")));
    }
    let lower = url.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .ok_or_else(|| invalid(format!("{label} must start with http:// or https://")))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err(invalid(format!("{label} has no valid host")));
    }
    Ok(())
}

fn check_date(label: &str, value: &str) -> Result<()> {
    let bytes = value.as_bytes();
    let ok = bytes.len() == 10
        && bytes.iter().enumerate().all(|(i, b)| match i {
            4 | 7 => *b == b'-',
            _ => b.is_ascii_digit(),
        })
        && chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok();
    if !ok {
        return Err(invalid(format!("{label} must be a date like 2026-11-20")));
    }
    Ok(())
}

fn check_len(label: &str, value: &str, max: usize) -> Result<()> {
    if value.trim().is_empty() {
        return Err(invalid(format!("{label} must not be empty")));
    }
    if value.chars().count() > max {
        return Err(invalid(format!("{label} must be at most {max} characters")));
    }
    Ok(())
}

/// Validate a report spec. Returns the sorted list of source numbers that no
/// text cites, so the model can be told (an uncited source is allowed).
pub fn validate(spec: &ReportSpec) -> Result<Vec<usize>> {
    check_len("title", &spec.title, MAX_FIELD_CHARS)?;
    check_len("question", &spec.question, MAX_QUESTION_CHARS)?;
    check_len("summary", &spec.summary, MAX_SUMMARY_CHARS)?;
    if spec.sections.is_empty() {
        return Err(invalid("sections must contain at least one section"));
    }
    if spec.sections.len() > MAX_SECTIONS {
        return Err(invalid(format!(
            "sections has {}; the limit is {MAX_SECTIONS}",
            spec.sections.len()
        )));
    }
    if spec.sources.is_empty() {
        return Err(invalid("sources must contain at least one source"));
    }
    if spec.sources.len() > MAX_SOURCES {
        return Err(invalid(format!(
            "sources has {}; the limit is {MAX_SOURCES}",
            spec.sources.len()
        )));
    }
    for (i, source) in spec.sources.iter().enumerate() {
        let n = i + 1;
        check_len(
            &format!("sources[{n}].title"),
            &source.title,
            MAX_FIELD_CHARS,
        )?;
        check_url(&format!("sources[{n}].url"), &source.url)?;
        check_date(&format!("sources[{n}].accessed"), &source.accessed)?;
        if let Some(publisher) = &source.publisher
            && publisher.chars().count() > MAX_FIELD_CHARS
        {
            return Err(invalid(format!(
                "sources[{n}].publisher must be at most {MAX_FIELD_CHARS} characters"
            )));
        }
        if let Some(quote) = &source.quote
            && quote.chars().count() > MAX_QUOTE_CHARS
        {
            return Err(invalid(format!(
                "sources[{n}].quote must be at most {MAX_QUOTE_CHARS} characters"
            )));
        }
    }

    let mut cited = BTreeSet::new();
    let mut note = |label: &str, text: &str| -> Result<()> {
        for n in citations(text) {
            if n == 0 || n > spec.sources.len() {
                return Err(invalid(format!(
                    "{label} cites [{n}] but there are only {} sources",
                    spec.sources.len()
                )));
            }
            cited.insert(n);
        }
        Ok(())
    };
    note("summary", &spec.summary)?;
    for (i, section) in spec.sections.iter().enumerate() {
        let label = format!("sections[{}]", i + 1);
        check_len(
            &format!("{label}.heading"),
            &section.heading,
            MAX_FIELD_CHARS,
        )?;
        if section.paragraphs.is_empty() || section.paragraphs.len() > MAX_PARAGRAPHS_PER_SECTION {
            return Err(invalid(format!(
                "{label}.paragraphs must have 1-{MAX_PARAGRAPHS_PER_SECTION} paragraphs"
            )));
        }
        for (j, paragraph) in section.paragraphs.iter().enumerate() {
            let plabel = format!("{label}.paragraphs[{}]", j + 1);
            check_len(&plabel, paragraph, MAX_PARAGRAPH_CHARS)?;
            note(&plabel, paragraph)?;
        }
    }
    if cited.is_empty() {
        return Err(invalid(
            "the report cites no source; add [n] markers that point at the sources",
        ));
    }
    Ok((1..=spec.sources.len())
        .filter(|n| !cited.contains(n))
        .collect())
}

fn block(kind: &str, level: Option<u8>, text: String) -> DocxBlockSpec {
    DocxBlockSpec {
        kind: kind.into(),
        level,
        text: Some(text),
        items: Vec::new(),
        header: Vec::new(),
        rows: Vec::new(),
    }
}

/// Turn a validated report spec into the DOCX builder's spec.
pub fn to_docx_spec(spec: &ReportSpec, date: &str) -> DocxSpec {
    let mut blocks = vec![
        block(
            "paragraph",
            None,
            format!("Question: {}", spec.question.trim()),
        ),
        block("heading", Some(1), "Summary".into()),
    ];
    for part in spec.summary.split("\n\n").filter(|p| !p.trim().is_empty()) {
        blocks.push(block("paragraph", None, part.trim().to_owned()));
    }
    for section in &spec.sections {
        blocks.push(block("heading", Some(1), section.heading.trim().to_owned()));
        for paragraph in &section.paragraphs {
            blocks.push(block("paragraph", None, paragraph.trim().to_owned()));
        }
    }
    blocks.push(block("heading", Some(1), "Sources".into()));
    for (i, source) in spec.sources.iter().enumerate() {
        let mut line = format!("[{}] {}.", i + 1, source.title.trim());
        if let Some(publisher) = source.publisher.as_deref().filter(|p| !p.trim().is_empty()) {
            line.push_str(&format!(" {}.", publisher.trim()));
        }
        line.push_str(&format!(" Read on {}. {}", source.accessed, source.url));
        blocks.push(block("paragraph", None, line));
        if let Some(quote) = source.quote.as_deref().filter(|q| !q.trim().is_empty()) {
            blocks.push(block(
                "paragraph",
                None,
                format!("Quote: \u{201c}{}\u{201d}", quote.trim()),
            ));
        }
    }
    DocxSpec {
        title: spec.title.trim().to_owned(),
        subtitle: Some(format!("Research report, {date}")),
        language: spec.language,
        blocks,
    }
}

/// Validate and build the report. Returns the DOCX bytes and the numbers of
/// sources no text cites.
pub fn build_report(spec: &ReportSpec, date: &str) -> Result<(Vec<u8>, Vec<usize>)> {
    let uncited = validate(spec)?;
    let bytes = build(&to_docx_spec(spec, date))?;
    Ok((bytes, uncited))
}

pub struct ResearchReportBuildTool {
    artifacts: Arc<ArtifactStore>,
    store: Store,
}

impl ResearchReportBuildTool {
    pub fn new(artifacts: Arc<ArtifactStore>, store: Store) -> Self {
        Self { artifacts, store }
    }
}

#[async_trait]
impl Tool for ResearchReportBuildTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "research_report_build".into(),
            description: "Build a research report (Word, optionally PDF) from sources you have \
                 already read. Give title, question, summary, sections [{heading, paragraphs}] and \
                 sources [{title, url, publisher?, accessed (YYYY-MM-DD), quote? (max 300 chars)}]. \
                 Cite sources in the text as [1], [2]; every cited number must exist in sources. \
                 Up to 25 sources and 20 sections; http(s) URLs only. Cite only pages you read. \
                 Set pdf true for a PDF as well."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "title": {"type": "string"},
                    "question": {"type": "string"},
                    "summary": {"type": "string", "description": "Short answer; blank line between paragraphs"},
                    "sections": {"type": "array", "maxItems": MAX_SECTIONS, "items": {
                        "type": "object",
                        "properties": {
                            "heading": {"type": "string"},
                            "paragraphs": {"type": "array", "items": {"type": "string"}}
                        },
                        "required": ["heading", "paragraphs"]
                    }},
                    "sources": {"type": "array", "maxItems": MAX_SOURCES, "items": {
                        "type": "object",
                        "properties": {
                            "title": {"type": "string"},
                            "url": {"type": "string"},
                            "publisher": {"type": "string"},
                            "accessed": {"type": "string"},
                            "quote": {"type": "string"}
                        },
                        "required": ["title", "url", "accessed"]
                    }},
                    "language": {"type": "string", "description": "it | fr | en"},
                    "pdf": {"type": "boolean"},
                    "filename": {"type": "string", "description": "Optional name ending in .docx"}
                },
                "required": ["title", "question", "summary", "sections", "sources"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let mut object = args
            .as_object()
            .cloned()
            .ok_or_else(|| invalid("arguments must be a JSON object"))?;
        let want_pdf = match object.remove("pdf") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(flag)) => flag,
            Some(_) => return Err(invalid("pdf must be true or false")),
        };
        let filename = match object.remove("filename") {
            None | Some(Value::Null) => None,
            Some(Value::String(name)) if !name.trim().is_empty() => {
                let name = name.trim().to_owned();
                if !name.to_ascii_lowercase().ends_with(".docx")
                    || name.chars().count() > 120
                    || name
                        .chars()
                        .any(|c| matches!(c, '/' | '\\' | ':') || c.is_control())
                    || name.contains("..")
                {
                    return Err(invalid("filename must be a plain name ending in .docx"));
                }
                Some(name)
            }
            Some(_) => return Err(invalid("filename must be text")),
        };
        let spec: ReportSpec = serde_json::from_value(Value::Object(object))
            .map_err(|error| invalid(format!("not a valid report spec: {error}")))?;
        let date = Utc::now().format("%Y-%m-%d").to_string();
        let built = spec.clone();
        let (docx, pdf, uncited, blocks) = tokio::task::spawn_blocking(move || {
            let (docx, uncited) = build_report(&built, &date)?;
            let blocks = inspect(&docx)?.block_count;
            let pdf = if want_pdf {
                let document = load(&docx)?;
                Some(
                    wordcraft_pdf::export(&document, &wordcraft_pdf::PdfOptions::default())
                        .map_err(|error| {
                            SeraError::Internal(format!("cannot render report to PDF: {error}"))
                        })?,
                )
            } else {
                None
            };
            Ok::<_, SeraError>((docx, pdf, uncited, blocks))
        })
        .await
        .map_err(|error| SeraError::Internal(format!("report task failed: {error}")))??;

        let docx_name = filename.unwrap_or_else(|| slug_name(&spec.title));
        let language = spec.language.unwrap_or(Language::En);
        let meta = |name: &str, source: Option<String>| ArtifactMeta {
            title: spec.title.clone(),
            filename: name.to_owned(),
            language,
            source_artifact: source,
            created_at: Utc::now(),
        };
        let stored = self
            .artifacts
            .save(&docx, &docx_name, meta(&docx_name, None))?;
        let mut pdf_hash = None;
        if let Some(pdf) = pdf {
            let pdf_name = format!("{}.pdf", docx_name.trim_end_matches(".docx"));
            let saved =
                self.artifacts
                    .save(&pdf, &pdf_name, meta(&pdf_name, Some(stored.hash.clone())))?;
            crate::maker::list_saved_artifact(&saved);
            pdf_hash = Some(saved.hash);
        }
        self.store.record_audit(
            &context.actor,
            "research.report_built",
            Some("research_report_build"),
            context.task_id.as_deref(),
            None,
            None,
            None,
            Some(&json!({
                "artifact_hash": stored.hash,
                "pdf_hash": pdf_hash,
                "sources": spec.sources.len(),
                "sections": spec.sections.len(),
            })),
        )?;
        let mut content = format!(
            "title: {}\nsources: {}\nsections: {}\nblocks: {blocks}\nfilename: {}\nartifact: {}\n",
            spec.title,
            spec.sources.len(),
            spec.sections.len(),
            stored.filename,
            stored.hash
        );
        if let Some(hash) = &pdf_hash {
            content.push_str(&format!("pdf_artifact: {hash}\n"));
        }
        if !uncited.is_empty() {
            content.push_str(&format!(
                "note: sources never cited in the text: {}\n",
                uncited
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        Ok(ToolOutput {
            content,
            data: Some(json!({
                "artifact_hash": stored.hash,
                "filename": stored.filename,
                "pdf_artifact_hash": pdf_hash,
                "sources": spec.sources.len(),
                "uncited_sources": uncited,
            })),
            artifact_path: Some(format!("artifacts/{}", stored.filename)),
            artifact_hash: Some(stored.hash),
            filename: Some(stored.filename),
            destination: None,
            cards: Vec::new(),
        })
    }
}

fn slug_name(title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(80).collect();
    if slug.is_empty() {
        "research-report.docx".to_owned()
    } else {
        format!("{slug}.docx")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn source(n: usize) -> ReportSource {
        ReportSource {
            title: format!("Source {n}"),
            url: format!("https://example.org/page-{n}"),
            publisher: Some("Example Press".into()),
            accessed: "2026-11-20".into(),
            quote: Some("A short synthetic quote.".into()),
        }
    }

    fn spec() -> ReportSpec {
        ReportSpec {
            title: "Synthetic Deep Dive".into(),
            question: "What is synthetic?".into(),
            summary: "It is fixture data [1].".into(),
            sections: vec![ReportSection {
                heading: "Findings".into(),
                paragraphs: vec![
                    "Sources agree [1, 2].".into(),
                    "They differ on size [2].".into(),
                ],
            }],
            sources: vec![source(1), source(2)],
            language: None,
        }
    }

    #[test]
    fn valid_spec_passes() {
        assert!(validate(&spec()).unwrap().is_empty());
    }

    #[test]
    fn missing_citation_target_is_rejected() {
        let mut s = spec();
        s.sections[0].paragraphs[0] = "Claim [3].".into();
        let error = validate(&s).unwrap_err().to_string();
        assert!(error.contains("[3]"), "{error}");
        s.sections[0].paragraphs[0] = "Claim [0].".into();
        assert!(validate(&s).is_err());
    }

    #[test]
    fn report_without_any_citation_is_rejected() {
        let mut s = spec();
        s.summary = "No marker.".into();
        s.sections[0].paragraphs = vec!["Nothing cited.".into()];
        assert!(validate(&s).is_err());
    }

    #[test]
    fn uncited_sources_are_reported_not_rejected() {
        let mut s = spec();
        s.sources.push(source(3));
        assert_eq!(validate(&s).unwrap(), vec![3]);
    }

    #[test]
    fn bad_urls_are_rejected() {
        for url in [
            "ftp://example.org/x",
            "javascript:alert(1)",
            "https://",
            "https://user:pw@example.org/",
            "https://exa mple.org/",
            "example.org/page",
            "file:///etc/passwd",
        ] {
            let mut s = spec();
            s.sources[0].url = url.into();
            assert!(validate(&s).is_err(), "{url} must be rejected");
        }
    }

    #[test]
    fn bad_date_and_long_quote_are_rejected() {
        let mut s = spec();
        s.sources[0].accessed = "20/11/2026".into();
        assert!(validate(&s).is_err());
        let mut s = spec();
        s.sources[0].quote = Some("q".repeat(MAX_QUOTE_CHARS + 1));
        assert!(validate(&s).is_err());
    }

    #[test]
    fn too_many_sources_or_sections_are_rejected() {
        let mut s = spec();
        s.sources = (1..=MAX_SOURCES + 1).map(source).collect();
        assert!(validate(&s).unwrap_err().to_string().contains("limit"));
        let mut s = spec();
        s.sections = (0..=MAX_SECTIONS)
            .map(|i| ReportSection {
                heading: format!("S{i}"),
                paragraphs: vec!["Text [1].".into()],
            })
            .collect();
        assert!(validate(&s).is_err());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let value = json!({
            "title": "t", "question": "q", "summary": "s [1]",
            "sections": [], "sources": [], "extra": 1
        });
        assert!(serde_json::from_value::<ReportSpec>(value).is_err());
    }

    #[test]
    fn docx_round_trip_has_sources_heading_and_links() {
        let (bytes, _) = build_report(&spec(), "2026-11-20").unwrap();
        let text = crate::docx_ops::plain_text(&bytes, 100_000).unwrap();
        assert!(text.contains("Synthetic Deep Dive"));
        assert!(text.contains("Summary"));
        assert!(text.contains("Sources"));
        assert!(text.contains(
            "[2] Source 2. Example Press. Read on 2026-11-20. https://example.org/page-2"
        ));
        let info = inspect(&bytes).unwrap();
        assert!(info.blocks.iter().any(|b| b.text.trim() == "Sources"));
    }

    fn context() -> ToolContext {
        ToolContext {
            user_id: crate::store::ADMIN_USER_ID.to_owned(),
            actor: "owner".into(),
            bot: "researcher".into(),
            task_id: None,
            turn_id: None,
            conversation_id: None,
            allowed_tools: ["research_report_build".to_string()].into_iter().collect(),
            require_approval: HashSet::new(),
        }
    }

    #[tokio::test]
    async fn tool_builds_docx_and_pdf() {
        let directory = tempfile::tempdir().unwrap();
        let artifacts = Arc::new(ArtifactStore::new(directory.path()).unwrap());
        let store = Store::open_in_memory(384).unwrap();
        let tool = ResearchReportBuildTool::new(artifacts.clone(), store);
        let mut args = serde_json::to_value(spec()).unwrap();
        args["pdf"] = json!(true);
        let output = tool.execute(args, &context()).await.unwrap();
        let hash = output.artifact_hash.clone().unwrap();
        let docx = artifacts.load(&hash).unwrap();
        assert!(docx.filename.ends_with(".docx"));
        let pdf_hash = output.data.unwrap()["pdf_artifact_hash"]
            .as_str()
            .unwrap()
            .to_owned();
        let pdf = artifacts.load(&pdf_hash).unwrap();
        assert!(pdf.bytes.starts_with(b"%PDF"));
    }

    #[tokio::test]
    async fn tool_rejects_bad_spec_and_bad_filename() {
        let directory = tempfile::tempdir().unwrap();
        let artifacts = Arc::new(ArtifactStore::new(directory.path()).unwrap());
        let store = Store::open_in_memory(384).unwrap();
        let tool = ResearchReportBuildTool::new(artifacts, store);
        let mut bad = spec();
        bad.sections[0].paragraphs[0] = "Claim [9].".into();
        let args = serde_json::to_value(bad).unwrap();
        assert!(tool.execute(args, &context()).await.is_err());
        let mut args = serde_json::to_value(spec()).unwrap();
        args["filename"] = json!("../x.docx");
        assert!(tool.execute(args, &context()).await.is_err());
    }
}
