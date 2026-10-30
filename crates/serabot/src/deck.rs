//! Bounded deck specification and validation.
//!
//! The model is deliberately text-only. Serde denies unknown fields, so a
//! larger JSON document cannot smuggle attachments, embedded files, links, or
//! script-like properties into a deck. Everything that reaches the renderer is
//! also checked for control characters, URLs, and characters the built-in
//! Helvetica faces cannot encode.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::domain::Language;
use crate::error::{Result, SeraError};

pub const MAX_DECK_TITLE_CHARS: usize = 140;
pub const MAX_SUBTITLE_CHARS: usize = 180;
pub const MAX_SECTION_TITLE_CHARS: usize = 140;
pub const MAX_PART_TITLE_CHARS: usize = 100;
pub const MAX_CARD_HEADING_CHARS: usize = 100;
pub const MAX_BODY_CHARS: usize = 1200;
pub const MAX_BULLET_CHARS: usize = 300;
pub const MAX_DATE_CHARS: usize = 40;
pub const MAX_AUTHOR_CHARS: usize = 80;
pub const MAX_FOOTER_CHARS: usize = 200;
pub const MAX_BRAND_NAME_CHARS: usize = 80;
pub const MAX_MONOGRAM_CHARS: usize = 4;
pub const MAX_SECTIONS: usize = 20;
pub const MAX_CARDS_PER_SECTION: usize = 8;
pub const MAX_BULLETS_PER_SECTION: usize = 12;

