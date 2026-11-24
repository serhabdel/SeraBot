//! SMTP/IMAP mail connector (0.74.1; multi-account + untagged-reply fix in
//! 0.74.4): per-user mailbox credentials (Hostinger or any provider) live in
//! the per-user envelope-encrypted store under provider `mail` (the default
//! mailbox) or `mail.<label>` (a named extra account); gateway tools
//! `mail_send` (approval-gated), `mail_list` and `mail_read` take an optional
//! `account`; HTTP API `/v1/mail/*`.
//!
//! The wire clients are minimal hand-rolled SMTP/IMAP over `tokio-rustls`
//! (already in the dependency tree — no new crates beyond the two direct
//! edges in Cargo.toml). Plain (`none`) security is refused for non-loopback
//! hosts. Message and attachment sizes are capped; every command has a hard
//! I/O timeout. Tests use a fake in-process SMTP server and canned IMAP
//! fixtures; nothing here ran against a real mailbox.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::error::{Result, SeraError};
use crate::gateway::{Tool, ToolContext, ToolOutput, ToolRegistry, ToolSpec};
use crate::user_secrets::UserSecrets;

pub const MAIL_PROVIDER: &str = "mail";
/// Prefix of the secret-store provider ids for named mail accounts.
pub const MAIL_ACCOUNT_PREFIX: &str = "mail.";
const MAX_ACCOUNT_LABEL_CHARS: usize = 24;
pub const MAIL_SEND_TOOL: &str = "mail_send";
pub const MAIL_LIST_TOOL: &str = "mail_list";
pub const MAIL_READ_TOOL: &str = "mail_read";
pub const MAIL_FOLDERS_TOOL: &str = "mail_folders";
pub const MAIL_SEARCH_TOOL: &str = "mail_search";
pub const MAIL_FLAG_TOOL: &str = "mail_flag";
pub const MAIL_MOVE_TOOL: &str = "mail_move";
pub const MAIL_DELETE_TOOL: &str = "mail_delete";
pub const MAX_MESSAGE_BYTES: usize = 25 * 1024 * 1024;
pub const MAX_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;
pub const MAX_ATTACHMENTS: usize = 10;
pub const MAX_RECIPIENTS: usize = 20;
pub const MAX_LIST: u32 = 50;
pub const MAX_BODY_CHARS: usize = 256 * 1024;
pub const MAX_SUBJECT_CHARS: usize = 500;
pub const MAX_SIGNATURE_CHARS: usize = 4096;
const MAX_INLINE_ATTACHMENT_BYTES: usize = 512 * 1024;
const MAX_INLINE_ATTACHMENTS_TOTAL: usize = 2 * 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_LINE: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Security {
    #[default]
    StartTls,
    Ssl,
    None,
}

/// Per-user mailbox credentials, stored as JSON under provider `mail`.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MailCredentials {
    pub smtp_host: String,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    #[serde(default)]
    pub smtp_security: Security,
    pub username: String,
    pub password: String,
    pub imap_host: String,
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    #[serde(default = "default_imap_ssl")]
    pub imap_security: Security,
    /// From header; defaults to the username.
    #[serde(default)]
    pub from: Option<String>,
    /// Signature appended to every sent message (`\n\n-- \n<signature>`),
    /// stored per account (0.74.5).
    #[serde(default)]
    pub signature: Option<String>,
}

pub fn default_smtp_port() -> u16 {
    587
}
pub fn default_imap_port() -> u16 {
    993
}
pub fn default_imap_ssl() -> Security {
    Security::Ssl
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "::1" | "localhost")
}

/// Validate credentials. Plain-text security is only allowed on loopback.
pub fn validate_credentials(creds: &MailCredentials) -> Result<()> {
    for (label, host) in [("smtp", &creds.smtp_host), ("imap", &creds.imap_host)] {
        let host = host.trim();
        if host.is_empty()
            || host.len() > 253
            || host.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(SeraError::Validation(format!("invalid {label} host")));
        }
        if creds.smtp_port == 0 || creds.imap_port == 0 {
            return Err(SeraError::Validation("port must be 1-65535".into()));
        }
    }
    if creds.smtp_security == Security::None && !is_loopback(creds.smtp_host.trim()) {
        return Err(SeraError::Validation(
            "plain SMTP is only allowed for loopback hosts; use starttls or ssl".into(),
        ));
    }
    if creds.imap_security == Security::None && !is_loopback(creds.imap_host.trim()) {
        return Err(SeraError::Validation(
            "plain IMAP is only allowed for loopback hosts; use ssl".into(),
        ));
    }
    if creds.username.is_empty() || creds.username.len() > 256 {
        return Err(SeraError::Validation("invalid username".into()));
    }
    if creds.password.is_empty() || creds.password.len() > 1024 {
        return Err(SeraError::Validation(
            "password must be 1-1024 characters".into(),
        ));
    }
    if let Some(from) = &creds.from
        && (from.is_empty() || from.len() > 256 || from.chars().any(|c| c == '\r' || c == '\n'))
    {
        return Err(SeraError::Validation("invalid from address".into()));
    }
    if let Some(sig) = &creds.signature {
        let bad = sig.is_empty()
            || sig.len() > MAX_SIGNATURE_CHARS
            || sig
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t');
        if bad {
            return Err(SeraError::Validation(
                "signature must be 1-4096 chars without control characters".into(),
            ));
        }
    }
    Ok(())
}

/// Secret-store provider id for one mail account: `mail` for the default
/// mailbox, `mail.<label>` for a named extra account. The label is lowercase
/// letters, digits, `-` or `_` (at most 24 chars) so the provider id stays
/// inside the store's provider-id rules.
pub fn account_provider(account: Option<&str>) -> Result<String> {
    match account.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(MAIL_PROVIDER.to_owned()),
        Some(label) => {
            let valid = label.len() <= MAX_ACCOUNT_LABEL_CHARS
                && label.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_')
                });
            if !valid {
                return Err(SeraError::Validation(
                    "account name must be 1-24 chars: lowercase letters, digits, '-' or '_'".into(),
                ));
            }
            Ok(format!("{MAIL_PROVIDER}.{label}"))
        }
    }
}

pub fn store_credentials(
    secrets: &UserSecrets,
    user_id: &str,
    account: Option<&str>,
    creds: &MailCredentials,
) -> Result<()> {
    validate_credentials(creds)?;
    let json = serde_json::to_string(creds)
        .map_err(|e| SeraError::Internal(format!("mail creds serialise: {e}")))?;
    secrets.set(user_id, &account_provider(account)?, &json)
}

pub fn load_credentials(
    secrets: &UserSecrets,
    user_id: &str,
    account: Option<&str>,
) -> Result<Option<MailCredentials>> {
    let Some(raw) = secrets.get(user_id, &account_provider(account)?)? else {
        return Ok(None);
    };
    let creds: MailCredentials = serde_json::from_str(raw.expose_secret())
        .map_err(|e| SeraError::Internal(format!("stored mail credentials are invalid: {e}")))?;
    validate_credentials(&creds)?;
    Ok(Some(creds))
}

pub fn delete_credentials(
    secrets: &UserSecrets,
    user_id: &str,
    account: Option<&str>,
) -> Result<bool> {
    secrets.delete(user_id, &account_provider(account)?)
}

/// One connected mailbox for status listings; `account` is empty for the
/// default mailbox. Never carries the password.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct MailAccountInfo {
    pub account: String,
    pub username: String,
    pub smtp_host: String,
    pub imap_host: String,
}

/// All mail accounts the user has connected (`mail` plus every `mail.*`).
pub fn list_accounts(secrets: &UserSecrets, user_id: &str) -> Result<Vec<MailAccountInfo>> {
    let mut out = Vec::new();
    for provider in secrets.store().list_user_credential_providers(user_id)? {
        let account: Option<&str> = if provider == MAIL_PROVIDER {
            Some("")
        } else {
            provider.strip_prefix(MAIL_ACCOUNT_PREFIX)
        };
        let Some(account) = account else { continue };
        let lookup = if account.is_empty() {
            None
        } else {
            Some(account)
        };
        if let Some(creds) = load_credentials(secrets, user_id, lookup)? {
            out.push(MailAccountInfo {
                account: account.to_owned(),
                username: creds.username,
                smtp_host: creds.smtp_host,
                imap_host: creds.imap_host,
            });
        }
    }
    Ok(out)
}

pub fn from_address(creds: &MailCredentials) -> String {
    creds.from.clone().unwrap_or_else(|| creds.username.clone())
}

/// Append the account's configured signature to an outgoing body.
pub fn body_with_signature(creds: &MailCredentials, body: String) -> Result<String> {
    match creds.signature.as_deref().filter(|s| !s.trim().is_empty()) {
        None => Ok(body),
        Some(sig) => {
            let out = format!("{body}\n\n-- \n{sig}");
            if out.chars().count() > MAX_BODY_CHARS {
                return Err(SeraError::Validation(format!(
                    "body plus signature must stay under {MAX_BODY_CHARS} characters"
                )));
            }
            Ok(out)
        }
    }
}

// ---------------------------------------------------------------- MIME ----

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    pub name: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

/// Sanitize a header value: strip CR/LF and control chars.
fn clean_header(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control() || *c == '\t')
        .collect::<String>()
        .trim()
        .to_owned()
}

/// RFC 2047 encode a header value when it is not plain ASCII.
fn encode_header(value: &str) -> String {
    if value.is_ascii() {
        clean_header(value)
    } else {
        format!(
            "=?UTF-8?B?{}?=",
            base64::engine::general_purpose::STANDARD.encode(value.as_bytes())
        )
    }
}

