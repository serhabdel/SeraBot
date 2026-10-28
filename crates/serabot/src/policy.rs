use std::collections::BTreeSet;
use std::path::Path;

use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use crate::error::Result;
use crate::util::sha256_hex;

#[derive(Clone)]
pub struct Policy {
    terms: Vec<String>,
    version: String,
}

impl std::fmt::Debug for Policy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Policy")
            .field("version", &self.version)
            .field("term_count", &self.terms.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyScan {
    pub allowed: bool,
    pub violations: usize,
}

impl Policy {
    pub fn empty() -> Self {
        Self {
            terms: Vec::new(),
            version: sha256_hex(b""),
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::empty());
        }
        let raw = std::fs::read_to_string(path)?;
        Ok(Self::from_text(&raw))
    }

    pub fn from_text(raw: &str) -> Self {
        let mut unique = BTreeSet::new();
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let normalized = normalize_terms(line);
            if !normalized.is_empty() && normalized.len() >= 3 {
                unique.insert(normalized);
            }
        }
        let terms: Vec<String> = unique.into_iter().collect();
        let version = sha256_hex(terms.join("\u{1f}").as_bytes());
        Self { terms, version }
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn term_count(&self) -> usize {
        self.terms.len()
    }

    /// Scan normalized text without ever returning the matched terms.
    pub fn scan(&self, text: &str) -> PolicyScan {
        let haystack = format!(" {} ", normalize_body(text));
        let mut violations = 0;
        for term in &self.terms {
            if haystack.contains(&format!(" {term} ")) {
                violations += 1;
            }
        }
        PolicyScan {
            allowed: violations == 0,
            violations,
        }
    }

    pub fn is_allowed(&self, text: &str) -> bool {
        self.scan(text).allowed
    }
}

fn normalize_body(text: &str) -> String {
    let lowered: String = text
        .nfkd()
        .filter(|c| !is_combining_mark(*c) && !is_zero_width(*c))
        .flat_map(char::to_lowercase)
        .collect();
    let mut output = String::with_capacity(lowered.len());
    let mut pending_space = false;
    for c in lowered.chars() {
        if c.is_alphanumeric() {
            if pending_space && !output.is_empty() {
                output.push(' ');
            }
            pending_space = false;
            output.push(c);
        } else {
            pending_space = true;
        }
    }
    output
}

fn normalize_terms(text: &str) -> String {
    normalize_body(text)
}

fn is_zero_width(c: char) -> bool {
    matches!(
        c,
        '\u{200b}'
            | '\u{200c}'
            | '\u{200d}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'
            | '\u{202b}'
            | '\u{202c}'
            | '\u{202d}'
            | '\u{202e}'
            | '\u{2060}'
            | '\u{feff}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_normalized_private_terms() {
        let policy = Policy::from_text("Project Falcon\nSensitive Client");
        assert!(!policy.scan("Notes on the SENSITIVE-CLIENT dossier").allowed);
        assert!(!policy.scan("project   falcon").allowed);
        assert!(policy.scan("A harmless public note").allowed);
    }

    #[test]
    fn ignores_zero_width_and_accents() {
        let policy = Policy::from_text("Société Exemple");
        let text = "socie\u{200b}té   exemple";
        assert!(!policy.scan(text).allowed);
    }

    #[test]
    fn does_not_report_terms() {
        let policy = Policy::from_text("Acme Secret");
        let scan = policy.scan("Acme Secret");
        assert_eq!(scan.violations, 1);
        let debug = format!("{scan:?}");
        assert!(!debug.contains("Acme"));
    }

    #[test]
    fn version_changes_with_terms() {
        let a = Policy::from_text("one");
        let b = Policy::from_text("two");
        assert_ne!(a.version(), b.version());
    }
}
