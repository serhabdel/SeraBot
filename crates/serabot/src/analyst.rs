//! Document analysis and transcript extraction.
//!
//! Everything in this module is deterministic and extractive: decisions,
//! actions and questions are copied from the source text, never generated.
//! Higher-level synthesis happens outside this module, using
//! [`synthesis_outline`] as bounded grounding input.

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Result, SeraError};
use crate::util::truncate_chars;

/// Maximum number of characters analyzed from a transcript.
const MAX_TRANSCRIPT_CHARS: usize = 200_000;
/// Maximum characters in a single transcript line.
const MAX_LINE_CHARS: usize = 4_000;
/// Maximum number of items kept per list.
const MAX_ITEMS: usize = 100;
/// Maximum characters for a section/heading line.
const MAX_SECTION_CHARS: usize = 60;
/// Maximum number of pages read from one PDF.
const MAX_PDF_PAGES: usize = 2_000;
/// Maximum number of outline candidates taken from one page.
const MAX_OUTLINE_CANDIDATES_PER_PAGE: usize = 12;
/// Maximum characters of a single sentence kept in an outline.
const MAX_OUTLINE_SENTENCE_CHARS: usize = 400;

/// One extracted action, with only explicitly labelled owner and due values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionItem {
    pub text: String,
    pub owner: Option<String>,
    pub due: Option<String>,
}

/// Deterministic transcript extraction result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptAnalysis {
    pub decisions: Vec<String>,
    pub actions: Vec<ActionItem>,
    pub open_questions: Vec<String>,
}

/// Text extracted from one PDF page, with 1-based page provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageText {
    pub page: usize,
    pub text: String,
}

/// Complete text extraction result for a PDF.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfText {
    pub pages: Vec<PageText>,
    pub total_chars: usize,
}

