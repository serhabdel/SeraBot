//! Cookie and consent banners.
//!
//! The backend collects candidate buttons inside known consent containers
//! ([`COLLECT_JS`]); [`choose`] picks one in Rust, so the multilingual word
//! lists are plain unit-testable data. Preference: reject non-essential, then
//! accept only what is necessary, then a plain accept as a last resort (a
//! one-button notice). Anything that looks like a payment or a form submit is
//! never clicked.

use std::time::Duration;

use serde::Deserialize;

use super::manager::BrowserBackend;
use crate::error::Result;

/// Runs in the page. Returns a JSON string: a list of candidate buttons found
/// inside consent containers (known CMP ids and classes, or dialogs and fixed
/// boxes whose text talks about cookies). The elements are kept on
/// `document[Symbol.for('serabot.banner')]` so [`CLICK_JS`] can click one by index.
pub const COLLECT_JS: &str = r#"(() => {
  const KNOWN = '#onetrust-banner-sdk,#onetrust-consent-sdk,#CybotCookiebotDialog,#cookiebanner,#cookie-banner,#cookie-notice,#cookie-law-info-bar,#didomi-host,#didomi-popup,.didomi-popup-container,#truste-consent-track,#usercentrics-root,#cmpbox,.cmp-container,.qc-cmp2-container,#qc-cmp2-container,[id^="sp_message_container"],.fc-consent-root,#consent-banner,.cc-window,.cc-banner,#cc-main,.cc-nb-main,#tarteaucitronRoot,#axeptio_overlay,.osano-cm-window,#cookie-consent,#cookieConsent,.cookie-consent,.cookie-banner,.cookie-notice,.cookies-banner,.cookie-bar,#cookie-bar';
  const GENERIC = '[role=dialog],[role=alertdialog],dialog[open],[class*="cookie"],[id*="cookie"],[class*="consent"],[id*="consent"],[class*="gdpr"],[id*="gdpr"],[aria-label*="cookie" i],[aria-label*="consent" i]';
  const WORDS = /cookie|consent|gdpr|datenschutz|privacy|confidentialit|vie priv|galleta|privacidad|cookies|ملفات تعريف|الكوكيز|ملفات الارتباط|الخصوصية/i;
  const visible = (el) => { const r = el.getBoundingClientRect(); const s = getComputedStyle(el); return r.width > 0 && r.height > 0 && s.visibility !== 'hidden' && s.display !== 'none'; };
  const roots = [];
  const add = (el) => { if (el && !roots.includes(el) && visible(el)) roots.push(el); };
  document.querySelectorAll(KNOWN).forEach(add);
  document.querySelectorAll(GENERIC).forEach((el) => { if (WORDS.test((el.innerText || '').slice(0, 1500))) add(el); });
  document.querySelectorAll('body *').forEach((el) => {
    if (roots.length > 8) return;
    const s = getComputedStyle(el);
    if ((s.position === 'fixed' || s.position === 'sticky') && visible(el) && WORDS.test((el.innerText || '').slice(0, 800)) && (el.innerText || '').length < 2500) add(el);
  });
  const sels = 'button,a,[role=button],input[type=button],input[type=submit],[onclick]';
  const els = [];
  const out = [];
  const take = (scope, hint) => {
    scope.querySelectorAll(sels).forEach((el) => {
      if (els.length >= 40 || els.includes(el) || !visible(el)) return;
      const text = ((el.innerText || el.value || el.getAttribute('aria-label') || el.title || '') + '').trim().replace(/\s+/g, ' ').slice(0, 80);
      if (!text && !el.id) return;
      const form = el.closest('form');
      els.push(el);
      out.push({
        text,
        hint: ((hint || '') + ' ' + (el.id || '') + ' ' + (typeof el.className === 'string' ? el.className : '')).slice(0, 200),
        tag: el.tagName.toLowerCase(),
        submit: (el.type || '').toLowerCase() === 'submit',
        in_form: !!form && !roots.includes(form) && !roots.some((r) => r.contains(form)),
      });
    });
    scope.querySelectorAll('*').forEach((host) => { if (host.shadowRoot && els.length < 40) take(host.shadowRoot, hint); });
  };
  roots.forEach((root) => take(root, (root.id || '') + ' ' + (typeof root.className === 'string' ? root.className : '')));
  document[Symbol.for('serabot.banner')] = els;
  return JSON.stringify(out);
})()"#;

