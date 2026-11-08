//! A small MCP "Streamable HTTP" client for Composio Connect
//! (<https://connect.composio.dev/mcp>).
//!
//! Composio Connect consumer keys (`ck_`) are sent as `x-consumer-api-key`
//! to the MCP endpoint, not to the REST API. The protocol work lives in the
//! shared [`crate::mcp_client`]; this wrapper only adds Composio's header and
//! turns failures into key-free, Composio-worded errors.

use std::time::Duration;

use serde_json::Value;

use crate::composio::{clean, scrub_key, transport_failure, upstream_error};
use crate::error::{Result, SeraError};
use crate::mcp_client::{self, McpAuth, McpError};

pub use crate::mcp_client::MAX_TOOLS;

/// The header that carries a Connect consumer key.
const KEY_HEADER: &str = "x-consumer-api-key";

pub struct McpClient {
    inner: mcp_client::McpClient,
    timeout: Duration,
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient")
            .field("url", &self.inner.url())
            .finish_non_exhaustive()
    }
}

/// An `http(s)` URL without credentials or a fragment, or a config error.
pub fn validate_url(url: &str) -> Result<String> {
    mcp_client::parse_url(url).ok_or_else(|| {
        SeraError::Config("composio mcp_base must be an http(s) URL without credentials".into())
    })
}

impl McpClient {
    pub fn new(url: &str, timeout: Duration, max_bytes: usize) -> Result<Self> {
        let inner = mcp_client::McpClient::new(
            "Composio",
            validate_url(url)?,
            McpAuth::Header(KEY_HEADER),
            timeout,
            max_bytes,
        )
        .map_err(|error| SeraError::Internal(format!("Composio MCP client: {error:?}")))?;
        Ok(Self { inner, timeout })
    }

    pub fn url(&self) -> &str {
        self.inner.url()
    }

    /// Every tool this key offers (bounded), as the server described them.
    pub async fn list_tools(&self, key: &str) -> Result<Vec<Value>> {
        self.inner
            .list_tools(key, self.timeout)
            .await
            .map_err(|error| failure(error, key))
    }

    /// Run one tool. Returns the MCP `tools/call` result object
    /// (`content`, `structuredContent`, `isError`).
    pub async fn call_tool(&self, key: &str, name: &str, arguments: &Value) -> Result<Value> {
        self.inner
            .call_tool(key, name, arguments, self.timeout)
            .await
            .map_err(|error| failure(error, key))
    }
}

/// A key-free, Composio-worded error for an MCP failure.
fn failure(error: McpError, key: &str) -> SeraError {
    match error {
        McpError::Http { status, body } => upstream_error(status, &body, key),
        McpError::Transport(error) => transport_failure(error),
        McpError::Other(message) => SeraError::External(message),
        McpError::Rpc { code, message } => {
            let text = clean(&scrub_key(&message, key), 240);
            SeraError::Upstream {
                status: 400,
                code: "composio_bad_request",
                message: if text.is_empty() {
                    format!("Composio Connect refused the request (error {code}).")
                } else {
                    format!("Composio Connect refused the request (error {code}): {text}")
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_checked() {
        assert!(validate_url("https://connect.composio.dev/mcp").is_ok());
        assert!(validate_url("not a url").is_err());
    }

    #[test]
    fn rpc_errors_are_scrubbed() {
        let error = failure(
            McpError::Rpc {
                code: -32602,
                message: "bad ck_synthetic_key_0001 value".into(),
            },
            "ck_synthetic_key_0001",
        );
        let text = error.to_string();
        assert!(!text.contains("ck_synthetic_key_0001"), "{text}");
        assert!(
            text.contains("-32602") && text.contains("[redacted]"),
            "{text}"
        );
    }
}