/// Extract decisions, actions and open questions from a transcript.
///
/// The input is bounded before processing. Recognized labels (English, French
/// and Italian) and sections drive classification; unrecognized labelled lines
/// are skipped rather than guessed. Every returned string is a trimmed copy of
/// source text. Items marked as uncertain are returned as open questions.
pub fn analyze_transcript(text: &str) -> TranscriptAnalysis {
    let mut analysis = TranscriptAnalysis::default();
    if text.trim().is_empty() {
        return analysis;
    }

    let (bounded, truncated) = bound_transcript(text, MAX_TRANSCRIPT_CHARS);
    let mut seen_decisions: HashSet<String> = HashSet::new();
    let mut seen_actions: HashSet<String> = HashSet::new();
    let mut seen_questions: HashSet<String> = HashSet::new();
    let mut section: Option<Kind> = None;
    let mut last_action: Option<usize> = None;
    let mut capped = false;

    for (index, raw_line) in bounded.lines().enumerate() {
        let line_number = index + 1;
        let line = clean_line(raw_line);
        if line.is_empty() {
            continue;
        }
        if line.chars().count() > MAX_LINE_CHARS {
            push_question(
                &mut analysis,
                &mut seen_questions,
                format!(
                    "Line {line_number} is longer than {MAX_LINE_CHARS} characters and was skipped; review it manually"
                ),
            );
            continue;
        }
        if let Some(heading) = section_heading(&line) {
            section = Some(heading);
            continue;
        }

        let mut kind = None;
        let mut content = line.clone();
        if let Some((label, rest)) = split_label(&line) {
            match classify_label(label) {
                Some(found) => {
                    kind = Some(found);
                    content = rest.trim().to_string();
                }
                // A short labelled field we do not understand (e.g. "Risk:") is
                // not silently promoted to a decision or action.
                None => continue,
            }
        }

        if kind.is_none() {
            if ends_with_question(&content) {
                kind = Some(Kind::Question);
            } else if let Some(todo) = todo_action_text(&content) {
                kind = Some(Kind::Action);
                content = todo;
            } else if content.eq_ignore_ascii_case("todo") || content.eq_ignore_ascii_case("to-do")
            {
                push_question(
                    &mut analysis,
                    &mut seen_questions,
                    format!("TODO without text on line {line_number}"),
                );
                continue;
            } else if let Some(current) = section {
                kind = Some(current);
            }
        }

        match kind {
            Some(Kind::Decision) => {
                let decision = content.trim();
                if decision.is_empty() {
                    push_question(
                        &mut analysis,
                        &mut seen_questions,
                        format!("Decision heading without text on line {line_number}"),
                    );
                    continue;
                }
                if is_uncertain(decision) {
                    push_question(&mut analysis, &mut seen_questions, decision);
                    continue;
                }
                if !push_decision(&mut analysis, &mut seen_decisions, decision)
                    && analysis.decisions.len() >= MAX_ITEMS
                {
                    capped = true;
                }
            }
            Some(Kind::Action) => {
                let tagged = extract_tags(content.trim());
                if tagged.text.is_empty() {
                    if let Some(owner) = tagged.owner {
                        push_question(
                            &mut analysis,
                            &mut seen_questions,
                            format!("Owner '{owner}' is not attached to an action text"),
                        );
                    } else {
                        push_question(
                            &mut analysis,
                            &mut seen_questions,
                            format!("Action heading without text on line {line_number}"),
                        );
                    }
                    last_action = None;
                    continue;
                }
                if is_uncertain(&tagged.text) {
                    push_question(&mut analysis, &mut seen_questions, tagged.text);
                    last_action = None;
                    continue;
                }
                if let Some(due) = &tagged.due {
                    if !looks_like_date(due) {
                        push_question(
                            &mut analysis,
                            &mut seen_questions,
                            format!("Unrecognized due date '{due}' for action '{}'", tagged.text),
                        );
                    }
                }
                let action = ActionItem {
                    text: tagged.text,
                    owner: tagged.owner,
                    due: tagged.due,
                };
                if push_action(&mut analysis, &mut seen_actions, action) {
                    last_action = Some(analysis.actions.len() - 1);
                } else if analysis.actions.len() >= MAX_ITEMS {
                    capped = true;
                }
            }
            Some(Kind::Question) => {
                let question = content.trim();
                if question.is_empty() {
                    push_question(
                        &mut analysis,
                        &mut seen_questions,
                        format!("Question heading without text on line {line_number}"),
                    );
                } else {
                    push_question(&mut analysis, &mut seen_questions, question);
                }
            }
            Some(Kind::Owner) => {
                let owner = shorten(&clean_tag_piece(content.trim()), 60);
                if owner.is_empty() {
                    push_question(
                        &mut analysis,
                        &mut seen_questions,
                        format!("Owner heading without a name on line {line_number}"),
                    );
                    continue;
                }
                match last_action {
                    Some(index) => {
                        if analysis.actions[index].owner.is_none() {
                            analysis.actions[index].owner = Some(owner);
                        }
                    }
                    None => push_question(
                        &mut analysis,
                        &mut seen_questions,
                        format!("Owner '{owner}' has no preceding action"),
                    ),
                }
            }
            Some(Kind::Due) => {
                let due = shorten(&clean_tag_piece(content.trim()), 60);
                if due.is_empty() {
                    push_question(
                        &mut analysis,
                        &mut seen_questions,
                        format!("Due heading without a date on line {line_number}"),
                    );
                    continue;
                }
                if !looks_like_date(&due) {
                    push_question(
                        &mut analysis,
                        &mut seen_questions,
                        format!("Unrecognized due date '{due}' on line {line_number}"),
                    );
                }
                match last_action {
                    Some(index) => {
                        if analysis.actions[index].due.is_none() {
                            analysis.actions[index].due = Some(due);
                        }
                    }
                    None => push_question(
                        &mut analysis,
                        &mut seen_questions,
                        format!("Due date '{due}' has no preceding action"),
                    ),
                }
            }
            None => {}
        }
    }

    if truncated {
        push_question(
            &mut analysis,
            &mut seen_questions,
            format!(
                "Transcript was truncated at {MAX_TRANSCRIPT_CHARS} characters; later content was not analyzed"
            ),
        );
    }
    if capped {
        push_question(
            &mut analysis,
            &mut seen_questions,
            format!(
                "Analysis stopped at the {MAX_ITEMS}-item limit; additional items were ignored"
            ),
        );
    }

    analysis
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Decision,
    Action,
    Question,
    Owner,
    Due,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagKind {
    Owner,
    Due,
}

const INLINE_TAGS: &[(&str, TagKind)] = &[
    ("assigned to", TagKind::Owner),
    ("responsable", TagKind::Owner),
    ("responsabile", TagKind::Owner),
    ("assignee", TagKind::Owner),
    ("owner", TagKind::Owner),
    ("due date", TagKind::Due),
    ("deadline", TagKind::Due),
    ("échéance", TagKind::Due),
    ("echeance", TagKind::Due),
    ("scadenza", TagKind::Due),
    ("entro", TagKind::Due),
    ("due", TagKind::Due),
];

const UNCERTAIN_MARKERS: &[&str] = &[
    "maybe",
    "perhaps",
    "possibly",
    "probably",
    "probably not",
    "tbd",
    "tbc",
    "unclear",
    "unsure",
    "to be confirmed",
    "to be defined",
    "if possible",
    "not sure",
    "peut-être",
    "peut-etre",
    "peut etre",
    "à confirmer",
    "a confirmer",
    "éventuellement",
    "eventuellement",
    "probablement",
    "si possible",
    "forse",
    "probabilmente",
    "da confermare",
    "da definire",
    "se possibile",
    "non certo",
];

const MONTH_NAMES: &[&str] = &[
    "jan",
    "feb",
    "mar",
    "apr",
    "may",
    "jun",
    "jul",
    "aug",
    "sep",
    "sept",
    "oct",
    "nov",
    "dec",
    "january",
    "february",
    "march",
    "april",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
    "janvier",
    "février",
    "fevrier",
    "mars",
    "avril",
    "mai",
    "juin",
    "juillet",
    "août",
    "aout",
    "septembre",
    "octobre",
    "novembre",
    "décembre",
    "decembre",
    "gennaio",
    "febbraio",
    "marzo",
    "aprile",
    "maggio",
    "giugno",
    "luglio",
    "agosto",
    "settembre",
    "ottobre",
    "dicembre",
];

fn bound_transcript(text: &str, max_chars: usize) -> (String, bool) {
    if text.chars().count() <= max_chars {
        return (text.to_string(), false);
    }
    let mut bounded: String = text.chars().take(max_chars).collect();
    if let Some(index) = bounded.rfind('\n') {
        bounded.truncate(index);
    }
    (bounded, true)
}

fn clean_line(raw: &str) -> String {
    let mut line = raw.trim();
    line = line.trim_start_matches(['#', '>', '*', '+', '|']);
    line = line.trim_start();
    if let Some(rest) = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("– "))
        .or_else(|| line.strip_prefix("— "))
        .or_else(|| line.strip_prefix("• "))
    {
        line = rest.trim_start();
    }
    strip_numbering(line).trim().to_string()
}

