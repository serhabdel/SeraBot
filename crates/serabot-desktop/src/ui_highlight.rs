//! Lightweight syntax highlighting for fenced code blocks. A hand-written
//! tokenizer for the languages the bots answer in most: keywords, strings,
//! comments and numbers. Anything else comes back as plain text. The
//! tokens always rebuild the original code exactly, so highlighting can
//! never change what the Copy button puts on the clipboard.

/// What a token is, for styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Plain,
    Keyword,
    Str,
    Comment,
    Number,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub text: String,
}

/// Per-language rules.
struct Language {
    keywords: &'static [&'static str],
    line_comment: Option<&'static str>,
    block_comment: Option<(&'static str, &'static str)>,
    quotes: &'static [char],
    /// SQL keywords are matched without regard to case.
    case_insensitive: bool,
}

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "false",
    "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
    "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe",
    "use", "where", "while",
];
const JS_KEYWORDS: &[&str] = &[
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "null",
    "return",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "yield",
    "interface",
    "type",
    "implements",
    "readonly",
    "enum",
    "as",
];
const PYTHON_KEYWORDS: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "False", "finally", "for", "from", "global", "if", "import", "in", "is",
    "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "True", "try", "while",
    "with", "yield",
];
const JSON_KEYWORDS: &[&str] = &["true", "false", "null"];
const SHELL_KEYWORDS: &[&str] = &[
    "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if", "in",
    "local", "return", "then", "while", "unset",
];
const SQL_KEYWORDS: &[&str] = &[
    "select",
    "from",
    "where",
    "insert",
    "into",
    "values",
    "update",
    "set",
    "delete",
    "create",
    "table",
    "drop",
    "alter",
    "join",
    "left",
    "right",
    "inner",
    "outer",
    "on",
    "group",
    "by",
    "order",
    "having",
    "limit",
    "offset",
    "as",
    "and",
    "or",
    "not",
    "null",
    "is",
    "in",
    "distinct",
    "union",
    "with",
    "case",
    "when",
    "then",
    "else",
    "end",
    "primary",
    "key",
    "foreign",
    "references",
    "index",
    "default",
    "true",
    "false",
];
const TOML_KEYWORDS: &[&str] = &["true", "false"];

/// The rules for a fence's language tag. Common aliases are accepted.
fn language(lang: &str) -> Option<Language> {
    let tag = lang.trim().to_ascii_lowercase();
    let rules = match tag.as_str() {
        "rust" | "rs" => Language {
            keywords: RUST_KEYWORDS,
            line_comment: Some("//"),
            block_comment: Some(("/*", "*/")),
            quotes: &['"'],
            case_insensitive: false,
        },
        "js" | "javascript" | "ts" | "typescript" | "jsx" | "tsx" => Language {
            keywords: JS_KEYWORDS,
            line_comment: Some("//"),
            block_comment: Some(("/*", "*/")),
            quotes: &['"', '\'', '`'],
            case_insensitive: false,
        },
        "python" | "py" => Language {
            keywords: PYTHON_KEYWORDS,
            line_comment: Some("#"),
            block_comment: None,
            quotes: &['"', '\''],
            case_insensitive: false,
        },
        "json" => Language {
            keywords: JSON_KEYWORDS,
            line_comment: None,
            block_comment: None,
            quotes: &['"'],
            case_insensitive: false,
        },
        "bash" | "sh" | "shell" | "zsh" => Language {
            keywords: SHELL_KEYWORDS,
            line_comment: Some("#"),
            block_comment: None,
            quotes: &['"', '\''],
            case_insensitive: false,
        },
        "sql" => Language {
            keywords: SQL_KEYWORDS,
            line_comment: Some("--"),
            block_comment: Some(("/*", "*/")),
            quotes: &['\''],
            case_insensitive: true,
        },
        "toml" => Language {
            keywords: TOML_KEYWORDS,
            line_comment: Some("#"),
            block_comment: None,
            quotes: &['"', '\''],
            case_insensitive: false,
        },
        _ => return None,
    };
    Some(rules)
}

/// The CSS class a token kind is styled with.
pub fn css_class(kind: TokenKind) -> &'static str {
    match kind {
        TokenKind::Plain => "tok",
        TokenKind::Keyword => "tok tok-kw",
        TokenKind::Str => "tok tok-str",
        TokenKind::Comment => "tok tok-com",
        TokenKind::Number => "tok tok-num",
    }
}

fn push(tokens: &mut Vec<Token>, kind: TokenKind, text: &str) {
    if text.is_empty() {
        return;
    }
    // Adjacent runs of one kind merge, so the output stays short.
    if let Some(last) = tokens.last_mut()
        && last.kind == kind
    {
        last.text.push_str(text);
        return;
    }
    tokens.push(Token {
        kind,
        text: text.to_owned(),
    });
}