/// Build a MIME message (multipart/mixed when there are attachments).
pub fn build_message(
    from: &str,
    to: &[String],
    subject: &str,
    body: &str,
    attachments: &[Attachment],
) -> Result<Vec<u8>> {
    if to.is_empty() || to.len() > MAX_RECIPIENTS {
        return Err(SeraError::Validation(format!(
            "1 to {MAX_RECIPIENTS} recipients are allowed"
        )));
    }
    for addr in to {
        if addr.is_empty()
            || addr.len() > 254
            || addr.chars().any(|c| c == '\r' || c == '\n' || c == ',')
        {
            return Err(SeraError::Validation(format!("invalid recipient {addr:?}")));
        }
    }
    if subject.chars().count() > MAX_SUBJECT_CHARS {
        return Err(SeraError::Validation("subject is too long".into()));
    }
    if body.chars().count() > MAX_BODY_CHARS {
        return Err(SeraError::Validation("body is too long".into()));
    }
    if attachments.len() > MAX_ATTACHMENTS {
        return Err(SeraError::Validation(format!(
            "at most {MAX_ATTACHMENTS} attachments"
        )));
    }
    for att in attachments {
        if att.name.is_empty()
            || att.name.len() > 200
            || att.name.chars().any(|c| c == '\r' || c == '\n' || c == '"')
        {
            return Err(SeraError::Validation("invalid attachment name".into()));
        }
        if att.content_type.is_empty()
            || att.content_type.len() > 100
            || att.content_type.chars().any(|c| c.is_control())
        {
            return Err(SeraError::Validation("invalid attachment type".into()));
        }
        if att.bytes.is_empty() || att.bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(SeraError::Validation(
                "each attachment must be 1 byte to 20 MiB".into(),
            ));
        }
    }
    let total: usize = body.len()
        + attachments
            .iter()
            .map(|a| a.bytes.len() * 4 / 3 + 200)
            .sum::<usize>();
    if total > MAX_MESSAGE_BYTES {
        return Err(SeraError::Validation(
            "the whole message must stay under 25 MiB".into(),
        ));
    }
    let boundary = format!("serabot-{}", uuid::Uuid::now_v7().simple());
    let mut out = String::new();
    out.push_str(&format!("From: {}\r\n", clean_header(from)));
    out.push_str(&format!(
        "To: {}\r\n",
        to.iter()
            .map(|a| clean_header(a))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    out.push_str(&format!("Subject: {}\r\n", encode_header(subject)));
    out.push_str(&format!("Date: {}\r\n", chrono::Utc::now().to_rfc2822()));
    out.push_str(&format!(
        "Message-ID: <{}@serabot>\r\n",
        uuid::Uuid::now_v7().simple()
    ));
    out.push_str("MIME-Version: 1.0\r\n");
    if attachments.is_empty() {
        out.push_str(
            "Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n",
        );
        out.push_str(body);
        if !body.ends_with('\n') {
            out.push('\n');
        }
    } else {
        out.push_str(&format!(
            "Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n"
        ));
        out.push_str(&format!("--{boundary}\r\n"));
        out.push_str(
            "Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n",
        );
        out.push_str(body);
        if !body.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!("\r\n--{boundary}\r\n"));
        for att in attachments {
            out.push_str(&format!("Content-Type: {}\r\n", att.content_type));
            out.push_str("Content-Transfer-Encoding: base64\r\n");
            out.push_str(&format!(
                "Content-Disposition: attachment; filename=\"{}\"\r\n\r\n",
                clean_header(&att.name)
            ));
            let encoded = base64::engine::general_purpose::STANDARD.encode(&att.bytes);
            for chunk in encoded.as_bytes().chunks(76) {
                out.push_str(std::str::from_utf8(chunk).unwrap_or(""));
                out.push_str("\r\n");
            }
            out.push_str(&format!("--{boundary}\r\n"));
        }
        out.push_str(&format!("--{boundary}--\r\n"));
    }
    Ok(out.into_bytes())
}

// ------------------------------------------------------------- wire I/O ----

