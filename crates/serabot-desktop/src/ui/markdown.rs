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
    /// A checklist (`- [ ] todo`, `- [x] done`).
    TaskList(Vec<TaskItem>),
    /// A handwritten note (`:::hand` … `:::`), at most a few lines, for a
    /// short note or sign-off (0.74.3). Rendered in the hand font; when the
    /// owner turns handwritten notes off it reads as a plain paragraph.
    HandNote(String),
    Paragraph(String),
}

/// One checkbox item of a markdown task list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskItem {
    pub checked: bool,
    pub text: String,
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
/// A table separator row: pipes around cells made of dashes and optional colons.
pub fn is_table_separator(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.contains('-') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

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
    Strike(Vec<InlineSpan>),
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

/// Parsed markdown blocks keyed by message id and content hash, with bounded
/// FIFO eviction so streaming replies do not flush the entire cache at once.
pub(super) fn parse_markdown_cached(
    message_id: &str,
    content: &str,
) -> std::sync::Arc<Vec<MdBlock>> {
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex, OnceLock};
    #[derive(Default)]
    struct MarkdownCache {
        order: VecDeque<(String, u64)>,
        entries: HashMap<(String, u64), Arc<Vec<MdBlock>>>,
    }
    static CACHE: OnceLock<Mutex<MarkdownCache>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(MarkdownCache::default()));
    let key = (message_id.to_owned(), markdown_content_hash(content));
    let mut guard = cache.lock().expect("markdown cache");
    if let Some(blocks) = guard.entries.get(&key) {
        return Arc::clone(blocks);
    }
    let blocks = Arc::new(parse_markdown_blocks(content));
    while guard.entries.len() >= MD_CACHE_MAX {
        if let Some(oldest) = guard.order.pop_front() {
            guard.entries.remove(&oldest);
        } else {
            guard.entries.clear();
            break;
        }
    }
    guard.order.push_back(key.clone());
    guard.entries.insert(key, Arc::clone(&blocks));
    blocks
}

fn is_horizontal_rule(trimmed: &str) -> bool {
    let mut non_space = trimmed.chars().filter(|c| !c.is_whitespace());
    let Some(marker) = non_space.next() else {
        return false;
    };
    if !matches!(marker, '-' | '*' | '_') {
        return false;
    }
    trimmed.chars().all(|c| c == marker || c.is_whitespace())
        && trimmed.chars().filter(|c| *c == marker).count() >= 3
}

fn parse_fence_open(trimmed: &str) -> Option<(char, usize, String)> {
    let ch = trimmed.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let len = trimmed.chars().take_while(|c| *c == ch).count();
    if len < 3 {
        return None;
    }
    let lang = trimmed[len..].trim();
    if ch == '`' && lang.contains('`') {
        return None;
    }
    Some((ch, len, lang.to_owned()))
}

fn is_fence_close(trimmed: &str, ch: char, open_len: usize) -> bool {
    trimmed.chars().all(|c| c == ch) && trimmed.chars().count() >= open_len
}

fn is_heading_line(trimmed: &str) -> Option<(usize, String)> {
    if !trimmed.starts_with('#') {
        return None;
    }
    let hash_count = trimmed.chars().take_while(|c| *c == '#').count();
    let after_hashes = trimmed.get(hash_count..).unwrap_or_default();
    if hash_count > 6 || !after_hashes.starts_with(' ') {
        return None;
    }
    let rest = after_hashes.trim();
    let title = rest
        .strip_prefix("**")
        .and_then(|inner| inner.strip_suffix("**"))
        .unwrap_or(rest);
    Some((hash_count, title.trim().to_owned()))
}

fn unordered_item_text(trimmed: &str) -> Option<&str> {
    trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
        .or_else(|| trimmed.strip_prefix("• "))
        .map(str::trim)
}

fn parse_task_marker(text: &str) -> Option<(bool, String)> {
    if let Some(rest) = text.strip_prefix("[ ] ") {
        Some((false, rest.trim().to_owned()))
    } else {
        text.strip_prefix("[x] ")
            .or_else(|| text.strip_prefix("[X] "))
            .map(|rest| (true, rest.trim().to_owned()))
    }
}

