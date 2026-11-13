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
        /// One per column, from the `:---:` markers of the separator row.
        aligns: Vec<Align>,
    },
    /// A list with indented items (up to three levels), or bullets inside a
    /// numbered list. Plain flat lists keep the simpler variants above.
    NestedList {
        start: usize,
        items: Vec<NestedItem>,
    },
    Paragraph(String),
}

/// Column alignment of a table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

impl Align {
    pub fn class(self) -> &'static str {
        match self {
            Align::Left => "md-al-left",
            Align::Center => "md-al-center",
            Align::Right => "md-al-right",
        }
    }
}

/// One line of a nested list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NestedItem {
    /// 0 to [`MAX_LIST_DEPTH`].
    pub depth: u8,
    pub ordered: bool,
    pub text: String,
}

/// Deepest list level shown (three levels: 0, 1, 2).
pub const MAX_LIST_DEPTH: u8 = 2;

/// Spaces before the first character; a tab counts as four.
pub fn leading_indent(line: &str) -> usize {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

/// The level of a list line, from its indent and the indents seen so far
/// (`stack`). A deeper indent opens a level, a shallower one closes levels, so
/// two- and four-space nesting both work. Capped at [`MAX_LIST_DEPTH`].
pub fn list_depth(stack: &mut Vec<usize>, indent: usize) -> u8 {
    while stack.last().is_some_and(|top| indent < *top) {
        stack.pop();
    }
    match stack.last() {
        Some(top) if *top == indent => {}
        _ => stack.push(indent),
    }
    (stack.len() - 1).min(MAX_LIST_DEPTH as usize) as u8
}

/// The marker text of each nested item: bullets that change with the level,
/// and numbers that count per level and restart under a new parent.
pub fn nested_markers(start: usize, items: &[NestedItem]) -> Vec<String> {
    let mut counters = [0usize; MAX_LIST_DEPTH as usize + 1];
    let mut markers = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let depth = item.depth as usize;
        for deeper in counters.iter_mut().skip(depth + 1) {
            *deeper = 0;
        }
        counters[depth] += 1;
        markers.push(if item.ordered {
            let number = if depth == 0 && index == 0 {
                start
            } else if depth == 0 {
                start + counters[0] - 1
            } else {
                counters[depth]
            };
            format!("{number}.")
        } else {
            ["\u{2022}", "\u{25E6}", "\u{25AA}"][depth].to_owned()
        });
    }
    markers
}

/// The alignment of each column from a table separator row such as
/// `|:---|:---:|---:|`.
pub fn parse_aligns(separator: &str) -> Vec<Align> {
    separator
        .trim()
        .trim_start_matches('|')
        .trim_end_matches('|')
        .split('|')
        .map(|cell| {
            let cell = cell.trim();
            match (cell.starts_with(':'), cell.ends_with(':')) {
                (true, true) => Align::Center,
                (false, true) => Align::Right,
                _ => Align::Left,
            }
        })
        .collect()
}

/// Code taller than this shows a Show all button.
pub const CODE_LINES_BEFORE_CAP: usize = 16;

pub fn code_is_long(code: &str) -> bool {
    code.lines().count() > CODE_LINES_BEFORE_CAP
}