/// Line-oriented reader/writer over any async stream, with caps and timeout.
struct Wire<S> {
    stream: S,
    buf: Vec<u8>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Wire<S> {
    fn new(stream: S) -> Self {
        Wire {
            stream,
            buf: Vec::new(),
        }
    }

    async fn read_line(&mut self) -> Result<String> {
        loop {
            if let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=pos).collect();
                let text = String::from_utf8_lossy(&line)
                    .trim_end_matches(['\r', '\n'])
                    .to_owned();
                return Ok(text);
            }
            if self.buf.len() > MAX_LINE {
                return Err(SeraError::External("server line too long".into()));
            }
            let mut chunk = [0u8; 4096];
            let n = tokio::time::timeout(IO_TIMEOUT, self.stream.read(&mut chunk))
                .await
                .map_err(|_| SeraError::External("mail server read timed out".into()))?
                .map_err(|e| SeraError::External(format!("mail server read: {e}")))?;
            if n == 0 {
                return Err(SeraError::External(
                    "mail server closed the connection".into(),
                ));
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// Read exactly `n` bytes, consuming the read buffer first (a previous
    /// `read_line` may already have buffered them from the socket).
    async fn read_buffered_exact(&mut self, n: usize) -> Result<Vec<u8>> {
        while self.buf.len() < n {
            if self.buf.len() > MAX_MESSAGE_BYTES {
                return Err(SeraError::External("IMAP literal too large".into()));
            }
            let mut chunk = [0u8; 4096];
            let read = tokio::time::timeout(IO_TIMEOUT, self.stream.read(&mut chunk))
                .await
                .map_err(|_| SeraError::External("mail server read timed out".into()))?
                .map_err(|e| SeraError::External(format!("mail server read: {e}")))?;
            if read == 0 {
                return Err(SeraError::External(
                    "mail server closed the connection".into(),
                ));
            }
            self.buf.extend_from_slice(&chunk[..read]);
        }
        Ok(self.buf.drain(..n).collect())
    }

    /// Read one logical line; an IMAP literal `{N}\r\n<bytes>` is folded in
    /// (at most one per line — every command this client sends yields a single
    /// `BODY[]` literal). The `{N}` marker stays in the folded text so
    /// `parse_fetch_block` can locate the body.
    async fn read_logical_line(&mut self) -> Result<String> {
        let mut line = self.read_line().await?;
        if let Some(open) = line.rfind("{")
            && let Some(close) = line[open..].find('}')
        {
            let size: usize = line[open + 1..open + close]
                .parse()
                .map_err(|_| SeraError::External("bad IMAP literal size".into()))?;
            if size > MAX_MESSAGE_BYTES {
                return Err(SeraError::External("IMAP literal too large".into()));
            }
            let literal = self.read_buffered_exact(size).await?;
            let _crlf = self.read_buffered_exact(2).await?;
            line.push_str(&String::from_utf8_lossy(&literal));
            // the rest of the response continues on the next physical line
            let rest = self.read_line().await?;
            line.push_str(&rest);
        }
        Ok(line)
    }

    async fn send_line(&mut self, line: &str) -> Result<()> {
        if line.chars().any(|c| c == '\r' || c == '\n') {
            return Err(SeraError::Validation("line injection attempt".into()));
        }
        tokio::time::timeout(IO_TIMEOUT, self.stream.write_all(line.as_bytes()))
            .await
            .map_err(|_| SeraError::External("mail server write timed out".into()))?
            .map_err(|e| SeraError::External(format!("mail server write: {e}")))?;
        tokio::time::timeout(IO_TIMEOUT, self.stream.write_all(b"\r\n"))
            .await
            .map_err(|_| SeraError::External("mail server write timed out".into()))?
            .map_err(|e| SeraError::External(format!("mail server write: {e}")))?;
        self.stream
            .flush()
            .await
            .map_err(|e| SeraError::External(format!("flush: {e}")))?;
        Ok(())
    }

    /// Read a reply and require it to start with `want` (e.g. "250" for
    /// SMTP, "* OK" for an IMAP greeting, "a1 OK" for a tagged IMAP reply).
    /// The whole prefix is compared: an earlier version compared only the
    /// first 3 bytes, which can never match a 4+-byte `want` and so rejected
    /// every IMAP greeting ("* OK [CAPABILITY ...] Server ready.") and
    /// every tagged IMAP reply ("a1 OK ...").
    async fn expect(&mut self, want: &str) -> Result<String> {
        let line = self.read_logical_line().await?;
        if line.starts_with(want) {
            Ok(line)
        } else {
            Err(SeraError::External(format!(
                "mail server replied unexpectedly: {}",
                clip(&line, 200)
            )))
        }
    }

    /// Read until the tagged reply for `tag` arrives; untagged lines are
    /// skipped (and returned via `untagged` when provided).
    async fn expect_tagged(&mut self, tag: &str) -> Result<(String, Vec<String>)> {
        let mut untagged = Vec::new();
        loop {
            let line = self.read_logical_line().await?;
            if line.starts_with(tag) {
                if line.starts_with(&format!("{tag} OK")) {
                    return Ok((line, untagged));
                }
                return Err(SeraError::External(format!(
                    "mail server replied unexpectedly: {}",
                    clip(&line, 200)
                )));
            }
            if line.starts_with('*') {
                untagged.push(line);
            }
        }
    }
}

fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn tls_connector() -> Result<TlsConnector> {
    use tokio_rustls::rustls::ClientConfig;
    let roots = tokio_rustls::rustls::RootCertStore::from_iter(
        webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
    );
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

enum MailStream {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

impl AsyncRead for MailStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match &mut *self {
            MailStream::Plain(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            MailStream::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for MailStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match &mut *self {
            MailStream::Plain(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            MailStream::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match &mut *self {
            MailStream::Plain(s) => std::pin::Pin::new(s).poll_flush(cx),
            MailStream::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_flush(cx),
        }
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match &mut *self {
            MailStream::Plain(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            MailStream::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

async fn connect(host: &str, port: u16, security: Security) -> Result<Wire<MailStream>> {
    let addr = format!("{host}:{port}");
    let tcp = tokio::time::timeout(IO_TIMEOUT, TcpStream::connect(&addr))
        .await
        .map_err(|_| SeraError::External(format!("connect to {addr} timed out")))?
        .map_err(|e| SeraError::External(format!("connect to {addr}: {e}")))?;
    tcp.set_nodelay(true).ok();
    match security {
        Security::Ssl => {
            let connector = tls_connector()?;
            let domain = tokio_rustls::rustls::pki_types::ServerName::try_from(host.to_owned())
                .map_err(|_| SeraError::Validation("invalid TLS host name".into()))?;
            let tls = tokio::time::timeout(IO_TIMEOUT, connector.connect(domain, tcp))
                .await
                .map_err(|_| SeraError::External("TLS handshake timed out".into()))?
                .map_err(|e| SeraError::External(format!("TLS handshake: {e}")))?;
            Ok(Wire::new(MailStream::Tls(Box::new(tls))))
        }
        _ => Ok(Wire::new(MailStream::Plain(tcp))),
    }
}

// ---------------------------------------------------------------- SMTP ----

/// Send one message. `security` drives plain/STARTTLS/implicit-TLS.
pub async fn smtp_send(
    creds: &MailCredentials,
    from: &str,
    to: &[String],
    message: &[u8],
) -> Result<()> {
    if message.len() > MAX_MESSAGE_BYTES {
        return Err(SeraError::Validation(
            "the message exceeds the 25 MiB cap".into(),
        ));
    }
    let mut wire = connect(creds.smtp_host.trim(), creds.smtp_port, creds.smtp_security).await?;
    wire.expect("220").await?;
    let host = creds.smtp_host.trim().to_owned();
    wire.send_line(&format!("EHLO {host}")).await?;
    // multi-line 250- reply: keep reading until "250 " (space, not dash)
    loop {
        let line = wire.read_line().await?;
        if line.starts_with("250 ") {
            break;
        }
        if line.starts_with("250") && !line.starts_with("250-") {
            break;
        }
    }
    if creds.smtp_security == Security::StartTls {
        wire.send_line("STARTTLS").await?;
        wire.expect("220").await?;
        let tcp = match wire.stream {
            MailStream::Plain(tcp) => tcp,
            _ => return Err(SeraError::Internal("STARTTLS on a TLS stream".into())),
        };
        let connector = tls_connector()?;
        let domain = tokio_rustls::rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|_| SeraError::Validation("invalid TLS host name".into()))?;
        let tls = tokio::time::timeout(IO_TIMEOUT, connector.connect(domain, tcp))
            .await
            .map_err(|_| SeraError::External("TLS handshake timed out".into()))?
            .map_err(|e| SeraError::External(format!("TLS handshake: {e}")))?;
        wire.stream = MailStream::Tls(Box::new(tls));
        wire.send_line(&format!("EHLO {host}")).await?;
        loop {
            let line = wire.read_line().await?;
            if line.starts_with("250 ") {
                break;
            }
            if line.starts_with("250") && !line.starts_with("250-") {
                break;
            }
        }
    }
    wire.send_line("AUTH LOGIN").await?;
    wire.expect("334").await?;
    wire.send_line(&base64::engine::general_purpose::STANDARD.encode(creds.username.as_bytes()))
        .await?;
    wire.expect("334").await?;
    wire.send_line(&base64::engine::general_purpose::STANDARD.encode(creds.password.as_bytes()))
        .await?;
    wire.expect("235").await?;
    wire.send_line(&format!("MAIL FROM:<{}>", extract_envelope_address(from)))
        .await?;
    wire.expect("250").await?;
    for rcpt in to {
        wire.send_line(&format!("RCPT TO:<{}>", extract_envelope_address(rcpt)))
            .await?;
        wire.expect("250").await?;
    }
    wire.send_line("DATA").await?;
    wire.expect("354").await?;
    // dot-stuff and normalize to CRLF
    let text = String::from_utf8_lossy(message)
        .replace("\r\n", "\n")
        .replace('\n', "\r\n");
    let mut stuffed = String::with_capacity(text.len() + 64);
    for line in text.split("\r\n") {
        if line.starts_with('.') {
            stuffed.push('.');
        }
        stuffed.push_str(line);
        stuffed.push_str("\r\n");
    }
    stuffed.push_str(".\r\n");
    tokio::time::timeout(IO_TIMEOUT, wire.stream.write_all(stuffed.as_bytes()))
        .await
        .map_err(|_| SeraError::External("DATA write timed out".into()))?
        .map_err(|e| SeraError::External(format!("DATA write: {e}")))?;
    wire.stream.flush().await.ok();
    wire.expect("250").await?;
    let _ = wire.send_line("QUIT").await;
    Ok(())
}

/// Connect + EHLO + AUTH + QUIT without sending (used by `POST /v1/mail/connect`).
pub async fn smtp_test(creds: &MailCredentials) -> Result<()> {
    let mut wire = connect(creds.smtp_host.trim(), creds.smtp_port, creds.smtp_security).await?;
    wire.expect("220").await?;
    let host = creds.smtp_host.trim().to_owned();
    wire.send_line(&format!("EHLO {host}")).await?;
    loop {
        let line = wire.read_line().await?;
        if line.starts_with("250 ") || (line.starts_with("250") && !line.starts_with("250-")) {
            break;
        }
    }
    if creds.smtp_security == Security::StartTls {
        wire.send_line("STARTTLS").await?;
        wire.expect("220").await?;
        let tcp = match wire.stream {
            MailStream::Plain(tcp) => tcp,
            _ => return Err(SeraError::Internal("STARTTLS on a TLS stream".into())),
        };
        let connector = tls_connector()?;
        let domain = tokio_rustls::rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|_| SeraError::Validation("invalid TLS host name".into()))?;
        let tls = tokio::time::timeout(IO_TIMEOUT, connector.connect(domain, tcp))
            .await
            .map_err(|_| SeraError::External("TLS handshake timed out".into()))?
            .map_err(|e| SeraError::External(format!("TLS handshake: {e}")))?;
        wire.stream = MailStream::Tls(Box::new(tls));
        wire.send_line(&format!("EHLO {host}")).await?;
        loop {
            let line = wire.read_line().await?;
            if line.starts_with("250 ") || (line.starts_with("250") && !line.starts_with("250-")) {
                break;
            }
        }
    }
    wire.send_line("AUTH LOGIN").await?;
    wire.expect("334").await?;
    wire.send_line(&base64::engine::general_purpose::STANDARD.encode(creds.username.as_bytes()))
        .await?;
    wire.expect("334").await?;
    wire.send_line(&base64::engine::general_purpose::STANDARD.encode(creds.password.as_bytes()))
        .await?;
    wire.expect("235").await?;
    let _ = wire.send_line("QUIT").await;
    Ok(())
}

// ---------------------------------------------------------------- IMAP ----

/// One attachment of a read message: metadata always, base64 payload only
/// when small enough to inline (0.74.5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MailAttachment {
    pub name: String,
    pub content_type: String,
    pub size: usize,
    pub data_base64: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MailMessage {
    pub uid: u32,
    pub subject: String,
    pub from: String,
    pub date: String,
    pub flags: Vec<String>,
    /// Best-effort text body: the first text/plain part of a multipart
    /// message, else the raw body (HTML parts are not decoded).
    pub body: String,
    pub attachments: Vec<MailAttachment>,
}

/// Quote an IMAP string literal.
fn imap_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

async fn imap_connect(creds: &MailCredentials) -> Result<Wire<MailStream>> {
    let mut wire = connect(creds.imap_host.trim(), creds.imap_port, creds.imap_security).await?;
    let greeting = wire.read_logical_line().await?;
    if greeting.starts_with("* PREAUTH") {
        // Already authenticated (e.g. trusted-IP preauth): skip LOGIN.
    } else if greeting.starts_with("* OK") {
        wire.send_line(&format!(
            "a1 LOGIN {} {}",
            imap_quote(&creds.username),
            imap_quote(&creds.password)
        ))
        .await?;
        // Servers (e.g. Hostinger/Dovecot) send untagged lines such as
        // "* FLAGS (...)", "* n EXISTS" or untagged "* OK [PERMANENTFLAGS ...]"
        // between LOGIN and the tagged reply; skip them.
        wire.expect_tagged("a1").await?;
    } else {
        return Err(SeraError::External(format!(
            "mail server replied unexpectedly: {}",
            clip(&greeting, 200)
        )));
    }
    Ok(wire)
}

/// Parse one raw RFC822 message into a MailMessage (header unfold + minimal
/// RFC 2047 subject decode + raw body).
pub fn parse_message(uid: u32, flags: Vec<String>, raw: &str) -> MailMessage {
    let (head, body) = match raw.find("\r\n\r\n") {
        Some(pos) => (&raw[..pos], &raw[pos + 4..]),
        None => match raw.find("\n\n") {
            Some(pos) => (&raw[..pos], &raw[pos + 2..]),
            None => (raw, ""),
        },
    };
    let mut subject = String::new();
    let mut from = String::new();
    let mut date = String::new();
    let mut last_key = String::new();
    for line in head.replace("\r\n", "\n").split('\n') {
        if (line.starts_with(' ') || line.starts_with('\t')) && !last_key.is_empty() {
            let target = match last_key.as_str() {
                "subject" => &mut subject,
                "from" => &mut from,
                "date" => &mut date,
                _ => continue,
            };
            target.push(' ');
            target.push_str(line.trim());
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            last_key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            match last_key.as_str() {
                "subject" => subject = decode_rfc2047(value),
                "from" => from = clean_header(value),
                "date" => date = clean_header(value),
                _ => {}
            }
        }
    }
    let (text_body, attachments) = extract_body_and_attachments(head, body);
    MailMessage {
        uid,
        subject,
        from,
        date,
        flags,
        body: text_body.replace("\r\n", "\n"),
        attachments,
    }
}

/// Split a message body into the readable text and its attachments. For
/// multipart messages the parts are split on the boundary from the
/// Content-Type header; single-part messages keep the raw body.
fn extract_body_and_attachments(head: &str, body: &str) -> (String, Vec<MailAttachment>) {
    let boundary = head.replace("\r\n", "\n").split('\n').find_map(|line| {
        let (key, value) = line.split_once(':')?;
        if key.trim().eq_ignore_ascii_case("content-type") {
            value.split(';').find_map(|param| {
                let (k, v) = param.split_once('=')?;
                if k.trim().eq_ignore_ascii_case("boundary") {
                    return Some(v.trim().trim_matches('"').to_owned());
                }
                None
            })
        } else {
            None
        }
    });
    let Some(boundary) = boundary else {
        return (body.to_owned(), Vec::new());
    };
    let delimiter = format!("--{boundary}");
    let mut plain: Option<String> = None;
    let mut html: Option<String> = None;
    let mut attachments = Vec::new();
    let mut inline_total = 0usize;
    for part in body.split(&delimiter).skip(1) {
        let part = part
            .strip_prefix("\r\n")
            .or_else(|| part.strip_prefix('\n'))
            .unwrap_or(part);
        if part.starts_with("--") {
            break; // closing delimiter
        }
        let part = part
            .strip_suffix("\r\n")
            .or_else(|| part.strip_suffix('\n'))
            .unwrap_or(part);
        let (part_head, part_body) = match part.find("\r\n\r\n") {
            Some(pos) => (&part[..pos], &part[pos + 4..]),
            None => match part.find("\n\n") {
                Some(pos) => (&part[..pos], &part[pos + 2..]),
                None => (part, ""),
            },
        };
        let head_lc = part_head.to_ascii_lowercase();
        let content_type = head_lc
            .split('\n')
            .find_map(|l| {
                l.split_once(':')
                    .filter(|(k, _)| k.trim() == "content-type")
            })
            .map(|(_, v)| v.split(';').next().unwrap_or("").trim().to_owned())
            .unwrap_or_else(|| "text/plain".to_owned());
        let filename = part_head.split('\n').find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if !k.trim().eq_ignore_ascii_case("content-disposition") {
                return None;
            }
            v.split(';').find_map(|p| {
                let (k, v) = p.split_once('=')?;
                if k.trim().eq_ignore_ascii_case("filename") {
                    Some(v.trim().trim_matches('"').to_owned())
                } else {
                    None
                }
            })
        });
        let decoded = if head_lc.contains("content-transfer-encoding: base64") {
            let compact: String = part_body.chars().filter(|c| !c.is_whitespace()).collect();
            base64::engine::general_purpose::STANDARD
                .decode(compact)
                .unwrap_or_default()
        } else {
            part_body.as_bytes().to_vec()
        };
        if let Some(name) = filename {
            let size = decoded.len();
            let data_base64 = if size <= MAX_INLINE_ATTACHMENT_BYTES
                && inline_total + size <= MAX_INLINE_ATTACHMENTS_TOTAL
            {
                inline_total += size;
                Some(base64::engine::general_purpose::STANDARD.encode(&decoded))
            } else {
                None
            };
            attachments.push(MailAttachment {
                name,
                content_type,
                size,
                data_base64,
            });
        } else if content_type.starts_with("text/plain") && plain.is_none() {
            plain = Some(String::from_utf8_lossy(&decoded).into_owned());
        } else if content_type.starts_with("text/html") && html.is_none() {
            html = Some(String::from_utf8_lossy(&decoded).into_owned());
        }
    }
    let text = plain.or(html).unwrap_or_default();
    (text, attachments)
}

fn extract_envelope_address(raw: &str) -> String {
    let cleaned = clean_header(raw);
    if let (Some(start), Some(end)) = (cleaned.rfind('<'), cleaned.rfind('>'))
        && start < end
    {
        return cleaned[start + 1..end].trim().to_owned();
    }
    cleaned.trim().to_owned()
}

fn extract_raw_attachment(raw: &str, index: usize) -> Result<(String, String, Vec<u8>)> {
    let (head, body) = match raw.find("\r\n\r\n") {
        Some(pos) => (&raw[..pos], &raw[pos + 4..]),
        None => match raw.find("\n\n") {
            Some(pos) => (&raw[..pos], &raw[pos + 2..]),
            None => (raw, ""),
        },
    };
    let boundary = head.replace("\r\n", "\n").split('\n').find_map(|line| {
        let (key, value) = line.split_once(':')?;
        if key.trim().eq_ignore_ascii_case("content-type") {
            value.split(';').find_map(|param| {
                let (k, v) = param.split_once('=')?;
                if k.trim().eq_ignore_ascii_case("boundary") {
                    return Some(v.trim().trim_matches('"').to_owned());
                }
                None
            })
        } else {
            None
        }
    });
    let Some(boundary) = boundary else {
        return Err(SeraError::NotFound("attachment".into()));
    };
    let delimiter = format!("--{boundary}");
    let mut att_idx = 0usize;
    for part in body.split(&delimiter).skip(1) {
        let part = part
            .strip_prefix("\r\n")
            .or_else(|| part.strip_prefix('\n'))
            .unwrap_or(part);
        if part.starts_with("--") {
            break;
        }
        let part = part
            .strip_suffix("\r\n")
            .or_else(|| part.strip_suffix('\n'))
            .unwrap_or(part);
        let (part_head, part_body) = match part.find("\r\n\r\n") {
            Some(pos) => (&part[..pos], &part[pos + 4..]),
            None => match part.find("\n\n") {
                Some(pos) => (&part[..pos], &part[pos + 2..]),
                None => (part, ""),
            },
        };
        let head_lc = part_head.to_ascii_lowercase();
        let content_type = head_lc
            .split('\n')
            .find_map(|l| {
                l.split_once(':')
                    .filter(|(k, _)| k.trim() == "content-type")
            })
            .map(|(_, v)| v.split(';').next().unwrap_or("").trim().to_owned())
            .unwrap_or_else(|| "application/octet-stream".to_owned());
        let filename = part_head.split('\n').find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if !k.trim().eq_ignore_ascii_case("content-disposition") {
                return None;
            }
            v.split(';').find_map(|p| {
                let (k, v) = p.split_once('=')?;
                if k.trim().eq_ignore_ascii_case("filename") {
                    Some(v.trim().trim_matches('"').to_owned())
                } else {
                    None
                }
            })
        });
        if let Some(name) = filename {
            if att_idx == index {
                let decoded = if head_lc.contains("content-transfer-encoding: base64") {
                    let compact: String =
                        part_body.chars().filter(|c| !c.is_whitespace()).collect();
                    base64::engine::general_purpose::STANDARD
                        .decode(compact)
                        .map_err(|_| SeraError::External("invalid attachment base64".into()))?
                } else {
                    part_body.as_bytes().to_vec()
                };
                if decoded.len() > MAX_ATTACHMENT_BYTES {
                    return Err(SeraError::Validation(
                        "attachment exceeds 20 MiB limit".into(),
                    ));
                }
                return Ok((name, content_type, decoded));
            }
            att_idx += 1;
        }
    }
    Err(SeraError::NotFound("attachment".into()))
}

fn decode_rfc2047(value: &str) -> String {
    // =?UTF-8?B?...?= (one encoded word is enough for subjects)
    if let Some(rest) = value
        .strip_prefix("=?UTF-8?B?")
        .and_then(|r| r.strip_suffix("?="))
        && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(rest.trim())
    {
        return String::from_utf8_lossy(&bytes).into_owned();
    }
    clean_header(value)
}

/// Extract flags and the BODY[...] literal from an untagged FETCH response block.
fn parse_fetch_block(block: &str) -> (Vec<String>, Option<String>) {
    let mut flags = Vec::new();
    if let Some(start) = block.find("FLAGS (")
        && let Some(end) = block[start..].find(')')
    {
        flags = block[start + 7..start + end]
            .split_whitespace()
            .map(|f| f.trim_matches(['\\', '(', ')']).to_owned())
            .filter(|f| !f.is_empty())
            .collect();
    }
    let body = block
        .find("BODY[")
        .and_then(|pos| block[pos..].find('{').map(|b| pos + b))
        .and_then(|brace| {
            let rest = &block[brace..];
            let close = rest.find('}')?;
            let size: usize = rest[1..close].parse().ok()?;
            let start = close + 1;
            // the literal content follows the size marker in the folded line
            let content = &rest[start..];
            Some(content.trim_end_matches(')').trim().to_owned()).filter(|c| {
                let _ = size;
                !c.is_empty()
            })
        });
    (flags, body)
}

/// Connect + LOGIN + LOGOUT without selecting a folder (connect test).
pub async fn imap_test(creds: &MailCredentials) -> Result<()> {
    let mut wire = imap_connect(creds).await?;
    let _ = wire.send_line("a9 LOGOUT").await;
    Ok(())
}

/// Normalise the folder argument (empty means INBOX) and validate it.
fn normalize_folder(folder: &str) -> Result<String> {
    let folder = folder.trim();
    let folder = if folder.is_empty() { "INBOX" } else { folder };
    if folder.len() > 512 || folder.chars().any(|c| c.is_control()) {
        return Err(SeraError::Validation("invalid folder name".into()));
    }
    Ok(folder.to_owned())
}

fn folder_or_inbox(folder: &str) -> String {
    if folder.trim().is_empty() {
        "INBOX".to_owned()
    } else {
        folder.trim().to_owned()
    }
}

/// SELECT a folder; untagged FLAGS/EXISTS/RECENT lines precede the tagged OK.
async fn select_folder<S: AsyncRead + AsyncWrite + Unpin>(
    wire: &mut Wire<S>,
    folder: &str,
) -> Result<()> {
    wire.send_line(&format!("a2 SELECT {}", imap_quote(folder)))
        .await?;
    wire.expect_tagged("a2").await?;
    Ok(())
}

/// Parse the UIDs out of an untagged `* SEARCH ...` line.
fn parse_search_uids(untagged: &[String]) -> Vec<u32> {
    let search = untagged
        .iter()
        .find(|l| l.starts_with("* SEARCH"))
        .cloned()
        .unwrap_or_default();
    search
        .split_whitespace()
        .skip(2)
        .filter_map(|w| w.parse::<u32>().ok())
        .collect()
}

/// Fetch the newest `limit` matching messages of a folder after a SEARCH.
async fn fetch_search_results<S: AsyncRead + AsyncWrite + Unpin>(
    wire: &mut Wire<S>,
    untagged: Vec<String>,
    limit: u32,
) -> Result<Vec<MailMessage>> {
    let uids: Vec<u32> = parse_search_uids(&untagged)
        .into_iter()
        .rev()
        .take(limit as usize)
        .collect();
    let mut out = Vec::new();
    for uid in uids {
        if let Ok(msg) = imap_fetch(wire, uid).await {
            out.push(msg);
        }
    }
    Ok(out)
}

/// List the newest `limit` messages of a folder (default INBOX).
pub async fn imap_list(
    creds: &MailCredentials,
    folder: &str,
    limit: u32,
) -> Result<Vec<MailMessage>> {
    let limit = limit.clamp(1, MAX_LIST);
    let mut wire = imap_connect(creds).await?;
    let folder = normalize_folder(folder)?;
    select_folder(&mut wire, &folder).await?;
    wire.send_line("a3 UID SEARCH ALL").await?;
    let (_, untagged) = wire.expect_tagged("a3").await?;
    let out = fetch_search_results(&mut wire, untagged, limit).await?;
    let _ = wire.send_line("a9 LOGOUT").await;
    Ok(out)
}

/// List the newest `limit` message headers (subject, from, date, flags; empty
/// body) of a folder read-only using `EXAMINE` and `BODY.PEEK[HEADER]`.
pub async fn imap_list_headers(
    creds: &MailCredentials,
    folder: &str,
    limit: u32,
) -> Result<Vec<MailMessage>> {
    let limit = limit.clamp(1, MAX_LIST);
    let mut wire = imap_connect(creds).await?;
    let folder = normalize_folder(folder)?;
    wire.send_line(&format!("a2 EXAMINE {}", imap_quote(&folder)))
        .await?;
    wire.expect_tagged("a2").await?;
    wire.send_line("a3 UID SEARCH ALL").await?;
    let (_, untagged) = wire.expect_tagged("a3").await?;
    let uids: Vec<u32> = parse_search_uids(&untagged)
        .into_iter()
        .rev()
        .take(limit as usize)
        .collect();
    let mut out = Vec::new();
    for uid in uids {
        if let Ok(msg) = imap_fetch_headers(&mut wire, uid).await {
            out.push(msg);
        }
    }
    let _ = wire.send_line("a9 LOGOUT").await;
    Ok(out)
}

/// Search a folder by subject/from/body text; returns the newest `limit` matches.
pub async fn imap_search(
    creds: &MailCredentials,
    folder: &str,
    query: &str,
    limit: u32,
) -> Result<Vec<MailMessage>> {
    let limit = limit.clamp(1, MAX_LIST);
    let query = query.trim();
    if query.is_empty() || query.len() > 256 {
        return Err(SeraError::Validation(
            "query must be 1-256 characters".into(),
        ));
    }
    let mut wire = imap_connect(creds).await?;
    let folder = normalize_folder(folder)?;
    select_folder(&mut wire, &folder).await?;
    let q = imap_quote(query);
    wire.send_line(&format!(
        "a3 UID SEARCH OR OR SUBJECT {q} FROM {q} TEXT {q}"
    ))
    .await?;
    let (_, untagged) = wire.expect_tagged("a3").await?;
    let out = fetch_search_results(&mut wire, untagged, limit).await?;
    let _ = wire.send_line("a9 LOGOUT").await;
    Ok(out)
}

/// List all folder names (`LIST "" "*"`).
pub async fn imap_folders(creds: &MailCredentials) -> Result<Vec<String>> {
    let mut wire = imap_connect(creds).await?;
    wire.send_line("a2 LIST \"\" \"*\"").await?;
    let (_, untagged) = wire.expect_tagged("a2").await?;
    let mut folders = Vec::new();
    for line in &untagged {
        if let Some(name) = parse_list_name(line) {
            folders.push(name);
        }
    }
    let _ = wire.send_line("a9 LOGOUT").await;
    Ok(folders)
}

/// Extract the folder name (last quoted string) from an untagged LIST reply.
fn parse_list_name(line: &str) -> Option<String> {
    let end = line.rfind('"')?;
    let start = line[..end].rfind('"')?;
    let name = &line[start + 1..end];
    if name.is_empty() || name == "*" {
        return None;
    }
    Some(name.replace("\\\"", "\"").replace("\\\\", "\\"))
}

/// Validate an IMAP flag (`\Seen`, `\Flagged`, ...): backslash + letters only.
fn validate_flag(flag: &str) -> Result<()> {
    let valid = flag.starts_with('\\')
        && flag.len() <= 32
        && flag[1..].chars().all(|c| c.is_ascii_alphabetic());
    if !valid {
        return Err(SeraError::Validation(
            "flags must look like \\Seen or \\Flagged".into(),
        ));
    }
    Ok(())
}

/// Add or remove flags on one message (`UID STORE ±FLAGS.SILENT`).
pub async fn imap_store_flags(
    creds: &MailCredentials,
    folder: &str,
    uid: u32,
    add: bool,
    flags: &[String],
) -> Result<()> {
    if flags.is_empty() || flags.len() > 8 {
        return Err(SeraError::Validation("1 to 8 flags are allowed".into()));
    }
    for f in flags {
        validate_flag(f)?;
    }
    let mut wire = imap_connect(creds).await?;
    let folder = normalize_folder(folder)?;
    select_folder(&mut wire, &folder).await?;
    let op = if add {
        "+FLAGS.SILENT"
    } else {
        "-FLAGS.SILENT"
    };
    wire.send_line(&format!("a4 UID STORE {uid} {op} ({})", flags.join(" ")))
        .await?;
    wire.expect_tagged("a4").await?;
    let _ = wire.send_line("a9 LOGOUT").await;
    Ok(())
}

/// Set or clear `\Seen` and/or `\Flagged` on one message.
pub async fn imap_flag(
    creds: &MailCredentials,
    folder: &str,
    uid: u32,
    seen: Option<bool>,
    flagged: Option<bool>,
) -> Result<()> {
    let mut add = Vec::new();
    let mut remove = Vec::new();
    if let Some(s) = seen {
        if s {
            add.push("\\Seen".to_owned());
        } else {
            remove.push("\\Seen".to_owned());
        }
    }
    if let Some(f) = flagged {
        if f {
            add.push("\\Flagged".to_owned());
        } else {
            remove.push("\\Flagged".to_owned());
        }
    }
    if add.is_empty() && remove.is_empty() {
        return Err(SeraError::Validation(
            "at least one of seen or flagged is required".into(),
        ));
    }
    let mut wire = imap_connect(creds).await?;
    let folder = normalize_folder(folder)?;
    select_folder(&mut wire, &folder).await?;
    if !add.is_empty() {
        wire.send_line(&format!(
            "a4 UID STORE {uid} +FLAGS.SILENT ({})",
            add.join(" ")
        ))
        .await?;
        wire.expect_tagged("a4").await?;
    }
    if !remove.is_empty() {
        wire.send_line(&format!(
            "a5 UID STORE {uid} -FLAGS.SILENT ({})",
            remove.join(" ")
        ))
        .await?;
        wire.expect_tagged("a5").await?;
    }
    let _ = wire.send_line("a9 LOGOUT").await;
    Ok(())
}

/// Move one message to another folder (`UID MOVE`, IMAP4rev1 extension that
/// Hostinger/Dovecot support).
pub async fn imap_move(creds: &MailCredentials, folder: &str, uid: u32, dest: &str) -> Result<()> {
    let dest = normalize_folder(dest)?;
    let mut wire = imap_connect(creds).await?;
    let folder = normalize_folder(folder)?;
    select_folder(&mut wire, &folder).await?;
    wire.send_line(&format!("a4 UID MOVE {uid} {}", imap_quote(&dest)))
        .await?;
    wire.expect_tagged("a4").await?;
    let _ = wire.send_line("a9 LOGOUT").await;
    Ok(())
}

/// Permanently delete one message: flag `\Deleted`, then `UID EXPUNGE`.
pub async fn imap_delete(creds: &MailCredentials, folder: &str, uid: u32) -> Result<()> {
    let mut wire = imap_connect(creds).await?;
    let folder = normalize_folder(folder)?;
    select_folder(&mut wire, &folder).await?;
    wire.send_line(&format!("a4 UID STORE {uid} +FLAGS.SILENT (\\Deleted)"))
        .await?;
    wire.expect_tagged("a4").await?;
    wire.send_line(&format!("a5 UID EXPUNGE {uid}")).await?;
    wire.expect_tagged("a5").await?;
    let _ = wire.send_line("a9 LOGOUT").await;
    Ok(())
}

/// Read one message by UID.
pub async fn imap_read(creds: &MailCredentials, folder: &str, uid: u32) -> Result<MailMessage> {
    let mut wire = imap_connect(creds).await?;
    let folder = normalize_folder(folder)?;
    select_folder(&mut wire, &folder).await?;
    let msg = imap_fetch(&mut wire, uid).await?;
    let _ = wire.send_line("a9 LOGOUT").await;
    Ok(msg)
}

/// Read one attachment's raw bytes (up to `MAX_ATTACHMENT_BYTES`) by UID and
/// attachment index, bypassing the 512 KiB inline preview cap.
pub async fn imap_read_attachment(
    creds: &MailCredentials,
    folder: &str,
    uid: u32,
    index: usize,
) -> Result<(String, String, Vec<u8>)> {
    let mut wire = imap_connect(creds).await?;
    let folder = normalize_folder(folder)?;
    select_folder(&mut wire, &folder).await?;
    let (_flags, raw) = imap_fetch_raw(&mut wire, uid, "BODY.PEEK[]").await?;
    let _ = wire.send_line("a9 LOGOUT").await;
    extract_raw_attachment(&raw, index)
}

async fn imap_fetch<S: AsyncRead + AsyncWrite + Unpin>(
    wire: &mut Wire<S>,
    uid: u32,
) -> Result<MailMessage> {
    imap_fetch_section(wire, uid, "BODY.PEEK[]").await
}

async fn imap_fetch_headers<S: AsyncRead + AsyncWrite + Unpin>(
    wire: &mut Wire<S>,
    uid: u32,
) -> Result<MailMessage> {
    imap_fetch_section(wire, uid, "BODY.PEEK[HEADER]").await
}

async fn imap_fetch_raw<S: AsyncRead + AsyncWrite + Unpin>(
    wire: &mut Wire<S>,
    uid: u32,
    section: &str,
) -> Result<(Vec<String>, String)> {
    wire.send_line(&format!("a4 UID FETCH {uid} (FLAGS {section})"))
        .await?;
    let mut block = String::new();
    loop {
        let line = wire.read_logical_line().await?;
        if line.starts_with("a4 ") {
            if !line.starts_with("a4 OK") {
                return Err(SeraError::External(format!(
                    "FETCH failed: {}",
                    clip(&line, 200)
                )));
            }
            break;
        }
        if line.starts_with('*') {
            block.push_str(&line);
            block.push('\n');
        }
    }
    let (flags, body) = parse_fetch_block(&block);
    let raw = body.unwrap_or_default();
    if raw.is_empty() {
        return Err(SeraError::NotFound(format!("message {uid} has no body")));
    }
    Ok((flags, raw))
}

async fn imap_fetch_section<S: AsyncRead + AsyncWrite + Unpin>(
    wire: &mut Wire<S>,
    uid: u32,
    section: &str,
) -> Result<MailMessage> {
    let (flags, raw) = imap_fetch_raw(wire, uid, section).await?;
    Ok(parse_message(uid, flags, &raw))
}

// --------------------------------------------------------------- tools ----

pub fn register(registry: &mut ToolRegistry, secrets: Arc<UserSecrets>) -> Result<()> {
    registry.register(MailSendTool {
        secrets: secrets.clone(),
    })?;
    registry.register(MailListTool {
        secrets: secrets.clone(),
    })?;
    registry.register(MailReadTool {
        secrets: secrets.clone(),
    })?;
    registry.register(MailFoldersTool {
        secrets: secrets.clone(),
    })?;
    registry.register(MailSearchTool {
        secrets: secrets.clone(),
    })?;
    registry.register(MailFlagTool {
        secrets: secrets.clone(),
    })?;
    registry.register(MailMoveTool {
        secrets: secrets.clone(),
    })?;
    registry.register(MailDeleteTool { secrets })?;
    Ok(())
}

fn require_creds(
    secrets: &UserSecrets,
    user_id: &str,
    account: Option<&str>,
) -> Result<MailCredentials> {
    load_credentials(secrets, user_id, account)?.ok_or_else(|| {
        SeraError::Validation(match account.filter(|a| !a.trim().is_empty()) {
            Some(label) => format!(
                "mail account '{label}' is not connected; connect it in Integrations → Mail first"
            ),
            None => "mail is not connected; connect it first".into(),
        })
    })
}

/// Optional `account` argument shared by all mail tools.
fn arg_account(args: &Value) -> Option<String> {
    args.get("account")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

pub struct MailSendTool {
    secrets: Arc<UserSecrets>,
}

#[async_trait]
impl Tool for MailSendTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: MAIL_SEND_TOOL.into(),
            description: "Send an email through the member's own SMTP mailbox (with optional \
                          attachments). Always asks for approval; the approval names the \
                          recipients and subject."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "to": {"type": "array", "items": {"type": "string"}, "description": "Recipient addresses"},
                    "subject": {"type": "string"},
                    "body": {"type": "string", "description": "Plain-text body"},
                    "account": {"type": "string", "description": "Named mailbox connected in Integrations → Mail; omit for the default account"},
                    "attachments": {"type": "array", "items": {"type": "object", "properties": {
                        "name": {"type": "string"},
                        "content_type": {"type": "string"},
                        "data_base64": {"type": "string"}
                    }, "required": ["name", "data_base64"]}},
                },
                "required": ["to", "subject", "body"],
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let account = arg_account(&args);
        let creds = require_creds(&self.secrets, &context.user_id, account.as_deref())?;
        let to: Vec<String> = args
            .get("to")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let subject = args
            .get("subject")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let body = args
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let body = body_with_signature(&creds, body)?;
        let mut attachments = Vec::new();
        if let Some(items) = args.get("attachments").and_then(Value::as_array) {
            for item in items {
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("file")
                    .to_owned();
                let content_type = item
                    .get("content_type")
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream")
                    .to_owned();
                let data = item
                    .get("data_base64")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|_| SeraError::Validation("attachment is not valid base64".into()))?;
                attachments.push(Attachment {
                    name,
                    content_type,
                    bytes,
                });
            }
        }
        let message = build_message(&from_address(&creds), &to, &subject, &body, &attachments)?;
        smtp_send(&creds, &from_address(&creds), &to, &message).await?;
        Ok(ToolOutput {
            content: format!(
                "Sent to {}: \"{}\" ({} attachment(s), {} bytes).",
                to.join(", "),
                clip(&subject, 120),
                attachments.len(),
                message.len()
            ),
            data: Some(json!({
                "to": to,
                "subject": subject,
                "attachments": attachments.iter().map(|a| json!({"name": a.name, "bytes": a.bytes.len()})).collect::<Vec<_>>(),
            })),
            ..ToolOutput::default()
        })
    }
}

pub struct MailListTool {
    secrets: Arc<UserSecrets>,
}

#[async_trait]
impl Tool for MailListTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: MAIL_LIST_TOOL.into(),
            description: "List the newest messages of the member's own IMAP mailbox (subject, \
                          from, date, flags). Read-only."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "folder": {"type": "string", "description": "Folder, default INBOX"},
                    "limit": {"type": "integer", "description": "How many, 1-50, default 10"},
                    "account": {"type": "string", "description": "Named mailbox connected in Integrations → Mail; omit for the default account"}
                },
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let account = arg_account(&args);
        let creds = require_creds(&self.secrets, &context.user_id, account.as_deref())?;
        let folder = args
            .get("folder")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as u32)
            .unwrap_or(10);
        let messages = imap_list(&creds, &folder, limit).await?;
        let lines: Vec<String> = messages
            .iter()
            .map(|m| {
                format!(
                    "#{} {} {} — {} [{}]",
                    m.uid,
                    clip(&m.from, 60),
                    clip(&m.subject, 80),
                    m.date,
                    m.flags.join(" ")
                )
            })
            .collect();
        Ok(ToolOutput {
            content: if lines.is_empty() {
                "No messages.".to_owned()
            } else {
                lines.join("\n")
            },
            data: Some(json!({"messages": messages})),
            ..ToolOutput::default()
        })
    }
}

