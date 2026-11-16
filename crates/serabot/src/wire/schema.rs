//! Tool-schema flattening for providers that reject full JSON Schema.
//!
//! Meta's Responses endpoint (and Anthropic's input schemas, in practice)
//! refuse recursive `$ref` graphs, objects with no declared shape and arrays
//! with no `items`. [`flatten`] resolves `$ref`, turns a cycle into a string,
//! gives every array an `items` and collapses open objects to a string.
//! [`flat_only`] goes further, for the retry after a "recursive JSON schema"
//! refusal: a flat map of scalar parameters and nothing else.
//!
//! The tool's name and description are never touched. The result is bounded in
//! depth, so a hostile schema cannot make this run away.

use serde_json::{Map, Value, json};

/// Deepest schema level kept; anything below it becomes a string.
const MAX_DEPTH: usize = 12;

/// Keys a flattened schema node may keep.
const KEPT: &[&str] = &[
    "type",
    "description",
    "enum",
    "const",
    "format",
    "nullable",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "minItems",
    "maxItems",
    "pattern",
    "default",
];

fn string_node(description: Option<&Value>) -> Value {
    let mut node = json!({"type": "string"});
    if let Some(description) = description.filter(|value| value.is_string()) {
        node["description"] = description.clone();
    }
    node
}

fn ref_name(reference: &str) -> Option<&str> {
    reference
        .strip_prefix("#/$defs/")
        .or_else(|| reference.strip_prefix("#/definitions/"))
        .filter(|name| !name.is_empty() && !name.contains('/'))
}

struct Walker<'a> {
    defs: &'a Map<String, Value>,
    stack: Vec<String>,
}

