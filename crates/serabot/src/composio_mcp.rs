//! A small MCP "Streamable HTTP" client for Composio Connect
//! (<https://connect.composio.dev/mcp>).
//!
//! Composio Connect consumer keys (`ck_`) are sent as `x-consumer-api-key`
//! to the MCP endpoint, not to the REST API. The wire format is JSON-RPC 2.0
//! over HTTPS `POST`: `initialize`, the `notifications/initialized`
//! notification, then `tools/list` or `tools/call`. A reply is either a
//! plain JSON body or a `text/event-stream` whose events carry the JSON-RPC
//! message; both are read here. The `Mcp-Session-Id` header from `initialize`
//! is echoed on every later request.
//!
//! Every operation opens its own short session (and closes it best effort),
//! so nothing is shared between users or calls. The client only ever talks to
//! the configured URL. Bodies are read with a hard byte limit and every
//! request has a timeout. Everything the server returns is untrusted data,
//! and the key is removed from all error text.

use std::time::Duration;

use serde_json::{Value, json};

use crate::composio::{clean, scrub_key, transport_failure, upstream_error};
use crate::error::{Result, SeraError};

/// The MCP protocol revision this client asks for. The server may answer with
/// another one; its choice is echoed back in `MCP-Protocol-Version`.
const PROTOCOL_VERSION: &str = "2025-03-26";
/// The header that carries a Connect consumer key.
const KEY_HEADER: &str = "x-consumer-api-key";
/// `tools/list` pages followed at most.
const MAX_PAGES: usize = 5;
/// Tools kept from one `tools/list`.
pub const MAX_TOOLS: usize = 200;

/// One open MCP session: the server's session id (if it sent one) and the
/// protocol revision it agreed to.
struct Session {
    id: Option<String>,
    version: String,
}

pub struct McpClient {
    http: reqwest::Client,
    url: String,
    max_bytes: usize,
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

/// An `http(s)` URL without credentials or a fragment, or a config error.
pub fn validate_url(url: &str) -> Result<String> {
    let url = url.trim();
    let parsed = reqwest::Url::parse(url)
        .map_err(|_| SeraError::Config("composio mcp_base must be a valid URL".into()))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return Err(SeraError::Config(
            "composio mcp_base must be an http(s) URL without credentials".into(),
        ));
    }
    Ok(url.to_owned())
}