pub struct MailReadTool {
    secrets: Arc<UserSecrets>,
}

#[async_trait]
impl Tool for MailReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: MAIL_READ_TOOL.into(),
            description: "Read one message (by UID from mail_list) from the member's own IMAP \
                          mailbox: headers plus body text. Read-only."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "uid": {"type": "integer"},
                    "folder": {"type": "string", "description": "Folder, default INBOX"},
                    "account": {"type": "string", "description": "Named mailbox connected in Integrations → Mail; omit for the default account"}
                },
                "required": ["uid"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let account = arg_account(&args);
        let creds = require_creds(&self.secrets, &context.user_id, account.as_deref())?;
        let uid =
            args.get("uid")
                .and_then(Value::as_u64)
                .ok_or_else(|| SeraError::Validation("uid is required".into()))? as u32;
        let folder = args
            .get("folder")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let message = imap_read(&creds, &folder, uid).await?;
        let attachment_line = if message.attachments.is_empty() {
            String::new()
        } else {
            format!(
                "\nAttachments: {}",
                message
                    .attachments
                    .iter()
                    .map(|a| format!("{} ({}, {} bytes)", a.name, a.content_type, a.size))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        Ok(ToolOutput {
            content: format!(
                "From: {}\nDate: {}\nSubject: {}\nFlags: {}\n{}\n\n{}",
                message.from,
                message.date,
                message.subject,
                message.flags.join(" "),
                attachment_line,
                clip(&message.body, 8000)
            ),
            data: Some(json!({"message": message})),
            ..ToolOutput::default()
        })
    }
}

pub struct MailFoldersTool {
    secrets: Arc<UserSecrets>,
}

#[async_trait]
impl Tool for MailFoldersTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: MAIL_FOLDERS_TOOL.into(),
            description: "List the folders of the member's own IMAP mailbox. Read-only.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "account": {"type": "string", "description": "Named mailbox connected in Integrations → Mail; omit for the default account"}
                },
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let account = arg_account(&args);
        let creds = require_creds(&self.secrets, &context.user_id, account.as_deref())?;
        let folders = imap_folders(&creds).await?;
        Ok(ToolOutput {
            content: if folders.is_empty() {
                "No folders.".to_owned()
            } else {
                folders.join("\n")
            },
            data: Some(json!({"folders": folders})),
            ..ToolOutput::default()
        })
    }
}