impl Walker<'_> {
    fn node(&mut self, node: &Value, depth: usize, root: bool) -> Value {
        let Some(map) = node.as_object() else {
            return string_node(None);
        };
        if depth > MAX_DEPTH {
            return string_node(map.get("description"));
        }
        if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
            let Some(name) = ref_name(reference) else {
                return string_node(map.get("description"));
            };
            if self.stack.iter().any(|open| open == name) {
                // A cycle cannot be flattened; the model gets a string.
                return string_node(map.get("description"));
            }
            let Some(target) = self.defs.get(name) else {
                return string_node(map.get("description"));
            };
            self.stack.push(name.to_owned());
            let mut resolved = self.node(target, depth + 1, root);
            self.stack.pop();
            if let (Some(out), Some(text)) = (resolved.as_object_mut(), map.get("description"))
                && text.is_string()
            {
                out.insert("description".into(), text.clone());
            }
            return resolved;
        }
        let mut out = Map::new();
        for key in KEPT {
            if let Some(value) = map.get(*key) {
                out.insert((*key).to_owned(), value.clone());
            }
        }
        for combinator in ["anyOf", "oneOf", "allOf"] {
            if let Some(options) = map.get(combinator).and_then(Value::as_array) {
                let options: Vec<Value> = options
                    .iter()
                    .map(|option| self.node(option, depth + 1, false))
                    .collect();
                if !options.is_empty() {
                    out.insert(combinator.to_owned(), Value::Array(options));
                }
            }
        }
        let declared = map.get("type").and_then(Value::as_str);
        // A union such as `["string", "null"]` is typed, just not by one word.
        let typed = map.contains_key("type");
        let has_props = map
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|props| !props.is_empty());
        if declared == Some("array") || (declared.is_none() && map.contains_key("items")) {
            out.insert("type".into(), json!("array"));
            let items = match map.get("items") {
                Some(items) => self.node(items, depth + 1, false),
                None => json!({"type": "string"}),
            };
            out.insert("items".into(), items);
            return Value::Object(out);
        }
        if has_props || (root && (declared == Some("object") || declared.is_none())) {
            out.insert("type".into(), json!("object"));
            let mut properties = Map::new();
            if let Some(props) = map.get("properties").and_then(Value::as_object) {
                for (name, property) in props {
                    properties.insert(name.clone(), self.node(property, depth + 1, false));
                }
            }
            let required: Vec<Value> = map
                .get("required")
                .and_then(Value::as_array)
                .map(|names| {
                    names
                        .iter()
                        .filter(|name| {
                            name.as_str()
                                .is_some_and(|name| properties.contains_key(name))
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            out.insert("properties".into(), Value::Object(properties));
            if !required.is_empty() {
                out.insert("required".into(), Value::Array(required));
            }
            return Value::Object(out);
        }
        if declared == Some("object") || (!typed && out.is_empty()) {
            // An open object, or a node that says nothing at all.
            return string_node(map.get("description"));
        }
        if !typed
            && ["anyOf", "oneOf", "allOf", "enum", "const"]
                .iter()
                .all(|key| !out.contains_key(*key))
        {
            return string_node(map.get("description"));
        }
        Value::Object(out)
    }
}

/// The tool parameter schema as a provider that dislikes full JSON Schema
/// accepts it. The root is always an object with `properties`.
pub fn flatten(schema: &Value) -> Value {
    let empty = Map::new();
    let defs = ["$defs", "definitions"]
        .iter()
        .find_map(|key| schema.get(*key).and_then(Value::as_object))
        .unwrap_or(&empty);
    let mut walker = Walker {
        defs,
        stack: Vec::new(),
    };
    let mut flat = walker.node(schema, 0, true);
    if !flat.is_object() || flat.get("properties").is_none() {
        flat = json!({"type": "object", "properties": {}});
    }
    flat
}

/// A flat map of scalar parameters. Anything that is not a string, number,
/// integer or boolean becomes a string the model fills with JSON text.
pub fn flat_only(schema: &Value) -> Value {
    let flat = flatten(schema);
    let mut properties = Map::new();
    if let Some(props) = flat.get("properties").and_then(Value::as_object) {
        for (name, property) in props {
            let scalar = property
                .get("type")
                .and_then(Value::as_str)
                .filter(|kind| matches!(*kind, "string" | "number" | "integer" | "boolean"));
            let mut node = Map::new();
            node.insert("type".into(), json!(scalar.unwrap_or("string")));
            if let Some(description) = property.get("description").filter(|d| d.is_string()) {
                node.insert("description".into(), description.clone());
            }
            if scalar.is_some()
                && let Some(options) = property.get("enum").filter(|value| value.is_array())
            {
                node.insert("enum".into(), options.clone());
            }
            properties.insert(name.clone(), Value::Object(node));
        }
    }
    let mut out = Map::new();
    out.insert("type".into(), json!("object"));
    let required = flat.get("required").cloned();
    out.insert("properties".into(), Value::Object(properties));
    if let Some(required) = required {
        out.insert("required".into(), required);
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_are_inlined_and_cycles_become_strings() {
        let schema = json!({
            "type": "object",
            "properties": {
                "node": {"$ref": "#/$defs/Node"},
                "tags": {"type": "array"}
            },
            "required": ["node", "missing"],
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "child": {"$ref": "#/$defs/Node", "description": "the next one"}
                    }
                }
            }
        });
        let flat = flatten(&schema);
        assert_eq!(flat["properties"]["node"]["type"], "object");
        assert_eq!(
            flat["properties"]["node"]["properties"]["name"]["type"],
            "string"
        );
        // The cycle is cut where it comes back to `Node`.
        assert_eq!(
            flat["properties"]["node"]["properties"]["child"],
            json!({"type": "string", "description": "the next one"})
        );
        assert_eq!(
            flat["properties"]["tags"]["items"],
            json!({"type": "string"})
        );
        assert_eq!(flat["required"], json!(["node"]));
        assert!(flat.get("$defs").is_none());
    }

    #[test]
    fn open_and_untyped_nodes_collapse_to_strings() {
        let schema = json!({
            "type": "object",
            "properties": {
                "blob": {"type": "object", "description": "anything"},
                "any": {},
                "pick": {"type": "string", "enum": ["a", "b"]}
            }
        });
        let flat = flatten(&schema);
        assert_eq!(
            flat["properties"]["blob"],
            json!({"type": "string", "description": "anything"})
        );
        assert_eq!(flat["properties"]["any"], json!({"type": "string"}));
        assert_eq!(flat["properties"]["pick"]["enum"], json!(["a", "b"]));
    }

    #[test]
    fn the_root_is_always_an_object_with_properties() {
        assert_eq!(
            flatten(&json!({})),
            json!({"type": "object", "properties": {}})
        );
        assert_eq!(
            flatten(&json!("nonsense")),
            json!({"type": "object", "properties": {}})
        );
        assert_eq!(flatten(&json!({"type": "object"}))["properties"], json!({}));
    }

    #[test]
    fn a_deeply_nested_schema_is_bounded() {
        let mut node = json!({"type": "string"});
        for _ in 0..60 {
            node = json!({"type": "object", "properties": {"n": node}});
        }
        let text = flatten(&node).to_string();
        assert!(text.len() < 4_000, "{}", text.len());
    }

    #[test]
    fn flat_only_keeps_only_scalars() {
        let schema = json!({
            "type": "object",
            "properties": {
                "n": {"type": "integer", "description": "count"},
                "list": {"type": "array", "items": {"type": "string"}},
                "obj": {"type": "object", "properties": {"a": {"type": "string"}}},
                "mode": {"type": "string", "enum": ["x", "y"]}
            },
            "required": ["n"]
        });
        let flat = flat_only(&schema);
        assert_eq!(
            flat["properties"]["n"],
            json!({"type": "integer", "description": "count"})
        );
        assert_eq!(flat["properties"]["list"], json!({"type": "string"}));
        assert_eq!(flat["properties"]["obj"], json!({"type": "string"}));
        assert_eq!(flat["properties"]["mode"]["enum"], json!(["x", "y"]));
        assert_eq!(flat["required"], json!(["n"]));
    }
}