fn strip_numbering(line: &str) -> &str {
    let digits = line
        .chars()
        .take_while(|value| value.is_ascii_digit())
        .count();
    if digits == 0 || digits > 3 {
        return line;
    }
    let rest = &line[digits..];
    if let Some(rest) = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')')) {
        if let Some(rest) = rest.strip_prefix(' ') {
            return rest.trim_start();
        }
    }
    line
}

fn normalize_spaces(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn section_heading(line: &str) -> Option<Kind> {
    let normalized = line.trim().trim_end_matches(':').trim().to_lowercase();
    if normalized.chars().count() > MAX_SECTION_CHARS {
        return None;
    }
    match normalized.as_str() {
        "decision" | "decisions" | "décision" | "décisions" | "décisions prises"
        | "decisions taken" | "decisione" | "decisioni" | "resolution" | "résolution" => {
            Some(Kind::Decision)
        }
        "action" | "actions" | "action item" | "action items" | "action point"
        | "action points" | "next step" | "next steps" | "prochaine étape"
        | "prochaines étapes" | "prossimo passo" | "prossimi passi" | "todo" | "to-do"
        | "todos" | "à faire" | "a faire" | "da fare" | "suivi" | "follow-up" | "follow-ups"
        | "follow up" | "follow ups" => Some(Kind::Action),
        "question" | "questions" | "open question" | "open questions" | "question ouverte"
        | "questions ouvertes" | "domanda" | "domande" | "domanda aperta" | "domande aperte" => {
            Some(Kind::Question)
        }
        _ => None,
    }
}

fn split_label(line: &str) -> Option<(&str, &str)> {
    let (index, colon) = line
        .char_indices()
        .find(|(_, value)| *value == ':' || *value == '：')?;
    let label = line[..index].trim();
    if label.is_empty() || label.chars().count() > 40 {
        return None;
    }
    if label.contains('.') || label.contains('?') || label.contains('!') {
        return None;
    }
    Some((label, &line[index + colon.len_utf8()..]))
}

fn classify_label(label: &str) -> Option<Kind> {
    let normalized = label
        .trim_matches(|value: char| value == '*' || value == '_' || value.is_whitespace())
        .trim_end_matches(':')
        .trim()
        .to_lowercase();
    match normalized.as_str() {
        "decision" | "decisions" | "décision" | "décisions" | "decisione" | "decisioni"
        | "resolved" | "résolu" | "resolu" | "resolution" | "résolution" | "agreed" | "accord"
        | "approved" | "approuvé" | "approvato" => Some(Kind::Decision),
        "action" | "actions" | "action item" | "action items" | "todo" | "to-do" | "task"
        | "tasks" | "azione" | "azioni" | "next step" | "next steps" | "prochaine étape"
        | "prochaines étapes" | "prossimo passo" | "prossimi passi" | "follow-up" | "follow up" => {
            Some(Kind::Action)
        }
        "question" | "questions" | "open question" | "open questions" | "question ouverte"
        | "questions ouvertes" | "domanda" | "domande" | "domanda aperta" | "domande aperte" => {
            Some(Kind::Question)
        }
        "owner" | "owners" | "responsable" | "responsables" | "responsabile" | "responsabili"
        | "assignee" | "assigned to" => Some(Kind::Owner),
        "due" | "due date" | "échéance" | "echeance" | "scadenza" | "deadline" | "entro" => {
            Some(Kind::Due)
        }
        _ => None,
    }
}

fn ends_with_question(line: &str) -> bool {
    line.trim_end()
        .chars()
        .last()
        .is_some_and(|value| value == '?' || value == '？')
}

fn todo_action_text(line: &str) -> Option<String> {
    let lower = line.to_lowercase();
    for prefix in ["todo", "to-do", "to do"] {
        if lower.starts_with(prefix) {
            let rest = &line[prefix.len()..];
            let rest = rest.trim_start_matches(|value: char| value == ':' || value.is_whitespace());
            if !rest.trim().is_empty() {
                return Some(rest.trim().to_string());
            }
            return None;
        }
    }
    None
}

fn is_uncertain(text: &str) -> bool {
    let lower = text.to_lowercase();
    UNCERTAIN_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

fn looks_like_date(value: &str) -> bool {
    let trimmed = value.trim();
    let bytes = trimmed.as_bytes();
    if trimmed.len() == 10
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'-' | b'/' | b'.'))
    {
        // YYYY-MM-DD, YYYY/MM/DD, DD/MM/YYYY, DD.MM.YYYY, DD-MM-YYYY.
        let iso = bytes[4] == bytes[7] && matches!(bytes[4], b'-' | b'/');
        let day_first = bytes[2] == bytes[5] && matches!(bytes[2], b'-' | b'/' | b'.');
        if iso || day_first {
            return true;
        }
    }
    let lower = trimmed.to_lowercase();
    let tokens: Vec<&str> = lower
        .split(|value: char| !value.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();
    let has_month = tokens.iter().any(|token| MONTH_NAMES.contains(token));
    let has_year = tokens
        .iter()
        .any(|token| token.len() == 4 && token.chars().all(|value| value.is_ascii_digit()));
    has_month && has_year
}

#[derive(Debug, Default)]
struct TaggedText {
    text: String,
    owner: Option<String>,
    due: Option<String>,
}

fn extract_tags(content: &str) -> TaggedText {
    let chars: Vec<char> = content.chars().collect();
    let mut pieces: Vec<String> = Vec::new();
    let mut owner = None;
    let mut due = None;
    let mut cursor = 0usize;
    let mut index = 0usize;

    while index < chars.len() {
        let boundary = index == 0 || !chars[index - 1].is_alphanumeric();
        if boundary {
            if let Some((kind, length)) = match_tag(&chars, index) {
                let before: String = chars[cursor..index].iter().collect();
                pieces.push(clean_tag_piece(&before));

                let mut value_start = index + length;
                while value_start < chars.len() && chars[value_start].is_whitespace() {
                    value_start += 1;
                }
                let mut value_end = value_start;
                while value_end < chars.len() {
                    let at_boundary =
                        value_end == value_start || !chars[value_end - 1].is_alphanumeric();
                    if at_boundary && match_tag(&chars, value_end).is_some() {
                        break;
                    }
                    value_end += 1;
                }
                let value: String = chars[value_start..value_end].iter().collect();
                let value = clean_tag_piece(&value);
                if !value.is_empty() {
                    match kind {
                        TagKind::Owner => {
                            if owner.is_none() {
                                owner = Some(shorten(&value, 60));
                            } else {
                                pieces.push(value);
                            }
                        }
                        TagKind::Due => {
                            if due.is_none() {
                                due = Some(shorten(&value, 60));
                            } else {
                                pieces.push(value);
                            }
                        }
                    }
                }
                cursor = value_end;
                index = value_end;
                continue;
            }
        }
        index += 1;
    }

    let tail: String = chars[cursor..].iter().collect();
    pieces.push(clean_tag_piece(&tail));
    let text = pieces
        .into_iter()
        .filter(|piece| !piece.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    TaggedText {
        text: normalize_spaces(&text),
        owner,
        due,
    }
}

fn match_tag(chars: &[char], start: usize) -> Option<(TagKind, usize)> {
    for (label, kind) in INLINE_TAGS {
        let label_chars: Vec<char> = label.chars().collect();
        let end = start + label_chars.len();
        if end > chars.len() {
            continue;
        }
        let matches = chars[start..end]
            .iter()
            .zip(label_chars.iter())
            .all(|(left, right)| left.to_lowercase().eq(right.to_lowercase()));
        if !matches {
            continue;
        }
        let mut index = end;
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        if index < chars.len() && (chars[index] == ':' || chars[index] == '：') {
            return Some((*kind, index + 1 - start));
        }
    }
    None
}

fn clean_tag_piece(value: &str) -> String {
    let mut cleaned = value
        .trim()
        .trim_matches(|character: char| {
            matches!(
                character,
                '-' | '–'
                    | '—'
                    | ';'
                    | ','
                    | '.'
                    | ':'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '"'
                    | '\''
                    | '«'
                    | '»'
            )
        })
        .trim()
        .to_string();
    for conjunction in [" and", " et", " e", " und"] {
        if cleaned.to_lowercase().ends_with(conjunction) {
            let length = cleaned.len() - conjunction.len();
            cleaned.truncate(length);
        }
    }
    cleaned.trim().to_string()
}

fn shorten(value: &str, max_chars: usize) -> String {
    truncate_chars(value.trim(), max_chars)
}

fn dedup_key(text: &str) -> String {
    normalize_spaces(text)
        .to_lowercase()
        .trim_end_matches(['.', '!', ';', ':', ','])
        .to_string()
}

fn push_decision(
    analysis: &mut TranscriptAnalysis,
    seen: &mut HashSet<String>,
    text: &str,
) -> bool {
    if analysis.decisions.len() >= MAX_ITEMS {
        return false;
    }
    let key = dedup_key(text);
    if key.is_empty() || !seen.insert(key) {
        return false;
    }
    analysis.decisions.push(text.trim().to_string());
    true
}

fn push_action(
    analysis: &mut TranscriptAnalysis,
    seen: &mut HashSet<String>,
    action: ActionItem,
) -> bool {
    if analysis.actions.len() >= MAX_ITEMS {
        return false;
    }
    let key = dedup_key(&action.text);
    if key.is_empty() || !seen.insert(key) {
        return false;
    }
    analysis.actions.push(action);
    true
}

fn push_question(
    analysis: &mut TranscriptAnalysis,
    seen: &mut HashSet<String>,
    text: impl Into<String>,
) {
    if analysis.open_questions.len() >= MAX_ITEMS {
        return;
    }
    let text = text.into();
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let key = dedup_key(text);
    if key.is_empty() || !seen.insert(key) {
        return;
    }
    analysis.open_questions.push(text.to_string());
}

/// Extract text from a PDF page by page, keeping 1-based page numbers.
///
/// The file is size-checked before loading. Encrypted PDFs, PDFs without
/// extractable text (typically scans) and unreadable files are rejected with an
/// actionable error. No OCR is attempted.
pub fn extract_pdf_text(path: impl AsRef<Path>, max_bytes: u64) -> Result<PdfText> {
    let path = path.as_ref();
    let metadata = std::fs::metadata(path).map_err(|error| {
        SeraError::Validation(format!("cannot read PDF {}: {error}", path.display()))
    })?;
    if !metadata.is_file() {
        return Err(SeraError::Validation(format!(
            "PDF {} is not a regular file",
            path.display()
        )));
    }
    if metadata.len() == 0 {
        return Err(SeraError::Validation(format!(
            "PDF {} is empty",
            path.display()
        )));
    }
    if metadata.len() > max_bytes {
        return Err(SeraError::Validation(format!(
            "PDF {} is {} bytes, above the {} byte limit",
            path.display(),
            metadata.len(),
            max_bytes
        )));
    }

    let document = load_pdf(path)?;
    if document.is_encrypted() {
        return Err(SeraError::Validation(format!(
            "PDF {} is encrypted; decrypt it before extraction (password-protected PDFs are not supported)",
            path.display()
        )));
    }
    let pages = document.get_pages();
    if pages.is_empty() {
        return Err(SeraError::Validation(format!(
            "PDF {} has no pages",
            path.display()
        )));
    }
    if pages.len() > MAX_PDF_PAGES {
        return Err(SeraError::Validation(format!(
            "PDF {} has {} pages, above the {MAX_PDF_PAGES} page limit",
            path.display(),
            pages.len()
        )));
    }

    let mut extracted = Vec::with_capacity(pages.len());
    let mut total_chars = 0usize;
    for page_number in pages.keys().copied() {
        let text = extract_page_text(&document, page_number)?;
        let text = normalize_pdf_text(&text);
        total_chars += text.chars().count();
        extracted.push(PageText {
            page: page_number as usize,
            text,
        });
    }

    if total_chars == 0 {
        return Err(SeraError::Validation(format!(
            "PDF {} contains no extractable text across {} page(s); it is probably a scanned image. OCR is not supported: run OCR first or provide a text-based PDF",
            path.display(),
            pages.len()
        )));
    }

    Ok(PdfText {
        pages: extracted,
        total_chars,
    })
}

fn load_pdf(path: &Path) -> Result<lopdf::Document> {
    // A malformed PDF must never take the process down through a parser panic.
    let outcome = catch_unwind(AssertUnwindSafe(|| lopdf::Document::load(path)));
    match outcome {
        Ok(Ok(document)) => Ok(document),
        Ok(Err(error)) => Err(SeraError::Validation(format!(
            "unsupported or corrupt PDF {}: {error}",
            path.display()
        ))),
        Err(_) => Err(SeraError::Validation(format!(
            "PDF {} could not be parsed safely; the file is malformed",
            path.display()
        ))),
    }
}

fn extract_page_text(document: &lopdf::Document, page: u32) -> Result<String> {
    let outcome = catch_unwind(AssertUnwindSafe(|| document.extract_text(&[page])));
    match outcome {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(error)) => Err(SeraError::Validation(format!(
            "PDF page {page}: text extraction failed ({error})"
        ))),
        Err(_) => Err(SeraError::Validation(format!(
            "PDF page {page}: text extraction panicked; the file is malformed"
        ))),
    }
}

fn normalize_pdf_text(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<String> = Vec::new();
    let mut pending_blank = false;
    for line in normalized.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            if !lines.is_empty() {
                pending_blank = true;
            }
            continue;
        }
        if pending_blank {
            lines.push(String::new());
            pending_blank = false;
        }
        lines.push(line.to_string());
    }
    lines.join("\n").trim().to_string()
}

