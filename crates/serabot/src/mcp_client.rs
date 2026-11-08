//! A small, provider-neutral MCP "Streamable HTTP" client.
//!
//! The wire format is JSON-RPC 2.0 over HTTPS `POST`: `initialize`, the
//! `notifications/initialized` notification, then `tools/list` or
//! `tools/call`. A reply is either a plain JSON body or a `text/event-stream`
//! whose events carry the JSON-RPC message; both are read here. The
//! `Mcp-Session-Id` header from `initialize` is echoed on every later request.
//!
//! Every operation opens its own short session (and closes it best effort),
//! so nothing is shared between users or calls. The client only ever talks to
//! the configured URL. Bodies are read with a hard byte limit and every
//! request has a timeout. Everything the server returns is untrusted data.
//! Callers own error wording (and secret scrubbing): this module returns the
//! structured [`McpError`] and never formats a secret into it.

use std::time::Duration;

use serde_json::{Value, json};

/// The MCP protocol revision this client asks for. The server may answer with
/// another one; its choice is echoed back in `MCP-Protocol-Version`.
const PROTOCOL_VERSION: &str = "2025-03-26";
/// `tools/list` pages followed at most.
const MAX_PAGES: usize = 5;
/// Tools kept from one `tools/list`.
pub const MAX_TOOLS: usize = 200;

/// How the secret is presented to the MCP endpoint.
#[derive(Debug, Clone, Copy)]
pub enum McpAuth {
    /// The secret is the whole value of this header (an API key header).
    Header(&'static str),
    /// `Authorization: Bearer <secret>` (an OAuth access token).
    Bearer,
}

/// Why an MCP exchange failed. Carries no credential.
#[derive(Debug)]
pub enum McpError {
    /// The server answered with a non-success HTTP status.
    Http { status: u16, body: Value },
    /// The server answered with a JSON-RPC error.
    Rpc { code: i64, message: String },
    /// The request could not be sent or read.
    Transport(reqwest::Error),
    /// Anything else (size limit, no answer, bad client setup).
    Other(String),
}

impl McpError {
    /// True for an HTTP 401: the bearer token is missing, stale or revoked.
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, McpError::Http { status: 401, .. })
    }
}

/// One open MCP session: the server's session id (if it sent one) and the
/// protocol revision it agreed to.
struct Session {
    id: Option<String>,
    version: String,
}

pub struct McpClient {
    http: reqwest::Client,
    url: String,
    auth: McpAuth,
    label: &'static str,
    max_bytes: usize,
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

/// An `http(s)` URL without credentials or a fragment, trimmed; `None` when
/// it is anything else.
pub fn parse_url(url: &str) -> Option<String> {
    let url = url.trim();
    let parsed = reqwest::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }
    Some(url.to_owned())
}

