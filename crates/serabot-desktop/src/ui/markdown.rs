use super::*;

// ------------------------------------------------ Pure-Rust Markdown Renderer ---

#[derive(Clone, Debug, PartialEq)]
pub enum MdBlock {
    Heading {
        level: usize,
        text: String,
    },
    CodeBlock {
        lang: String,
        code: String,
    },
    Rule,
    Quote(Vec<String>),
    UnorderedList(Vec<String>),
    OrderedList {
        start: usize,
        items: Vec<String>,
    },
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Paragraph(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum InlineSpan {
    Text(String),
    Code(String),
    Bold(Vec<InlineSpan>),
    Italic(String),
    Link { label: String, url: String },
}

pub(super) const MD_CACHE_MAX: usize = 384;

pub(super) fn markdown_content_hash(content: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

/// Parsed markdown blocks keyed by message id and content hash.
pub(super) fn parse_markdown_cached(message_id: &str, content: &str) -> Vec<MdBlock> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    type MarkdownCache = HashMap<(String, u64), Vec<MdBlock>>;
    static CACHE: OnceLock<Mutex<MarkdownCache>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (message_id.to_owned(), markdown_content_hash(content));
    let mut guard = cache.lock().expect("markdown cache");
    if let Some(blocks) = guard.get(&key) {
        return blocks.clone();
    }
    let blocks = parse_markdown_blocks(content);
    if guard.len() >= MD_CACHE_MAX {
        guard.clear();
    }
    guard.insert(key, blocks.clone());
    blocks
}

pub fn parse_markdown_blocks(content: &str) -> Vec<MdBlock> {
    let mut blocks = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        // Fenced code block
        if trimmed.starts_with("```") {
            let lang = trimmed.trim_start_matches('`').trim().to_owned();
            let mut code_lines = Vec::new();
            i += 1;
            while i < lines.len() {
                if lines[i].trim().starts_with("```") {
                    i += 1;
                    break;
                }
                code_lines.push(lines[i]);
                i += 1;
            }
            blocks.push(MdBlock::CodeBlock {
                lang,
                code: code_lines.join("\n"),
            });
            continue;
        }

        // Horizontal rule (---, ***, ___)
        if (trimmed == "---" || trimmed == "***" || trimmed == "___")
            || (trimmed.len() >= 3 && trimmed.chars().all(|c| c == '-' || c == '*' || c == '_'))
        {
            blocks.push(MdBlock::Rule);
            i += 1;
            continue;
        }

        // Heading
        if trimmed.starts_with('#') {
            let hash_count = trimmed.chars().take_while(|c| *c == '#').count();
            let after_hashes = trimmed.get(hash_count..).unwrap_or_default();
            if hash_count <= 6 && after_hashes.starts_with(' ') {
                let rest = after_hashes.trim();
                let title = rest
                    .strip_prefix("**")
                    .and_then(|inner| inner.strip_suffix("**"))
                    .unwrap_or(rest);
                blocks.push(MdBlock::Heading {
                    level: hash_count,
                    text: title.trim().to_owned(),
                });
                i += 1;
                continue;
            }
        }

        // Blockquote
        if trimmed.starts_with("> ") || trimmed == ">" {
            let mut quote_lines = Vec::new();
            while i < lines.len() {
                let q_trim = lines[i].trim();
                if let Some(stripped) = q_trim.strip_prefix("> ") {
                    quote_lines.push(stripped.trim().to_owned());
                    i += 1;
                } else if q_trim == ">" {
                    quote_lines.push(String::new());
                    i += 1;
                } else if !q_trim.is_empty()
                    && !quote_lines.is_empty()
                    && !q_trim.starts_with('#')
                    && !q_trim.starts_with('-')
                    && !q_trim.starts_with("```")
                {
                    quote_lines.push(q_trim.to_owned());
                    i += 1;
                } else {
                    break;
                }
            }
            blocks.push(MdBlock::Quote(quote_lines));
            continue;
        }

        // Markdown Table
        if trimmed.starts_with('|')
            && i + 1 < lines.len()
            && lines[i + 1].trim().starts_with('|')
            && lines[i + 1].contains("---")
        {
            let parse_table_row = |row: &str| -> Vec<String> {
                let trimmed_row = row.trim().trim_start_matches('|').trim_end_matches('|');
                trimmed_row
                    .split('|')
                    .map(|c| c.trim().to_owned())
                    .collect()
            };
            let headers = parse_table_row(trimmed);
            i += 2;
            let mut rows = Vec::new();
            while i < lines.len() && lines[i].trim().starts_with('|') {
                rows.push(parse_table_row(lines[i]));
                i += 1;
            }
            blocks.push(MdBlock::Table { headers, rows });
            continue;
        }

        // Unordered list
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("• ") {
            let mut items = Vec::new();
            while i < lines.len() {
                let l_trim = lines[i].trim();
                if l_trim.starts_with("- ") || l_trim.starts_with("* ") || l_trim.starts_with("• ")
                {
                    let marker_len = if l_trim.starts_with("• ") { 3 } else { 2 };
                    items.push(
                        l_trim
                            .get(marker_len..)
                            .unwrap_or_default()
                            .trim()
                            .to_owned(),
                    );
                    i += 1;
                } else if !l_trim.is_empty()
                    && (lines[i].starts_with("  ") || lines[i].starts_with('\t'))
                    && !items.is_empty()
                {
                    if let Some(last) = items.last_mut() {
                        last.push(' ');
                        last.push_str(l_trim);
                    }
                    i += 1;
                } else {
                    break;
                }
            }
            blocks.push(MdBlock::UnorderedList(items));
            continue;
        }

        // Ordered list
        let first_word = trimmed.split_whitespace().next().unwrap_or("");
        if first_word.ends_with('.')
            && first_word
                .trim_end_matches('.')
                .chars()
                .all(|c| c.is_ascii_digit())
        {
            // The first number is the list's start: a list that continues
            // after other blocks (or a blank line) must not restart at 1.
            let start = first_word
                .trim_end_matches('.')
                .parse::<usize>()
                .unwrap_or(1);
            let mut items = Vec::new();
            while i < lines.len() {
                let l_trim = lines[i].trim();
                let fw = l_trim.split_whitespace().next().unwrap_or("");
                if l_trim.is_empty() && !items.is_empty() {
                    // A blank line inside a loose list keeps it one list.
                    let next = lines[i + 1..]
                        .iter()
                        .map(|l| l.trim())
                        .find(|l| !l.is_empty());
                    let continues = next.is_some_and(|l| {
                        let w = l.split_whitespace().next().unwrap_or("");
                        w.ends_with('.')
                            && !w.trim_end_matches('.').is_empty()
                            && w.trim_end_matches('.').chars().all(|c| c.is_ascii_digit())
                    });
                    if continues {
                        i += 1;
                        continue;
                    }
                    break;
                }
                if fw.ends_with('.') && fw.trim_end_matches('.').chars().all(|c| c.is_ascii_digit())
                {
                    let item_text = l_trim.get(fw.len()..).unwrap_or_default().trim().to_owned();
                    items.push(item_text);
                    i += 1;
                } else if !l_trim.is_empty()
                    && (lines[i].starts_with("  ") || lines[i].starts_with('\t'))
                    && !items.is_empty()
                {
                    if let Some(last) = items.last_mut() {
                        last.push(' ');
                        last.push_str(l_trim);
                    }
                    i += 1;
                } else {
                    break;
                }
            }
            blocks.push(MdBlock::OrderedList { start, items });
            continue;
        }

        // Paragraph
        let mut para_lines = Vec::new();
        while i < lines.len() {
            let p_trim = lines[i].trim();
            if p_trim.is_empty()
                || p_trim.starts_with("```")
                || p_trim.starts_with('#')
                || p_trim.starts_with("> ")
                || p_trim == ">"
                || p_trim.starts_with("- ")
                || p_trim.starts_with("* ")
                || p_trim.starts_with("• ")
                || (p_trim.starts_with('|')
                    && i + 1 < lines.len()
                    && lines[i + 1].trim().starts_with('|'))
                || (p_trim == "---" || p_trim == "***" || p_trim == "___")
            {
                break;
            }
            para_lines.push(p_trim);
            i += 1;
        }
        if !para_lines.is_empty() {
            blocks.push(MdBlock::Paragraph(para_lines.join(" ")));
        }
    }
    blocks
}

pub fn parse_inline(text: &str) -> Vec<InlineSpan> {
    let mut spans = Vec::new();
    let mut rest = text;

    while !rest.is_empty() {
        // 1. Bold with inline code: **`code`**
        if rest.starts_with("**`")
            && let Some(end_code) = rest[3..].find('`')
        {
            let code = &rest[3..3 + end_code];
            let after_code = &rest[3 + end_code + 1..];
            if let Some(stripped) = after_code.strip_prefix("**") {
                spans.push(InlineSpan::Bold(vec![InlineSpan::Code(code.to_owned())]));
                rest = stripped;
                continue;
            }
        }

        // 2. Bold: **...**
        if rest.starts_with("**")
            && let Some(end) = rest[2..].find("**")
        {
            let inner = &rest[2..2 + end];
            let sub_spans = parse_inline(inner);
            spans.push(InlineSpan::Bold(sub_spans));
            rest = &rest[2 + end + 2..];
            continue;
        }

        // 3. Inline Code: `...`
        if rest.starts_with('`')
            && let Some(end) = rest[1..].find('`')
        {
            let code = &rest[1..1 + end];
            spans.push(InlineSpan::Code(code.to_owned()));
            rest = &rest[1 + end + 1..];
            continue;
        }

        // 4. Link: [label](url)
        if rest.starts_with('[')
            && let Some(close_bracket) = rest.find(']')
            && rest[close_bracket..].starts_with("](")
        {
            let after_bracket = &rest[close_bracket + 2..];
            if let Some(close_paren) = after_bracket.find(')') {
                let label = &rest[1..close_bracket];
                let url = &after_bracket[..close_paren];
                spans.push(InlineSpan::Link {
                    label: label.to_owned(),
                    url: url.to_owned(),
                });
                rest = &after_bracket[close_paren + 1..];
                continue;
            }
        }

        // 5. Italic: *...* (where not **)
        if rest.starts_with('*')
            && !rest.starts_with("**")
            && let Some(end) = rest[1..].find('*')
        {
            let inner = &rest[1..1 + end];
            if !inner.is_empty() && !inner.starts_with('*') && !inner.ends_with('*') {
                spans.push(InlineSpan::Italic(inner.to_owned()));
                rest = &rest[1 + end + 1..];
                continue;
            }
        }

        // Consume normal text up to the next formatting token. Skip the first
        // *char* (not byte): slicing at byte 1 panics when `rest` starts with a
        // multibyte character (emoji, accented letters).
        let next_pos = rest
            .char_indices()
            .skip(1)
            .find(|(_, c)| matches!(c, '*' | '`' | '['))
            .map(|(idx, _)| idx)
            .unwrap_or(rest.len());

        let chunk = &rest[..next_pos];
        if let Some(InlineSpan::Text(last_text)) = spans.last_mut() {
            last_text.push_str(chunk);
        } else {
            spans.push(InlineSpan::Text(chunk.to_owned()));
        }
        rest = &rest[next_pos..];
    }

    spans
}

/// A fenced code block: the language label, a Copy button that confirms on
/// the button itself, and token colours from `ui_highlight`.
#[component]
pub(super) fn CodeBlock(lang: String, code: String) -> Element {
    let mut copied = use_signal(|| false);
    let label = match lang.trim() {
        "" => "text".to_owned(),
        name => name.to_owned(),
    };
    let tokens = ui_highlight::tokenize(&lang, &code);
    rsx! {
        div { class: "md-code-block",
            div { class: "md-code-head",
                span { class: "md-code-lang", "{label}" }
                button {
                    class: "md-code-copy",
                    "aria-label": "Copy code",
                    onclick: {
                        let code = code.clone();
                        move |_| {
                            let _ = dioxus::document::eval(&copy_script(&code));
                            copied.set(true);
                            spawn(async move {
                                let mut done = dioxus::document::eval(CONFIRM_RESET_JS);
                                let _ = done.recv::<bool>().await;
                                copied.set(false);
                            });
                        }
                    },
                    if copied() { "Copied" } else { "Copy" }
                }
            }
            pre {
                code {
                    for (index, token) in tokens.into_iter().enumerate() {
                        span { key: "{index}", class: ui_highlight::css_class(token.kind), "{token.text}" }
                    }
                }
            }
        }
    }
}

#[component]
pub fn MarkdownBody(message_id: String, content: String) -> Element {
    let blocks = use_memo(move || parse_markdown_cached(message_id.as_str(), content.as_str()));
    rsx! {
        div { class: "md-body",
            for (idx, block) in blocks().into_iter().enumerate() {
                {match block {
                    MdBlock::Heading { level, text } => match level {
                        1 => rsx! { h1 { key: "{idx}", class: "md-h1", InlineMarkdown { text } } },
                        2 => rsx! { h2 { key: "{idx}", class: "md-h2", InlineMarkdown { text } } },
                        3 => rsx! { h3 { key: "{idx}", class: "md-h3", InlineMarkdown { text } } },
                        _ => rsx! { h4 { key: "{idx}", class: "md-h4", InlineMarkdown { text } } },
                    },
                    MdBlock::CodeBlock { lang, code } => rsx! {
                        CodeBlock { key: "{idx}", lang, code }
                    },
                    MdBlock::Rule => rsx! {
                        hr { key: "{idx}", class: "md-hr" }
                    },
                    MdBlock::Quote(lines) => rsx! {
                        blockquote { key: "{idx}", class: "md-quote",
                            for (l_idx, line) in lines.into_iter().enumerate() {
                                p { key: "{l_idx}", InlineMarkdown { text: line } }
                            }
                        }
                    },
                    MdBlock::UnorderedList(items) => rsx! {
                        ul { key: "{idx}", class: "md-list",
                            for (i_idx, item) in items.into_iter().enumerate() {
                                li { key: "{i_idx}", InlineMarkdown { text: item } }
                            }
                        }
                    },
                    MdBlock::OrderedList { start, items } => rsx! {
                        ol { key: "{idx}", class: "md-list md-ol", start: "{start}",
                            for (i_idx, item) in items.into_iter().enumerate() {
                                li { key: "{i_idx}", InlineMarkdown { text: item } }
                            }
                        }
                    },
                    MdBlock::Table { headers, rows } => rsx! {
                        div { key: "{idx}", class: "md-table-wrap",
                            table { class: "md-table",
                                thead {
                                    tr {
                                        for (h_idx, header) in headers.into_iter().enumerate() {
                                            th { key: "{h_idx}", InlineMarkdown { text: header } }
                                        }
                                    }
                                }
                                tbody {
                                    for (r_idx, row) in rows.into_iter().enumerate() {
                                        tr { key: "{r_idx}",
                                            for (c_idx, cell) in row.into_iter().enumerate() {
                                                td { key: "{c_idx}", InlineMarkdown { text: cell } }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    },
                    MdBlock::Paragraph(text) => rsx! {
                        p { key: "{idx}", class: "md-p", InlineMarkdown { text } }
                    },
                }}
            }
        }
    }
}

#[component]
pub fn InlineMarkdown(text: String) -> Element {
    let spans = parse_inline(&text);
    rsx! {
        for (idx, span) in spans.into_iter().enumerate() {
            RenderSpan { key: "{idx}", span }
        }
    }
}

#[component]
pub(super) fn RenderSpan(span: InlineSpan) -> Element {
    match span {
        InlineSpan::Text(s) => rsx! { "{s}" },
        InlineSpan::Code(code) => rsx! { code { class: "md-inline-code", "{code}" } },
        InlineSpan::Bold(sub_spans) => rsx! {
            strong {
                for (idx, sub) in sub_spans.into_iter().enumerate() {
                    RenderSpan { key: "{idx}", span: sub }
                }
            }
        },
        InlineSpan::Italic(s) => rsx! { em { "{s}" } },
        InlineSpan::Link { label, url } => rsx! {
            a { class: "md-link", href: "{url}", target: "_blank", rel: "noopener noreferrer", "{label}" }
        },
    }
}