/// Build a deterministic extractive outline with page markers.
///
/// Sentences and headings are copied from `pages` in document order; nothing is
/// generated. The result never exceeds `max_chars` characters and ends with
/// `[outline truncated]` when content had to be dropped.
pub fn synthesis_outline(pages: &[PageText], max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let mut output = String::new();
    let mut used = 0usize;
    let mut seen: HashSet<String> = HashSet::new();
    let mut truncated = false;

    'pages: for page in pages {
        let candidates = outline_candidates(&page.text);
        if candidates.is_empty() {
            continue;
        }
        let marker = format!("[page {}]", page.page);
        if !append_outline_line(&mut output, &mut used, max_chars, &marker) {
            truncated = true;
            break;
        }
        for candidate in candidates {
            let key = dedup_key(&candidate);
            if key.is_empty() || !seen.insert(key) {
                continue;
            }
            if !append_outline_line(&mut output, &mut used, max_chars, &candidate) {
                truncated = true;
                break 'pages;
            }
        }
    }

    if output.is_empty() {
        return output;
    }
    if truncated {
        let note = "[outline truncated]";
        if used + note.chars().count() < max_chars {
            output.push('\n');
            output.push_str(note);
        }
    }
    output
}

fn append_outline_line(
    output: &mut String,
    used: &mut usize,
    max_chars: usize,
    line: &str,
) -> bool {
    let newline = usize::from(!output.is_empty());
    let needed = line.chars().count() + newline;
    if used.saturating_add(needed) > max_chars {
        return false;
    }
    if newline == 1 {
        output.push('\n');
    }
    output.push_str(line);
    *used += needed;
    true
}