pub struct MailSearchTool {
    secrets: Arc<UserSecrets>,
}

#[async_trait]
impl Tool for MailSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: MAIL_SEARCH_TOOL.into(),
            description: "Search the member's own IMAP mailbox by subject, sender, or body text \
                          (newest matches first). Read-only."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search text, 1-256 chars"},
                    "folder": {"type": "string", "description": "Folder, default INBOX"},
                    "limit": {"type": "integer", "description": "How many matches, 1-50, default 10"},
                    "account": {"type": "string", "description": "Named mailbox connected in Integrations → Mail; omit for the default account"}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let account = arg_account(&args);
        let creds = require_creds(&self.secrets, &context.user_id, account.as_deref())?;
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("query is required".into()))?;
        let folder = args
            .get("folder")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as u32)
            .unwrap_or(10);
        let messages = imap_search(&creds, &folder, query, limit).await?;
        let lines: Vec<String> = messages
            .iter()
            .map(|m| {
                format!(
                    "#{} {} {} — {} [{}]",
                    m.uid,
                    clip(&m.from, 60),
                    clip(&m.subject, 80),
                    m.date,
                    m.flags.join(" ")
                )
            })
            .collect();
        Ok(ToolOutput {
            content: if lines.is_empty() {
                format!("No messages matching \"{}\".", clip(query, 120))
            } else {
                lines.join("\n")
            },
            data: Some(json!({"messages": messages})),
            ..ToolOutput::default()
        })
    }
}