fn parse_ordered_prefix(trimmed: &str) -> Option<(usize, &str)> {
    let digits_end = trimmed
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map(|(i, _)| i)?;
    if digits_end == 0 {
        return None;
    }
    let after = &trimmed[digits_end..];
    let sep = after.chars().next()?;
    if sep != '.' && sep != ')' {
        return None;
    }
    let rest = &after[1..];
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let num = trimmed[..digits_end].parse::<usize>().ok()?;
    Some((num, rest.trim()))
}

fn parse_table_row(row: &str) -> Vec<String> {
    let mut trimmed = row.trim();
    if let Some(stripped) = trimmed.strip_prefix('|') {
        trimmed = stripped;
    }
    if trimmed.ends_with('|') && !trimmed.ends_with("\\|") {
        trimmed = &trimmed[..trimmed.len() - 1];
    }
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut in_code = false;
    let mut chars = trimmed.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' && chars.peek() == Some(&'|') {
            chars.next();
            current.push('|');
            continue;
        }
        if ch == '`' {
            in_code = !in_code;
            current.push(ch);
            continue;
        }
        if ch == '|' && !in_code {
            cells.push(current.trim().to_owned());
            current.clear();
            continue;
        }
        current.push(ch);
    }
    cells.push(current.trim().to_owned());
    cells
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

        // Handwritten note (`:::hand` … `:::`): a short note or sign-off in
        // the hand font. The closing fence is exactly `:::`; without one the
        // block never opens and the lines stay a plain paragraph.
        if trimmed == ":::hand" {
            let mut note_lines = Vec::new();
            let mut closed = false;
            let mut j = i + 1;
            while j < lines.len() {
                if lines[j].trim() == ":::" {
                    closed = true;
                    break;
                }
                note_lines.push(lines[j]);
                j += 1;
            }
            if closed {
                i = j + 1;
                blocks.push(MdBlock::HandNote(note_lines.join("\n")));
                continue;
            }
            // Unclosed: fall through so `:::hand` and its lines are parsed
            // as an ordinary paragraph.
        }

        // Fenced code block (``` or ~~~, supporting longer outer fences)
        if let Some((fence_ch, fence_len, lang)) = parse_fence_open(trimmed) {
            let mut code_lines = Vec::new();
            i += 1;
            while i < lines.len() {
                if is_fence_close(lines[i].trim(), fence_ch, fence_len) {
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
        if is_horizontal_rule(trimmed) {
            blocks.push(MdBlock::Rule);
            i += 1;
            continue;
        }

        // Heading
        if let Some((level, text)) = is_heading_line(trimmed) {
            blocks.push(MdBlock::Heading { level, text });
            i += 1;
            continue;
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
                    && is_heading_line(q_trim).is_none()
                    && unordered_item_text(q_trim).is_none()
                    && parse_ordered_prefix(q_trim).is_none()
                    && parse_fence_open(q_trim).is_none()
                    && !is_horizontal_rule(q_trim)
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
        if trimmed.starts_with('|') && i + 1 < lines.len() && is_table_separator(lines[i + 1]) {
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

        // Unordered list (including loose lists, `+ ` bullets, and `- [ ]` tasks)
        if unordered_item_text(trimmed).is_some() {
            let mut items = Vec::new();
            let mut depths: Vec<u8> = Vec::new();
            let mut indents: Vec<usize> = Vec::new();
            while i < lines.len() {
                let l_trim = lines[i].trim();
                if l_trim.is_empty() && !items.is_empty() {
                    let next = lines[i + 1..]
                        .iter()
                        .map(|l| l.trim())
                        .find(|l| !l.is_empty());
                    if next.is_some_and(|l| unordered_item_text(l).is_some()) {
                        i += 1;
                        continue;
                    }
                    break;
                }
                if let Some(item_text) = unordered_item_text(l_trim) {
                    depths.push(list_depth(&mut indents, leading_indent(lines[i])));
                    items.push(item_text.to_owned());
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
            } else if !items.is_empty()
                && items.iter().all(|item| parse_task_marker(item).is_some())
            {
                let tasks = items
                    .into_iter()
                    .filter_map(|item| {
                        parse_task_marker(&item).map(|(checked, text)| TaskItem { checked, text })
                    })
                    .collect();
                blocks.push(MdBlock::TaskList(tasks));
            } else {
                blocks.push(MdBlock::UnorderedList(items));
            }
            continue;
        }

        // Ordered list (`1. ` or `1) `)
        if let Some((start, _)) = parse_ordered_prefix(trimmed) {
            // The first number is the list's start: a list that continues
            // after other blocks (or a blank line) must not restart at 1.
            let mut items = Vec::new();
            let mut depths: Vec<u8> = Vec::new();
            let mut kinds: Vec<bool> = Vec::new();
            let mut indents: Vec<usize> = Vec::new();
            while i < lines.len() {
                let l_trim = lines[i].trim();
                if l_trim.is_empty() && !items.is_empty() {
                    // A blank line inside a loose list keeps it one list.
                    let next = lines[i + 1..]
                        .iter()
                        .map(|l| l.trim())
                        .find(|l| !l.is_empty());
                    let continues = next.is_some_and(|l| parse_ordered_prefix(l).is_some());
                    if continues {
                        i += 1;
                        continue;
                    }
                    break;
                }
                if let Some((_, item_text)) = parse_ordered_prefix(l_trim) {
                    depths.push(list_depth(&mut indents, leading_indent(lines[i])));
                    kinds.push(true);
                    items.push(item_text.to_owned());
                    i += 1;
                } else if !items.is_empty()
                    && leading_indent(lines[i]) > 0
                    && let Some(item_text) = unordered_item_text(l_trim)
                {
                    // A bullet under a numbered item.
                    depths.push(list_depth(&mut indents, leading_indent(lines[i])));
                    kinds.push(false);
                    items.push(item_text.to_owned());
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
                || parse_fence_open(p_trim).is_some()
                || is_heading_line(p_trim).is_some()
                || p_trim.starts_with("> ")
                || p_trim == ">"
                || unordered_item_text(p_trim).is_some()
                || parse_ordered_prefix(p_trim).is_some()
                || (p_trim.starts_with('|')
                    && i + 1 < lines.len()
                    && is_table_separator(lines[i + 1]))
                || is_horizontal_rule(p_trim)
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

fn push_inline_text(spans: &mut Vec<InlineSpan>, chunk: &str) {
    if chunk.is_empty() {
        return;
    }
    if let Some(InlineSpan::Text(last_text)) = spans.last_mut() {
        last_text.push_str(chunk);
    } else {
        spans.push(InlineSpan::Text(chunk.to_owned()));
    }
}

fn is_escapeable_punct(c: char) -> bool {
    matches!(
        c,
        '\\' | '*' | '_' | '`' | '[' | ']' | '(' | ')' | '~' | '|' | '!' | '#' | '-' | '+' | '.'
    )
}

fn prev_char_is_word(spans: &[InlineSpan]) -> bool {
    spans
        .last()
        .and_then(|span| match span {
            InlineSpan::Text(s) => s.chars().last(),
            _ => None,
        })
        .is_some_and(char::is_alphanumeric)
}

fn trim_bare_url(raw: &str) -> &str {
    let mut end = raw.len();
    while end > "https://".len() {
        let ch = raw[..end].chars().next_back().unwrap_or(' ');
        if matches!(ch, '.' | ',' | ';' | ':' | '!' | '?') {
            end -= ch.len_utf8();
            continue;
        }
        if ch == ')' {
            let slice = &raw[..end];
            let opens = slice.matches('(').count();
            let closes = slice.matches(')').count();
            if closes > opens {
                end -= 1;
                continue;
            }
        }
        break;
    }
    &raw[..end]
}

pub fn parse_inline(text: &str) -> Vec<InlineSpan> {
    parse_inline_with_opts(text, true, true)
}

fn parse_inline_with_opts(text: &str, allow_italic: bool, allow_links: bool) -> Vec<InlineSpan> {
    let mut spans = Vec::new();
    let mut rest = text;

    while !rest.is_empty() {
        // 0. Backslash escape on markdown punctuation.
        if rest.starts_with('\\')
            && let Some(next) = rest[1..].chars().next()
            && is_escapeable_punct(next)
        {
            let mut buf = [0u8; 4];
            push_inline_text(&mut spans, next.encode_utf8(&mut buf));
            rest = &rest[1 + next.len_utf8()..];
            continue;
        }

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

        // 1b. Bold + italic: ***...***
        if allow_italic
            && rest.starts_with("***")
            && let Some(end) = rest[3..].find("***")
        {
            let inner = &rest[3..3 + end];
            if !inner.is_empty() {
                spans.push(InlineSpan::Bold(vec![InlineSpan::Italic(inner.to_owned())]));
                rest = &rest[3 + end + 3..];
                continue;
            }
        }

        // 2. Bold: **...**
        if rest.starts_with("**")
            && let Some(end) = rest[2..].find("**")
        {
            let inner = &rest[2..2 + end];
            let sub_spans = parse_inline_with_opts(inner, allow_italic, allow_links);
            spans.push(InlineSpan::Bold(sub_spans));
            rest = &rest[2 + end + 2..];
            continue;
        }

        // 2b. Strikethrough: ~~...~~
        if rest.starts_with("~~")
            && let Some(end) = rest[2..].find("~~")
        {
            let inner = &rest[2..2 + end];
            if !inner.is_empty() {
                let sub_spans = parse_inline_with_opts(inner, allow_italic, allow_links);
                spans.push(InlineSpan::Strike(sub_spans));
                rest = &rest[2 + end + 2..];
                continue;
            }
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
        if allow_links
            && rest.starts_with('[')
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

        // 4b. Bare https://... autolink (checked via safe_chat_link)
        if allow_links && rest.starts_with("https://") && !prev_char_is_word(&spans) {
            let raw_end = rest
                .char_indices()
                .find(|(_, c)| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\''))
                .map(|(idx, _)| idx)
                .unwrap_or(rest.len());
            let candidate = trim_bare_url(&rest[..raw_end]);
            if candidate.len() > "https://".len() && chat_links::safe_chat_link(candidate).is_some()
            {
                spans.push(InlineSpan::Link {
                    label: candidate.to_owned(),
                    url: candidate.to_owned(),
                });
                rest = &rest[candidate.len()..];
                continue;
            }
        }

        // 5. Italic: *...* (where not **)
        if allow_italic
            && rest.starts_with('*')
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

        // 5b. Italic with underscores: _..._ on word boundaries (never breaks snake_case)
        if allow_italic
            && rest.starts_with('_')
            && !rest.starts_with("__")
            && !prev_char_is_word(&spans)
            && let Some(end) = rest[1..].find('_')
        {
            let inner = &rest[1..1 + end];
            let after = &rest[1 + end + 1..];
            let followed_by_word = after.chars().next().is_some_and(char::is_alphanumeric);
            if !inner.is_empty()
                && !inner.starts_with(['_', ' '])
                && !inner.ends_with(['_', ' '])
                && !followed_by_word
            {
                spans.push(InlineSpan::Italic(inner.to_owned()));
                rest = after;
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
                matches!(c, '\\' | '*' | '_' | '`' | '~' | '[')
                    || (*c == '!' && rest[idx + 1..].starts_with('['))
                    || (allow_links && *c == 'h' && rest[*idx..].starts_with("https://"))
            })
            .map(|(idx, _)| idx)
            .unwrap_or(rest.len());

        push_inline_text(&mut spans, &rest[..next_pos]);
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
                span { class: "md-code-meta",
                    if lines == 1 { "1 line" } else { "{lines} lines" }
                }
                button {
                    class: if copied() { "md-code-copy copied" } else { "md-code-copy" },
                    r#type: "button",
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
                    if copied() {
                        Icon { name: "check", size: 12 }
                        " Copied"
                    } else {
                        Icon { name: "copy", size: 12 }
                        " Copy"
                    }
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
pub fn MarkdownBody(
    message_id: String,
    content: String,
    #[props(default = true)] handwritten: bool,
) -> Element {
    let blocks = use_memo(move || parse_markdown_cached(message_id.as_str(), content.as_str()));
    rsx! {
        div { class: "md-body",
            for (idx, block) in blocks().iter().cloned().enumerate() {
                {match block {
                    MdBlock::HandNote(text) => {
                        if handwritten {
                            let lines: Vec<String> = text
                                .lines()
                                .map(str::trim)
                                .filter(|line| !line.is_empty())
                                .map(str::to_owned)
                                .collect();
                            rsx! {
                                div { key: "{idx}", class: "md-hand",
                                    for (l_idx, line) in lines.into_iter().enumerate() {
                                        if l_idx > 0 {
                                            br { key: "b{l_idx}" }
                                        }
                                        // No key here: Dioxus only allows a key on the
                                        // first node in a block, and the `br` above is
                                        // that node when present. A key on this
                                        // component is a `-D warnings` error on Windows
                                        // (deprecated `Warning` struct in the RSX
                                        // expansion) while Linux clippy stays silent.
                                        InlineMarkdown { text: line }
                                    }
                                }
                            }
                        } else {
                            rsx! {
                                p { key: "{idx}", class: "md-p",
                                    InlineMarkdown { text: text.lines().collect::<Vec<_>>().join(" ") }
                                }
                            }
                        }
                    },
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
                                if !line.is_empty() {
                                    p { key: "{l_idx}", InlineMarkdown { text: line } }
                                }
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
                    MdBlock::TaskList(items) => rsx! {
                        ul { key: "{idx}", class: "md-list md-task-list", role: "list",
                            for (i_idx, item) in items.into_iter().enumerate() {
                                li {
                                    key: "{i_idx}",
                                    class: if item.checked { "md-task done" } else { "md-task" },
                                    span {
                                        class: if item.checked { "md-task-box checked" } else { "md-task-box" },
                                        "aria-hidden": "true",
                                        if item.checked {
                                            Icon { name: "check", size: 11 }
                                        }
                                    }
                                    span { class: "md-li-text", InlineMarkdown { text: item.text } }
                                }
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
        InlineSpan::Italic(s) => {
            let sub_spans = parse_inline_with_opts(&s, false, true);
            rsx! {
                em {
                    for (idx, sub) in sub_spans.into_iter().enumerate() {
                        RenderSpan { key: "{idx}", span: sub }
                    }
                }
            }
        }
        InlineSpan::Strike(sub_spans) => rsx! {
            del { class: "md-del",
                for (idx, sub) in sub_spans.into_iter().enumerate() {
                    RenderSpan { key: "{idx}", span: sub }
                }
            }
        },
        InlineSpan::Link { label, url } => {
            let mut session = try_consume_context::<Signal<Session>>();
            let label_spans = parse_inline_with_opts(&label, true, false);
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
                        for (idx, sub) in label_spans.into_iter().enumerate() {
                            RenderSpan { key: "{idx}", span: sub }
                        }
                    }
                },
                // Anything else is plain text: the label, no link.
                None => rsx! {
                    span { class: "md-link-blocked", title: "Not a link the app opens (only https links open)",
                        for (idx, sub) in label_spans.into_iter().enumerate() {
                            RenderSpan { key: "{idx}", span: sub }
                        }
                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hand_note_parses_closed_and_unclosed_and_keeps_inline_marks() {
        let blocks =
            parse_markdown_blocks("Done.\n\n:::hand\nSee you tomorrow\n— Main\n:::\n\nAfter.");
        assert_eq!(
            blocks,
            vec![
                MdBlock::Paragraph("Done.".into()),
                MdBlock::HandNote("See you tomorrow\n— Main".into()),
                MdBlock::Paragraph("After.".into()),
            ]
        );
        // No closing fence: the opener and its lines stay a plain paragraph.
        let blocks = parse_markdown_blocks(":::hand\nNever closed\nstill going");
        assert_eq!(
            blocks,
            vec![MdBlock::Paragraph(
                ":::hand Never closed still going".into()
            )]
        );
        // Bold-italic survives inside a hand note.
        let blocks = parse_markdown_blocks(":::hand\n***both***\n:::");
        assert_eq!(blocks, vec![MdBlock::HandNote("***both***".into())]);
        assert_eq!(
            parse_inline("***both***"),
            vec![InlineSpan::Bold(vec![InlineSpan::Italic("both".into())])]
        );
        // A closing fence that is not exactly `:::` does not close the note.
        let blocks = parse_markdown_blocks(":::hand\none\n::: more\n:::");
        assert_eq!(blocks, vec![MdBlock::HandNote("one\n::: more".into())]);
    }

    #[test]
    fn paragraphs_break_before_ordered_lists_and_fences_and_tasks() {
        let blocks =
            parse_markdown_blocks("Intro line\n1. First\n2) Second\n\n- [ ] Todo\n- [x] Done");
        assert_eq!(
            blocks,
            vec![
                MdBlock::Paragraph("Intro line".into()),
                MdBlock::OrderedList {
                    start: 1,
                    items: vec!["First".into(), "Second".into()],
                },
                MdBlock::TaskList(vec![
                    TaskItem {
                        checked: false,
                        text: "Todo".into(),
                    },
                    TaskItem {
                        checked: true,
                        text: "Done".into(),
                    },
                ]),
            ]
        );
    }

    #[test]
    fn loose_unordered_lists_and_tilde_or_longer_fences_parse() {
        let md = "- one\n\n+ two\n\n~~~~markdown\n```rust\nfn x() {}\n```\n~~~~";
        let blocks = parse_markdown_blocks(md);
        assert_eq!(
            blocks,
            vec![
                MdBlock::UnorderedList(vec!["one".into(), "two".into()]),
                MdBlock::CodeBlock {
                    lang: "markdown".into(),
                    code: "```rust\nfn x() {}\n```".into(),
                },
            ]
        );
    }

    #[test]
    fn table_row_keeps_escaped_and_code_pipes() {
        let md = "| Expr | Escaped |\n|---|---|\n| `a | b` | x \\| y |";
        let blocks = parse_markdown_blocks(md);
        assert_eq!(
            blocks,
            vec![MdBlock::Table {
                headers: vec!["Expr".into(), "Escaped".into()],
                rows: vec![vec!["`a | b`".into(), "x | y".into()]],
                aligns: vec![Align::Left, Align::Left],
            }]
        );
    }

    #[test]
    fn inline_supports_strike_underscore_escapes_bold_italic_and_safe_autolinks() {
        assert_eq!(
            parse_inline("~~old~~ _em_ snake_case_name \\*lit\\* ***both***"),
            vec![
                InlineSpan::Strike(vec![InlineSpan::Text("old".into())]),
                InlineSpan::Text(" ".into()),
                InlineSpan::Italic("em".into()),
                InlineSpan::Text(" snake_case_name *lit* ".into()),
                InlineSpan::Bold(vec![InlineSpan::Italic("both".into())]),
            ]
        );
        assert_eq!(
            parse_inline("visit https://docs.example.com/a_(b). now"),
            vec![
                InlineSpan::Text("visit ".into()),
                InlineSpan::Link {
                    label: "https://docs.example.com/a_(b)".into(),
                    url: "https://docs.example.com/a_(b)".into(),
                },
                InlineSpan::Text(". now".into()),
            ]
        );
        // http:// is never autolinked as clickable.
        assert_eq!(
            parse_inline("plain http://insecure.example"),
            vec![InlineSpan::Text("plain http://insecure.example".into())]
        );
    }

    #[test]
    fn markdown_cache_evicts_bounded_without_clearing_recent() {
        for idx in 0..=MD_CACHE_MAX + 5 {
            let id = format!("cache-test-{idx}");
            let parsed = parse_markdown_cached(&id, &format!("msg {idx}"));
            assert_eq!(*parsed, vec![MdBlock::Paragraph(format!("msg {idx}"))]);
        }
    }
}