fn outline_candidates(text: &str) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.chars().count() < 2 {
            continue;
        }
        if is_heading_like(line) {
            candidates.push(line.to_string());
            continue;
        }
        for sentence in split_sentences(line) {
            if is_informative(&sentence) {
                candidates.push(truncated_sentence(&sentence));
            }
        }
    }
    let mut seen: HashSet<String> = HashSet::new();
    candidates.retain(|candidate| seen.insert(dedup_key(candidate)));
    candidates.truncate(MAX_OUTLINE_CANDIDATES_PER_PAGE);
    candidates
}

fn is_heading_like(line: &str) -> bool {
    let chars = line.chars().count();
    if !(4..=90).contains(&chars) {
        return false;
    }
    let words = line.split_whitespace().count();
    if words == 0 || words > 12 {
        return false;
    }
    if line
        .chars()
        .last()
        .is_some_and(|value| matches!(value, '.' | '!' | '?' | ',' | ';'))
    {
        return false;
    }
    line.chars()
        .next()
        .is_some_and(|value| value.is_uppercase() || value.is_ascii_digit())
}

fn split_sentences(line: &str) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut sentences = Vec::new();
    let mut current = String::new();
    for (index, value) in chars.iter().enumerate() {
        current.push(*value);
        let next_is_break = index + 1 >= chars.len() || chars[index + 1].is_whitespace();
        let is_break =
            matches!(value, '!' | '?' | ';') || (*value == '.' && next_is_break && index > 0);
        if is_break {
            let sentence = current.trim();
            if !sentence.is_empty() {
                sentences.push(sentence.to_string());
            }
            current.clear();
        }
    }
    let tail = current.trim();
    if !tail.is_empty() {
        sentences.push(tail.to_string());
    }
    sentences
}