pub struct MailFlagTool {
    secrets: Arc<UserSecrets>,
}

#[async_trait]
impl Tool for MailFlagTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: MAIL_FLAG_TOOL.into(),
            description: "Add or remove flags (\\Seen, \\Flagged, \\Answered, \\Draft, \\Deleted) on one \
                          message of the member's own mailbox. Changes the mailbox — approval-gated."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "uid": {"type": "integer"},
                    "flags": {"type": "array", "items": {"type": "string"}, "description": "Flags like \\Seen or \\Flagged"},
                    "add": {"type": "boolean", "description": "true to add (default), false to remove"},
                    "folder": {"type": "string", "description": "Folder, default INBOX"},
                    "account": {"type": "string", "description": "Named mailbox connected in Integrations → Mail; omit for the default account"}
                },
                "required": ["uid", "flags"],
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let account = arg_account(&args);
        let creds = require_creds(&self.secrets, &context.user_id, account.as_deref())?;
        let uid =
            args.get("uid")
                .and_then(Value::as_u64)
                .ok_or_else(|| SeraError::Validation("uid is required".into()))? as u32;
        let flags: Vec<String> = args
            .get("flags")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let add = args.get("add").and_then(Value::as_bool).unwrap_or(true);
        let folder = args
            .get("folder")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        imap_store_flags(&creds, &folder, uid, add, &flags).await?;
        Ok(ToolOutput {
            content: format!(
                "{} flags {} on message {uid} ({}).",
                if add { "Added" } else { "Removed" },
                flags.join(" "),
                folder_or_inbox(&folder)
            ),
            data: Some(json!({"uid": uid, "flags": flags, "add": add, "folder": folder})),
            ..ToolOutput::default()
        })
    }
}

pub struct MailMoveTool {
    secrets: Arc<UserSecrets>,
}