/// Clicks candidate number `%INDEX%` from the last collection.
pub fn click_js(index: usize) -> String {
    format!(
        "(() => {{ const els = document[Symbol.for('serabot.banner')]; const el = els && els[{index}]; if (!el || !el.isConnected) return false; el.click(); return true; }})()"
    )
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct BannerCandidate {
    #[serde(default)]
    pub text: String,
    /// Container and button ids and classes, space separated.
    #[serde(default)]
    pub hint: String,
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub submit: bool,
    /// Inside a form that is not the banner itself.
    #[serde(default)]
    pub in_form: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rank {
    /// Last resort: a plain "accept" / "OK".
    Accept = 1,
    /// "Only necessary cookies".
    Necessary = 2,
    /// "Reject all" and friends: the best choice.
    Reject = 3,
}

impl Rank {
    pub fn label(self) -> &'static str {
        match self {
            Rank::Reject => "rejected non-essential cookies",
            Rank::Necessary => "accepted only the necessary cookies",
            Rank::Accept => "accepted the cookie notice",
        }
    }
}

const PAYMENT: &[&str] = &[
    "pay",
    "payer",
    "paiement",
    "buy",
    "purchase",
    "order",
    "checkout",
    "subscribe",
    "submit",
    "add to cart",
    "add to basket",
    "donate",
    "acheter",
    "commander",
    "souscrire",
    "valider",
    "pagar",
    "comprar",
    "pedir",
    "suscribirse",
    "donar",
    "ادفع",
    "شراء",
    "اشتر",
    "اطلب",
    "تأكيد الطلب",
    "اشترك",
    "تبرع",
    "sign up",
    "register",
    "log in",
    "login",
    "sign in",
    "connexion",
];
const SETTINGS: &[&str] = &[
    "settings",
    "preferences",
    "manage",
    "customize",
    "customise",
    "options",
    "more",
    "learn",
    "personnaliser",
    "paramétrer",
    "parametrer",
    "gérer",
    "gerer",
    "préférences",
    "preferences",
    "configurar",
    "personalizar",
    "gestionar",
    "más",
    "إعدادات",
    "تخصيص",
    "إدارة",
    "المزيد",
    "privacy policy",
    "politique",
    "política",
    "سياسة",
    "vendors",
    "partners",
    "partenaires",
];
const NECESSARY: &[&str] = &[
    "necessary only",
    "only necessary",
    "essential only",
    "only essential",
    "accept necessary",
    "accept only necessary",
    "accept essential",
    "necessary cookies only",
    "essential cookies only",
    "use necessary cookies only",
    "allow necessary",
    "allow only necessary",
    "required only",
    "only required",
    "nécessaires uniquement",
    "uniquement nécessaires",
    "uniquement les cookies nécessaires",
    "cookies nécessaires uniquement",
    "cookies essentiels uniquement",
    "seulement les cookies nécessaires",
    "uniquement essentiels",
    "solo necesarias",
    "solo las necesarias",
    "solo esenciales",
    "sólo necesarias",
    "sólo esenciales",
    "solo cookies necesarias",
    "الضرورية فقط",
    "الضرورية",
    "ضروري فقط",
    "الأساسية فقط",
];
const REJECT: &[&str] = &[
    "reject all",
    "reject non-essential",
    "reject nonessential",
    "reject optional",
    "reject",
    "decline all",
    "decline",
    "deny all",
    "deny",
    "refuse all",
    "refuse",
    "disagree",
    "do not accept",
    "don't accept",
    "do not consent",
    "continue without accepting",
    "continue without agreeing",
    "no thanks",
    "tout refuser",
    "refuser tout",
    "refuser",
    "continuer sans accepter",
    "non merci",
    "rechazar todo",
    "rechazar todas",
    "rechazar",
    "denegar",
    "no aceptar",
    "continuar sin aceptar",
    "رفض الكل",
    "رفض جميع",
    "رفض",
    "ارفض",
    "لا أوافق",
    "لا اوافق",
    "المتابعة دون قبول",
];
const ACCEPT: &[&str] = &[
    "accept all",
    "accept all cookies",
    "accept cookies",
    "accept and close",
    "accept",
    "allow all",
    "allow all cookies",
    "allow cookies",
    "allow",
    "i agree",
    "agree",
    "got it",
    "okay",
    "ok",
    "tout accepter",
    "accepter tout",
    "accepter et fermer",
    "accepter",
    "j'accepte",
    "d'accord",
    "aceptar todo",
    "aceptar todas",
    "aceptar",
    "entendido",
    "vale",
    "de acuerdo",
    "موافق",
    "أوافق",
    "اوافق",
    "قبول الكل",
    "قبول الجميع",
    "قبول",
    "قبول جميع",
    "حسنا",
    "حسناً",
    "فهمت",
];

/// Lowercase, straighten apostrophes, keep letters (any script), digits and
/// apostrophes, collapse everything else into single spaces.
fn normalise(text: &str) -> String {
    let lowered = text.to_lowercase().replace(['’', '‘', '`'], "'");
    let mut out = String::with_capacity(lowered.len() + 2);
    let mut last_space = true;
    for ch in lowered.chars() {
        if ch.is_alphanumeric() || ch == '\'' || ch == '-' {
            out.push(ch);
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    out.trim().to_owned()
}

fn has_phrase(normalised: &str, phrases: &[&str]) -> bool {
    let padded = format!(" {normalised} ");
    phrases
        .iter()
        .any(|phrase| padded.contains(&format!(" {} ", normalise(phrase))))
}

/// True when the text looks like a purchase, sign-up or login control.
pub fn looks_like_payment(text: &str) -> bool {
    text.contains(['€', '$', '£']) || has_phrase(&normalise(text), PAYMENT)
}

/// How good a candidate is for dismissing a banner, or `None` when it must
/// not be clicked.
pub fn classify(candidate: &BannerCandidate) -> Option<Rank> {
    if candidate.submit && candidate.in_form {
        return None;
    }
    if candidate.in_form && candidate.tag != "button" {
        return None;
    }
    if looks_like_payment(&candidate.text) {
        return None;
    }
    let text = normalise(&candidate.text);
    let hint = candidate.hint.to_lowercase();
    if !text.is_empty() {
        if has_phrase(&text, SETTINGS) && !has_phrase(&text, NECESSARY) {
            return None;
        }
        if has_phrase(&text, NECESSARY) {
            return Some(Rank::Necessary);
        }
        if has_phrase(&text, REJECT) {
            return Some(Rank::Reject);
        }
        if text.chars().count() <= 40 && has_phrase(&text, ACCEPT) {
            return Some(Rank::Accept);
        }
        // Visible text that matches no known phrase: leave it alone.
        return None;
    }
    hint_rank(&hint)
}

/// Known CMP ids and classes, used only for buttons with no visible text.
fn hint_rank(hint: &str) -> Option<Rank> {
    let reject = [
        "reject-all",
        "reject_all",
        "rejectall",
        "disagree",
        "do-not-consent",
        "decline",
        "btn-reject",
        "optout",
    ];
    let necessary = ["necessary", "refuse-all", "deny"];
    let accept = [
        "accept-btn",
        "accept-all",
        "accept_all",
        "acceptall",
        "agree-button",
        "allowall",
        "btn-accept",
        "optin",
    ];
    if reject.iter().any(|token| hint.contains(token)) {
        return Some(Rank::Reject);
    }
    if necessary.iter().any(|token| hint.contains(token)) {
        return Some(Rank::Necessary);
    }
    if accept.iter().any(|token| hint.contains(token)) {
        return Some(Rank::Accept);
    }
    None
}

/// The best candidate: highest rank, then the first in page order.
pub fn choose(candidates: &[BannerCandidate]) -> Option<(usize, Rank)> {
    let mut best: Option<(usize, Rank)> = None;
    for (index, candidate) in candidates.iter().enumerate() {
        if let Some(rank) = classify(candidate)
            && best.is_none_or(|(_, current)| rank > current)
        {
            best = Some((index, rank));
        }
    }
    best
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BannerOutcome {
    pub dismissed: bool,
    /// What was done, in our own words (never page text).
    pub action: Option<&'static str>,
    pub candidates: usize,
}

impl BannerOutcome {
    pub fn none() -> Self {
        Self {
            dismissed: false,
            action: None,
            candidates: 0,
        }
    }
}

/// Try to dismiss a consent banner on the active page. With `patient`, waits
/// once for a late banner to appear. At most two clicks (a second layer).
pub async fn dismiss(backend: &dyn BrowserBackend, patient: bool) -> Result<BannerOutcome> {
    let mut outcome = BannerOutcome::none();
    for round in 0..2 {
        let mut candidates = backend.banner_candidates().await?;
        if candidates.is_empty() && patient && round == 0 {
            tokio::time::sleep(Duration::from_millis(700)).await;
            candidates = backend.banner_candidates().await?;
        }
        outcome.candidates = outcome.candidates.max(candidates.len());
        let Some((index, rank)) = choose(&candidates) else {
            break;
        };
        backend.click_banner(index).await?;
        outcome.dismissed = true;
        outcome.action = Some(match (outcome.action, rank) {
            (Some(first), _) => first,
            (None, rank) => rank.label(),
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn button(text: &str) -> BannerCandidate {
        BannerCandidate {
            text: text.into(),
            tag: "button".into(),
            ..Default::default()
        }
    }

    fn rank(text: &str) -> Option<Rank> {
        classify(&button(text))
    }

    #[test]
    fn reject_beats_necessary_beats_accept() {
        let candidates = vec![
            button("Accept all"),
            button("Only necessary"),
            button("Reject all"),
        ];
        assert_eq!(choose(&candidates), Some((2, Rank::Reject)));
        let candidates = vec![button("Accept all"), button("Only necessary cookies")];
        assert_eq!(choose(&candidates), Some((1, Rank::Necessary)));
        let candidates = vec![button("Manage preferences"), button("Accept")];
        assert_eq!(choose(&candidates), Some((1, Rank::Accept)));
        assert_eq!(choose(&[button("Manage preferences")]), None);
        assert_eq!(choose(&[]), None);
    }

    #[test]
    fn english_french_arabic_and_spanish_texts() {
        assert_eq!(rank("Reject All"), Some(Rank::Reject));
        assert_eq!(rank("Decline"), Some(Rank::Reject));
        assert_eq!(rank("Tout refuser"), Some(Rank::Reject));
        assert_eq!(rank("Continuer sans accepter"), Some(Rank::Reject));
        assert_eq!(rank("Rechazar todo"), Some(Rank::Reject));
        assert_eq!(rank("رفض الكل"), Some(Rank::Reject));
        assert_eq!(rank("Accepter"), Some(Rank::Accept));
        assert_eq!(rank("Tout accepter"), Some(Rank::Accept));
        assert_eq!(rank("J’accepte"), Some(Rank::Accept));
        assert_eq!(rank("Aceptar todas"), Some(Rank::Accept));
        assert_eq!(rank("موافق"), Some(Rank::Accept));
        assert_eq!(rank("قبول الكل"), Some(Rank::Accept));
        assert_eq!(rank("Got it!"), Some(Rank::Accept));
        assert_eq!(
            rank("Uniquement les cookies nécessaires"),
            Some(Rank::Necessary)
        );
        assert_eq!(rank("Solo las necesarias"), Some(Rank::Necessary));
        assert_eq!(rank("الضرورية فقط"), Some(Rank::Necessary));
        assert_eq!(rank("Accept only necessary"), Some(Rank::Necessary));
    }

    #[test]
    fn settings_and_unrelated_buttons_are_left_alone() {
        for text in [
            "Cookie settings",
            "Personnaliser",
            "Gérer les préférences",
            "Más opciones",
            "إعدادات",
            "Read more",
            "Home",
            "Book a room",
        ] {
            assert_eq!(rank(text), None, "{text}");
        }
    }

    #[test]
    fn nothing_that_looks_like_payment_or_submit_is_clicked() {
        for text in [
            "Pay now",
            "Pay $20 and accept",
            "Buy and accept",
            "Subscribe",
            "Accept and checkout",
            "Acheter",
            "Commander",
            "Pagar ahora",
            "ادفع الآن",
            "شراء",
            "Place order",
            "Sign up",
            "Log in",
            "Accept 12€",
        ] {
            assert_eq!(rank(text), None, "{text}");
        }
        let submit = BannerCandidate {
            text: "Accept".into(),
            tag: "input".into(),
            submit: true,
            in_form: true,
            ..Default::default()
        };
        assert_eq!(classify(&submit), None);
        // A link inside an unrelated form is not a consent control either.
        let link = BannerCandidate {
            text: "Accept".into(),
            tag: "a".into(),
            in_form: true,
            ..Default::default()
        };
        assert_eq!(classify(&link), None);
        // A submit button of the banner's own form is fine (not in an outside form).
        let own = BannerCandidate {
            text: "Accept".into(),
            tag: "input".into(),
            submit: true,
            in_form: false,
            ..Default::default()
        };
        assert_eq!(classify(&own), Some(Rank::Accept));
    }

    #[test]
    fn known_cmp_ids_rank_icon_only_buttons() {
        let onetrust = BannerCandidate {
            hint: "onetrust-banner-sdk onetrust-reject-all-handler".into(),
            tag: "button".into(),
            ..Default::default()
        };
        assert_eq!(classify(&onetrust), Some(Rank::Reject));
        let accept = BannerCandidate {
            hint: "x onetrust-accept-btn-handler".into(),
            tag: "button".into(),
            ..Default::default()
        };
        assert_eq!(classify(&accept), Some(Rank::Accept));
        let none = BannerCandidate {
            hint: "cookie-banner close".into(),
            tag: "button".into(),
            ..Default::default()
        };
        assert_eq!(classify(&none), None);
    }

    #[test]
    fn word_matching_respects_boundaries() {
        // "ok" must not match inside "book", "cookie" or "Facebook".
        assert_eq!(rank("Facebook"), None);
        assert_eq!(rank("Cookies"), None);
        assert_eq!(rank("OK"), Some(Rank::Accept));
        assert!(looks_like_payment("Pay"));
        assert!(!looks_like_payment("Paypal-free"));
    }
}