fn is_informative(sentence: &str) -> bool {
    let chars = sentence.chars().count();
    if chars < 25 {
        return false;
    }
    if sentence.split_whitespace().count() < 4 {
        return false;
    }
    if !sentence.chars().any(|value| value.is_alphabetic()) {
        return false;
    }
    if sentence.chars().all(|value| value.is_ascii_digit()) {
        return false;
    }
    true
}

fn truncated_sentence(sentence: &str) -> String {
    if sentence.chars().count() <= MAX_OUTLINE_SENTENCE_CHARS {
        sentence.to_string()
    } else {
        format!("{}…", truncate_chars(sentence, MAX_OUTLINE_SENTENCE_CHARS))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_ITEMS, PageText, TranscriptAnalysis, analyze_transcript, extract_pdf_text,
        synthesis_outline,
    };
    use std::io::BufWriter;
    use std::path::Path as FsPath;

    #[test]
    fn extracts_multilingual_transcript_items() {
        let text = "\
Decisions
- Décision : lancer le pilote en janvier
- Decision: adopt the new pricing
Actions
- Action: préparer le devis — Owner: Marco — Due: 2026-11-15
- Azione: inviare il contratto (Responsabile: Giulia, Scadenza: 2026-12-01)
- TODO: follow up with the client
Questions
- Question: faut-il former les équipes ?
- Domanda: chi approva il budget?
";
        let analysis = analyze_transcript(text);
        assert_eq!(analysis.decisions.len(), 2, "{analysis:?}");
        assert!(
            analysis
                .decisions
                .iter()
                .any(|decision| decision.contains("pilote")),
            "{analysis:?}"
        );
        assert_eq!(analysis.actions.len(), 3, "{analysis:?}");
        let devis = analysis
            .actions
            .iter()
            .find(|action| action.text.contains("devis"))
            .expect("devis action");
        assert_eq!(devis.owner.as_deref(), Some("Marco"));
        assert_eq!(devis.due.as_deref(), Some("2026-11-15"));
        let contrat = analysis
            .actions
            .iter()
            .find(|action| action.text.contains("contratto"))
            .expect("contratto action");
        assert_eq!(contrat.owner.as_deref(), Some("Giulia"));
        assert_eq!(contrat.due.as_deref(), Some("2026-12-01"));
        assert!(
            analysis
                .open_questions
                .iter()
                .any(|question| question.contains("faut-il")),
            "{analysis:?}"
        );
        assert!(
            analysis
                .open_questions
                .iter()
                .any(|question| question.contains("chi approva")),
            "{analysis:?}"
        );
    }

    #[test]
    fn deduplicates_and_marks_uncertain_items() {
        let text = "\
Decision: maybe migrate the CRM
Decision: maybe migrate the CRM
Decision: keep the current process
Action: prepare the budget
Action: prepare the budget
";
        let analysis = analyze_transcript(text);
        assert_eq!(
            analysis.decisions,
            vec!["keep the current process".to_string()]
        );
        assert_eq!(analysis.actions.len(), 1);
        assert!(
            analysis
                .open_questions
                .iter()
                .any(|question| question.contains("maybe migrate")),
            "{analysis:?}"
        );
    }

    #[test]
    fn ignores_plain_prose_and_empty_input() {
        let prose = analyze_transcript("The meeting was productive and the team left.");
        assert!(prose.decisions.is_empty());
        assert!(prose.actions.is_empty());
        assert!(prose.open_questions.is_empty());

        let empty = analyze_transcript("");
        assert_eq!(empty, TranscriptAnalysis::default());
    }

    #[test]
    fn question_marks_become_open_questions() {
        let analysis = analyze_transcript("Do we have budget for the pilot?\n");
        assert!(analysis.decisions.is_empty());
        assert_eq!(analysis.open_questions.len(), 1);
    }

    #[test]
    fn item_cap_is_respected() {
        let mut text = String::new();
        for index in 0..(MAX_ITEMS + 50) {
            text.push_str(&format!("Decision: item {index}\n"));
        }
        let analysis = analyze_transcript(&text);
        assert_eq!(analysis.decisions.len(), MAX_ITEMS);
    }

    fn write_pdf(path: &FsPath, pages: &[&str]) {
        use printpdf::{BuiltinFont, Mm, PdfDocument};
        let (document, first_page, first_layer) =
            PdfDocument::new("test", Mm(210.0), Mm(297.0), "Layer 1");
        let font = document
            .add_builtin_font(BuiltinFont::Helvetica)
            .expect("builtin font");
        if let Some(text) = pages.first() {
            document
                .get_page(first_page)
                .get_layer(first_layer)
                .use_text(*text, 12.0, Mm(20.0), Mm(250.0), &font);
        }
        for text in pages.iter().skip(1) {
            let (page, layer) = document.add_page(Mm(210.0), Mm(297.0), "Layer 1");
            document.get_page(page).get_layer(layer).use_text(
                *text,
                12.0,
                Mm(20.0),
                Mm(250.0),
                &font,
            );
        }
        let file = std::fs::File::create(path).expect("create pdf");
        let mut writer = BufWriter::new(file);
        document.save(&mut writer).expect("save pdf");
    }

    #[test]
    fn extracts_pdf_text_with_page_provenance() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("sample.pdf");
        write_pdf(
            &path,
            &[
                "Budget review for the fourth quarter.",
                "Decision: approve the pilot.",
            ],
        );

        let extracted = extract_pdf_text(&path, 2_000_000).expect("text extraction");
        assert_eq!(extracted.pages.len(), 2);
        assert_eq!(extracted.pages[0].page, 1);
        assert!(extracted.pages[0].text.contains("Budget"));
        assert_eq!(extracted.pages[1].page, 2);
        assert!(extracted.pages[1].text.contains("Decision"));
        let counted: usize = extracted
            .pages
            .iter()
            .map(|page| page.text.chars().count())
            .sum();
        assert_eq!(extracted.total_chars, counted);
    }

    #[test]
    fn rejects_pdf_without_extractable_text() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("blank.pdf");
        write_pdf(&path, &[]);

        let error = extract_pdf_text(&path, 2_000_000).expect_err("blank pdf must fail");
        let message = error.to_string().to_lowercase();
        assert!(message.contains("no extractable text"), "{message}");
        assert!(message.contains("ocr"), "{message}");
    }

    #[test]
    fn rejects_oversized_pdf() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("sample.pdf");
        write_pdf(&path, &["Some text that makes the file non-trivial."]);
        let error = extract_pdf_text(&path, 16).expect_err("oversized pdf must fail");
        assert!(error.to_string().contains("byte limit"), "{error}");
    }

    #[test]
    fn outline_has_page_markers_and_respects_budget() {
        let pages = vec![
            PageText {
                page: 1,
                text: "Introduction\nThis document explains the budget for the next fiscal year. The total amount is 120000 EUR. Risks remain around hiring.".into(),
            },
            PageText {
                page: 2,
                text: "Recommendations\nThe team should approve the pilot before December.".into(),
            },
        ];
        let outline = synthesis_outline(&pages, 400);
        assert!(outline.contains("[page 1]"), "{outline}");
        assert!(outline.contains("[page 2]"), "{outline}");
        assert!(outline.contains("budget"), "{outline}");

        let short = synthesis_outline(&pages, 60);
        assert!(short.chars().count() <= 60, "{short:?}");
        assert!(!short.is_empty());

        assert_eq!(synthesis_outline(&pages, 0), "");
    }

    #[test]
    fn outline_is_extractive() {
        let page = PageText {
            page: 1,
            text: "Summary\nThe project costs forty thousand euros in total. Margins look healthy for next year.".into(),
        };
        let outline = synthesis_outline(std::slice::from_ref(&page), 500);
        for line in outline.lines().filter(|line| !line.starts_with("[page")) {
            assert!(
                page.text.contains(line),
                "outline line not found in source: {line}"
            );
        }
    }
}