#[async_trait]
impl Tool for MailMoveTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: MAIL_MOVE_TOOL.into(),
            description: "Move one message of the member's own mailbox to another folder. \
                          Changes the mailbox — approval-gated."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "uid": {"type": "integer"},
                    "dest": {"type": "string", "description": "Destination folder, e.g. Archive"},
                    "folder": {"type": "string", "description": "Current folder, default INBOX"},
                    "account": {"type": "string", "description": "Named mailbox connected in Integrations → Mail; omit for the default account"}
                },
                "required": ["uid", "dest"],
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let account = arg_account(&args);
        let creds = require_creds(&self.secrets, &context.user_id, account.as_deref())?;
        let uid =
            args.get("uid")
                .and_then(Value::as_u64)
                .ok_or_else(|| SeraError::Validation("uid is required".into()))? as u32;
        let dest = args
            .get("dest")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("dest is required".into()))?;
        let folder = args
            .get("folder")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        imap_move(&creds, &folder, uid, dest).await?;
        Ok(ToolOutput {
            content: format!("Moved message {uid} to {dest}."),
            data: Some(json!({"uid": uid, "dest": dest, "folder": folder})),
            ..ToolOutput::default()
        })
    }
}

pub struct MailDeleteTool {
    secrets: Arc<UserSecrets>,
}

#[async_trait]
impl Tool for MailDeleteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: MAIL_DELETE_TOOL.into(),
            description: "Permanently delete one message from the member's own mailbox (flags \
                          \\Deleted then UID EXPUNGE). Irreversible — approval-gated."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "uid": {"type": "integer"},
                    "folder": {"type": "string", "description": "Folder, default INBOX"},
                    "account": {"type": "string", "description": "Named mailbox connected in Integrations → Mail; omit for the default account"}
                },
                "required": ["uid"],
                "additionalProperties": false
            }),
            external_effect: true,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let account = arg_account(&args);
        let creds = require_creds(&self.secrets, &context.user_id, account.as_deref())?;
        let uid =
            args.get("uid")
                .and_then(Value::as_u64)
                .ok_or_else(|| SeraError::Validation("uid is required".into()))? as u32;
        let folder = args
            .get("folder")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        imap_delete(&creds, &folder, uid).await?;
        Ok(ToolOutput {
            content: format!("Deleted message {uid} from {}.", folder_or_inbox(&folder)),
            data: Some(json!({"uid": uid, "folder": folder})),
            ..ToolOutput::default()
        })
    }
}

