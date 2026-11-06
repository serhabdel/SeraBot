//! Accessibility-tree outline with stable element refs (`e1..eN`), and the
//! small key table for `browser_press`.
//!
//! Pure functions over CDP JSON so they are unit-testable without Chromium.

use std::collections::HashMap;

use serde_json::Value;

use crate::error::{Result, SeraError};
use crate::util::truncate_chars;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefTarget {
    pub backend_node_id: i64,
    pub role: String,
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct Outline {
    pub text: String,
    /// `refs[0]` is `e1`.
    pub refs: Vec<RefTarget>,
}

impl Outline {
    pub fn target(&self, reference: &str) -> Result<&RefTarget> {
        let index = parse_ref(reference)?;
        self.refs.get(index - 1).ok_or_else(|| {
            SeraError::Validation(format!(
                "unknown element ref '{reference}'; take a new browser_snapshot"
            ))
        })
    }
}

/// `"e12"` -> 12. Refs are 1-based; anything else is rejected.
pub fn parse_ref(reference: &str) -> Result<usize> {
    reference
        .strip_prefix('e')
        .and_then(|digits| {
            if digits.len() <= 5 && digits.bytes().all(|b| b.is_ascii_digit()) {
                digits.parse::<usize>().ok()
            } else {
                None
            }
        })
        .filter(|index| *index >= 1)
        .ok_or_else(|| SeraError::Validation("ref must look like e1, e2, ...".into()))
}

const INTERACTIVE: &[&str] = &[
    "link",
    "button",
    "textbox",
    "searchbox",
    "combobox",
    "checkbox",
    "radio",
    "switch",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "tab",
    "option",
    "slider",
    "spinbutton",
    "listbox",
    "treeitem",
];

const LANDMARKS: &[&str] = &[
    "main",
    "navigation",
    "banner",
    "contentinfo",
    "form",
    "search",
    "dialog",
    "alertdialog",
    "complementary",
    "region",
];

const MAX_DEPTH: usize = 60;
const MAX_NAME_CHARS: usize = 160;

fn prop_str(node: &Value, key: &str) -> String {
    node.get(key)
        .and_then(|v| v.get("value"))
        .map(|v| match v {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        })
        .unwrap_or_default()
}

fn state_flags(node: &Value) -> Vec<String> {
    let mut flags = Vec::new();
    if let Some(props) = node.get("properties").and_then(Value::as_array) {
        for prop in props {
            let name = prop.get("name").and_then(Value::as_str).unwrap_or("");
            let value = prop.get("value").and_then(|v| v.get("value"));
            match (name, value) {
                ("checked", Some(Value::String(s))) if s == "true" => flags.push("checked".into()),
                ("checked", Some(Value::Bool(true))) => flags.push("checked".into()),
                ("checked", Some(Value::String(s))) if s == "mixed" => flags.push("mixed".into()),
                ("disabled", Some(Value::Bool(true))) => flags.push("disabled".into()),
                ("expanded", Some(Value::Bool(true))) => flags.push("expanded".into()),
                ("required", Some(Value::Bool(true))) => flags.push("required".into()),
                ("selected", Some(Value::Bool(true))) => flags.push("selected".into()),
                ("level", Some(Value::Number(n))) => flags.push(format!("level {n}")),
                _ => {}
            }
        }
    }
    flags
}

/// Build the outline from `Accessibility.getFullAXTree` nodes.
pub fn build_outline(nodes: &[Value], max_chars: usize, max_refs: usize) -> Outline {
    let by_id: HashMap<&str, &Value> = nodes
        .iter()
        .filter_map(|node| {
            node.get("nodeId")
                .and_then(Value::as_str)
                .map(|id| (id, node))
        })
        .collect();
    let root = nodes
        .iter()
        .find(|node| node.get("parentId").is_none())
        .or_else(|| nodes.first());
    let mut out = Outline::default();
    let mut lines: Vec<String> = Vec::new();
    let mut used = 0usize;
    if let Some(root) = root {
        let mut stack: Vec<(&Value, usize)> = vec![(root, 0)];
        while let Some((node, depth)) = stack.pop() {
            if used >= max_chars {
                lines.push("... (outline truncated)".into());
                break;
            }
            let role = prop_str(node, "role");
            let ignored = node
                .get("ignored")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let name = truncate_chars(prop_str(node, "name").trim(), MAX_NAME_CHARS);
            let mut descend = depth < MAX_DEPTH;
            if !ignored {
                let is_interactive = INTERACTIVE.contains(&role.as_str());
                if is_interactive {
                    let backend = node.get("backendDOMNodeId").and_then(Value::as_i64);
                    let mut line = String::new();
                    if let Some(backend) = backend.filter(|_| out.refs.len() < max_refs) {
                        out.refs.push(RefTarget {
                            backend_node_id: backend,
                            role: role.clone(),
                            name: name.clone(),
                        });
                        line.push_str(&format!("[e{}] ", out.refs.len()));
                    }
                    line.push_str(&role);
                    if !name.is_empty() {
                        line.push_str(&format!(" \"{name}\""));
                    }
                    let value = truncate_chars(prop_str(node, "value").trim(), 80);
                    if !value.is_empty() {
                        line.push_str(&format!(" value=\"{value}\""));
                    }
                    let flags = state_flags(node);
                    if !flags.is_empty() {
                        line.push_str(&format!(" ({})", flags.join(", ")));
                    }
                    used += line.len();
                    lines.push(line);
                    // The text children of a control repeat its name.
                    descend = false;
                } else if role == "heading" {
                    if !name.is_empty() {
                        let line = format!("# {name}");
                        used += line.len();
                        lines.push(line);
                    }
                    descend = false;
                } else if role == "StaticText" || role == "text" {
                    if !name.is_empty() {
                        used += name.len();
                        lines.push(name);
                    }
                } else if LANDMARKS.contains(&role.as_str()) {
                    let line = if name.is_empty() {
                        format!("-- {role}")
                    } else {
                        format!("-- {role} \"{name}\"")
                    };
                    used += line.len();
                    lines.push(line);
                } else if (role == "image" || role == "img") && !name.is_empty() {
                    let line = format!("image \"{name}\"");
                    used += line.len();
                    lines.push(line);
                }
            }
            if descend && let Some(children) = node.get("childIds").and_then(Value::as_array) {
                for child in children.iter().rev() {
                    if let Some(child) = child.as_str().and_then(|id| by_id.get(id)) {
                        stack.push((child, depth + 1));
                    }
                }
            }
        }
    }
    if lines.is_empty() {
        lines.push("(empty page)".into());
    }
    out.text = lines.join("\n");
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyDef {
    pub key: String,
    pub code: String,
    pub windows_virtual_key_code: i64,
    pub text: Option<String>,
}

/// Keys the bot or the user may press. No modifier chords on purpose.
pub fn key_def(name: &str) -> Option<KeyDef> {
    let make = |key: &str, code: &str, vk: i64, text: Option<&str>| KeyDef {
        key: key.into(),
        code: code.into(),
        windows_virtual_key_code: vk,
        text: text.map(str::to_owned),
    };
    let lower = name.trim().to_ascii_lowercase();
    let def = match lower.as_str() {
        "enter" | "return" => make("Enter", "Enter", 13, Some("\r")),
        "tab" => make("Tab", "Tab", 9, None),
        "escape" | "esc" => make("Escape", "Escape", 27, None),
        "backspace" => make("Backspace", "Backspace", 8, None),
        "delete" => make("Delete", "Delete", 46, None),
        "space" | " " => make(" ", "Space", 32, Some(" ")),
        "arrowup" | "up" => make("ArrowUp", "ArrowUp", 38, None),
        "arrowdown" | "down" => make("ArrowDown", "ArrowDown", 40, None),
        "arrowleft" | "left" => make("ArrowLeft", "ArrowLeft", 37, None),
        "arrowright" | "right" => make("ArrowRight", "ArrowRight", 39, None),
        "home" => make("Home", "Home", 36, None),
        "end" => make("End", "End", 35, None),
        "pageup" => make("PageUp", "PageUp", 33, None),
        "pagedown" => make("PageDown", "PageDown", 34, None),
        _ => {
            let mut chars = lower.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphabetic() => {
                    let upper = c.to_ascii_uppercase();
                    make(
                        &c.to_string(),
                        &format!("Key{upper}"),
                        upper as i64,
                        Some(&c.to_string()),
                    )
                }
                (Some(c), None) if c.is_ascii_digit() => make(
                    &c.to_string(),
                    &format!("Digit{c}"),
                    c as i64,
                    Some(&c.to_string()),
                ),
                _ => return None,
            }
        }
    };
    Some(def)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(
        id: &str,
        parent: Option<&str>,
        role: &str,
        name: &str,
        kids: &[&str],
        backend: Option<i64>,
    ) -> Value {
        let mut n = json!({
            "nodeId": id,
            "ignored": false,
            "role": {"type": "role", "value": role},
            "name": {"type": "computedString", "value": name},
            "childIds": kids,
        });
        if let Some(p) = parent {
            n["parentId"] = json!(p);
        }
        if let Some(b) = backend {
            n["backendDOMNodeId"] = json!(b);
        }
        n
    }

    fn sample() -> Vec<Value> {
        let mut input = node("5", Some("2"), "textbox", "Email", &[], Some(105));
        input["value"] = json!({"type": "string", "value": "a@b.c"});
        input["properties"] =
            json!([{"name": "required", "value": {"type": "boolean", "value": true}}]);
        vec![
            node("1", None, "RootWebArea", "Login", &["2", "3", "9"], Some(1)),
            node("2", Some("1"), "form", "", &["5", "6", "7"], Some(102)),
            node("3", Some("1"), "heading", "Welcome back", &["4"], Some(103)),
            node("4", Some("3"), "StaticText", "Welcome back", &[], Some(104)),
            input,
            node("6", Some("2"), "button", "Sign in", &["8"], Some(106)),
            node("7", Some("2"), "link", "Forgot password?", &[], Some(107)),
            node("8", Some("6"), "StaticText", "Sign in", &[], Some(108)),
            node("9", Some("1"), "StaticText", "Plain text", &[], Some(109)),
        ]
    }

    #[test]
    fn builds_outline_with_refs_in_document_order() {
        let outline = build_outline(&sample(), 10_000, 100);
        assert_eq!(
            outline.text,
            "-- form\n[e1] textbox \"Email\" value=\"a@b.c\" (required)\n[e2] button \"Sign in\"\n[e3] link \"Forgot password?\"\n# Welcome back\nPlain text"
        );
        assert_eq!(outline.refs.len(), 3);
        assert_eq!(outline.target("e2").unwrap().backend_node_id, 106);
        assert_eq!(outline.target("e3").unwrap().role, "link");
    }

    #[test]
    fn ref_lookup_rejects_bad_and_unknown_refs() {
        let outline = build_outline(&sample(), 10_000, 100);
        for bad in ["", "e", "e0", "1", "E1", "e1x", "e-1", "e999999", "e1 "] {
            assert!(outline.target(bad).is_err(), "{bad}");
        }
        assert!(outline.target("e4").is_err());
    }

    #[test]
    fn ignored_nodes_are_skipped_but_children_visited() {
        let mut nodes = sample();
        nodes[1]["ignored"] = json!(true);
        let outline = build_outline(&nodes, 10_000, 100);
        assert!(!outline.text.contains("-- form"));
        assert!(outline.text.contains("[e1] textbox"));
    }

    #[test]
    fn caps_refs_and_characters() {
        let outline = build_outline(&sample(), 10_000, 1);
        assert_eq!(outline.refs.len(), 1);
        assert!(outline.text.contains("[e1] textbox"));
        assert!(outline.text.contains("button \"Sign in\""));
        assert!(!outline.text.contains("[e2]"));
        let tiny = build_outline(&sample(), 5, 100);
        assert!(tiny.text.contains("outline truncated"));
    }

    #[test]
    fn empty_tree_is_reported() {
        assert_eq!(build_outline(&[], 100, 10).text, "(empty page)");
    }

    #[test]
    fn key_table() {
        assert_eq!(key_def("Enter").unwrap().windows_virtual_key_code, 13);
        assert_eq!(key_def("ESC").unwrap().key, "Escape");
        assert_eq!(key_def("a").unwrap().code, "KeyA");
        assert_eq!(key_def("7").unwrap().code, "Digit7");
        assert!(key_def("Control+C").is_none());
        assert!(key_def("F13").is_none());
        assert!(key_def("").is_none());
    }
}