impl McpClient {
    /// `label` names the service in size/answer errors (for example
    /// "Composio" or "ElevenLabs"). The caller validates `url` first.
    pub fn new(
        label: &'static str,
        url: String,
        auth: McpAuth,
        timeout: Duration,
        max_bytes: usize,
    ) -> Result<Self, McpError> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| McpError::Other(format!("{label} MCP client: {error}")))?;
        Ok(Self {
            http,
            url,
            auth,
            label,
            max_bytes,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    fn with_secret(
        &self,
        request: reqwest::RequestBuilder,
        secret: &str,
    ) -> reqwest::RequestBuilder {
        match self.auth {
            McpAuth::Header(name) => request.header(name, secret),
            McpAuth::Bearer => request.bearer_auth(secret),
        }
    }

    /// Every tool this credential offers (bounded), as the server described them.
    pub async fn list_tools(&self, secret: &str) -> Result<Vec<Value>, McpError> {
        let session = self.open(secret).await?;
        let outcome = self.list_in(secret, &session).await;
        self.close(secret, &session).await;
        outcome
    }

    async fn list_in(&self, secret: &str, session: &Session) -> Result<Vec<Value>, McpError> {
        let mut tools: Vec<Value> = Vec::new();
        let mut cursor: Option<String> = None;
        for page in 0..MAX_PAGES {
            let params = match &cursor {
                Some(cursor) => json!({"cursor": cursor}),
                None => json!({}),
            };
            let result = self
                .request(secret, session, 2 + page as u64, "tools/list", params)
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
    pub async fn call_tool(
        &self,
        secret: &str,
        name: &str,
        arguments: &Value,
    ) -> Result<Value, McpError> {
        let session = self.open(secret).await?;
        let outcome = self
            .request(
                secret,
                &session,
                2,
                "tools/call",
                json!({"name": name, "arguments": arguments}),
            )
            .await;
        self.close(secret, &session).await;
        outcome
    }

    /// `initialize`, then the `initialized` notification.
    async fn open(&self, secret: &str) -> Result<Session, McpError> {
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
        let (message, id) = self.post(secret, &session, &body, Some(1)).await?;
        let result = rpc_result(self.label, message)?;
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
        self.post(secret, &session, &note, None).await?;
        Ok(session)
    }

    async fn request(
        &self,
        secret: &str,
        session: &Session,
        id: u64,
        method: &str,
        params: Value,
    ) -> Result<Value, McpError> {
        let (message, _) = self
            .post(secret, session, &rpc_request(id, method, params), Some(id))
            .await?;
        rpc_result(self.label, message)
    }

    /// End the session. Best effort: a server that does not support it (405)
    /// or is gone changes nothing for the caller.
    async fn close(&self, secret: &str, session: &Session) {
        let Some(id) = &session.id else { return };
        let request = self
            .http
            .delete(&self.url)
            .header("mcp-session-id", id.as_str())
            .header("mcp-protocol-version", session.version.as_str());
        let _ = self.with_secret(request, secret).send().await;
    }

    /// One `POST`. With `want_id`, returns the JSON-RPC message that answers
    /// that id; without it (a notification), no message is expected. Also
    /// returns the `Mcp-Session-Id` response header, if any.
    async fn post(
        &self,
        secret: &str,
        session: &Session,
        body: &Value,
        want_id: Option<u64>,
    ) -> Result<(Option<Value>, Option<String>), McpError> {
        let mut request = self
            .with_secret(self.http.post(&self.url), secret)
            .header("accept", "application/json, text/event-stream")
            .json(body);
        if let Some(id) = &session.id {
            request = request.header("mcp-session-id", id.as_str());
        }
        if session.id.is_some() || want_id != Some(1) {
            request = request.header("mcp-protocol-version", session.version.as_str());
        }
        let mut response = request.send().await.map_err(McpError::Transport)?;
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
        while let Some(chunk) = response.chunk().await.map_err(McpError::Transport)? {
            if bytes.len() + chunk.len() > self.max_bytes {
                return Err(McpError::Other(format!(
                    "{} response exceeded the size limit",
                    self.label
                )));
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
            let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            return Err(McpError::Http {
                status: status.as_u16(),
                body,
            });
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
            None => Err(McpError::Other(format!(
                "{} returned no answer to the request",
                self.label
            ))),
        }
    }
}
fn rpc_request(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

/// The `result` of a JSON-RPC answer, or the server's error.
fn rpc_result(label: &str, message: Option<Value>) -> Result<Value, McpError> {
    let message = message.ok_or_else(|| McpError::Other(format!("{label} returned no answer")))?;
    if let Some(error) = message.get("error") {
        return Err(McpError::Rpc {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        });
    }
    message
        .get("result")
        .cloned()
        .ok_or_else(|| McpError::Other(format!("{label} returned an invalid answer")))
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
        assert!(parse_url("https://api.elevenlabs.io/v1/mcp").is_some());
        assert!(parse_url("http://127.0.0.1:1/mcp").is_some());
        for bad in [
            "",
            "ftp://x/mcp",
            "https://user:pw@x/mcp",
            "https://x/mcp#f",
            "not a url",
        ] {
            assert!(parse_url(bad).is_none(), "{bad}");
        }
    }
}