impl McpClient {
    pub fn new(url: &str, timeout: Duration, max_bytes: usize) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| SeraError::Internal(format!("Composio MCP client: {error}")))?;
        Ok(Self {
            http,
            url: validate_url(url)?,
            max_bytes,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Every tool this key offers (bounded), as the server described them.
    pub async fn list_tools(&self, key: &str) -> Result<Vec<Value>> {
        let session = self.open(key).await?;
        let outcome = self.list_in(key, &session).await;
        self.close(key, &session).await;
        outcome
    }

    async fn list_in(&self, key: &str, session: &Session) -> Result<Vec<Value>> {
        let mut tools: Vec<Value> = Vec::new();
        let mut cursor: Option<String> = None;
        for page in 0..MAX_PAGES {
            let params = match &cursor {
                Some(cursor) => json!({"cursor": cursor}),
                None => json!({}),
            };
            let result = self
                .request(key, session, 2 + page as u64, "tools/list", params)
                .await?;
            if let Some(items) = result.get("tools").and_then(Value::as_array) {
                tools.extend(items.iter().cloned());
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty() && c.len() <= 512)
                .map(str::to_owned);
            if cursor.is_none() || tools.len() >= MAX_TOOLS {
                break;
            }
        }
        tools.truncate(MAX_TOOLS);
        Ok(tools)
    }

    /// Run one tool. Returns the MCP `tools/call` result object
    /// (`content`, `structuredContent`, `isError`).
    pub async fn call_tool(&self, key: &str, name: &str, arguments: &Value) -> Result<Value> {
        let session = self.open(key).await?;
        let outcome = self
            .request(
                key,
                &session,
                2,
                "tools/call",
                json!({"name": name, "arguments": arguments}),
            )
            .await;
        self.close(key, &session).await;
        outcome
    }

    /// `initialize`, then the `initialized` notification.
    async fn open(&self, key: &str) -> Result<Session> {
        let mut session = Session {
            id: None,
            version: PROTOCOL_VERSION.to_owned(),
        };
        let body = rpc_request(
            1,
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "serabot", "version": env!("CARGO_PKG_VERSION")},
            }),
        );
        let (message, id) = self.post(key, &session, &body, Some(1)).await?;
        let result = rpc_result(message, key)?;
        if let Some(version) = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty() && v.len() <= 32 && v.bytes().all(|b| b.is_ascii_graphic()))
        {
            session.version = version.to_owned();
        }
        session.id = id.filter(|id| {
            !id.is_empty() && id.len() <= 256 && id.bytes().all(|b| b.is_ascii_graphic())
        });
        let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        self.post(key, &session, &note, None).await?;
        Ok(session)
    }

    async fn request(
        &self,
        key: &str,
        session: &Session,
        id: u64,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        let (message, _) = self
            .post(key, session, &rpc_request(id, method, params), Some(id))
            .await?;
        rpc_result(message, key)
    }

    /// End the session. Best effort: a server that does not support it (405)
    /// or is gone changes nothing for the caller.
    async fn close(&self, key: &str, session: &Session) {
        let Some(id) = &session.id else { return };
        let _ = self
            .http
            .delete(&self.url)
            .header(KEY_HEADER, key)
            .header("mcp-session-id", id.as_str())
            .header("mcp-protocol-version", session.version.as_str())
            .send()
            .await;
    }

    /// One `POST`. With `want_id`, returns the JSON-RPC message that answers
    /// that id; without it (a notification), no message is expected. Also
    /// returns the `Mcp-Session-Id` response header, if any.
    async fn post(
        &self,
        key: &str,
        session: &Session,
        body: &Value,
        want_id: Option<u64>,
    ) -> Result<(Option<Value>, Option<String>)> {
        let mut request = self
            .http
            .post(&self.url)
            .header(KEY_HEADER, key)
            .header("accept", "application/json, text/event-stream")
            .json(body);
        if let Some(id) = &session.id {
            request = request.header("mcp-session-id", id.as_str());
        }
        if session.id.is_some() || want_id != Some(1) {
            request = request.header("mcp-protocol-version", session.version.as_str());
        }
        let mut response = request.send().await.map_err(transport_failure)?;
        let status = response.status();
        let session_id = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let sse = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_ascii_lowercase().starts_with("text/event-stream"));
        let mut bytes: Vec<u8> = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport_failure)? {
            if bytes.len() + chunk.len() > self.max_bytes {
                return Err(SeraError::External(
                    "Composio response exceeded the size limit".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
            // An event stream may stay open after the answer: stop at it.
            if status.is_success()
                && sse
                && want_id.is_some_and(|id| find_sse_message(&bytes, id, false).is_some())
            {
                break;
            }
        }
        if !status.is_success() {
            let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            return Err(upstream_error(status.as_u16(), &value, key));
        }
        let Some(id) = want_id else {
            return Ok((None, session_id));
        };
        let message = if sse {
            find_sse_message(&bytes, id, true)
        } else {
            find_json_message(&bytes, id)
        };
        match message {
            Some(message) => Ok((Some(message), session_id)),
            None => Err(SeraError::External(
                "Composio Connect returned no answer to the request".into(),
            )),
        }
    }
}

fn rpc_request(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

/// The `result` of a JSON-RPC answer, or the server's error as a safe,
/// key-free message.
fn rpc_result(message: Option<Value>, key: &str) -> Result<Value> {
    let message =
        message.ok_or_else(|| SeraError::External("Composio Connect returned no answer".into()))?;
    if let Some(error) = message.get("error") {
        let text = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
        let text = clean(&scrub_key(text, key), 240);
        return Err(SeraError::Upstream {
            status: 400,
            code: "composio_bad_request",
            message: if text.is_empty() {
                format!("Composio Connect refused the request (error {code}).")
            } else {
                format!("Composio Connect refused the request (error {code}): {text}")
            },
        });
    }
    message
        .get("result")
        .cloned()
        .ok_or_else(|| SeraError::External("Composio Connect returned an invalid answer".into()))
}

fn is_answer_to(message: &Value, id: u64) -> bool {
    message.get("id").and_then(Value::as_u64) == Some(id)
        && (message.get("result").is_some() || message.get("error").is_some())
}

/// The answer to `id` in a plain JSON body (a single message or a batch).
fn find_json_message(bytes: &[u8], id: u64) -> Option<Value> {
    match serde_json::from_slice::<Value>(bytes).ok()? {
        Value::Array(list) => list.into_iter().find(|m| is_answer_to(m, id)),
        message if is_answer_to(&message, id) => Some(message),
        _ => None,
    }
}

/// The answer to `id` in a server-sent-events body. While the stream is still
/// open (`finished` false) only events ended by a blank line count; once it
/// has ended, the last partial event counts too.
fn find_sse_message(bytes: &[u8], id: u64, finished: bool) -> Option<Value> {
    let text = String::from_utf8_lossy(bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut events: Vec<&str> = text.split("\n\n").collect();
    if !finished {
        events.pop();
    }
    for event in events {
        let data: Vec<&str> = event
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(|rest| rest.strip_prefix(' ').unwrap_or(rest))
            .collect();
        if data.is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&data.join("\n")) else {
            continue;
        };
        match message {
            Value::Array(list) => {
                if let Some(found) = list.into_iter().find(|m| is_answer_to(m, id)) {
                    return Some(found);
                }
            }
            message if is_answer_to(&message, id) => return Some(message),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_events_are_read_by_id_and_partial_ones_wait() {
        let body = "event: message\r\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\r\n\r\nid: 7\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\ndata: \"result\":{\"ok\":true}}\n\n";
        let found = find_sse_message(body.as_bytes(), 2, false).unwrap();
        assert_eq!(found["result"]["ok"], true);
        assert!(find_sse_message(body.as_bytes(), 3, true).is_none());
        // The final event is not ended yet: only counts once the stream ends.
        let open = "data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}";
        assert!(find_sse_message(open.as_bytes(), 2, false).is_none());
        assert!(find_sse_message(open.as_bytes(), 2, true).is_some());
    }

    #[test]
    fn json_bodies_may_be_one_message_or_a_batch() {
        let one = br#"{"jsonrpc":"2.0","id":1,"result":{"a":1}}"#;
        assert!(find_json_message(one, 1).is_some());
        assert!(find_json_message(one, 2).is_none());
        let batch =
            br#"[{"jsonrpc":"2.0","id":9,"result":{}},{"jsonrpc":"2.0","id":2,"result":{"b":2}}]"#;
        assert_eq!(find_json_message(batch, 2).unwrap()["result"]["b"], 2);
        assert!(find_json_message(b"not json", 1).is_none());
    }

    #[test]
    fn urls_are_checked() {
        assert!(validate_url("https://connect.composio.dev/mcp").is_ok());
        assert!(validate_url("http://127.0.0.1:1/mcp").is_ok());
        for bad in [
            "",
            "ftp://x/mcp",
            "https://user:pw@x/mcp",
            "https://x/mcp#f",
            "not a url",
        ] {
            assert!(validate_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn rpc_errors_are_scrubbed() {
        let message = Some(json!({"jsonrpc": "2.0", "id": 2,
            "error": {"code": -32602, "message": "bad ck_synthetic_key_0001 value"}}));
        let error = rpc_result(message, "ck_synthetic_key_0001").unwrap_err();
        let text = error.to_string();
        assert!(!text.contains("ck_synthetic_key_0001"), "{text}");
        assert!(
            text.contains("-32602") && text.contains("[redacted]"),
            "{text}"
        );
    }
}