// --------------------------------------------------------------- tests ----

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::TcpListener;

    fn creds(security: Security) -> MailCredentials {
        MailCredentials {
            smtp_host: "127.0.0.1".into(),
            smtp_port: 2525,
            smtp_security: security,
            username: "me@example.com".into(),
            password: "app-password".into(),
            imap_host: "127.0.0.1".into(),
            imap_port: 1143,
            imap_security: Security::None,
            from: None,
            signature: None,
        }
    }

    #[test]
    fn credentials_validation() {
        let mut c = creds(Security::None);
        assert!(validate_credentials(&c).is_ok());
        c.smtp_security = Security::None;
        c.smtp_host = "mail.hostinger.com".into();
        assert!(
            validate_credentials(&c).is_err(),
            "plain refused off-loopback"
        );
        let mut c = creds(Security::StartTls);
        c.smtp_host = "mail.hostinger.com".into();
        assert!(validate_credentials(&c).is_ok());
        c.password = String::new();
        assert!(validate_credentials(&c).is_err());
    }

    #[test]
    fn mime_with_attachment_roundtrips() {
        let msg = build_message(
            "me@example.com",
            &["you@example.com".to_owned()],
            "Rapport hébdo",
            "Bonjour,\nvoir pièce jointe.\n",
            &[Attachment {
                name: "rapport.pdf".into(),
                content_type: "application/pdf".into(),
                bytes: b"%PDF-1.4 fake".to_vec(),
            }],
        )
        .unwrap();
        let text = String::from_utf8(msg).unwrap();
        assert!(text.contains("multipart/mixed; boundary=\"serabot-"));
        assert!(text.contains("Subject: =?UTF-8?B?"));
        assert!(text.contains("Content-Disposition: attachment; filename=\"rapport.pdf\""));
        assert!(
            text.contains("JVBERi0xLjQgZmFrZQ=="),
            "base64 of the pdf bytes"
        );
        assert!(text.contains("Bonjour,"));
        // injection attempts are refused
        assert!(build_message("a@b", &["x@y\r\nBcc: evil@z".into()], "s", "b", &[]).is_err());
    }

    #[test]
    fn parses_rfc822_message() {
        let raw = "From: Alice <alice@example.com>\r\nSubject: =?UTF-8?B?SMOpbGxv?=\r\nDate: Mon, 1 Jan 2026 10:00:00 +0000\r\nX-Long: one\r\n two\r\n\r\nHello body\r\n";
        let msg = parse_message(42, vec!["\\Seen".into()], raw);
        assert_eq!(msg.uid, 42);
        assert_eq!(msg.subject, "Héllo");
        assert_eq!(msg.from, "Alice <alice@example.com>");
        assert_eq!(msg.body, "Hello body\n");
        assert_eq!(msg.flags, vec!["\\Seen"]);
    }

    #[test]
    fn parses_fetch_block() {
        let block = "* 1 FETCH (FLAGS (\\Seen) UID 7 BODY[] {20}\r\nSubject: Hi\r\n\r\nbody here)";
        let (flags, body) = parse_fetch_block(block);
        assert_eq!(flags, vec!["Seen"]);
        assert!(body.unwrap().contains("Subject: Hi"));
    }

    #[tokio::test]
    async fn smtp_send_against_fake_server() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = socket.into_split();
            let mut lines = BufReader::new(reader).lines();
            let mut data = String::new();
            writer.write_all(b"220 fake ESMTP\r\n").await.unwrap();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.starts_with("EHLO") {
                    writer
                        .write_all(b"250-fake\r\n250 AUTH LOGIN\r\n")
                        .await
                        .unwrap();
                } else if line == "AUTH LOGIN" {
                    writer.write_all(b"334 VXNlcm5hbWU6\r\n").await.unwrap();
                    lines.next_line().await.unwrap().unwrap();
                    writer.write_all(b"334 UGFzc3dvcmQ6\r\n").await.unwrap();
                    lines.next_line().await.unwrap().unwrap();
                    writer.write_all(b"235 ok\r\n").await.unwrap();
                } else if line.starts_with("MAIL FROM") || line.starts_with("RCPT TO") {
                    writer.write_all(b"250 ok\r\n").await.unwrap();
                } else if line == "DATA" {
                    writer.write_all(b"354 go\r\n").await.unwrap();
                    loop {
                        let l = lines.next_line().await.unwrap().unwrap();
                        if l == "." {
                            break;
                        }
                        data.push_str(&l);
                        data.push('\n');
                    }
                    writer.write_all(b"250 queued\r\n").await.unwrap();
                } else if line == "QUIT" {
                    writer.write_all(b"221 bye\r\n").await.unwrap();
                    break;
                } else {
                    writer.write_all(b"250 ok\r\n").await.unwrap();
                }
            }
            data
        });
        let mut c = creds(Security::None);
        c.smtp_port = port;
        let message = build_message(
            "me@example.com",
            &["you@example.com".to_owned()],
            "Hi",
            "line one\n.stuffed line\n",
            &[],
        )
        .unwrap();
        smtp_send(
            &c,
            "me@example.com",
            &["you@example.com".to_owned()],
            &message,
        )
        .await
        .unwrap();
        let data = server.await.unwrap();
        assert!(data.contains("line one"));
        assert!(
            data.contains("..stuffed line"),
            "dot-stuffing applied: {data:?}"
        );
        assert!(data.contains("Subject: Hi"));
        assert!(!data.contains("\r"), "server saw CRLF-normalized lines");
    }

    #[tokio::test]
    async fn imap_connect_accepts_untagged_ok_greeting_with_capability() {
        // Real-world greeting (e.g. Hostinger): an untagged "* OK" with a
        // [CAPABILITY ...] bracket. `expect` used to compare only the first
        // 3 bytes, which can never match "* OK", so every IMAP connect
        // failed here with "mail server replied unexpectedly: * OK ...".
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = socket.into_split();
            let mut lines = BufReader::new(reader).lines();
            writer
                .write_all(
                    b"* OK [CAPABILITY IMAP4rev1 SASL-IR LOGIN-REFERRALS ID ENABLE IDLE LITERAL+ AUTH=PLAIN AUTH=LOGIN] Server ready.\r\n",
                )
                .await
                .unwrap();
            let login = lines.next_line().await.unwrap().unwrap();
            assert!(login.starts_with("a1 LOGIN"), "login line: {login}");
            writer.write_all(b"a1 OK Logged in\r\n").await.unwrap();
        });
        let mut c = creds(Security::None);
        c.imap_port = port;
        imap_connect(&c).await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn imap_connect_rejects_non_ok_greeting() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (_reader, mut writer) = socket.into_split();
            writer
                .write_all(b"* BYE Server unavailable\r\n")
                .await
                .unwrap();
        });
        let mut c = creds(Security::None);
        c.imap_port = port;
        assert!(imap_connect(&c).await.is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn imap_connect_tolerates_untagged_lines_after_login() {
        // Hostinger/Dovecot answer LOGIN with untagged FLAGS/EXISTS/RECENT
        // and untagged OK lines before the tagged "a1 OK"; reading only the
        // first line used to fail with
        // "mail server replied unexpectedly: * FLAGS (...)".
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = socket.into_split();
            let mut lines = BufReader::new(reader).lines();
            writer
                .write_all(b"* OK [CAPABILITY IMAP4rev1 SASL-IR LOGIN-REFERRALS ID ENABLE IDLE LITERAL+ AUTH=PLAIN AUTH=LOGIN] Server ready.\r\n")
                .await
                .unwrap();
            let login = lines.next_line().await.unwrap().unwrap();
            assert!(login.starts_with("a1 LOGIN"), "login line: {login}");
            writer
                .write_all(
                    b"* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)\r\n\
                      * OK [PERMANENTFLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft \\*)] Flags permitted.\r\n\
                      * 3 EXISTS\r\n\
                      * 0 RECENT\r\n\
                      a1 OK [CAPABILITY IMAP4rev1] LOGIN completed\r\n",
                )
                .await
                .unwrap();
        });
        let mut c = creds(Security::None);
        c.imap_port = port;
        imap_connect(&c).await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn imap_list_tolerates_untagged_select_and_search() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = socket.into_split();
            let mut lines = BufReader::new(reader).lines();
            writer
                .write_all(b"* OK [CAPABILITY IMAP4rev1 AUTH=LOGIN] Server ready.\r\n")
                .await
                .unwrap();
            assert!(
                lines
                    .next_line()
                    .await
                    .unwrap()
                    .unwrap()
                    .starts_with("a1 LOGIN")
            );
            writer
                .write_all(
                    b"* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)\r\n\
                      * 2 EXISTS\r\n\
                      * 0 RECENT\r\n\
                      a1 OK LOGIN completed\r\n",
                )
                .await
                .unwrap();
            assert!(
                lines
                    .next_line()
                    .await
                    .unwrap()
                    .unwrap()
                    .starts_with("a2 SELECT")
            );
            writer
                .write_all(
                    b"* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)\r\n\
                      * 2 EXISTS\r\n\
                      * 0 RECENT\r\n\
                      * OK [UNSEEN 1] First unseen\r\n\
                      * OK [UIDVALIDITY 1] UIDs valid\r\n\
                      a2 OK [READ-WRITE] SELECT completed\r\n",
                )
                .await
                .unwrap();
            assert!(
                lines
                    .next_line()
                    .await
                    .unwrap()
                    .unwrap()
                    .starts_with("a3 UID SEARCH")
            );
            writer
                .write_all(b"* SEARCH 1 2\r\na3 OK SEARCH completed\r\n")
                .await
                .unwrap();
            for uid in [2u32, 1] {
                assert!(
                    lines
                        .next_line()
                        .await
                        .unwrap()
                        .unwrap()
                        .starts_with("a4 UID FETCH")
                );
                let body = format!("Subject: Msg {uid}\r\nFrom: a@b.example\r\n\r\nbody {uid}\r\n");
                writer
                    .write_all(
                        format!(
                            "* {uid} FETCH (FLAGS (\\Seen) UID {uid} BODY[] {{{}}}\r\n{}\r\n)\r\na4 OK FETCH completed\r\n",
                            body.len(),
                            body
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
            let _ = lines.next_line().await; // a9 LOGOUT
        });
        let mut c = creds(Security::None);
        c.imap_port = port;
        let msgs = imap_list(&c, "INBOX", 10).await.unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].uid, 2, "newest first");
        assert_eq!(msgs[0].subject, "Msg 2");
        assert_eq!(msgs[0].flags, vec!["Seen"]);
        server.await.unwrap();
    }

    #[test]
    fn account_provider_labels() {
        assert_eq!(account_provider(None).unwrap(), "mail");
        assert_eq!(account_provider(Some("   ")).unwrap(), "mail");
        assert_eq!(account_provider(Some("work")).unwrap(), "mail.work");
        assert_eq!(account_provider(Some("shop-2")).unwrap(), "mail.shop-2");
        assert!(account_provider(Some("Work")).is_err());
        assert!(account_provider(Some("a b")).is_err());
        assert!(account_provider(Some("a.b")).is_err());
        assert!(account_provider(Some(&"x".repeat(25))).is_err());
    }

    #[test]
    fn multi_account_store_roundtrip() {
        let store = crate::store::Store::open_in_memory(384).unwrap();
        let user = store
            .create_user("mail-multi", "m@example.example", "M")
            .unwrap();
        let secrets =
            UserSecrets::with_master_key(store, [7u8; crate::user_secrets::KEY_BYTES]).unwrap();
        let mut c = creds(Security::None);
        store_credentials(&secrets, &user.id, None, &c).unwrap();
        c.username = "work@example.com".into();
        store_credentials(&secrets, &user.id, Some("work"), &c).unwrap();
        assert_eq!(
            load_credentials(&secrets, &user.id, None)
                .unwrap()
                .unwrap()
                .username,
            "me@example.com"
        );
        assert_eq!(
            load_credentials(&secrets, &user.id, Some("work"))
                .unwrap()
                .unwrap()
                .username,
            "work@example.com"
        );
        let accounts = list_accounts(&secrets, &user.id).unwrap();
        assert_eq!(accounts.len(), 2);
        assert!(
            accounts
                .iter()
                .any(|a| a.account.is_empty() && a.username == "me@example.com")
        );
        assert!(accounts.iter().any(|a| a.account == "work"));
        assert!(delete_credentials(&secrets, &user.id, Some("work")).unwrap());
        assert_eq!(list_accounts(&secrets, &user.id).unwrap().len(), 1);
        assert!(
            load_credentials(&secrets, &user.id, Some("work"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn signature_is_appended_once_and_capped() {
        let mut c = creds(Security::None);
        assert_eq!(body_with_signature(&c, "Hi".into()).unwrap(), "Hi");
        c.signature = Some("Jane — Acme".into());
        assert_eq!(
            body_with_signature(&c, "Hi".into()).unwrap(),
            "Hi\n\n-- \nJane — Acme"
        );
        c.signature = Some("x".repeat(MAX_SIGNATURE_CHARS + 1));
        assert!(validate_credentials(&c).is_err());
    }

    #[test]
    fn parses_list_names_and_flags() {
        assert_eq!(
            parse_list_name("* LIST (\\HasNoChildren) \"/\" \"INBOX\""),
            Some("INBOX".to_owned())
        );
        assert_eq!(
            parse_list_name("* LIST (\\Noselect) \"/\" \"Sent Items\""),
            Some("Sent Items".to_owned())
        );
        assert_eq!(parse_list_name("* LIST (\\HasNoChildren) \"/\" \"\""), None);
        assert!(validate_flag("\\Seen").is_ok());
        assert!(validate_flag("\\Flagged").is_ok());
        assert!(validate_flag("Seen").is_err());
        assert!(validate_flag("\\Seen); DROP").is_err());
    }

    #[test]
    fn parses_multipart_message_with_attachment() {
        let pdf = b"%PDF-1.4 fake";
        let raw = format!(
            "From: Alice <alice@example.com>\r\nSubject: Doc\r\nContent-Type: multipart/mixed; boundary=\"bnd\"\r\n\r\n\
             --bnd\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHello there\r\n\
             --bnd\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=\"doc.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n\
             --bnd--\r\n",
            base64::engine::general_purpose::STANDARD.encode(pdf)
        );
        let msg = parse_message(7, vec!["\\Seen".into()], &raw);
        assert_eq!(msg.subject, "Doc");
        assert_eq!(msg.body, "Hello there");
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0].name, "doc.pdf");
        assert_eq!(msg.attachments[0].content_type, "application/pdf");
        assert_eq!(msg.attachments[0].size, pdf.len());
        assert_eq!(
            msg.attachments[0].data_base64.as_deref(),
            Some(
                base64::engine::general_purpose::STANDARD
                    .encode(pdf)
                    .as_str()
            )
        );
    }

    #[tokio::test]
    async fn imap_management_commands_against_fake_server() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            // Five connections: folders, search, store, move, delete.
            for _ in 0..5 {
                let (socket, _) = listener.accept().await.unwrap();
                let (reader, mut writer) = socket.into_split();
                let mut lines = BufReader::new(reader).lines();
                writer
                    .write_all(b"* OK [CAPABILITY IMAP4rev1 UIDPLUS MOVE AUTH=LOGIN] ready.\r\n")
                    .await
                    .unwrap();
                let login = lines.next_line().await.unwrap().unwrap();
                assert!(login.starts_with("a1 LOGIN"), "{login}");
                writer
                    .write_all(
                        b"* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)\r\na1 OK ok\r\n",
                    )
                    .await
                    .unwrap();
                let cmd = lines.next_line().await.unwrap().unwrap();
                if cmd.starts_with("a2 LIST") {
                    writer
                        .write_all(b"* LIST (\\HasNoChildren) \"/\" \"INBOX\"\r\n* LIST (\\HasNoChildren) \"/\" \"Archive\"\r\na2 OK done\r\n")
                        .await
                        .unwrap();
                } else {
                    assert!(cmd.starts_with("a2 SELECT"), "{cmd}");
                    writer
                        .write_all(b"* 2 EXISTS\r\na2 OK [READ-WRITE] selected\r\n")
                        .await
                        .unwrap();
                    let cmd = lines.next_line().await.unwrap().unwrap();
                    if cmd.starts_with("a3 UID SEARCH OR") {
                        writer
                            .write_all(b"* SEARCH 5 9\r\na3 OK done\r\n")
                            .await
                            .unwrap();
                        for uid in [9u32, 5] {
                            let fetch = lines.next_line().await.unwrap().unwrap();
                            assert!(fetch.starts_with(&format!("a4 UID FETCH {uid}")), "{fetch}");
                            let body = format!("Subject: Hit {uid}\r\n\r\nbody\r\n");
                            writer
                                .write_all(
                                    format!(
                                        "* {uid} FETCH (FLAGS (\\Seen) UID {uid} BODY[] {{{}}}\r\n{}\r\n)\r\na4 OK done\r\n",
                                        body.len(),
                                        body
                                    )
                                    .as_bytes(),
                                )
                                .await
                                .unwrap();
                        }
                    } else if cmd.starts_with("a4 UID STORE") {
                        writer.write_all(b"a4 OK stored\r\n").await.unwrap();
                        if cmd.contains("\\Deleted") {
                            let exp = lines.next_line().await.unwrap().unwrap();
                            assert!(exp.starts_with("a5 UID EXPUNGE"), "{exp}");
                            writer
                                .write_all(b"* 1 EXPUNGE\r\na5 OK expunged\r\n")
                                .await
                                .unwrap();
                        }
                    } else if cmd.starts_with("a4 UID MOVE") {
                        writer
                            .write_all(b"* 1 EXPUNGE\r\na4 OK moved\r\n")
                            .await
                            .unwrap();
                    } else {
                        panic!("unexpected command: {cmd}");
                    }
                }
                let _ = lines.next_line().await; // a9 LOGOUT
            }
        });
        let mut c = creds(Security::None);
        c.imap_port = port;

        let folders = imap_folders(&c).await.unwrap();
        assert_eq!(folders, vec!["INBOX".to_owned(), "Archive".to_owned()]);

        let hits = imap_search(&c, "INBOX", "invoice", 10).await.unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].uid, 9);
        assert_eq!(hits[0].subject, "Hit 9");

        imap_store_flags(&c, "INBOX", 9, true, &["\\Flagged".to_owned()])
            .await
            .unwrap();
        imap_move(&c, "INBOX", 9, "Archive").await.unwrap();
        imap_delete(&c, "INBOX", 5).await.unwrap();
        assert!(
            imap_store_flags(&c, "INBOX", 9, true, &["bad flag".to_owned()])
                .await
                .is_err()
        );
        server.await.unwrap();
    }
}