/// Does `chars[at..]` start with `needle`?
fn starts_with(chars: &[char], at: usize, needle: &str) -> bool {
    needle
        .chars()
        .enumerate()
        .all(|(offset, expected)| chars.get(at + offset) == Some(&expected))
}

/// Split code into highlighted tokens. Unknown languages yield one plain
/// token. Concatenating every token's text gives back `code` exactly.
pub fn tokenize(lang: &str, code: &str) -> Vec<Token> {
    let Some(rules) = language(lang) else {
        let mut tokens = Vec::new();
        push(&mut tokens, TokenKind::Plain, code);
        return tokens;
    };
    let chars: Vec<char> = code.chars().collect();
    let mut tokens: Vec<Token> = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        // Line comment: to the end of the line.
        if let Some(prefix) = rules.line_comment
            && starts_with(&chars, index, prefix)
        {
            let start = index;
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
            push(
                &mut tokens,
                TokenKind::Comment,
                &chars[start..index].iter().collect::<String>(),
            );
            continue;
        }
        // Block comment: to its closer, or to the end of the input.
        if let Some((open, close)) = rules.block_comment
            && starts_with(&chars, index, open)
        {
            let start = index;
            index += open.chars().count();
            while index < chars.len() && !starts_with(&chars, index, close) {
                index += 1;
            }
            index = (index + close.chars().count()).min(chars.len());
            push(
                &mut tokens,
                TokenKind::Comment,
                &chars[start..index].iter().collect::<String>(),
            );
            continue;
        }
        // String literal. Single and double quotes end at the line break so
        // an unclosed quote cannot swallow the rest of the block.
        if rules.quotes.contains(&ch) {
            let start = index;
            index += 1;
            while index < chars.len() {
                let current = chars[index];
                if current == '\\' {
                    index += 2;
                    continue;
                }
                if current == ch {
                    index += 1;
                    break;
                }
                if current == '\n' && ch != '`' {
                    break;
                }
                index += 1;
            }
            let end = index.min(chars.len());
            push(
                &mut tokens,
                TokenKind::Str,
                &chars[start..end].iter().collect::<String>(),
            );
            index = end;
            continue;
        }
        // Number: a digit not glued to a preceding identifier.
        let glued = index > 0 && (chars[index - 1].is_alphanumeric() || chars[index - 1] == '_');
        if ch.is_ascii_digit() && !glued {
            let start = index;
            while index < chars.len()
                && (chars[index].is_ascii_alphanumeric()
                    || chars[index] == '.'
                    || chars[index] == '_')
            {
                index += 1;
            }
            push(
                &mut tokens,
                TokenKind::Number,
                &chars[start..index].iter().collect::<String>(),
            );
            continue;
        }
        // Identifier or keyword.
        if ch.is_alphabetic() || ch == '_' {
            let start = index;
            while index < chars.len() && (chars[index].is_alphanumeric() || chars[index] == '_') {
                index += 1;
            }
            let word: String = chars[start..index].iter().collect();
            let is_keyword = if rules.case_insensitive {
                rules
                    .keywords
                    .iter()
                    .any(|keyword| keyword.eq_ignore_ascii_case(&word))
            } else {
                rules.keywords.contains(&word.as_str())
            };
            let kind = if is_keyword {
                TokenKind::Keyword
            } else {
                TokenKind::Plain
            };
            push(&mut tokens, kind, &word);
            continue;
        }
        // Anything else: one character of plain text.
        push(&mut tokens, TokenKind::Plain, &ch.to_string());
        index += 1;
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(tokens: &[Token], kind: TokenKind) -> Vec<&str> {
        tokens
            .iter()
            .filter(|token| token.kind == kind)
            .map(|token| token.text.as_str())
            .collect()
    }

    fn rebuilt(tokens: &[Token]) -> String {
        tokens.iter().map(|token| token.text.as_str()).collect()
    }

    #[test]
    fn rust_keywords_strings_comments_and_numbers() {
        let code = "fn main() {\n    let n = 42; // answer\n    let s = \"hi\";\n}";
        let tokens = tokenize("rust", code);
        assert_eq!(kinds(&tokens, TokenKind::Keyword), vec!["fn", "let", "let"]);
        assert_eq!(kinds(&tokens, TokenKind::Number), vec!["42"]);
        assert_eq!(kinds(&tokens, TokenKind::Comment), vec!["// answer"]);
        assert_eq!(kinds(&tokens, TokenKind::Str), vec!["\"hi\""]);
        assert_eq!(rebuilt(&tokens), code);
    }

    #[test]
    fn python_uses_hash_comments_and_capitalised_keywords() {
        let code = "def f(x):\n    # note\n    return None if x is None else 3.5\n";
        let tokens = tokenize("py", code);
        assert_eq!(
            kinds(&tokens, TokenKind::Keyword),
            vec!["def", "return", "None", "if", "is", "None", "else"]
        );
        assert_eq!(kinds(&tokens, TokenKind::Comment), vec!["# note"]);
        assert_eq!(kinds(&tokens, TokenKind::Number), vec!["3.5"]);
        assert_eq!(rebuilt(&tokens), code);
    }

    #[test]
    fn json_highlights_literals_and_strings() {
        let code = "{\"ok\": true, \"n\": -12, \"v\": null}";
        let tokens = tokenize("json", code);
        assert_eq!(kinds(&tokens, TokenKind::Keyword), vec!["true", "null"]);
        assert_eq!(
            kinds(&tokens, TokenKind::Str),
            vec!["\"ok\"", "\"n\"", "\"v\""]
        );
        assert_eq!(kinds(&tokens, TokenKind::Number), vec!["12"]);
        assert_eq!(rebuilt(&tokens), code);
    }

    #[test]
    fn sql_comments_are_dashes_and_keywords_ignore_case() {
        let code = "SELECT name FROM users -- all rows\nwhere id = 7;";
        let tokens = tokenize("sql", code);
        assert_eq!(
            kinds(&tokens, TokenKind::Keyword),
            vec!["SELECT", "FROM", "where"]
        );
        assert_eq!(kinds(&tokens, TokenKind::Comment), vec!["-- all rows"]);
        assert_eq!(rebuilt(&tokens), code);
    }

    #[test]
    fn bash_and_toml_and_js_basics() {
        let shell = tokenize("bash", "if [ -f x ]; then echo \"ok\"; fi # done");
        assert_eq!(kinds(&shell, TokenKind::Keyword), vec!["if", "then", "fi"]);
        assert_eq!(kinds(&shell, TokenKind::Comment), vec!["# done"]);

        let toml = tokenize("toml", "enabled = true\nport = 8080 # http");
        assert_eq!(kinds(&toml, TokenKind::Keyword), vec!["true"]);
        assert_eq!(kinds(&toml, TokenKind::Number), vec!["8080"]);

        let js = "const t = `a\nb`; /* block */ let x = 0x1F;";
        let tokens = tokenize("ts", js);
        assert_eq!(kinds(&tokens, TokenKind::Str), vec!["`a\nb`"]);
        assert_eq!(kinds(&tokens, TokenKind::Comment), vec!["/* block */"]);
        assert_eq!(kinds(&tokens, TokenKind::Number), vec!["0x1F"]);
        assert_eq!(rebuilt(&tokens), js);
    }

    #[test]
    fn every_kind_has_a_class() {
        assert_eq!(css_class(TokenKind::Plain), "tok");
        assert_eq!(css_class(TokenKind::Keyword), "tok tok-kw");
        assert_eq!(css_class(TokenKind::Str), "tok tok-str");
        assert_eq!(css_class(TokenKind::Comment), "tok tok-com");
        assert_eq!(css_class(TokenKind::Number), "tok tok-num");
    }

    #[test]
    fn unknown_language_is_one_plain_token() {
        let code = "let x = 1; // not highlighted";
        let tokens = tokenize("cobol", code);
        assert_eq!(
            tokens,
            vec![Token {
                kind: TokenKind::Plain,
                text: code.to_owned()
            }]
        );
        assert!(tokenize("", "").is_empty());
    }

    #[test]
    fn unclosed_strings_and_comments_stop_safely() {
        let quote = "let s = \"open\nnext line";
        let tokens = tokenize("rust", quote);
        assert_eq!(kinds(&tokens, TokenKind::Str), vec!["\"open"]);
        assert_eq!(rebuilt(&tokens), quote);

        let block = "/* never closed";
        let tokens = tokenize("js", block);
        assert_eq!(kinds(&tokens, TokenKind::Comment), vec!["/* never closed"]);
        assert_eq!(rebuilt(&tokens), block);

        let escaped = "\"a\\\"b\"";
        assert_eq!(rebuilt(&tokenize("json", escaped)), escaped);
    }

    #[test]
    fn identifiers_with_digits_are_not_numbers() {
        let tokens = tokenize("rust", "let x2 = v1 + 7;");
        assert_eq!(kinds(&tokens, TokenKind::Number), vec!["7"]);
        assert_eq!(rebuilt(&tokens), "let x2 = v1 + 7;");
    }

    #[test]
    fn non_ascii_text_round_trips() {
        let code = "let café = \"héllo 🌍\"; // café";
        let tokens = tokenize("rust", code);
        assert_eq!(rebuilt(&tokens), code);
    }
}
