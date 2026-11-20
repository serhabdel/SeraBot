//! Actionability checks before a click, hover, type or select.
//!
//! The backend resolves the element with `DOM.resolveNode`, scrolls it into
//! view, reads its box with `DOM.getBoxModel` and runs [`PROBE_JS`] on it
//! through `Runtime.callFunctionOn`. The probe answers: is it still attached,
//! visible, enabled, editable, and does `elementFromPoint` at the click point
//! land on the element (or a descendant, or its own label)? Everything here is
//! pure (JSON in, message out) so it is tested without Chromium.

use serde::Deserialize;
use serde_json::Value;

use crate::error::{Result, SeraError};
use crate::util::truncate_chars;

/// What the caller is about to do; decides which checks apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Needs visible, enabled and not covered.
    Click,
    /// Pointer move: visible and not covered, but disabled elements may be hovered.
    Hover,
    /// Typing or selecting: visible, enabled and editable. Covering does not matter
    /// because focus is set directly.
    Edit,
    /// Set a file input: only needs to be attached and enabled (file inputs are
    /// usually hidden behind a styled button).
    Upload,
}

/// The function run on the element. Arguments: `x`, `y` (or `null` to skip
/// the point test). Returns a JSON string so `returnByValue` is trivial.
pub const PROBE_JS: &str = r#"function(x, y) {
  const out = {attached: false, display_none: false, hidden: false, disabled: false,
    aria_disabled: false, readonly: false, tag: '', input_type: '', cover: null, covered: false};
  try {
    out.attached = !!this.isConnected;
    if (!out.attached) return JSON.stringify(out);
    out.tag = (this.tagName || '').toLowerCase();
    out.input_type = (this.type || '').toString().toLowerCase();
    const style = getComputedStyle(this);
    out.hidden = style.visibility === 'hidden' || style.visibility === 'collapse';
    out.display_none = style.display === 'none' || this.getClientRects().length === 0;
    out.disabled = !!(this.matches && this.matches(':disabled'));
    let n = this;
    while (n && n.getAttribute) {
      if (n.getAttribute('aria-disabled') === 'true') { out.aria_disabled = true; break; }
      n = n.parentElement;
    }
    out.readonly = !!this.readOnly || this.getAttribute('aria-readonly') === 'true';
    if (typeof x === 'number' && typeof y === 'number' && window.self === window.top) {
      const inside = (a, b) => { for (let k = b; k; k = k.parentNode || k.host) { if (k === a) return true; } return false; };
      let top = document.elementFromPoint(x, y);
      while (top && top.shadowRoot) {
        const inner = top.shadowRoot.elementFromPoint(x, y);
        if (!inner || inner === top) break;
        top = inner;
      }
      if (!top) { out.covered = true; out.cover = {tag: 'nothing', text: ''}; }
      else if (!inside(this, top)) {
        const labels = this.labels ? Array.from(this.labels) : [];
        const viaLabel = labels.some((l) => inside(l, top)) || (this.closest && inside(this.closest('label'), top));
        if (!viaLabel) {
          out.covered = true;
          out.cover = {
            tag: (top.tagName || '').toLowerCase(),
            role: top.getAttribute ? (top.getAttribute('role') || '') : '',
            id: (top.id || '').toString().slice(0, 80),
            cls: (typeof top.className === 'string' ? top.className : '').slice(0, 120),
            text: (top.innerText || top.getAttribute('aria-label') || '').toString().trim().slice(0, 60),
            ancestors: (() => { const ids = []; for (let k = top.parentElement, i = 0; k && i < 6; k = k.parentElement, i++) { ids.push(((k.id || '') + ' ' + (typeof k.className === 'string' ? k.className : '')).slice(0, 120)); } return ids.join(' | '); })(),
          };
        }
      }
    }
  } catch (e) { out.error = String(e); }
  return JSON.stringify(out);
}"#;

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct Cover {
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub cls: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub ancestors: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct Probe {
    #[serde(default)]
    pub attached: bool,
    #[serde(default)]
    pub display_none: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub aria_disabled: bool,
    #[serde(default)]
    pub readonly: bool,
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub input_type: String,
    #[serde(default)]
    pub covered: bool,
    #[serde(default)]
    pub cover: Option<Cover>,
}

impl Probe {
    /// Parse the value `Runtime.callFunctionOn` returned (a JSON string).
    pub fn from_call_result(result: &Value) -> Result<Self> {
        let raw = result["result"]["value"].as_str().ok_or_else(|| {
            SeraError::External("browser: the element could not be inspected".into())
        })?;
        serde_json::from_str(raw)
            .map_err(|_| SeraError::External("browser: unreadable element state".into()))
    }

    /// The first thing that stops `action`, as a sentence for the model, or
    /// `None` when the element is ready. `phase_static` is true for the check
    /// before scrolling (the point test has not run yet).
    pub fn blocker(&self, action: Action, role: &str) -> Option<String> {
        let what = describe_role(role);
        if !self.attached {
            return Some(format!(
                "{what} is no longer on the page; take a new browser_snapshot"
            ));
        }
        let file_input = self.tag == "input" && self.input_type == "file";
        if action != Action::Upload && (self.display_none || self.hidden) {
            // A visually hidden checkbox or radio is clicked through its label.
            let toggle =
                self.tag == "input" && matches!(self.input_type.as_str(), "checkbox" | "radio");
            if !(toggle && action == Action::Click && self.hidden) {
                return Some(format!(
                    "{what} is not visible (hidden on the page); scroll, open its menu, or take a new browser_snapshot"
                ));
            }
        }
        if action != Action::Hover && (self.disabled || self.aria_disabled) {
            return Some(format!("{what} is disabled"));
        }
        if action == Action::Edit && self.readonly && !file_input {
            return Some(format!("{what} is read-only"));
        }
        if matches!(action, Action::Click | Action::Hover)
            && self.covered
            && let Some(cover) = &self.cover
        {
            return Some(format!(
                "{what} is covered by {}; close it first (browser_dismiss_banners for cookie banners) or scroll, then retry",
                describe_cover(cover)
            ));
        }
        None
    }
}

fn describe_role(role: &str) -> String {
    match role {
        "button" => "the button".into(),
        "link" => "the link".into(),
        "textbox" | "searchbox" | "spinbutton" => "the field".into(),
        "combobox" | "listbox" => "the dropdown".into(),
        "checkbox" | "switch" => "the checkbox".into(),
        "radio" => "the radio button".into(),
        "tab" => "the tab".into(),
        "" => "the element".into(),
        other => format!("the {other}"),
    }
}

const BANNER_WORDS: &[&str] = &[
    "cookie",
    "consent",
    "gdpr",
    "onetrust",
    "cookiebot",
    "didomi",
    "truste",
    "usercentrics",
    "cmp",
    "privacy",
    "tarteaucitron",
    "axeptio",
    "osano",
    "termly",
];

/// A short phrase for whatever sits on top of the element.
pub fn describe_cover(cover: &Cover) -> String {
    let hints = format!(
        "{} {} {} {}",
        cover.id, cover.cls, cover.ancestors, cover.text
    )
    .to_lowercase();
    if BANNER_WORDS.iter().any(|word| hints.contains(word)) {
        return "a cookie banner".into();
    }
    if cover.role == "dialog"
        || cover.role == "alertdialog"
        || hints.contains("modal")
        || hints.contains("popup")
        || hints.contains("overlay")
        || hints.contains("dialog")
    {
        return "a popup or dialog".into();
    }
    if cover.tag == "nothing" {
        return "nothing clickable (the point is outside the page)".into();
    }
    let tag = if cover.tag.is_empty() {
        "another element"
    } else {
        cover.tag.as_str()
    };
    if cover.text.is_empty() {
        format!("another element ({tag})")
    } else {
        format!(
            "another element ({tag}: \"{}\")",
            truncate_chars(&cover.text, 40)
        )
    }
}

/// Centre and size of a box-model quad (8 numbers). `None` for a malformed or
/// zero-area quad (display:none, collapsed, detached).
pub fn quad_centre(quad: &[Value]) -> Option<(f64, f64)> {
    if quad.len() != 8 {
        return None;
    }
    let coords: Vec<f64> = quad.iter().filter_map(Value::as_f64).collect();
    if coords.len() != 8 {
        return None;
    }
    let xs = [coords[0], coords[2], coords[4], coords[6]];
    let ys = [coords[1], coords[3], coords[5], coords[7]];
    let width =
        xs.iter().cloned().fold(f64::MIN, f64::max) - xs.iter().cloned().fold(f64::MAX, f64::min);
    let height =
        ys.iter().cloned().fold(f64::MIN, f64::max) - ys.iter().cloned().fold(f64::MAX, f64::min);
    if width < 1.0 || height < 1.0 {
        return None;
    }
    Some((xs.iter().sum::<f64>() / 4.0, ys.iter().sum::<f64>() / 4.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ready() -> Probe {
        Probe {
            attached: true,
            tag: "button".into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_ready_element_has_no_blocker() {
        for action in [Action::Click, Action::Hover, Action::Edit, Action::Upload] {
            assert_eq!(ready().blocker(action, "button"), None);
        }
    }

    #[test]
    fn detached_hidden_and_disabled_say_why() {
        let gone = Probe::default();
        assert!(
            gone.blocker(Action::Click, "button")
                .unwrap()
                .contains("no longer on the page")
        );
        let hidden = Probe {
            display_none: true,
            ..ready()
        };
        assert!(
            hidden
                .blocker(Action::Click, "link")
                .unwrap()
                .starts_with("the link is not visible")
        );
        let invisible = Probe {
            hidden: true,
            ..ready()
        };
        assert!(
            invisible
                .blocker(Action::Edit, "textbox")
                .unwrap()
                .contains("not visible")
        );
        let disabled = Probe {
            disabled: true,
            ..ready()
        };
        assert_eq!(
            disabled.blocker(Action::Click, "textbox").unwrap(),
            "the field is disabled"
        );
        let aria = Probe {
            aria_disabled: true,
            ..ready()
        };
        assert_eq!(
            aria.blocker(Action::Click, "button").unwrap(),
            "the button is disabled"
        );
        // Hovering a disabled control is fine.
        assert_eq!(disabled.blocker(Action::Hover, "button"), None);
    }

    #[test]
    fn readonly_blocks_typing_only() {
        let readonly = Probe {
            readonly: true,
            tag: "input".into(),
            ..ready()
        };
        assert!(
            readonly
                .blocker(Action::Edit, "textbox")
                .unwrap()
                .contains("read-only")
        );
        assert_eq!(readonly.blocker(Action::Click, "textbox"), None);
    }

    #[test]
    fn a_covering_cookie_banner_is_named() {
        let probe = Probe {
            covered: true,
            cover: Some(Cover {
                tag: "div".into(),
                id: "onetrust-banner-sdk".into(),
                text: "We use cookies".into(),
                ..Default::default()
            }),
            ..ready()
        };
        let message = probe.blocker(Action::Click, "button").unwrap();
        assert!(
            message.starts_with("the button is covered by a cookie banner"),
            "{message}"
        );
        assert!(message.contains("browser_dismiss_banners"));
        // The ancestors count too: a button inside a consent container.
        let nested = Probe {
            covered: true,
            cover: Some(Cover {
                tag: "button".into(),
                ancestors: "cookie-wall fixed".into(),
                text: "Accept".into(),
                ..Default::default()
            }),
            ..ready()
        };
        assert!(
            nested
                .blocker(Action::Hover, "link")
                .unwrap()
                .contains("cookie banner")
        );
        // Typing is not affected by covering.
        assert_eq!(nested.blocker(Action::Edit, "textbox"), None);
    }

    #[test]
    fn other_covers_are_described_without_trusting_their_text() {
        let modal = Cover {
            tag: "div".into(),
            role: "dialog".into(),
            ..Default::default()
        };
        assert_eq!(describe_cover(&modal), "a popup or dialog");
        let plain = Cover {
            tag: "span".into(),
            text: "Sale ends soon".into(),
            ..Default::default()
        };
        assert_eq!(
            describe_cover(&plain),
            "another element (span: \"Sale ends soon\")"
        );
        let void = Cover {
            tag: "nothing".into(),
            ..Default::default()
        };
        assert!(describe_cover(&void).contains("outside the page"));
    }

    #[test]
    fn a_hidden_checkbox_may_be_clicked_through_its_label() {
        let checkbox = Probe {
            hidden: true,
            tag: "input".into(),
            input_type: "checkbox".into(),
            ..ready()
        };
        assert_eq!(checkbox.blocker(Action::Click, "checkbox"), None);
        let text = Probe {
            hidden: true,
            tag: "input".into(),
            input_type: "text".into(),
            ..ready()
        };
        assert!(text.blocker(Action::Click, "textbox").is_some());
    }

    #[test]
    fn file_inputs_may_be_hidden_for_upload() {
        let file = Probe {
            display_none: true,
            tag: "input".into(),
            input_type: "file".into(),
            ..ready()
        };
        assert_eq!(file.blocker(Action::Upload, "button"), None);
        let disabled = Probe {
            disabled: true,
            ..file
        };
        assert!(
            disabled
                .blocker(Action::Upload, "button")
                .unwrap()
                .contains("disabled")
        );
    }

    #[test]
    fn probe_parses_the_call_result() {
        let value = json!({"result": {"type": "string", "value": "{\"attached\":true,\"disabled\":true,\"tag\":\"button\"}"}});
        let probe = Probe::from_call_result(&value).unwrap();
        assert!(probe.attached && probe.disabled);
        assert!(Probe::from_call_result(&json!({"result": {}})).is_err());
        assert!(Probe::from_call_result(&json!({"result": {"value": "nope"}})).is_err());
    }

    #[test]
    fn quads_need_area() {
        let quad = |v: [f64; 8]| v.iter().map(|n| json!(n)).collect::<Vec<_>>();
        assert_eq!(
            quad_centre(&quad([10.0, 10.0, 30.0, 10.0, 30.0, 20.0, 10.0, 20.0])),
            Some((20.0, 15.0))
        );
        assert_eq!(
            quad_centre(&quad([5.0, 5.0, 5.0, 5.0, 5.0, 5.0, 5.0, 5.0])),
            None
        );
        assert_eq!(quad_centre(&[json!(1)]), None);
    }
}