#[derive(Clone, Debug, PartialEq)]
pub enum InlineSpan {
    Text(String),
    Code(String),
    Bold(Vec<InlineSpan>),
    Italic(String),
    Link {
        label: String,
        url: String,
    },
    /// `![alt](url)`: shown as a chip, never loaded.
    Image {
        alt: String,
        url: String,
    },
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
            let mut aligns = parse_aligns(lines[i + 1]);
            aligns.resize(headers.len(), Align::Left);
            i += 2;
            let mut rows = Vec::new();
            while i < lines.len() && lines[i].trim().starts_with('|') {
                rows.push(parse_table_row(lines[i]));
                i += 1;
            }
            blocks.push(MdBlock::Table {
                headers,
                rows,
                aligns,
            });
            continue;
        }

        // Unordered list
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("• ") {
            let mut items = Vec::new();
            let mut depths: Vec<u8> = Vec::new();
            let mut indents: Vec<usize> = Vec::new();
            while i < lines.len() {
                let l_trim = lines[i].trim();
                if l_trim.starts_with("- ") || l_trim.starts_with("* ") || l_trim.starts_with("• ")
                {
                    depths.push(list_depth(&mut indents, leading_indent(lines[i])));
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
            if depths.iter().any(|depth| *depth > 0) {
                blocks.push(MdBlock::NestedList {
                    start: 1,
                    items: items
                        .into_iter()
                        .zip(depths)
                        .map(|(text, depth)| NestedItem {
                            depth,
                            ordered: false,
                            text,
                        })
                        .collect(),
                });
            } else {
                blocks.push(MdBlock::UnorderedList(items));
            }
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
            let mut depths: Vec<u8> = Vec::new();
            let mut kinds: Vec<bool> = Vec::new();
            let mut indents: Vec<usize> = Vec::new();
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
                    depths.push(list_depth(&mut indents, leading_indent(lines[i])));
                    kinds.push(true);
                    items.push(item_text);
                    i += 1;
                } else if !items.is_empty()
                    && leading_indent(lines[i]) > 0
                    && (l_trim.starts_with("- ")
                        || l_trim.starts_with("* ")
                        || l_trim.starts_with("• "))
                {
                    // A bullet under a numbered item.
                    let marker_len = if l_trim.starts_with("• ") { 3 } else { 2 };
                    depths.push(list_depth(&mut indents, leading_indent(lines[i])));
                    kinds.push(false);
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
            if depths.iter().any(|depth| *depth > 0) || kinds.iter().any(|ordered| !ordered) {
                blocks.push(MdBlock::NestedList {
                    start,
                    items: items
                        .into_iter()
                        .zip(depths.into_iter().zip(kinds))
                        .map(|(text, (depth, ordered))| NestedItem {
                            depth,
                            ordered,
                            text,
                        })
                        .collect(),
                });
            } else {
                blocks.push(MdBlock::OrderedList { start, items });
            }
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

        // 4a. Image: ![alt](url). Kept as a chip; the app never fetches it.
        if rest.starts_with("![")
            && let Some(close_bracket) = rest.find(']')
            && rest[close_bracket..].starts_with("](")
        {
            let after_bracket = &rest[close_bracket + 2..];
            if let Some(close_paren) = after_bracket.find(')') {
                spans.push(InlineSpan::Image {
                    alt: rest[2..close_bracket].to_owned(),
                    url: after_bracket[..close_paren].to_owned(),
                });
                rest = &after_bracket[close_paren + 1..];
                continue;
            }
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
            .find(|(idx, c)| {
                matches!(c, '*' | '`' | '[') || (*c == '!' && rest[idx + 1..].starts_with('['))
            })
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
    let mut expanded = use_signal(|| false);
    let long = code_is_long(&code);
    let capped = long && !expanded();
    let lines = code.lines().count();
    rsx! {
        div { class: if capped { "md-code-block capped" } else { "md-code-block" },
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
            if long {
                button {
                    class: "md-code-more",
                    r#type: "button",
                    "aria-expanded": if expanded() { "true" } else { "false" },
                    onclick: move |_| expanded.set(!expanded()),
                    if expanded() { "Show less" } else { "Show all {lines} lines" }
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
                    MdBlock::NestedList { start, items } => {
                        let markers = nested_markers(start, &items);
                        rsx! {
                            ul { key: "{idx}", class: "md-list md-nested", role: "list",
                                for (i_idx, (item, marker)) in items.into_iter().zip(markers).enumerate() {
                                    li { key: "{i_idx}", class: "md-li md-d{item.depth}",
                                        span { class: "md-marker", "aria-hidden": "true", "{marker}" }
                                        span { class: "md-li-text", InlineMarkdown { text: item.text } }
                                    }
                                }
                            }
                        }
                    }
                    MdBlock::Table { headers, rows, aligns } => rsx! {
                        div { key: "{idx}", class: "md-table-wrap",
                            table { class: "md-table",
                                thead {
                                    tr {
                                        for (h_idx, header) in headers.into_iter().enumerate() {
                                            th {
                                                key: "{h_idx}",
                                                class: aligns.get(h_idx).copied().unwrap_or_default().class(),
                                                InlineMarkdown { text: header }
                                            }
                                        }
                                    }
                                }
                                tbody {
                                    for (r_idx, row) in rows.into_iter().enumerate() {
                                        tr { key: "{r_idx}",
                                            for (c_idx, cell) in row.into_iter().enumerate() {
                                                td {
                                                    key: "{c_idx}",
                                                    class: aligns.get(c_idx).copied().unwrap_or_default().class(),
                                                    InlineMarkdown { text: cell }
                                                }
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
        InlineSpan::Link { label, url } => {
            let mut session = try_consume_context::<Signal<Session>>();
            match chat_links::safe_chat_link(&url) {
                // A button, not an anchor: the page never navigates by itself,
                // and the click goes through the checked open path.
                Some(link) => rsx! {
                    button {
                        class: "md-link",
                        r#type: "button",
                        title: "{link}",
                        onclick: move |_| {
                            if let Some(session) = session.as_mut() {
                                session.write().open_in_browser(link.clone());
                            }
                        },
                        "{label}"
                    }
                },
                // Anything else is plain text: the label, no link.
                None => rsx! {
                    span { class: "md-link-blocked", title: "Not a link the app opens (only https links open)", "{label}" }
                },
            }
        }
        InlineSpan::Image { alt, url } => {
            let mut session = try_consume_context::<Signal<Session>>();
            let name = if alt.trim().is_empty() {
                "image".to_owned()
            } else {
                alt.trim().to_owned()
            };
            match chat_links::safe_chat_link(&url) {
                Some(link) => rsx! {
                    button {
                        class: "md-image-chip",
                        r#type: "button",
                        title: "Open the image in your browser: {link}",
                        onclick: move |_| {
                            if let Some(session) = session.as_mut() {
                                session.write().open_in_browser(link.clone());
                            }
                        },
                        Icon { name: "image", size: 13 }
                        span { "{name}" }
                    }
                },
                None => rsx! {
                    span { class: "md-image-chip blocked", title: "Image not shown", Icon { name: "image", size: 13 } span { "{name}" } }
                },
            }
        }
    }
}
