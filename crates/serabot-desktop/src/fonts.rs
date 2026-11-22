//! The handwriting font for `:::hand` notes (0.74.3): Caveat, SIL Open Font
//! License 1.1 (see `assets/fonts/OFL.txt`).
//!
//! The CSS is inlined with `include_str!`, so a relative `url(fonts/…)` can
//! never resolve at runtime. The font therefore ships as base64 text — the
//! same `include_bytes!` + base64 pattern `branding.rs` uses for marks —
//! and is embedded as a `data:` URI in one extra `<style>` block (see
//! `ui.rs`). Only the Latin subset is bundled (hand notes are Latin-script
//! only by the model rule), variable weight 400–700, 74,932 bytes as woff2.
//!
//! Source: Google Fonts, family Caveat, latin subset
//! (`https://fonts.googleapis.com/css2?family=Caveat:wght@400..700`),
//! fetched 2026-11-21. If the bundled file is ever removed, the
//! `--font-hand` stack falls back to a system Caveat, Segoe Script, then
//! generic cursive, so notes still read as handwriting.

/// The Latin-subset Caveat variable font (woff2) as base64 text. Stored as a
/// text file rather than a binary asset because the desktop CSS is inlined.
const CAVEAT_WOFF2_B64: &str = include_str!("../assets/fonts/caveat-latin.woff2.b64");

/// The `@font-face` rule that registers the bundled font as "SeraBot Hand".
/// Injected as its own `<style>` beside the app CSS in `ui.rs`.
pub fn font_face_css() -> String {
    format!(
        "@font-face {{ font-family: 'SeraBot Hand'; font-style: normal; \
         font-weight: 400 700; font-display: swap; \
         src: url(data:font/woff2;base64,{CAVEAT_WOFF2_B64}) format('woff2'); }}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    #[test]
    fn bundled_caveat_is_a_woff2_and_registers_serabot_hand() {
        let css = font_face_css();
        assert!(css.contains("@font-face"));
        assert!(css.contains("font-family: 'SeraBot Hand'"));
        assert!(css.contains("data:font/woff2;base64,"));
        assert!(css.contains("font-weight: 400 700"));
        let payload = CAVEAT_WOFF2_B64.trim();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .expect("bundled font is valid base64");
        assert_eq!(&bytes[..4], b"wOF2", "bundled font is a woff2");
        assert!(
            !payload.contains('\n'),
            "one line: no whitespace in the data URI"
        );
    }
}