pub(crate) const CARDS_PER_PAGE: usize = 4;
pub(crate) const BULLETS_PER_PAGE: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeckSpec {
    pub title: String,
    pub subtitle: Option<String>,
    pub language: Language,
    pub date_label: Option<String>,
    pub author: Option<String>,
    pub sections: Vec<DeckSection>,
    pub footer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeckSection {
    pub part_title: Option<String>,
    pub title: String,
    pub cards: Vec<DeckCard>,
    pub bullets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeckCard {
    pub heading: String,
    pub body: String,
    pub bullets: Vec<String>,
    pub monogram: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Brand {
    pub name: String,
    pub palette: BrandPalette,
    pub monogram_text: String,
    pub footer: Option<String>,
    pub font_family: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrandPalette {
    pub background: String,
    pub foreground: String,
    pub accent: String,
    pub muted: String,
}

impl Brand {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)?;
        let brand: Self = toml::from_str(&raw)
            .map_err(|error| SeraError::Config(format!("{}: {error}", path.display())))?;
        brand.validate()?;
        Ok(brand)
    }

    pub fn validate(&self) -> Result<()> {
        check_text("brand.name", &self.name, MAX_BRAND_NAME_CHARS)?;
        check_text(
            "brand.monogram_text",
            &self.monogram_text,
            MAX_MONOGRAM_CHARS,
        )?;
        check_optional("brand.footer", self.footer.as_deref(), MAX_FOOTER_CHARS)?;

        for (label, color) in [
            ("brand.palette.background", &self.palette.background),
            ("brand.palette.foreground", &self.palette.foreground),
            ("brand.palette.accent", &self.palette.accent),
            ("brand.palette.muted", &self.palette.muted),
        ] {
            if parse_hex_color(color).is_none() {
                return Err(SeraError::Validation(format!(
                    "{label} must be a #RRGGBB colour"
                )));
            }
        }

        if let Some(font) = &self.font_family
            && !font.trim().eq_ignore_ascii_case("helvetica")
        {
            return Err(SeraError::Validation(
                "brand.font_family: only the built-in Helvetica family is supported".into(),
            ));
        }

        Ok(())
    }
}

/// Validate a deck before rendering.
///
/// The page budget is checked against the same pagination plan the renderer
/// uses, so a deck accepted here cannot silently expand past `max_pages`.
pub fn validate(spec: &DeckSpec, brand: &Brand, max_pages: usize) -> Result<()> {
    brand.validate()?;
    if max_pages == 0 {
        return Err(SeraError::Validation("max_pages must be at least 1".into()));
    }

    check_text("deck.title", &spec.title, MAX_DECK_TITLE_CHARS)?;
    check_optional(
        "deck.subtitle",
        spec.subtitle.as_deref(),
        MAX_SUBTITLE_CHARS,
    )?;
    check_optional(
        "deck.date_label",
        spec.date_label.as_deref(),
        MAX_DATE_CHARS,
    )?;
    check_optional("deck.author", spec.author.as_deref(), MAX_AUTHOR_CHARS)?;
    check_optional("deck.footer", spec.footer.as_deref(), MAX_FOOTER_CHARS)?;

    if spec.sections.is_empty() {
        return Err(SeraError::Validation(
            "deck must contain at least one section".into(),
        ));
    }
    if spec.sections.len() > MAX_SECTIONS {
        return Err(SeraError::Validation(format!(
            "deck.sections must contain at most {MAX_SECTIONS} sections"
        )));
    }

    // Language has exactly three variants, so serde has already rejected
    // anything else; spelling the match out keeps the supported set explicit.
    match spec.language {
        Language::It | Language::Fr | Language::En => {}
    }

    for (section_index, section) in spec.sections.iter().enumerate() {
        check_text(
            &format!("sections[{section_index}].title"),
            &section.title,
            MAX_SECTION_TITLE_CHARS,
        )?;
        check_optional(
            &format!("sections[{section_index}].part_title"),
            section.part_title.as_deref(),
            MAX_PART_TITLE_CHARS,
        )?;

        if section.cards.len() > MAX_CARDS_PER_SECTION {
            return Err(SeraError::Validation(format!(
                "sections[{section_index}].cards must contain at most {MAX_CARDS_PER_SECTION} cards"
            )));
        }
        if section.bullets.len() > MAX_BULLETS_PER_SECTION {
            return Err(SeraError::Validation(format!(
                "sections[{section_index}].bullets must contain at most {MAX_BULLETS_PER_SECTION} bullets"
            )));
        }
        if section.cards.is_empty() && section.bullets.is_empty() {
            return Err(SeraError::Validation(format!(
                "sections[{section_index}] must contain at least one card or bullet"
            )));
        }

        for (card_index, card) in section.cards.iter().enumerate() {
            check_text(
                &format!("sections[{section_index}].cards[{card_index}].heading"),
                &card.heading,
                MAX_CARD_HEADING_CHARS,
            )?;
            if !card.body.is_empty() {
                check_text(
                    &format!("sections[{section_index}].cards[{card_index}].body"),
                    &card.body,
                    MAX_BODY_CHARS,
                )?;
            }
            if card.bullets.len() > MAX_BULLETS_PER_SECTION {
                return Err(SeraError::Validation(format!(
                    "sections[{section_index}].cards[{card_index}].bullets must contain at most {MAX_BULLETS_PER_SECTION} bullets"
                )));
            }
            for (bullet_index, bullet) in card.bullets.iter().enumerate() {
                check_text(
                    &format!(
                        "sections[{section_index}].cards[{card_index}].bullets[{bullet_index}]"
                    ),
                    bullet,
                    MAX_BULLET_CHARS,
                )?;
            }
            if let Some(monogram) = card.monogram.as_deref() {
                check_text(
                    &format!("sections[{section_index}].cards[{card_index}].monogram"),
                    monogram,
                    MAX_MONOGRAM_CHARS,
                )?;
            }
        }

        for (bullet_index, bullet) in section.bullets.iter().enumerate() {
            check_text(
                &format!("sections[{section_index}].bullets[{bullet_index}]"),
                bullet,
                MAX_BULLET_CHARS,
            )?;
        }
    }

    let pages = paginate(spec).len();
    if pages > max_pages {
        return Err(SeraError::Validation(format!(
            "deck needs {pages} pages but max_pages is {max_pages}"
        )));
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum DeckPage<'a> {
    Cover,
    Part {
        title: &'a str,
    },
    Section {
        part: Option<&'a str>,
        title: &'a str,
    },
    Cards {
        part: Option<&'a str>,
        title: &'a str,
        cards: &'a [DeckCard],
    },
    Bullets {
        part: Option<&'a str>,
        title: &'a str,
        bullets: &'a [String],
    },
}

/// Deterministic page order shared by validation and rendering.
pub(crate) fn paginate(spec: &DeckSpec) -> Vec<DeckPage<'_>> {
    let mut pages = Vec::new();
    pages.push(DeckPage::Cover);

    let mut previous_part: Option<&str> = None;
    for section in &spec.sections {
        let part = section.part_title.as_deref();
        if let Some(part_title) = part {
            if previous_part != Some(part_title) {
                pages.push(DeckPage::Part { title: part_title });
            }
            previous_part = Some(part_title);
        } else {
            previous_part = None;
        }

        pages.push(DeckPage::Section {
            part,
            title: &section.title,
        });
        for cards in section.cards.chunks(CARDS_PER_PAGE) {
            pages.push(DeckPage::Cards {
                part,
                title: &section.title,
                cards,
            });
        }
        for bullets in section.bullets.chunks(BULLETS_PER_PAGE) {
            pages.push(DeckPage::Bullets {
                part,
                title: &section.title,
                bullets,
            });
        }
    }

    pages
}

pub(crate) fn parse_hex_color(value: &str) -> Option<(u8, u8, u8)> {
    let bytes = value.as_bytes();
    if bytes.len() != 7 || bytes[0] != b'#' {
        return None;
    }
    let r = (hex_nibble(bytes[1])? << 4) | hex_nibble(bytes[2])?;
    let g = (hex_nibble(bytes[3])? << 4) | hex_nibble(bytes[4])?;
    let b = (hex_nibble(bytes[5])? << 4) | hex_nibble(bytes[6])?;
    Some((r, g, b))
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn check_optional(label: &str, value: Option<&str>, max_chars: usize) -> Result<()> {
    match value {
        Some(value) => check_text(label, value, max_chars),
        None => Ok(()),
    }
}

fn check_text(label: &str, value: &str, max_chars: usize) -> Result<()> {
    if value.trim().is_empty() {
        return Err(SeraError::Validation(format!("{label} must not be empty")));
    }
    if value.chars().count() > max_chars {
        return Err(SeraError::Validation(format!(
            "{label} must be at most {max_chars} characters"
        )));
    }
    for character in value.chars() {
        if character.is_control() || is_bidi_or_zero_width(character) {
            return Err(SeraError::Validation(format!(
                "{label} contains control or invisible formatting characters"
            )));
        }
        if !is_builtin_encodable(character) {
            return Err(SeraError::Validation(format!(
                "{label} contains characters outside the built-in font encoding"
            )));
        }
    }
    if contains_reference(value) {
        return Err(SeraError::Validation(format!(
            "{label} contains a URL or script reference"
        )));
    }
    Ok(())
}

fn is_bidi_or_zero_width(character: char) -> bool {
    matches!(
        character,
        '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'
            | '\u{2066}'..='\u{2069}'
            | '\u{00ad}'
            | '\u{feff}'
    )
}

/// Windows-1252 is what printpdf uses for the built-in Type1 faces. Rejecting
/// anything outside it avoids the encoder silently dropping glyphs.
fn is_builtin_encodable(character: char) -> bool {
    let code = character as u32;
    matches!(code, 0x20..=0x7e | 0xa0..=0xff)
        || matches!(
            character,
            '\u{0152}'
                | '\u{0153}'
                | '\u{0160}'
                | '\u{0161}'
                | '\u{0178}'
                | '\u{017d}'
                | '\u{017e}'
                | '\u{0192}'
                | '\u{02c6}'
                | '\u{02dc}'
                | '\u{2013}'
                | '\u{2014}'
                | '\u{2018}'
                | '\u{2019}'
                | '\u{201a}'
                | '\u{201c}'
                | '\u{201d}'
                | '\u{201e}'
                | '\u{2020}'
                | '\u{2021}'
                | '\u{2022}'
                | '\u{2026}'
                | '\u{2030}'
                | '\u{2039}'
                | '\u{203a}'
                | '\u{20ac}'
                | '\u{2122}'
        )
}

fn contains_reference(value: &str) -> bool {
    let lowered = value.to_lowercase();
    if lowered.contains("://") || lowered.contains("www.") {
        return true;
    }
    [
        "http:",
        "https:",
        "ftp:",
        "mailto:",
        "javascript:",
        "vbscript:",
        "data:",
        "file:",
    ]
    .iter()
    .any(|scheme| has_scheme(&lowered, scheme))
}

fn has_scheme(lowered: &str, scheme: &str) -> bool {
    lowered.match_indices(scheme).any(|(index, _)| {
        index == 0
            || !lowered[..index]
                .chars()
                .next_back()
                .is_some_and(|character| character.is_ascii_alphanumeric())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brand() -> Brand {
        Brand {
            name: "Synthetic Example Brand".into(),
            palette: BrandPalette {
                background: "#F5F1E8".into(),
                foreground: "#1F2933".into(),
                accent: "#2F6F6A".into(),
                muted: "#6B7280".into(),
            },
            monogram_text: "SB".into(),
            footer: Some("Alpha placeholder branding".into()),
            font_family: Some("helvetica".into()),
        }
    }

    fn card() -> DeckCard {
        DeckCard {
            heading: "Offre".into(),
            body: "Café, œuvres et croissance à Paris.".into(),
            bullets: vec!["Réduction des coûts".into()],
            monogram: None,
        }
    }

    fn spec() -> DeckSpec {
        DeckSpec {
            title: "Étude de marché".into(),
            subtitle: Some("Synthèse bilingue".into()),
            language: Language::Fr,
            date_label: Some("Octobre 2026".into()),
            author: Some("SeraBot".into()),
            sections: vec![DeckSection {
                part_title: Some("Partenariats".into()),
                title: "Opportunités à l'étranger".into(),
                cards: vec![card()],
                bullets: vec!["Priorité: marchés francophones".into()],
            }],
            footer: None,
        }
    }

    #[test]
    fn accepts_a_bounded_french_deck() {
        assert!(validate(&spec(), &brand(), 30).is_ok());
    }

    #[test]
    fn rejects_empty_deck() {
        let mut deck = spec();
        deck.sections.clear();
        assert!(validate(&deck, &brand(), 30).is_err());
    }

    #[test]
    fn rejects_oversized_text() {
        let mut deck = spec();
        deck.title = "x".repeat(MAX_DECK_TITLE_CHARS + 1);
        assert!(validate(&deck, &brand(), 30).is_err());

        let mut deck = spec();
        deck.sections[0].cards[0].heading = "x".repeat(MAX_CARD_HEADING_CHARS + 1);
        assert!(validate(&deck, &brand(), 30).is_err());

        let mut deck = spec();
        deck.sections[0].cards[0].body = "x".repeat(MAX_BODY_CHARS + 1);
        assert!(validate(&deck, &brand(), 30).is_err());

        let mut deck = spec();
        deck.sections[0].bullets[0] = "x".repeat(MAX_BULLET_CHARS + 1);
        assert!(validate(&deck, &brand(), 30).is_err());
    }

    #[test]
    fn rejects_too_many_sections_cards_and_bullets() {
        let mut deck = spec();
        deck.sections = (0..MAX_SECTIONS + 1)
            .map(|index| DeckSection {
                part_title: None,
                title: format!("Section {index}"),
                cards: Vec::new(),
                bullets: vec!["Un point".into()],
            })
            .collect();
        assert!(validate(&deck, &brand(), 100).is_err());

        let mut deck = spec();
        deck.sections[0].cards = (0..MAX_CARDS_PER_SECTION + 1).map(|_| card()).collect();
        assert!(validate(&deck, &brand(), 100).is_err());

        let mut deck = spec();
        deck.sections[0].bullets = (0..MAX_BULLETS_PER_SECTION + 1)
            .map(|_| "Un point".to_string())
            .collect();
        assert!(validate(&deck, &brand(), 100).is_err());
    }

    #[test]
    fn rejects_too_many_pages() {
        let mut deck = spec();
        deck.sections = (0..6)
            .map(|index| DeckSection {
                part_title: None,
                title: format!("Section {index}"),
                cards: (0..MAX_CARDS_PER_SECTION).map(|_| card()).collect(),
                bullets: Vec::new(),
            })
            .collect();
        // Cover + six dividers + twelve card pages.
        assert_eq!(paginate(&deck).len(), 19);
        assert!(validate(&deck, &brand(), 18).is_err());
        assert!(validate(&deck, &brand(), 19).is_ok());
    }

    #[test]
    fn pagination_matches_the_page_budget() {
        let deck = spec();
        // Cover, part divider, section divider, one card page, one bullet page.
        assert_eq!(paginate(&deck).len(), 5);
        assert!(validate(&deck, &brand(), 5).is_ok());
        assert!(validate(&deck, &brand(), 4).is_err());
    }

    #[test]
    fn rejects_control_characters_and_urls() {
        let mut deck = spec();
        deck.title = "Titre\u{202e} inversé".into();
        assert!(validate(&deck, &brand(), 30).is_err());

        let mut deck = spec();
        deck.title = "Voir https://example.invalid".into();
        assert!(validate(&deck, &brand(), 30).is_err());

        let mut deck = spec();
        deck.sections[0].bullets[0] = "javascript:alert(1)".into();
        assert!(validate(&deck, &brand(), 30).is_err());
    }

    #[test]
    fn rejects_characters_the_builtin_font_cannot_encode() {
        let mut deck = spec();
        deck.title = "Résumé 漢字".into();
        assert!(validate(&deck, &brand(), 30).is_err());
    }

    #[test]
    fn rejects_unsupported_brand_layout() {
        let mut invalid_brand = brand();
        invalid_brand.font_family = Some("comic-sans".into());
        assert!(invalid_brand.validate().is_err());

        let mut invalid_brand = brand();
        invalid_brand.palette.accent = "teal".into();
        assert!(invalid_brand.validate().is_err());
    }

    #[test]
    fn serde_rejects_unknown_fields() {
        let raw = r##"{
            "title": "Deck",
            "subtitle": null,
            "language": "fr",
            "date_label": null,
            "author": null,
            "sections": [],
            "footer": null,
            "attachments": ["payload.bin"]
        }"##;
        assert!(serde_json::from_str::<DeckSpec>(raw).is_err());
    }

    #[test]
    fn loads_a_brand_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("brand.toml");
        std::fs::write(
            &path,
            r##"
name = "Synthetic Example Brand"
monogram_text = "SB"
footer = "Alpha placeholder"
font_family = "helvetica"

[palette]
background = "#F5F1E8"
foreground = "#1F2933"
accent = "#2F6F6A"
muted = "#6B7280"
"##,
        )
        .unwrap();
        let loaded = Brand::load(&path).unwrap();
        assert_eq!(loaded.monogram_text, "SB");
        assert_eq!(loaded.palette.accent, "#2F6F6A");
    }

    #[test]
    fn parses_strict_hex_colours() {
        assert_eq!(parse_hex_color("#2F6F6A"), Some((0x2f, 0x6f, 0x6a)));
        assert_eq!(parse_hex_color("2F6F6A"), None);
        assert_eq!(parse_hex_color("#GGGGGG"), None);
    }
}
