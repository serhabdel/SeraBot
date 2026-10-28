use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Result, SeraError};

pub fn sha256_hex(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(bytes.as_ref()))
}

/// Produce a stable representation for hashing approval-bound arguments.
///
/// Object keys are sorted recursively. JSON numbers and strings retain their
/// exact serde representation, so callers must normalize semantic values
/// (absolute paths, timestamps) before binding them to an approval.
pub fn canonical_json(value: &Value) -> Result<String> {
    fn write(value: &Value, out: &mut String) -> Result<()> {
        match value {
            Value::Null => out.push_str("null"),
            Value::Bool(v) => out.push_str(if *v { "true" } else { "false" }),
            Value::Number(v) => out.push_str(&v.to_string()),
            Value::String(v) => {
                out.push_str(&serde_json::to_string(v)?);
            }
            Value::Array(values) => {
                out.push('[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write(value, out)?;
                }
                out.push(']');
            }
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (index, key) in keys.into_iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(key)?);
                    out.push(':');
                    write(&map[key], out)?;
                }
                out.push('}');
            }
        }
        Ok(())
    }

    let mut output = String::new();
    write(value, &mut output)?;
    Ok(output)
}

pub fn canonical_hash(value: &Value) -> Result<String> {
    Ok(sha256_hex(canonical_json(value)?))
}

pub fn constant_time_eq(left: &str, right: &str) -> bool {
    use subtle::ConstantTimeEq;
    left.as_bytes().ct_eq(right.as_bytes()).into()
}

pub fn validate_id(value: &str) -> Result<()> {
    if value.len() > 128
        || value.is_empty()
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
    {
        return Err(SeraError::Validation("invalid identifier".into()));
    }
    Ok(())
}

pub fn truncate_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}
