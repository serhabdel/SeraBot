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

use std::collections::HashMap;
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
use crate::mail_ux;
use crate::maker::ArtifactStore;
use crate::policy::Policy;
use crate::store::Store;
use crate::user_secrets::UserSecrets;

pub const MAIL_PROVIDER: &str = "mail";
/// Prefix of the secret-store provider ids for named mail accounts.
pub const MAIL_ACCOUNT_PREFIX: &str = "mail.";
const MAIL_SIG_PROVIDER: &str = "mail_sig";
const MAX_ACCOUNT_LABEL_CHARS: usize = 24;
pub const MAIL_SEND_TOOL: &str = "mail_send";
pub const MAIL_REPLY_TOOL: &str = "mail_reply";
pub const MAIL_DRAFT_TOOL: &str = "mail_draft";
pub const MAIL_TRIAGE_TOOL: &str = "mail_triage";
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
pub const MAX_SIGNATURE_HTML_CHARS: usize = 65536;
const MAX_INLINE_ATTACHMENT_BYTES: usize = 512 * 1024;
const MAX_INLINE_ATTACHMENTS_TOTAL: usize = 2 * 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(30);
const DATA_IO_TIMEOUT: Duration = Duration::from_secs(60);
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

/// Extended per-account signature settings (HTML signature + inline logo hash),
/// stored alongside `MailCredentials` under `mail_sig` / `mail_sig.<label>`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MailSignatureConfig {
    #[serde(default)]
    pub signature: String,
    #[serde(default)]
    pub signature_html: Option<String>,
    #[serde(default)]
    pub signature_logo_hash: Option<String>,
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
        validate_plain_signature(sig, true)?;
    }
    Ok(())
}

fn validate_plain_signature(sig: &str, reject_empty: bool) -> Result<()> {
    let bad = (reject_empty && sig.is_empty())
        || sig.len() > MAX_SIGNATURE_CHARS
        || sig
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t');
    if bad {
        return Err(SeraError::Validation(
            "signature must be 1-4096 chars without control characters".into(),
        ));
    }
    Ok(())
}

pub fn validate_signature_config(cfg: &MailSignatureConfig) -> Result<()> {
    validate_plain_signature(&cfg.signature, false)?;
    if let Some(html) = &cfg.signature_html
        && (html.len() > MAX_SIGNATURE_HTML_CHARS || html.contains('\0'))
    {
        return Err(SeraError::Validation(format!(
            "signature_html must be at most {MAX_SIGNATURE_HTML_CHARS} chars without NUL bytes"
        )));
    }
    if let Some(hash) = &cfg.signature_logo_hash
        && (hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(SeraError::Validation(
            "signature_logo_hash must be a 64-char hex artifact hash".into(),
        ));
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

fn signature_provider(account: Option<&str>) -> Result<String> {
    let base = account_provider(account)?;
    if base == MAIL_PROVIDER {
        Ok(MAIL_SIG_PROVIDER.to_owned())
    } else {
        Ok(base.replacen(MAIL_PROVIDER, MAIL_SIG_PROVIDER, 1))
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

pub fn store_signature_config(
    secrets: &UserSecrets,
    user_id: &str,
    account: Option<&str>,
    cfg: &MailSignatureConfig,
) -> Result<()> {
    validate_signature_config(cfg)?;
    let mut creds = load_credentials(secrets, user_id, account)?
        .ok_or_else(|| SeraError::Validation("mail is not connected; connect it first".into()))?;
    creds.signature = if cfg.signature.trim().is_empty() {
        None
    } else {
        Some(cfg.signature.clone())
    };
    store_credentials(secrets, user_id, account, &creds)?;
    let normalized = MailSignatureConfig {
        signature: cfg.signature.clone(),
        signature_html: cfg
            .signature_html
            .as_ref()
            .filter(|s| !s.trim().is_empty())
            .cloned(),
        signature_logo_hash: cfg
            .signature_logo_hash
            .as_ref()
            .filter(|s| !s.trim().is_empty())
            .cloned(),
    };
    let json = serde_json::to_string(&normalized)
        .map_err(|e| SeraError::Internal(format!("mail sig serialise: {e}")))?;
    secrets.set(user_id, &signature_provider(account)?, &json)
}

pub fn load_signature_config(
    secrets: &UserSecrets,
    user_id: &str,
    account: Option<&str>,
) -> Result<MailSignatureConfig> {
    let Some(raw) = secrets.get(user_id, &signature_provider(account)?)? else {
        let plain = load_credentials(secrets, user_id, account)?
            .and_then(|c| c.signature)
            .unwrap_or_default();
        return Ok(MailSignatureConfig {
            signature: plain,
            signature_html: None,
            signature_logo_hash: None,
        });
    };
    let mut cfg: MailSignatureConfig = serde_json::from_str(raw.expose_secret())
        .map_err(|e| SeraError::Internal(format!("stored mail signature is invalid: {e}")))?;
    if cfg.signature.is_empty()
        && let Some(plain) = load_credentials(secrets, user_id, account)?.and_then(|c| c.signature)
    {
        cfg.signature = plain;
    }
    Ok(cfg)
}

pub fn delete_credentials(
    secrets: &UserSecrets,
    user_id: &str,
    account: Option<&str>,
) -> Result<bool> {
    let _ = secrets.delete(user_id, &signature_provider(account)?);
    secrets.delete(user_id, &account_provider(account)?)
}

/// One connected mailbox for status listings; `account` is empty for the
/// default mailbox. Never carries the password.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct MailAccountInfo {
    pub account: String,
    pub username: String,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_security: Security,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_security: Security,
    pub from: Option<String>,
    pub signature: Option<String>,
    pub signature_html: Option<String>,
    pub signature_logo_hash: Option<String>,
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
            let sig_cfg = load_signature_config(secrets, user_id, lookup).unwrap_or_default();
            let signature = if !sig_cfg.signature.trim().is_empty() {
                Some(sig_cfg.signature)
            } else {
                creds.signature
            };
            out.push(MailAccountInfo {
                account: account.to_owned(),
                username: creds.username,
                smtp_host: creds.smtp_host,
                smtp_port: creds.smtp_port,
                smtp_security: creds.smtp_security,
                imap_host: creds.imap_host,
                imap_port: creds.imap_port,
                imap_security: creds.imap_security,
                from: creds.from,
                signature,
                signature_html: sig_cfg.signature_html,
                signature_logo_hash: sig_cfg.signature_logo_hash,
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

fn escape_html_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

fn plain_to_basic_html(plain: &str) -> String {
    escape_html_text(plain)
        .replace("\r\n", "\n")
        .replace('\n', "<br>\n")
}

/// Build the final HTML body (with signature HTML / plain signature / inline
/// logo reference) if any HTML component is present.
pub fn html_with_signature(
    creds: &MailCredentials,
    sig_cfg: &MailSignatureConfig,
    body_plain: &str,
    body_html: Option<&str>,
    has_inline_logo: bool,
) -> Result<Option<String>> {
    let sig_html = sig_cfg
        .signature_html
        .as_deref()
        .filter(|s| !s.trim().is_empty());
    let body_html_clean = body_html.filter(|s| !s.trim().is_empty());
    if body_html_clean.is_none() && sig_html.is_none() && !has_inline_logo {
        return Ok(None);
    }
    let mut html = match body_html_clean {
        Some(h) => h.to_owned(),
        None => format!("<div>{}</div>", plain_to_basic_html(body_plain)),
    };
    if let Some(sh) = sig_html {
        html.push_str("\n<br><div class=\"serabot-signature\">");
        html.push_str(sh);
        html.push_str("</div>");
    } else if let Some(sig) = creds.signature.as_deref().filter(|s| !s.trim().is_empty()) {
        html.push_str("\n<br><div class=\"serabot-signature\">-- <br>");
        html.push_str(&plain_to_basic_html(sig));
        html.push_str("</div>");
    }
    if has_inline_logo && !html.contains("cid:signature-logo@serabot") {
        html.push_str(
            "\n<br><img src=\"cid:signature-logo@serabot\" alt=\"Signature logo\" style=\"max-height:80px;\">",
        );
    }
    if html.chars().count() > MAX_BODY_CHARS {
        return Err(SeraError::Validation(format!(
            "HTML body plus signature must stay under {MAX_BODY_CHARS} characters"
        )));
    }
    Ok(Some(html))
}

// ---------------------------------------------------------------- MIME ----

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    pub name: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineAttachment {
    pub content_id: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

/// Find `<img ... src="data:image/(png|jpeg|gif|webp);base64,...">` tags,
/// decode each image (up to `MAX_INLINE_ATTACHMENT_BYTES` per image and
/// `MAX_INLINE_ATTACHMENTS_TOTAL` total), and rewrite `src` to
/// `cid:inline-{i}@serabot`.
pub fn extract_inline_data_images(html: &str) -> (String, Vec<InlineAttachment>) {
    let mut out = String::with_capacity(html.len());
    let mut attachments = Vec::new();
    let mut total_bytes: usize = 0;
    let mut pos = 0;
    let bytes = html.as_bytes();

    while pos < bytes.len() {
        let Some(rel_lt) = html[pos..].find('<') else {
            out.push_str(&html[pos..]);
            break;
        };
        let tag_start = pos + rel_lt;
        out.push_str(&html[pos..tag_start]);

        // Check if this is an `<img` tag (followed by whitespace, `/`, or `>`).
        let is_img = html.get(tag_start + 1..tag_start + 4).is_some_and(|s| {
            s.eq_ignore_ascii_case("img")
                && html[tag_start + 4..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_whitespace() || c == '/' || c == '>')
        });

        // Find end of tag, respecting quoted attribute values.
        let mut i = tag_start + 1;
        let mut quote: Option<u8> = None;
        while i < bytes.len() {
            let b = bytes[i];
            if let Some(q) = quote {
                if b == q {
                    quote = None;
                }
            } else if b == b'"' || b == b'\'' {
                quote = Some(b);
            } else if b == b'>' {
                i += 1;
                break;
            }
            i += 1;
        }
        let tag_end = i;
        let tag_str = &html[tag_start..tag_end];

        if !is_img {
            out.push_str(tag_str);
            pos = tag_end;
            continue;
        }

        if let Some((val_start, val_end, rewritten_cid, inline_att)) =
            try_extract_img_data_src(tag_str, attachments.len(), total_bytes)
        {
            total_bytes += inline_att.bytes.len();
            attachments.push(inline_att);
            out.push_str(&tag_str[..val_start]);
            out.push_str("cid:");
            out.push_str(&rewritten_cid);
            out.push_str(&tag_str[val_end..]);
        } else {
            out.push_str(tag_str);
        }
        pos = tag_end;
    }

    (out, attachments)
}

fn try_extract_img_data_src(
    tag_str: &str,
    next_index: usize,
    total_bytes: usize,
) -> Option<(usize, usize, String, InlineAttachment)> {
    let bytes = tag_str.as_bytes();
    let mut i = 4; // skip "<img"
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'>' || bytes[i] == b'/' {
            if i < bytes.len() && bytes[i] == b'/' {
                i += 1;
                continue;
            }
            break;
        }
        let name_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'>'
        {
            i += 1;
        }
        let attr_name = &tag_str[name_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let (val_start, val_end) = if bytes[i] == b'"' || bytes[i] == b'\'' {
            let q = bytes[i];
            i += 1;
            let vs = i;
            while i < bytes.len() && bytes[i] != q {
                i += 1;
            }
            let ve = i;
            if i < bytes.len() {
                i += 1;
            }
            (vs, ve)
        } else {
            let vs = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                i += 1;
            }
            (vs, i)
        };

        if attr_name.eq_ignore_ascii_case("src") {
            let raw_val = tag_str[val_start..val_end].trim();
            let prefix_len = "data:image/".len();
            if raw_val.len() <= prefix_len
                || !raw_val[..prefix_len].eq_ignore_ascii_case("data:image/")
            {
                return None;
            }
            let rest = &raw_val[prefix_len..];
            let (mime_sub, payload) = rest.split_once(',')?;
            let (subtype_raw, encoding_raw) = mime_sub.split_once(';')?;
            if !encoding_raw.trim().eq_ignore_ascii_case("base64") {
                return None;
            }
            let content_type = match subtype_raw.trim().to_ascii_lowercase().as_str() {
                "png" => "image/png",
                "jpeg" => "image/jpeg",
                "gif" => "image/gif",
                "webp" => "image/webp",
                _ => return None,
            };
            let compact_b64: String = payload
                .chars()
                .filter(|c| !c.is_ascii_whitespace())
                .collect();
            if compact_b64.is_empty() {
                return None;
            }
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(compact_b64.as_bytes())
                .ok()?;
            if decoded.is_empty()
                || decoded.len() > MAX_INLINE_ATTACHMENT_BYTES
                || total_bytes.saturating_add(decoded.len()) > MAX_INLINE_ATTACHMENTS_TOTAL
            {
                return None;
            }
            let content_id = format!("inline-{next_index}@serabot");
            return Some((
                val_start,
                val_end,
                content_id.clone(),
                InlineAttachment {
                    content_id,
                    content_type: content_type.to_owned(),
                    bytes: decoded,
                },
            ));
        }
    }
    None
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
    build_message_full(from, to, &[], subject, body, None, None, attachments)
}

fn validate_recipient_list(addrs: &[String]) -> Result<()> {
    for addr in addrs {
        if addr.is_empty()
            || addr.len() > 254
            || addr.chars().any(|c| c == '\r' || c == '\n' || c == ',')
        {
            return Err(SeraError::Validation(format!("invalid recipient {addr:?}")));
        }
    }
    Ok(())
}

fn write_alternative_body(out: &mut String, body_text: &str, body_html: &str) {
    let alt_bnd = format!("serabot-alt-{}", uuid::Uuid::now_v7().simple());
    out.push_str(&format!(
        "Content-Type: multipart/alternative; boundary=\"{alt_bnd}\"\r\n\r\n"
    ));
    out.push_str(&format!("--{alt_bnd}\r\n"));
    out.push_str(
        "Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n",
    );
    out.push_str(body_text);
    if !body_text.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("\r\n--{alt_bnd}\r\n"));
    out.push_str(
        "Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n",
    );
    out.push_str(body_html);
    if !body_html.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("\r\n--{alt_bnd}--\r\n"));
}

fn write_related_or_alternative(
    out: &mut String,
    body_text: &str,
    body_html: &str,
    inline_parts: &[InlineAttachment],
) {
    if !inline_parts.is_empty() {
        let rel_bnd = format!("serabot-rel-{}", uuid::Uuid::now_v7().simple());
        out.push_str(&format!(
            "Content-Type: multipart/related; boundary=\"{rel_bnd}\"\r\n\r\n"
        ));
        out.push_str(&format!("--{rel_bnd}\r\n"));
        write_alternative_body(out, body_text, body_html);
        for part in inline_parts {
            out.push_str(&format!("--{rel_bnd}\r\n"));
            let ext = match part.content_type.as_str() {
                "image/png" => "png",
                "image/jpeg" => "jpg",
                "image/gif" => "gif",
                "image/webp" => "webp",
                _ => "bin",
            };
            let cid = part
                .content_id
                .trim()
                .trim_start_matches('<')
                .trim_end_matches('>');
            let fname_stem = cid.split('@').next().unwrap_or("inline");
            out.push_str(&format!("Content-Type: {}\r\n", part.content_type));
            out.push_str("Content-Transfer-Encoding: base64\r\n");
            out.push_str(&format!("Content-ID: <{cid}>\r\n"));
            out.push_str(&format!(
                "Content-Disposition: inline; filename=\"{}.{ext}\"\r\n\r\n",
                clean_header(fname_stem)
            ));
            let encoded = base64::engine::general_purpose::STANDARD.encode(&part.bytes);
            for chunk in encoded.as_bytes().chunks(76) {
                out.push_str(std::str::from_utf8(chunk).unwrap_or(""));
                out.push_str("\r\n");
            }
        }
        out.push_str(&format!("--{rel_bnd}--\r\n"));
    } else {
        write_alternative_body(out, body_text, body_html);
    }
}

/// Build a MIME message supporting `cc`, `body_html`, `In-Reply-To`, `References`,
/// inline `cid:` parts, and regular attachments (`multipart/mixed` ->
/// `multipart/related` -> `multipart/alternative`).
/// `Bcc` recipients are passed to `smtp_send` envelope `RCPT TO` and never written to headers.
#[allow(clippy::too_many_arguments)]
pub fn build_message_with_inline(
    from: &str,
    to: &[String],
    cc: &[String],
    subject: &str,
    body: &str,
    body_html: Option<&str>,
    in_reply_to: Option<&str>,
    references: Option<&str>,
    inline_parts: &[InlineAttachment],
    attachments: &[Attachment],
) -> Result<Vec<u8>> {
    if to.is_empty() || to.len() + cc.len() > MAX_RECIPIENTS {
        return Err(SeraError::Validation(format!(
            "1 to {MAX_RECIPIENTS} recipients are allowed"
        )));
    }
    validate_recipient_list(to)?;
    validate_recipient_list(cc)?;
    if subject.chars().count() > MAX_SUBJECT_CHARS {
        return Err(SeraError::Validation("subject is too long".into()));
    }
    if body.chars().count() > MAX_BODY_CHARS {
        return Err(SeraError::Validation("body is too long".into()));
    }
    if let Some(html) = body_html
        && html.chars().count() > MAX_BODY_CHARS
    {
        return Err(SeraError::Validation("HTML body is too long".into()));
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
    let inline_overhead: usize = inline_parts
        .iter()
        .map(|p| p.bytes.len() * 4 / 3 + 200)
        .sum();
    let total: usize = body.len()
        + body_html.map_or(0, str::len)
        + inline_overhead
        + attachments
            .iter()
            .map(|a| a.bytes.len() * 4 / 3 + 200)
            .sum::<usize>();
    if total > MAX_MESSAGE_BYTES {
        return Err(SeraError::Validation(
            "the whole message must stay under 25 MiB".into(),
        ));
    }
    let mut out = String::new();
    out.push_str(&format!("From: {}\r\n", clean_header(from)));
    out.push_str(&format!(
        "To: {}\r\n",
        to.iter()
            .map(|a| clean_header(a))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    if !cc.is_empty() {
        out.push_str(&format!(
            "Cc: {}\r\n",
            cc.iter()
                .map(|a| clean_header(a))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out.push_str(&format!("Subject: {}\r\n", encode_header(subject)));
    out.push_str(&format!("Date: {}\r\n", chrono::Utc::now().to_rfc2822()));
    out.push_str(&format!(
        "Message-ID: <{}@serabot>\r\n",
        uuid::Uuid::now_v7().simple()
    ));
    if let Some(irt) = in_reply_to.map(str::trim).filter(|s| !s.is_empty()) {
        out.push_str(&format!("In-Reply-To: {}\r\n", clean_header(irt)));
    }
    if let Some(refs) = references.map(str::trim).filter(|s| !s.is_empty()) {
        out.push_str(&format!("References: {}\r\n", clean_header(refs)));
    }
    out.push_str("MIME-Version: 1.0\r\n");

    if attachments.is_empty() {
        if let Some(html) = body_html {
            write_related_or_alternative(&mut out, body, html, inline_parts);
        } else {
            out.push_str(
                "Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n",
            );
            out.push_str(body);
            if !body.ends_with('\n') {
                out.push('\n');
            }
        }
    } else {
        let boundary = format!("serabot-{}", uuid::Uuid::now_v7().simple());
        out.push_str(&format!(
            "Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n"
        ));
        out.push_str(&format!("--{boundary}\r\n"));
        if let Some(html) = body_html {
            write_related_or_alternative(&mut out, body, html, inline_parts);
            out.push_str(&format!("\r\n--{boundary}\r\n"));
        } else {
            out.push_str(
                "Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n",
            );
            out.push_str(body);
            if !body.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&format!("\r\n--{boundary}\r\n"));
        }
        for (idx, att) in attachments.iter().enumerate() {
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
            if idx + 1 < attachments.len() {
                out.push_str(&format!("--{boundary}\r\n"));
            }
        }
        out.push_str(&format!("--{boundary}--\r\n"));
    }
    Ok(out.into_bytes())
}

/// Build a MIME message supporting `cc`, `body_html`, inline `cid:signature-logo@serabot`,
/// and regular attachments (`multipart/mixed` -> `multipart/related` -> `multipart/alternative`).
/// `Bcc` recipients are passed to `smtp_send` envelope `RCPT TO` and never written to headers.
#[allow(clippy::too_many_arguments)]
pub fn build_message_full(
    from: &str,
    to: &[String],
    cc: &[String],
    subject: &str,
    body: &str,
    body_html: Option<&str>,
    inline_logo: Option<(&str, &[u8])>,
    attachments: &[Attachment],
) -> Result<Vec<u8>> {
    let inline_parts: Vec<InlineAttachment> = inline_logo
        .map(|(mime, bytes)| InlineAttachment {
            content_id: "signature-logo@serabot".to_owned(),
            content_type: mime.to_owned(),
            bytes: bytes.to_vec(),
        })
        .into_iter()
        .collect();
    build_message_with_inline(
        from,
        to,
        cc,
        subject,
        body,
        body_html,
        None,
        None,
        &inline_parts,
        attachments,
    )
}

pub struct SendAccountMailParams<'a> {
    pub user_id: &'a str,
    pub account: Option<&'a str>,
    pub to: &'a [String],
    pub cc: &'a [String],
    pub bcc: &'a [String],
    pub subject: &'a str,
    pub body: &'a str,
    pub body_html: Option<&'a str>,
    pub in_reply_to: Option<&'a str>,
    pub references: Option<&'a str>,
    pub attachment_hashes: &'a [String],
}

#[derive(Clone, Debug)]
pub struct SendMailResult {
    pub to_count: usize,
    pub attachments: usize,
    pub bytes: usize,
}

fn scan_mail_text(store: &Store, policy: &Policy, user_id: &str, text: &str) -> Result<()> {
    if !policy.is_allowed(text) {
        let _ = store.record_audit(
            user_id,
            "policy.blocked_egress",
            None,
            None,
            None,
            None,
            Some(policy.version()),
            None,
        );
        return Err(SeraError::PolicyViolation);
    }
    Ok(())
}

pub fn is_safe_inline_image_mime(mime: &str) -> bool {
    matches!(
        mime.to_ascii_lowercase().as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}

pub(crate) fn inline_image_mime_for_filename(filename: &str) -> &'static str {
    let ext = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => crate::util::mime_for_filename(filename),
    }
}

fn load_inline_logo_attachment(
    store: &Store,
    artifacts: &ArtifactStore,
    user_id: &str,
    sig_cfg: &MailSignatureConfig,
) -> Result<Option<InlineAttachment>> {
    let Some(logo_hash) = sig_cfg
        .signature_logo_hash
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    else {
        return Ok(None);
    };
    if !store.user_owns_artifact(user_id, logo_hash)? {
        return Err(SeraError::Forbidden("artifact".into()));
    }
    let stored = artifacts.load(logo_hash)?;
    let mime = inline_image_mime_for_filename(&stored.filename).to_owned();
    if !is_safe_inline_image_mime(&mime) {
        return Err(SeraError::Validation(
            "signature logo must be png, jpeg, gif, or webp".into(),
        ));
    }
    if stored.bytes.is_empty() || stored.bytes.len() > MAX_INLINE_ATTACHMENT_BYTES {
        return Err(SeraError::Validation(
            "signature logo must be 1 byte to 512 KiB".into(),
        ));
    }
    Ok(Some(InlineAttachment {
        content_id: "signature-logo@serabot".to_owned(),
        content_type: mime,
        bytes: stored.bytes,
    }))
}

/// Extract unique 64-hex-char hashes from `cid:sig-img-<64hex>@serabot` references in HTML.
fn extract_sig_img_cid_hashes(html: &str) -> Vec<String> {
    const PREFIX: &str = "cid:sig-img-";
    const SUFFIX: &str = "@serabot";
    let mut hashes = Vec::new();
    let mut rest = html;
    while let Some(pos) = rest.find(PREFIX) {
        let after_prefix = &rest[pos + PREFIX.len()..];
        if after_prefix.len() >= 64 + SUFFIX.len() {
            let candidate = &after_prefix[..64];
            let tail = &after_prefix[64..];
            if tail.starts_with(SUFFIX) && candidate.chars().all(|c| c.is_ascii_hexdigit()) {
                let hash = candidate.to_owned();
                if !hashes.contains(&hash) {
                    hashes.push(hash);
                    if hashes.len() >= MAX_ATTACHMENTS {
                        break;
                    }
                }
                rest = &tail[SUFFIX.len()..];
                continue;
            }
        }
        rest = after_prefix;
    }
    hashes
}

fn load_inline_signature_images(
    store: &Store,
    artifacts: &ArtifactStore,
    user_id: &str,
    sig_cfg: &MailSignatureConfig,
) -> Result<Vec<InlineAttachment>> {
    let Some(sig_html) = sig_cfg.signature_html.as_deref() else {
        return Ok(Vec::new());
    };
    let hashes = extract_sig_img_cid_hashes(sig_html);
    let mut out = Vec::with_capacity(hashes.len());
    for hash in hashes {
        if !store.user_owns_artifact(user_id, &hash)? {
            return Err(SeraError::Forbidden("artifact".into()));
        }
        let stored = artifacts.load(&hash)?;
        let mime = inline_image_mime_for_filename(&stored.filename).to_owned();
        if !is_safe_inline_image_mime(&mime) {
            return Err(SeraError::Validation(
                "signature image must be png, jpeg, gif, or webp".into(),
            ));
        }
        if stored.bytes.is_empty() || stored.bytes.len() > MAX_INLINE_ATTACHMENT_BYTES {
            return Err(SeraError::Validation(
                "signature image must be 1 byte to 512 KiB".into(),
            ));
        }
        out.push(InlineAttachment {
            content_id: format!("sig-img-{hash}@serabot"),
            content_type: mime,
            bytes: stored.bytes,
        });
    }
    Ok(out)
}

/// Validate policy, load attachments & signature logo from the user's artifact
/// store, build the MIME message, and send via SMTP. Shared by `/v1/mail/send`
/// and the 30-second scheduled-mail background loop.
pub async fn send_account_mail(
    secrets: &UserSecrets,
    store: &Store,
    artifacts: &ArtifactStore,
    policy: &Policy,
    params: SendAccountMailParams<'_>,
) -> Result<SendMailResult> {
    let account = params.account.map(str::trim).filter(|s| !s.is_empty());
    account_provider(account)?;
    if params.to.is_empty() || params.to.len() + params.cc.len() + params.bcc.len() > MAX_RECIPIENTS
    {
        return Err(SeraError::Validation(format!(
            "1 to {MAX_RECIPIENTS} recipients are allowed"
        )));
    }
    validate_recipient_list(params.to)?;
    validate_recipient_list(params.cc)?;
    validate_recipient_list(params.bcc)?;

    scan_mail_text(store, policy, params.user_id, params.subject)?;
    scan_mail_text(store, policy, params.user_id, params.body)?;
    if let Some(html) = params.body_html {
        scan_mail_text(store, policy, params.user_id, html)?;
    }
    for rcpt in params
        .to
        .iter()
        .chain(params.cc.iter())
        .chain(params.bcc.iter())
    {
        scan_mail_text(store, policy, params.user_id, rcpt)?;
    }
    if params.attachment_hashes.len() > MAX_ATTACHMENTS {
        return Err(SeraError::Validation(format!(
            "at most {MAX_ATTACHMENTS} attachments are allowed"
        )));
    }
    let mut attachments = Vec::with_capacity(params.attachment_hashes.len());
    for hash in params.attachment_hashes {
        if !store.user_owns_artifact(params.user_id, hash)? {
            return Err(SeraError::Forbidden("artifact".into()));
        }
        let stored = artifacts.load(hash)?;
        if !policy.is_allowed(&stored.filename) {
            return Err(SeraError::PolicyViolation);
        }
        if stored.bytes.is_empty() || stored.bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(SeraError::Validation(
                "each attachment must be 1 byte to 20 MiB".into(),
            ));
        }
        let content_type = crate::util::mime_for_filename(&stored.filename).to_owned();
        attachments.push(Attachment {
            name: stored.filename,
            content_type,
            bytes: stored.bytes,
        });
    }

    let creds = require_creds(secrets, params.user_id, account)?;
    let sig_cfg = load_signature_config(secrets, params.user_id, account).unwrap_or_default();
    let inline_logo = load_inline_logo_attachment(store, artifacts, params.user_id, &sig_cfg)?;
    let sig_images = load_inline_signature_images(store, artifacts, params.user_id, &sig_cfg)?;

    let from = from_address(&creds);
    let final_plain = body_with_signature(&creds, params.body.to_owned())?;
    scan_mail_text(store, policy, params.user_id, &final_plain)?;
    let mut final_html = html_with_signature(
        &creds,
        &sig_cfg,
        params.body,
        params.body_html,
        inline_logo.is_some(),
    )?;
    if let Some(html) = &final_html {
        scan_mail_text(store, policy, params.user_id, html)?;
    }

    let mut inline_parts = Vec::new();
    if let Some(html) = final_html.take() {
        let (rewritten, extracted) = extract_inline_data_images(&html);
        inline_parts.extend(extracted);
        final_html = Some(rewritten);
    }
    if let Some(logo) = inline_logo {
        inline_parts.push(logo);
    }
    inline_parts.extend(sig_images);

    let msg = build_message_with_inline(
        &from,
        params.to,
        params.cc,
        params.subject,
        &final_plain,
        final_html.as_deref(),
        params.in_reply_to,
        params.references,
        &inline_parts,
        &attachments,
    )?;
    let mut all_rcpts = Vec::with_capacity(params.to.len() + params.cc.len() + params.bcc.len());
    all_rcpts.extend(params.to.iter().cloned());
    all_rcpts.extend(params.cc.iter().cloned());
    all_rcpts.extend(params.bcc.iter().cloned());
    smtp_send(&creds, &from, &all_rcpts, &msg).await?;
    Ok(SendMailResult {
        to_count: all_rcpts.len(),
        attachments: attachments.len(),
        bytes: msg.len(),
    })
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
        self.read_line_with_timeout(IO_TIMEOUT).await
    }

    async fn read_line_with_timeout(&mut self, timeout: Duration) -> Result<String> {
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
            let n = tokio::time::timeout(timeout, self.stream.read(&mut chunk))
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

    /// Write an IMAP literal (raw bytes) followed by the closing CRLF.
    async fn send_literal(&mut self, bytes: &[u8]) -> Result<()> {
        tokio::time::timeout(IO_TIMEOUT, async {
            self.stream.write_all(bytes).await?;
            self.stream.write_all(b"\r\n").await?;
            self.stream.flush().await
        })
        .await
        .map_err(|_| SeraError::External("mail server write timed out".into()))?
        .map_err(|e| SeraError::External(format!("mail server write: {e}")))?;
        Ok(())
    }

    /// Read a reply and require it to start with `want` (e.g. "250" for
    /// SMTP, "* OK" for an IMAP greeting, "a1 OK" for a tagged IMAP reply).
    /// The whole prefix is compared: an earlier version compared only the
    /// first 3 bytes, which can never match a 4+-byte `want` and so rejected
    /// every IMAP greeting ("* OK [CAPABILITY ...] Server ready.") and
    /// every tagged IMAP reply ("a1 OK ...").
    #[allow(dead_code)]
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

    /// Read a (possibly multi-line) SMTP reply (`220-...` / `250-...`) until
    /// the final line (`<code> ` or bare `<code>`) arrives.
    async fn read_smtp_reply(&mut self, expected_code: &str) -> Result<Vec<String>> {
        self.read_smtp_reply_with_timeout(expected_code, IO_TIMEOUT)
            .await
    }

    async fn read_smtp_reply_with_timeout(
        &mut self,
        expected_code: &str,
        timeout: Duration,
    ) -> Result<Vec<String>> {
        const MAX_SMTP_REPLY_LINES: usize = 128;
        let mut lines = Vec::new();
        loop {
            let line = self.read_line_with_timeout(timeout).await?;
            if !line.starts_with(expected_code) {
                return Err(SeraError::External(format!(
                    "mail server replied unexpectedly: {}",
                    clip(&line, 200)
                )));
            }
            let rest = &line[expected_code.len()..];
            let is_continuation = rest.starts_with('-');
            let is_final = rest.is_empty() || rest.starts_with(' ');
            if !is_continuation && !is_final {
                return Err(SeraError::External(format!(
                    "mail server replied unexpectedly: {}",
                    clip(&line, 200)
                )));
            }
            lines.push(line);
            if is_final {
                break;
            }
            if lines.len() >= MAX_SMTP_REPLY_LINES {
                return Err(SeraError::External(
                    "SMTP server reply exceeded maximum line count".into(),
                ));
            }
        }
        Ok(lines)
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

fn ehlo_supports_auth_plain(ehlo_lines: &[String]) -> bool {
    ehlo_lines.iter().any(|line| {
        let rest = line.get(4..).unwrap_or("").trim();
        let upper = rest.to_ascii_uppercase();
        if let Some(mechs) = upper
            .strip_prefix("AUTH ")
            .or_else(|| upper.strip_prefix("AUTH="))
        {
            mechs.split_ascii_whitespace().any(|m| m == "PLAIN")
        } else {
            false
        }
    })
}

async fn smtp_connect_and_ehlo(creds: &MailCredentials) -> Result<(Wire<MailStream>, Vec<String>)> {
    let host = creds.smtp_host.trim().to_owned();
    let mut wire = connect(&host, creds.smtp_port, creds.smtp_security).await?;
    wire.read_smtp_reply("220").await?;
    wire.send_line("EHLO serabot.local").await?;
    let mut ehlo_lines = wire.read_smtp_reply("250").await?;
    if creds.smtp_security == Security::StartTls {
        wire.send_line("STARTTLS").await?;
        wire.read_smtp_reply("220").await?;
        let tcp = match wire.stream {
            MailStream::Plain(tcp) => tcp,
            _ => return Err(SeraError::Internal("STARTTLS on a TLS stream".into())),
        };
        let connector = tls_connector()?;
        let domain = tokio_rustls::rustls::pki_types::ServerName::try_from(host)
            .map_err(|_| SeraError::Validation("invalid TLS host name".into()))?;
        let tls = tokio::time::timeout(IO_TIMEOUT, connector.connect(domain, tcp))
            .await
            .map_err(|_| SeraError::External("TLS handshake timed out".into()))?
            .map_err(|e| SeraError::External(format!("TLS handshake: {e}")))?;
        wire.stream = MailStream::Tls(Box::new(tls));
        wire.send_line("EHLO serabot.local").await?;
        ehlo_lines = wire.read_smtp_reply("250").await?;
    }
    Ok((wire, ehlo_lines))
}

async fn smtp_authenticate(
    wire: &mut Wire<MailStream>,
    creds: &MailCredentials,
    ehlo_lines: &[String],
) -> Result<()> {
    if ehlo_supports_auth_plain(ehlo_lines) {
        let mut plain_payload = Vec::with_capacity(2 + creds.username.len() + creds.password.len());
        plain_payload.push(0);
        plain_payload.extend_from_slice(creds.username.as_bytes());
        plain_payload.push(0);
        plain_payload.extend_from_slice(creds.password.as_bytes());
        let encoded = base64::engine::general_purpose::STANDARD.encode(&plain_payload);
        wire.send_line(&format!("AUTH PLAIN {encoded}")).await?;
        wire.read_smtp_reply("235").await?;
    } else {
        wire.send_line("AUTH LOGIN").await?;
        wire.read_smtp_reply("334").await?;
        wire.send_line(
            &base64::engine::general_purpose::STANDARD.encode(creds.username.as_bytes()),
        )
        .await?;
        wire.read_smtp_reply("334").await?;
        wire.send_line(
            &base64::engine::general_purpose::STANDARD.encode(creds.password.as_bytes()),
        )
        .await?;
        wire.read_smtp_reply("235").await?;
    }
    Ok(())
}

/// Normalize LF to CRLF and dot-stuff lines starting with `.` in a single pass.
fn dot_stuff_crlf(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len() + message.len() / 64 + 16);
    let mut at_line_start = true;
    let mut i = 0;
    while i < message.len() {
        let b = message[i];
        if at_line_start && b == b'.' {
            out.push(b'.');
        }
        if b == b'\r' && message.get(i + 1) == Some(&b'\n') {
            out.extend_from_slice(b"\r\n");
            at_line_start = true;
            i += 2;
        } else if b == b'\n' {
            out.extend_from_slice(b"\r\n");
            at_line_start = true;
            i += 1;
        } else {
            out.push(b);
            at_line_start = false;
            i += 1;
        }
    }
    if !at_line_start {
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b".\r\n");
    out
}

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
    let total_start = std::time::Instant::now();
    let (mut wire, ehlo_lines) = smtp_connect_and_ehlo(creds).await?;
    let connect_ms = total_start.elapsed().as_millis() as u64;

    let auth_start = std::time::Instant::now();
    smtp_authenticate(&mut wire, creds, &ehlo_lines).await?;
    let auth_ms = auth_start.elapsed().as_millis() as u64;

    wire.send_line(&format!("MAIL FROM:<{}>", extract_envelope_address(from)))
        .await?;
    wire.read_smtp_reply("250").await?;
    for rcpt in to {
        wire.send_line(&format!("RCPT TO:<{}>", extract_envelope_address(rcpt)))
            .await?;
        wire.read_smtp_reply("250").await?;
    }
    wire.send_line("DATA").await?;
    wire.read_smtp_reply("354").await?;

    let data_start = std::time::Instant::now();
    let stuffed = dot_stuff_crlf(message);
    let bytes = stuffed.len();
    tokio::time::timeout(DATA_IO_TIMEOUT, async {
        wire.stream.write_all(&stuffed).await?;
        wire.stream.flush().await
    })
    .await
    .map_err(|_| SeraError::External("DATA write timed out".into()))?
    .map_err(|e| SeraError::External(format!("DATA write: {e}")))?;
    wire.read_smtp_reply_with_timeout("250", DATA_IO_TIMEOUT)
        .await?;
    let data_ms = data_start.elapsed().as_millis() as u64;
    let total_ms = total_start.elapsed().as_millis() as u64;

    tracing::info!(
        connect_ms,
        auth_ms,
        data_ms,
        total_ms,
        bytes,
        "smtp_send completed"
    );

    let _ = wire.send_line("QUIT").await;
    Ok(())
}

/// Connect + EHLO + AUTH + QUIT without sending (used by `POST /v1/mail/connect`).
pub async fn smtp_test(creds: &MailCredentials) -> Result<()> {
    let (mut wire, ehlo_lines) = smtp_connect_and_ehlo(creds).await?;
    smtp_authenticate(&mut wire, creds, &ehlo_lines).await?;
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

/// Safe `List-Unsubscribe` targets of a message (0.74.14).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct MailUnsubscribe {
    /// `mailto:` target without the scheme; the server sends the request.
    pub mailto: Option<String>,
    /// `https:` link; clients open it in the user's browser.
    pub url: Option<String>,
    pub one_click: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
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
    pub to: String,
    pub cc: String,
    pub message_id: String,
    pub in_reply_to: String,
    pub references: Vec<String>,
    /// First words of the plain body, for list rows.
    pub snippet: String,
    /// Attachments seen, or `multipart/mixed` announced by a headers-only fetch.
    pub has_attachments: bool,
    /// Messages of one conversation share a key (set on list and search results).
    pub thread_key: String,
    pub unsubscribe: Option<MailUnsubscribe>,
}

/// Full parsed message for `GET /v1/mail/messages/{uid}`, including sanitized
/// HTML body, `cid:` inline images, and remote-image indicator.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct MailMessageDetail {
    pub uid: u32,
    pub subject: String,
    pub from: String,
    pub date: String,
    pub flags: Vec<String>,
    pub body: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub has_remote_images: bool,
    pub attachments: Vec<MailAttachment>,
    pub to: String,
    pub cc: String,
    pub message_id: String,
    pub in_reply_to: String,
    pub references: Vec<String>,
    pub snippet: String,
    pub has_attachments: bool,
    pub thread_key: String,
    pub unsubscribe: Option<MailUnsubscribe>,
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

fn unfold_headers(head: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in head.replace("\r\n", "\n").split('\n') {
        if (line.starts_with(' ') || line.starts_with('\t')) && !out.is_empty() {
            if let Some((_, val)) = out.last_mut() {
                val.push(' ');
                val.push_str(line.trim());
            }
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            out.push((key.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    out
}

fn split_head_body(raw: &str) -> (&str, &str) {
    match raw.find("\r\n\r\n") {
        Some(pos) => (&raw[..pos], &raw[pos + 4..]),
        None => match raw.find("\n\n") {
            Some(pos) => (&raw[..pos], &raw[pos + 2..]),
            None => (raw, ""),
        },
    }
}

/// Parse one raw RFC822 message into a MailMessage.
pub fn parse_message(uid: u32, flags: Vec<String>, raw: &str) -> MailMessage {
    let detail = parse_message_detail(uid, flags, raw, false);
    MailMessage {
        uid: detail.uid,
        subject: detail.subject,
        from: detail.from,
        date: detail.date,
        flags: detail.flags,
        body: detail.body,
        attachments: detail.attachments,
        to: detail.to,
        cc: detail.cc,
        message_id: detail.message_id,
        in_reply_to: detail.in_reply_to,
        references: detail.references,
        snippet: detail.snippet,
        has_attachments: detail.has_attachments,
        thread_key: detail.thread_key,
        unsubscribe: detail.unsubscribe,
    }
}

/// Parse one raw RFC822 message into a `MailMessageDetail` with recursive MIME
/// traversal, charset/transfer-encoding decoding, `cid:` inline image rewriting,
/// and strict HTML sanitization.
pub fn parse_message_detail(
    uid: u32,
    flags: Vec<String>,
    raw: &str,
    allow_remote_images: bool,
) -> MailMessageDetail {
    let (head, body) = split_head_body(raw);
    let headers = unfold_headers(head);
    let mut subject = String::new();
    let mut from = String::new();
    let mut date = String::new();
    let mut to = String::new();
    let mut cc = String::new();
    let mut message_id = String::new();
    let mut in_reply_to = String::new();
    let mut references: Vec<String> = Vec::new();
    let mut list_unsubscribe = String::new();
    let mut list_unsubscribe_post = String::new();
    let mut announces_mixed = false;
    for (k, v) in &headers {
        match k.as_str() {
            "subject" if subject.is_empty() => subject = decode_rfc2047(v),
            "from" if from.is_empty() => from = decode_rfc2047(v),
            "date" if date.is_empty() => date = clean_header(v),
            "to" if to.is_empty() => to = clip(&decode_rfc2047(v), 1000),
            "cc" if cc.is_empty() => cc = clip(&decode_rfc2047(v), 1000),
            "message-id" if message_id.is_empty() => {
                message_id = mail_ux::message_ids(v)
                    .into_iter()
                    .next()
                    .unwrap_or_default();
            }
            "in-reply-to" if in_reply_to.is_empty() => {
                in_reply_to = mail_ux::message_ids(v)
                    .into_iter()
                    .next()
                    .unwrap_or_default();
            }
            "references" if references.is_empty() => {
                let mut ids = mail_ux::message_ids(v);
                let keep_from = ids.len().saturating_sub(50);
                references = ids.split_off(keep_from);
            }
            "list-unsubscribe" if list_unsubscribe.is_empty() => {
                list_unsubscribe = clean_header(v);
            }
            "list-unsubscribe-post" if list_unsubscribe_post.is_empty() => {
                list_unsubscribe_post = clean_header(v);
            }
            "content-type" => {
                announces_mixed = v.to_ascii_lowercase().contains("multipart/mixed");
            }
            _ => {}
        }
    }
    let unsubscribe = if list_unsubscribe.is_empty() {
        None
    } else {
        let parsed = mail_ux::parse_list_unsubscribe(&list_unsubscribe, &list_unsubscribe_post);
        (parsed.mailto.is_some() || parsed.url.is_some()).then_some(MailUnsubscribe {
            mailto: parsed.mailto,
            url: parsed.url,
            one_click: parsed.one_click,
        })
    };
    let mut walk = MimeWalkState::default();
    walk_mime(head, body, 0, &mut walk);

    let (sanitized_html, has_remote_images) = match walk.html {
        Some(raw_html) => {
            let (clean, remote) =
                sanitize_email_html(&raw_html, &walk.inline_parts, allow_remote_images);
            (Some(clean), remote)
        }
        None => (None, false),
    };
    let body_text = walk
        .plain
        .unwrap_or_else(|| {
            sanitized_html
                .as_deref()
                .map(html_to_plain_fallback)
                .unwrap_or_default()
        })
        .replace("\r\n", "\n");
    MailMessageDetail {
        uid,
        subject,
        from,
        date,
        flags,
        body: body_text.clone(),
        snippet: mail_ux::snippet_from(&body_text),
        body_text,
        body_html: sanitized_html,
        has_remote_images,
        has_attachments: announces_mixed || !walk.attachments.is_empty(),
        attachments: walk.attachments,
        to,
        cc,
        message_id,
        in_reply_to,
        references,
        thread_key: String::new(),
        unsubscribe,
    }
}

/// Give every message of a list or search result its conversation key.
pub fn assign_thread_keys(messages: &mut [MailMessage]) {
    let keys = {
        let inputs: Vec<mail_ux::ThreadInput<'_>> = messages
            .iter()
            .map(|m| mail_ux::ThreadInput {
                message_id: &m.message_id,
                in_reply_to: &m.in_reply_to,
                references: &m.references,
                subject: &m.subject,
            })
            .collect();
        mail_ux::thread_keys(&inputs)
    };
    for (message, key) in messages.iter_mut().zip(keys) {
        message.thread_key = key;
    }
}

const MAX_MIME_DEPTH: usize = 16;

#[derive(Clone, Debug)]
struct InlinePart {
    content_type: String,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct MimeWalkState {
    plain: Option<String>,
    html: Option<String>,
    inline_parts: HashMap<String, InlinePart>,
    attachments: Vec<MailAttachment>,
    raw_attachments: Vec<(String, String, Vec<u8>)>,
    inline_total: usize,
}

fn header_param(header_val: &str, param_name: &str) -> Option<String> {
    let mut rest = header_val;
    // Skip main value before first ';'
    let semi = rest.find(';')?;
    rest = &rest[semi + 1..];
    while !rest.is_empty() {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let eq = match rest.find('=') {
            Some(i) => i,
            None => break,
        };
        let key = rest[..eq].trim();
        let after_eq = rest[eq + 1..].trim_start();
        let (val, next_rest) = if let Some(quoted) = after_eq.strip_prefix('"') {
            let mut v = String::new();
            let mut chars = quoted.char_indices();
            let mut end_idx = quoted.len();
            while let Some((i, ch)) = chars.next() {
                if ch == '\\' {
                    if let Some((_, esc)) = chars.next() {
                        v.push(esc);
                    }
                } else if ch == '"' {
                    end_idx = i + 1;
                    break;
                } else {
                    v.push(ch);
                }
            }
            let tail = &quoted[end_idx..];
            let next = tail.find(';').map(|s| &tail[s + 1..]).unwrap_or("");
            (v, next)
        } else if let Some(s) = after_eq.find(';') {
            (after_eq[..s].trim().to_owned(), &after_eq[s + 1..])
        } else {
            (after_eq.trim().to_owned(), "")
        };
        if key.eq_ignore_ascii_case(param_name) {
            return Some(decode_rfc2047(&val));
        }
        rest = next_rest;
    }
    None
}

fn decode_quoted_printable(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' {
            if i + 2 < bytes.len() && bytes[i + 1] == b'\r' && bytes[i + 2] == b'\n' {
                i += 3;
                continue;
            }
            if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                i += 2;
                continue;
            }
            if i + 2 < bytes.len()
                && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
            {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn decode_charset(bytes: &[u8], charset: &str) -> String {
    let cs = charset
        .split('*')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match cs.as_str() {
        "" | "utf-8" | "utf8" | "us-ascii" | "ascii" => String::from_utf8_lossy(bytes).into_owned(),
        "iso-8859-1" | "iso_8859-1" | "latin1" | "latin-1" => {
            bytes.iter().map(|&b| b as char).collect()
        }
        "windows-1252" | "cp1252" => bytes.iter().map(|&b| win1252_char(b)).collect(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn win1252_char(b: u8) -> char {
    const TABLE: [char; 32] = [
        '\u{20AC}', '\u{FFFD}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{FFFD}',
        '\u{017D}', '\u{FFFD}', '\u{FFFD}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}',
        '\u{2022}', '\u{2013}', '\u{2014}', '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}',
        '\u{0153}', '\u{FFFD}', '\u{017E}', '\u{0178}',
    ];
    if (0x80..=0x9F).contains(&b) {
        TABLE[(b - 0x80) as usize]
    } else {
        b as char
    }
}

fn decode_part_bytes(encoding: &str, body: &str) -> Vec<u8> {
    match encoding.trim().to_ascii_lowercase().as_str() {
        "base64" => {
            let compact: String = body.chars().filter(|c| !c.is_whitespace()).collect();
            base64::engine::general_purpose::STANDARD
                .decode(compact)
                .unwrap_or_default()
        }
        "quoted-printable" => decode_quoted_printable(body),
        _ => body.as_bytes().to_vec(),
    }
}

fn walk_mime(head: &str, body: &str, depth: usize, state: &mut MimeWalkState) {
    if depth > MAX_MIME_DEPTH {
        return;
    }
    let headers = unfold_headers(head);
    let ct_raw = headers
        .iter()
        .find(|(k, _)| k == "content-type")
        .map(|(_, v)| v.as_str())
        .unwrap_or("text/plain; charset=utf-8");
    let content_type = ct_raw
        .split(';')
        .next()
        .unwrap_or("text/plain")
        .trim()
        .to_ascii_lowercase();
    let charset = header_param(ct_raw, "charset").unwrap_or_else(|| "utf-8".to_owned());

    if content_type.starts_with("multipart/")
        && let Some(boundary) = header_param(ct_raw, "boundary")
        && !boundary.is_empty()
    {
        let delimiter = format!("--{boundary}");
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
            let (part_head, part_body) = split_head_body(part);
            walk_mime(part_head, part_body, depth + 1, state);
        }
        return;
    }

    let encoding = headers
        .iter()
        .find(|(k, _)| k == "content-transfer-encoding")
        .map(|(_, v)| v.as_str())
        .unwrap_or("7bit");
    let decoded = decode_part_bytes(encoding, body);

    let cd_raw = headers
        .iter()
        .find(|(k, _)| k == "content-disposition")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    let disposition = cd_raw
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let filename = header_param(cd_raw, "filename").or_else(|| header_param(ct_raw, "name"));
    let content_id = headers
        .iter()
        .find(|(k, _)| k == "content-id")
        .map(|(_, v)| {
            v.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .trim()
                .to_ascii_lowercase()
        })
        .filter(|s| !s.is_empty());

    if let Some(cid) = content_id
        && disposition != "attachment"
    {
        state.inline_parts.insert(
            cid,
            InlinePart {
                content_type,
                bytes: decoded,
            },
        );
        return;
    }

    if disposition == "attachment" || (filename.is_some() && !content_type.starts_with("text/")) {
        let name = filename.unwrap_or_else(|| "attachment.bin".to_owned());
        let size = decoded.len();
        let data_base64 = if size <= MAX_INLINE_ATTACHMENT_BYTES
            && state.inline_total + size <= MAX_INLINE_ATTACHMENTS_TOTAL
        {
            state.inline_total += size;
            Some(base64::engine::general_purpose::STANDARD.encode(&decoded))
        } else {
            None
        };
        state.attachments.push(MailAttachment {
            name: name.clone(),
            content_type: content_type.clone(),
            size,
            data_base64,
        });
        state.raw_attachments.push((name, content_type, decoded));
        return;
    }

    if content_type == "text/plain" && state.plain.is_none() {
        state.plain = Some(decode_charset(&decoded, &charset));
    } else if content_type == "text/html" && state.html.is_none() {
        state.html = Some(decode_charset(&decoded, &charset));
    }
}

fn html_to_plain_fallback(html: &str) -> String {
    let doc = scraper::Html::parse_document(html);
    let mut out = String::new();
    for node in doc.tree.nodes() {
        if let scraper::Node::Text(t) = node.value() {
            let trimmed = t.trim();
            if !trimmed.is_empty() {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(trimmed);
            }
        }
    }
    out
}

fn sanitize_url_scheme(val: &str) -> String {
    val.chars()
        .filter(|c| !c.is_control() && !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase()
}

fn is_safe_data_image_url(val: &str) -> bool {
    let compact = sanitize_url_scheme(val);
    let Some(rest) = compact.strip_prefix("data:") else {
        return false;
    };
    let Some((meta, payload)) = rest.split_once(',') else {
        return false;
    };
    let meta_ok = matches!(
        meta,
        "image/png;base64" | "image/jpeg;base64" | "image/gif;base64" | "image/webp;base64"
    );
    meta_ok && !payload.is_empty()
}

fn strip_css_comments(input: &str) -> String {
    let mut no_comments = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            while let Some(inner) = chars.next() {
                if inner == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    break;
                }
            }
            continue;
        }
        no_comments.push(c);
    }
    no_comments
}

fn normalize_css_for_scan(input: &str) -> String {
    // 1. Strip CSS comments /* ... */
    let no_comments = strip_css_comments(input);
    // 2. Decode CSS backslash escapes and strip whitespace/control chars for matching.
    let mut out = String::with_capacity(no_comments.len());
    let bytes = no_comments.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 1;
            let start = i;
            while i < bytes.len() && i - start < 6 && bytes[i].is_ascii_hexdigit() {
                i += 1;
            }
            if i > start {
                if let Ok(hex) = std::str::from_utf8(&bytes[start..i])
                    && let Ok(code) = u32::from_str_radix(hex, 16)
                    && let Some(ch) = char::from_u32(code)
                    && !ch.is_whitespace()
                    && !ch.is_control()
                {
                    out.push(ch.to_ascii_lowercase());
                }
                if i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
            } else {
                let ch = bytes[i] as char;
                if !ch.is_whitespace() && !ch.is_control() {
                    out.push(ch.to_ascii_lowercase());
                }
                i += 1;
            }
            continue;
        }
        let ch = no_comments[i..].chars().next().unwrap();
        let len = ch.len_utf8();
        if !ch.is_whitespace() && !ch.is_control() {
            for lc in ch.to_lowercase() {
                out.push(lc);
            }
        }
        i += len;
    }
    out
}

fn sanitize_inline_style(
    style: &str,
    allow_remote_images: bool,
    has_remote_images: &mut bool,
) -> Option<String> {
    let norm_all = normalize_css_for_scan(style);
    if norm_all.contains("expression(")
        || norm_all.contains("behavior:")
        || norm_all.contains("-moz-binding")
        || norm_all.contains("@import")
        || norm_all.contains("javascript:")
        || norm_all.contains("vbscript:")
    {
        return None;
    }

    let mut cleaned = Vec::new();
    for decl in style.split(';') {
        let trimmed = decl.trim();
        if trimmed.is_empty() {
            continue;
        }
        let norm = normalize_css_for_scan(trimmed);
        if norm.contains("url(") {
            let is_remote = norm.contains("http://")
                || norm.contains("https://")
                || norm.contains("url(//")
                || norm.contains("url(\"//")
                || norm.contains("url('//");
            if is_remote {
                *has_remote_images = true;
                if !allow_remote_images {
                    continue;
                }
                cleaned.push(trimmed);
            } else if let Some(data_pos) = norm.find("data:") {
                let data_slice = &norm[data_pos..];
                let data_end = data_slice
                    .find([')', '"', '\''])
                    .unwrap_or(data_slice.len());
                if is_safe_data_image_url(&data_slice[..data_end]) {
                    cleaned.push(trimmed);
                }
            }
            continue;
        }
        cleaned.push(trimmed);
    }

    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.join("; "))
    }
}

fn css_has_dangerous_token(norm: &str) -> bool {
    norm.contains("expression(")
        || norm.contains("behavior:")
        || norm.contains("-moz-binding")
        || norm.contains("@import")
        || norm.contains("@charset")
        || norm.contains("javascript:")
        || norm.contains("vbscript:")
        || norm.contains("</style")
        || norm.contains("position:fixed")
}

fn sanitize_css_declarations(
    decls: &str,
    allow_remote_images: bool,
    has_remote_images: &mut bool,
) -> Option<String> {
    let mut cleaned = Vec::new();
    for decl in decls.split(';') {
        let trimmed = decl.trim();
        if trimmed.is_empty() {
            continue;
        }
        let norm = normalize_css_for_scan(trimmed);
        if css_has_dangerous_token(&norm) {
            continue;
        }
        if norm.contains("url(") {
            let is_remote = norm.contains("http://")
                || norm.contains("https://")
                || norm.contains("url(//")
                || norm.contains("url(\"//")
                || norm.contains("url('//");
            if is_remote {
                *has_remote_images = true;
                if !allow_remote_images {
                    continue;
                }
                cleaned.push(trimmed);
            } else if let Some(data_pos) = norm.find("data:") {
                let data_slice = &norm[data_pos..];
                let data_end = data_slice
                    .find([')', '"', '\''])
                    .unwrap_or(data_slice.len());
                if is_safe_data_image_url(&data_slice[..data_end]) {
                    cleaned.push(trimmed);
                }
            }
            continue;
        }
        cleaned.push(trimmed);
    }
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.join(";"))
    }
}

fn find_matching_brace(s: &str, open_idx: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    let mut i = open_idx;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == q {
                quote = None;
            }
        } else if b == b'"' || b == b'\'' {
            quote = Some(b);
        } else if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn contains_unquoted_brace(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == q {
                quote = None;
            }
        } else if b == b'"' || b == b'\'' {
            quote = Some(b);
        } else if b == b'{' {
            return true;
        }
        i += 1;
    }
    false
}

fn sanitize_css_block(
    input: &str,
    allow_remote_images: bool,
    has_remote_images: &mut bool,
) -> String {
    let mut out = String::new();
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }

        // Skip @import and @charset at-rules up to ';' or newline.
        let rest = &input[i..];
        let is_banned_at = rest
            .get(..7)
            .is_some_and(|p| p.eq_ignore_ascii_case("@import"))
            || rest
                .get(..8)
                .is_some_and(|p| p.eq_ignore_ascii_case("@charset"));
        if is_banned_at {
            let mut quote: Option<u8> = None;
            while i < bytes.len() {
                let b = bytes[i];
                if let Some(q) = quote {
                    if b == b'\\' && i + 1 < bytes.len() {
                        i += 2;
                        continue;
                    }
                    if b == q {
                        quote = None;
                    }
                } else if b == b'"' || b == b'\'' {
                    quote = Some(b);
                } else if b == b';' || b == b'\n' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }

        // Scan until '{', ';', or '}' outside quotes.
        let stmt_start = i;
        let mut brace_idx = None;
        let mut quote: Option<u8> = None;
        while i < bytes.len() {
            let b = bytes[i];
            if let Some(q) = quote {
                if b == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if b == q {
                    quote = None;
                }
            } else if b == b'"' || b == b'\'' {
                quote = Some(b);
            } else if b == b'{' {
                brace_idx = Some(i);
                break;
            } else if b == b';' || b == b'}' {
                i += 1;
                break;
            }
            i += 1;
        }

        let Some(open_idx) = brace_idx else {
            continue;
        };
        let Some(close_idx) = find_matching_brace(input, open_idx) else {
            break;
        };

        let header = input[stmt_start..open_idx].trim();
        let inner = &input[open_idx + 1..close_idx];
        i = close_idx + 1;

        if header.is_empty() || css_has_dangerous_token(&normalize_css_for_scan(header)) {
            continue;
        }

        if contains_unquoted_brace(inner) {
            let sanitized_inner = sanitize_css_block(inner, allow_remote_images, has_remote_images);
            if !sanitized_inner.trim().is_empty() {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(header);
                out.push('{');
                out.push_str(&sanitized_inner);
                out.push('}');
            }
        } else if let Some(clean_decls) =
            sanitize_css_declarations(inner, allow_remote_images, has_remote_images)
        {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(header);
            out.push('{');
            out.push_str(&clean_decls);
            out.push('}');
        }
    }

    out
}

pub fn sanitize_style_sheet(
    css: &str,
    allow_remote_images: bool,
    has_remote_images: &mut bool,
) -> Option<String> {
    let no_comments = strip_css_comments(css);
    // Strip any literal `</style` sequence (case-insensitive) so it cannot break out.
    let mut clean_raw = String::with_capacity(no_comments.len());
    let mut idx = 0;
    let bytes = no_comments.as_bytes();
    while idx < bytes.len() {
        if no_comments[idx..]
            .get(..7)
            .is_some_and(|p| p.eq_ignore_ascii_case("</style"))
        {
            idx += 7;
            continue;
        }
        let ch = no_comments[idx..].chars().next().unwrap();
        clean_raw.push(ch);
        idx += ch.len_utf8();
    }

    let result = sanitize_css_block(&clean_raw, allow_remote_images, has_remote_images);
    let trimmed = result.trim();
    if trimmed.is_empty() || css_has_dangerous_token(&normalize_css_for_scan(trimmed)) {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn sanitize_email_html(
    html: &str,
    inline_parts: &HashMap<String, InlinePart>,
    allow_remote_images: bool,
) -> (String, bool) {
    let doc = scraper::Html::parse_document(html);
    let mut out = String::with_capacity(html.len());
    let mut has_remote_images = false;
    sanitize_element(
        doc.root_element(),
        inline_parts,
        allow_remote_images,
        &mut has_remote_images,
        &mut out,
    );
    (out, has_remote_images)
}

fn sanitize_element(
    el_ref: scraper::ElementRef<'_>,
    inline_parts: &HashMap<String, InlinePart>,
    allow_remote_images: bool,
    has_remote_images: &mut bool,
    out: &mut String,
) {
    let el = el_ref.value();
    let tag = el.name();
    // Strip dangerous tags and their entire subtrees.
    if matches!(
        tag,
        "script"
            | "iframe"
            | "object"
            | "embed"
            | "form"
            | "input"
            | "button"
            | "textarea"
            | "select"
            | "meta"
            | "link"
            | "base"
            | "noscript"
            | "template"
            | "svg"
            | "math"
            | "title"
    ) {
        return;
    }
    if tag == "style" {
        let raw_css: String = el_ref.text().collect();
        if let Some(clean_css) =
            sanitize_style_sheet(&raw_css, allow_remote_images, has_remote_images)
        {
            out.push_str("<style>");
            out.push_str(&clean_css);
            out.push_str("</style>");
        }
        return;
    }
    if tag == "head" {
        for child in el_ref.children() {
            if let Some(child_el) = scraper::ElementRef::wrap(child) {
                sanitize_element(
                    child_el,
                    inline_parts,
                    allow_remote_images,
                    has_remote_images,
                    out,
                );
            }
        }
        return;
    }
    // Unwrap document-level wrappers so email HTML embeds cleanly.
    if matches!(tag, "html" | "body") {
        for child in el_ref.children() {
            match child.value() {
                scraper::Node::Text(text) => out.push_str(&escape_html_text(text)),
                scraper::Node::Element(_) => {
                    if let Some(child_el) = scraper::ElementRef::wrap(child) {
                        sanitize_element(
                            child_el,
                            inline_parts,
                            allow_remote_images,
                            has_remote_images,
                            out,
                        );
                    }
                }
                _ => {}
            }
        }
        return;
    }
    let allowed_tag = matches!(
        tag,
        "p" | "div"
            | "span"
            | "br"
            | "hr"
            | "a"
            | "img"
            | "b"
            | "strong"
            | "i"
            | "em"
            | "u"
            | "s"
            | "strike"
            | "del"
            | "ins"
            | "sub"
            | "sup"
            | "small"
            | "big"
            | "code"
            | "pre"
            | "blockquote"
            | "q"
            | "cite"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "ul"
            | "ol"
            | "li"
            | "dl"
            | "dt"
            | "dd"
            | "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "th"
            | "td"
            | "caption"
            | "colgroup"
            | "col"
            | "font"
            | "center"
            | "article"
            | "section"
            | "header"
            | "footer"
            | "main"
            | "nav"
            | "aside"
            | "figure"
            | "figcaption"
            | "address"
            | "time"
            | "mark"
            | "abbr"
            | "details"
            | "summary"
            | "wbr"
    );
    if !allowed_tag {
        for child in el_ref.children() {
            match child.value() {
                scraper::Node::Text(text) => out.push_str(&escape_html_text(text)),
                scraper::Node::Element(_) => {
                    if let Some(child_el) = scraper::ElementRef::wrap(child) {
                        sanitize_element(
                            child_el,
                            inline_parts,
                            allow_remote_images,
                            has_remote_images,
                            out,
                        );
                    }
                }
                _ => {}
            }
        }
        return;
    }

    out.push('<');
    out.push_str(tag);
    for (attr, val) in el.attrs() {
        let attr_lc = attr.to_ascii_lowercase();
        if attr_lc.starts_with("on")
            || matches!(
                attr_lc.as_str(),
                "srcdoc" | "formaction" | "action" | "ping" | "xlink:href" | "xmlns"
            )
        {
            continue;
        }
        match attr_lc.as_str() {
            "src" => {
                let scheme_check = sanitize_url_scheme(val);
                if let Some(cid_raw) = val.trim().strip_prefix("cid:").or_else(|| {
                    val.trim()
                        .get(..4)
                        .filter(|p| p.eq_ignore_ascii_case("cid:"))
                        .map(|_| &val.trim()[4..])
                }) {
                    let cid = cid_raw
                        .trim()
                        .trim_start_matches('<')
                        .trim_end_matches('>')
                        .to_ascii_lowercase();
                    if tag == "img"
                        && let Some(part) = inline_parts.get(&cid)
                        && is_safe_inline_image_mime(&part.content_type)
                    {
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&part.bytes);
                        out.push_str(&format!(
                            " src=\"data:{};base64,{}\"",
                            part.content_type, b64
                        ));
                    }
                } else if scheme_check.starts_with("https://")
                    || scheme_check.starts_with("http://")
                {
                    *has_remote_images = true;
                    if allow_remote_images && tag == "img" {
                        out.push_str(" src=\"");
                        out.push_str(&escape_html_text(val.trim()));
                        out.push('"');
                    }
                } else if tag == "img" && is_safe_data_image_url(val) {
                    out.push_str(" src=\"");
                    out.push_str(&escape_html_text(val.trim()));
                    out.push('"');
                }
            }
            "href" => {
                let scheme_check = sanitize_url_scheme(val);
                if scheme_check.starts_with("https://")
                    || scheme_check.starts_with("http://")
                    || scheme_check.starts_with("mailto:")
                    || scheme_check.starts_with("tel:")
                    || scheme_check.starts_with('#')
                {
                    out.push_str(" href=\"");
                    out.push_str(&escape_html_text(val.trim()));
                    out.push('"');
                }
            }
            "background" => {
                let scheme_check = sanitize_url_scheme(val);
                if scheme_check.starts_with("https://") || scheme_check.starts_with("http://") {
                    *has_remote_images = true;
                    if allow_remote_images {
                        out.push_str(" background=\"");
                        out.push_str(&escape_html_text(val.trim()));
                        out.push('"');
                    }
                }
            }
            "style" => {
                if let Some(clean_style) =
                    sanitize_inline_style(val, allow_remote_images, has_remote_images)
                {
                    out.push_str(" style=\"");
                    out.push_str(&escape_html_text(&clean_style));
                    out.push('"');
                }
            }
            "alt" | "title" | "width" | "height" | "align" | "valign" | "bgcolor" | "color"
            | "border" | "cellpadding" | "cellspacing" | "colspan" | "rowspan" | "class" | "id"
            | "dir" | "lang" | "face" | "size" | "target" => {
                out.push(' ');
                out.push_str(&attr_lc);
                out.push_str("=\"");
                out.push_str(&escape_html_text(val));
                out.push('"');
            }
            "rel" if tag != "a" => {
                out.push_str(" rel=\"");
                out.push_str(&escape_html_text(val));
                out.push('"');
            }
            _ => {}
        }
    }
    if tag == "a" {
        out.push_str(" rel=\"noopener noreferrer\"");
    }
    out.push('>');

    let is_void = matches!(tag, "br" | "hr" | "img" | "wbr" | "col");
    if !is_void {
        for child in el_ref.children() {
            match child.value() {
                scraper::Node::Text(text) => out.push_str(&escape_html_text(text)),
                scraper::Node::Element(_) => {
                    if let Some(child_el) = scraper::ElementRef::wrap(child) {
                        sanitize_element(
                            child_el,
                            inline_parts,
                            allow_remote_images,
                            has_remote_images,
                            out,
                        );
                    }
                }
                _ => {}
            }
        }
        out.push_str("</");
        out.push_str(tag);
        out.push('>');
    }
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
    let (head, body) = split_head_body(raw);
    let mut walk = MimeWalkState::default();
    walk_mime(head, body, 0, &mut walk);
    let Some((name, content_type, decoded)) = walk.raw_attachments.into_iter().nth(index) else {
        return Err(SeraError::NotFound("attachment".into()));
    };
    if decoded.len() > MAX_ATTACHMENT_BYTES {
        return Err(SeraError::Validation(
            "attachment exceeds 20 MiB limit".into(),
        ));
    }
    Ok((name, content_type, decoded))
}

fn decode_rfc2047_word(word: &str) -> Option<String> {
    let inner = word.strip_prefix("=?")?.strip_suffix("?=")?;
    let mut parts = inner.splitn(3, '?');
    let charset = parts.next()?;
    let encoding = parts.next()?;
    let text = parts.next()?;
    if text.contains('?') || text.chars().any(char::is_whitespace) {
        return None;
    }
    let bytes = if encoding.eq_ignore_ascii_case("B") {
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .ok()?
    } else if encoding.eq_ignore_ascii_case("Q") {
        let mut out = Vec::with_capacity(text.len());
        let raw = text.as_bytes();
        let mut i = 0;
        while i < raw.len() {
            if raw[i] == b'_' {
                out.push(b' ');
                i += 1;
            } else if raw[i] == b'='
                && i + 2 < raw.len()
                && let (Some(hi), Some(lo)) = (hex_val(raw[i + 1]), hex_val(raw[i + 2]))
            {
                out.push((hi << 4) | lo);
                i += 3;
            } else {
                out.push(raw[i]);
                i += 1;
            }
        }
        out
    } else {
        return None;
    };
    Some(decode_charset(&bytes, charset))
}

/// Decode RFC 2047 encoded words (`=?charset?B|Q?...?=`), including multi-word
/// headers, UTF-8 / ISO-8859-1 / Windows-1252, and RFC 2047 whitespace-collapse
/// between adjacent encoded words.
fn decode_rfc2047(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut pos = 0;
    let mut prev_was_encoded = false;
    while pos < value.len() {
        let rest = &value[pos..];
        if let Some(rel_start) = rest.find("=?") {
            let gap = &rest[..rel_start];
            let abs_start = pos + rel_start;
            let after_prefix = &value[abs_start + 2..];
            if let Some(q1) = after_prefix.find('?')
                && let Some(q2_rel) = after_prefix[q1 + 1..].find('?')
                && let payload_start = q1 + 1 + q2_rel + 1
                && let Some(rel_end) = after_prefix[payload_start..].find("?=")
            {
                let word_end = abs_start + 2 + payload_start + rel_end + 2;
                let candidate = &value[abs_start..word_end];
                if let Some(decoded) = decode_rfc2047_word(candidate) {
                    if !(prev_was_encoded && gap.chars().all(|c| c == ' ' || c == '\t')) {
                        out.push_str(gap);
                    }
                    out.push_str(&decoded);
                    pos = word_end;
                    prev_was_encoded = true;
                    continue;
                }
            }
            out.push_str(&rest[..rel_start + 2]);
            pos = abs_start + 2;
            prev_was_encoded = false;
        } else {
            out.push_str(rest);
            break;
        }
    }
    clean_header(&out)
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

/// How much of each message a list row needs: the headers and the start of the body.
const PREVIEW_BYTES: usize = 8192;

/// The UID of one untagged `FETCH` response, read from before its body literal.
fn fetch_response_uid(block: &str) -> Option<u32> {
    let head = &block[..block.find("BODY[").unwrap_or(block.len())];
    let at = head.find("UID ")?;
    head[at + 4..]
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// Fetch the first [`PREVIEW_BYTES`] of several messages with one command and parse
/// them like full messages. Attachments are not seen in so little text, but a
/// `multipart/mixed` message still reports `has_attachments`. Order follows `uids`.
async fn fetch_previews<S: AsyncRead + AsyncWrite + Unpin>(
    wire: &mut Wire<S>,
    uids: &[u32],
) -> Result<Vec<MailMessage>> {
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    let set = uids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    wire.send_line(&format!(
        "a5 UID FETCH {set} (UID FLAGS BODY.PEEK[]<0.{PREVIEW_BYTES}>)"
    ))
    .await?;
    let (_, untagged) = wire.expect_tagged("a5").await?;
    let mut by_uid: HashMap<u32, MailMessage> = HashMap::new();
    for block in untagged {
        let Some(uid) = fetch_response_uid(&block) else {
            continue;
        };
        let (flags, body) = parse_fetch_block(&block);
        let Some(raw) = body.filter(|b| !b.is_empty()) else {
            continue;
        };
        by_uid.insert(uid, parse_message(uid, flags, &raw));
    }
    Ok(uids.iter().filter_map(|u| by_uid.remove(u)).collect())
}

/// Previews for list rows. If the server refuses or garbles the batched partial
/// fetch, fall back to fetching each message whole, as before.
async fn fetch_list_rows<S: AsyncRead + AsyncWrite + Unpin>(
    wire: &mut Wire<S>,
    untagged: Vec<String>,
    limit: u32,
) -> Result<Vec<MailMessage>> {
    let uids: Vec<u32> = parse_search_uids(&untagged)
        .into_iter()
        .rev()
        .take(limit as usize)
        .collect();
    if let Ok(rows) = fetch_previews(wire, &uids).await
        && (!rows.is_empty() || uids.is_empty())
    {
        return Ok(rows);
    }
    let mut out = Vec::new();
    for uid in uids {
        if let Ok(msg) = imap_fetch(wire, uid).await {
            out.push(msg);
        }
    }
    Ok(out)
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
    let mut out = fetch_search_results(&mut wire, untagged, limit).await?;
    assign_thread_keys(&mut out);
    let _ = wire.send_line("a9 LOGOUT").await;
    Ok(out)
}

/// List the newest `limit` messages for list rows: one batched fetch of the start of
/// each message (headers, snippet, attachment hint) instead of every message whole.
pub async fn imap_list_preview(
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
    let mut out = fetch_list_rows(&mut wire, untagged, limit).await?;
    assign_thread_keys(&mut out);
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
    assign_thread_keys(&mut out);
    let _ = wire.send_line("a9 LOGOUT").await;
    Ok(out)
}

/// UIDs of the unread messages of a folder. One `SEARCH UNSEEN` on a read-only
/// `EXAMINE`; nothing is fetched or flagged, so it is cheap enough to poll.
pub async fn imap_unread_uids(creds: &MailCredentials, folder: &str) -> Result<Vec<u32>> {
    let mut wire = imap_connect(creds).await?;
    let folder = normalize_folder(folder)?;
    wire.send_line(&format!("a2 EXAMINE {}", imap_quote(&folder)))
        .await?;
    wire.expect_tagged("a2").await?;
    wire.send_line("a3 UID SEARCH UNSEEN").await?;
    let (_, untagged) = wire.expect_tagged("a3").await?;
    let _ = wire.send_line("a9 LOGOUT").await;
    Ok(parse_search_uids(&untagged))
}

/// Search a folder; returns the newest `limit` matches. A bare word searches
/// subject, sender and body. Operators narrow it (`from:`, `to:`, `subject:`,
/// `has:attachment`, `is:unread`, `is:starred`, `before:`, `after:`, see
/// [`mail_ux::parse_query`]).
pub async fn imap_search(
    creds: &MailCredentials,
    folder: &str,
    query: &str,
    limit: u32,
) -> Result<Vec<MailMessage>> {
    search_rows(creds, folder, query, limit, false).await
}

/// Like [`imap_search`] but for list rows: one batched fetch of the start of each
/// message instead of every message whole.
pub async fn imap_search_preview(
    creds: &MailCredentials,
    folder: &str,
    query: &str,
    limit: u32,
) -> Result<Vec<MailMessage>> {
    search_rows(creds, folder, query, limit, true).await
}

async fn search_rows(
    creds: &MailCredentials,
    folder: &str,
    query: &str,
    limit: u32,
    preview: bool,
) -> Result<Vec<MailMessage>> {
    let limit = limit.clamp(1, MAX_LIST);
    let parsed = mail_ux::parse_query(query).map_err(SeraError::Validation)?;
    let mut wire = imap_connect(creds).await?;
    let folder = normalize_folder(folder)?;
    select_folder(&mut wire, &folder).await?;
    let criteria = parsed.imap_criteria(&imap_quote);
    wire.send_line(&format!("a3 UID SEARCH {criteria}")).await?;
    let (_, untagged) = wire.expect_tagged("a3").await?;
    // `has:attachment` is checked after the fetch, so look at more candidates.
    let fetch_limit = if parsed.needs_attachment_filter() {
        MAX_LIST
    } else {
        limit
    };
    let mut out = if preview {
        fetch_list_rows(&mut wire, untagged, fetch_limit).await?
    } else {
        fetch_search_results(&mut wire, untagged, fetch_limit).await?
    };
    if parsed.needs_attachment_filter() {
        out.retain(|m| m.has_attachments);
        out.truncate(limit as usize);
    }
    assign_thread_keys(&mut out);
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

/// Extract the folder name after the hierarchy delimiter from an untagged
/// `LIST` reply (supports Dovecot/Hostinger unquoted names like `INBOX.Sent`,
/// quoted names like `"Sent Items"`, and `NIL` delimiters).
fn parse_list_name(line: &str) -> Option<String> {
    let rest = line.trim();
    let rest = if rest
        .get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("* list "))
    {
        &rest[7..]
    } else {
        return None;
    };
    let flags_end = rest.find(')')?;
    let mut rest = rest[flags_end + 1..].trim_start();
    // Skip hierarchy delimiter: either `"..."` or `NIL` or unquoted token.
    if let Some(after_q) = rest.strip_prefix('"') {
        let mut chars = after_q.char_indices();
        let mut end_idx = None;
        while let Some((i, ch)) = chars.next() {
            if ch == '\\' {
                let _ = chars.next();
            } else if ch == '"' {
                end_idx = Some(i + 1);
                break;
            }
        }
        rest = after_q[end_idx?..].trim_start();
    } else {
        let delim_end = rest.find(char::is_whitespace)?;
        rest = rest[delim_end..].trim_start();
    }
    if rest.is_empty() {
        return None;
    }
    let name = if let Some(after_q) = rest.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = after_q.char_indices();
        let mut closed = false;
        while let Some((_, ch)) = chars.next() {
            if ch == '\\' {
                if let Some((_, esc)) = chars.next() {
                    out.push(esc);
                }
            } else if ch == '"' {
                closed = true;
                break;
            } else {
                out.push(ch);
            }
        }
        if !closed {
            return None;
        }
        out
    } else {
        rest.trim().to_owned()
    };
    if name.is_empty() || name == "*" {
        return None;
    }
    Some(name)
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

/// Build the RFC 822 bytes of a draft. Unlike a sent message, a draft may have no
/// recipients yet.
pub fn build_draft_message(
    from: &str,
    to: &[String],
    cc: &[String],
    subject: &str,
    body: &str,
    body_html: Option<&str>,
) -> Result<Vec<u8>> {
    const PLACEHOLDER: &str = "draft-placeholder@serabot.invalid";
    let recipients: Vec<String> = if to.is_empty() {
        vec![PLACEHOLDER.to_owned()]
    } else {
        to.to_vec()
    };
    let raw = build_message_full(from, &recipients, cc, subject, body, body_html, None, &[])?;
    if !to.is_empty() {
        return Ok(raw);
    }
    let text = String::from_utf8_lossy(&raw).replacen(&format!("To: {PLACEHOLDER}\r\n"), "", 1);
    Ok(text.into_bytes())
}

/// Save a draft into `folder` with `IMAP APPEND`, flagged `\Draft \Seen`. When
/// `replace_uid` is given, the older copy is deleted afterwards. Returns the new
/// UID when the server reports `APPENDUID`.
pub async fn imap_save_draft(
    creds: &MailCredentials,
    folder: &str,
    raw: &[u8],
    replace_uid: Option<u32>,
) -> Result<Option<u32>> {
    if raw.is_empty() || raw.len() > MAX_MESSAGE_BYTES {
        return Err(SeraError::Validation("draft size is out of range".into()));
    }
    let folder = normalize_folder(folder)?;
    let mut wire = imap_connect(creds).await?;
    wire.send_line(&format!(
        "a4 APPEND {} (\\Draft \\Seen) {{{}}}",
        imap_quote(&folder),
        raw.len()
    ))
    .await?;
    let go_ahead = wire.read_line().await?;
    if !go_ahead.starts_with('+') {
        return Err(SeraError::External(format!(
            "mail server refused the draft: {}",
            clip(&go_ahead, 200)
        )));
    }
    wire.send_literal(raw).await?;
    let (done, _) = wire.expect_tagged("a4").await?;
    let _ = wire.send_line("a9 LOGOUT").await;
    let new_uid = mail_ux::parse_appenduid(&done);
    if let Some(old) = replace_uid
        && Some(old) != new_uid
    {
        // The new copy is already stored; a failed cleanup only leaves a stale draft.
        let _ = imap_delete(creds, &folder, old).await;
    }
    Ok(new_uid)
}

/// Send the plain request a `List-Unsubscribe: <mailto:...>` header asks for.
/// Goes through the same blocklist scan as every other outgoing mail.
pub async fn send_unsubscribe_mailto(
    secrets: &UserSecrets,
    store: &Store,
    policy: &Policy,
    user_id: &str,
    account: Option<&str>,
    target: &str,
) -> Result<()> {
    let account = account.map(str::trim).filter(|s| !s.is_empty());
    account_provider(account)?;
    let (addr, subject, body) = mail_ux::split_mailto(target)
        .ok_or_else(|| SeraError::Validation("unsubscribe address is not valid".into()))?;
    let subject = if subject.trim().is_empty() {
        "unsubscribe".to_owned()
    } else {
        subject
    };
    let body = if body.trim().is_empty() {
        "unsubscribe".to_owned()
    } else {
        body
    };
    for text in [&addr, &subject, &body] {
        scan_mail_text(store, policy, user_id, text)?;
    }
    let creds = require_creds(secrets, user_id, account)?;
    let from = from_address(&creds);
    let rcpts = vec![addr];
    let msg = build_message_full(&from, &rcpts, &[], &subject, &body, None, None, &[])?;
    smtp_send(&creds, &from, &rcpts, &msg).await
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

/// Read one message by UID with sanitized HTML, `cid:` inline images, and
/// optional remote image allowance.
pub async fn imap_read_detail(
    creds: &MailCredentials,
    folder: &str,
    uid: u32,
    allow_remote_images: bool,
) -> Result<MailMessageDetail> {
    let mut wire = imap_connect(creds).await?;
    let folder = normalize_folder(folder)?;
    select_folder(&mut wire, &folder).await?;
    let (flags, raw) = imap_fetch_raw(&mut wire, uid, "BODY.PEEK[]").await?;
    let _ = wire.send_line("a9 LOGOUT").await;
    Ok(parse_message_detail(uid, flags, &raw, allow_remote_images))
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
    register_inner(registry, secrets, None, None)
}

pub fn register_with_artifacts(
    registry: &mut ToolRegistry,
    secrets: Arc<UserSecrets>,
    store: Store,
    artifacts: Arc<ArtifactStore>,
) -> Result<()> {
    register_inner(registry, secrets, Some(store), Some(artifacts))
}

fn register_inner(
    registry: &mut ToolRegistry,
    secrets: Arc<UserSecrets>,
    store: Option<Store>,
    artifacts: Option<Arc<ArtifactStore>>,
) -> Result<()> {
    registry.register(MailSendTool {
        secrets: secrets.clone(),
        store: store.clone(),
        artifacts: artifacts.clone(),
    })?;
    registry.register(MailReplyTool {
        secrets: secrets.clone(),
        store: store.clone(),
        artifacts: artifacts.clone(),
    })?;
    registry.register(MailDraftTool {
        secrets: secrets.clone(),
        store,
        artifacts,
    })?;
    registry.register(MailTriageTool {
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

fn arg_string_list(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|v| {
                    v.as_str()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
fn prepare_html_and_inline(
    secrets: &UserSecrets,
    store: Option<&Store>,
    artifacts: Option<&Arc<ArtifactStore>>,
    user_id: &str,
    account: Option<&str>,
    creds: &MailCredentials,
    body_plain: &str,
    body_html: Option<&str>,
) -> Result<(Option<String>, Vec<InlineAttachment>)> {
    let sig_cfg = load_signature_config(secrets, user_id, account).unwrap_or_default();
    let inline_logo = match (store, artifacts) {
        (Some(s), Some(a)) => load_inline_logo_attachment(s, a, user_id, &sig_cfg)?,
        _ => None,
    };
    let sig_images = match (store, artifacts) {
        (Some(s), Some(a)) => load_inline_signature_images(s, a, user_id, &sig_cfg)?,
        _ => Vec::new(),
    };
    let mut final_html = html_with_signature(
        creds,
        &sig_cfg,
        body_plain,
        body_html,
        inline_logo.is_some(),
    )?;
    let mut inline_parts = Vec::new();
    if let Some(html) = final_html.take() {
        let (rewritten, extracted) = extract_inline_data_images(&html);
        inline_parts.extend(extracted);
        final_html = Some(rewritten);
    }
    if let Some(logo) = inline_logo {
        inline_parts.push(logo);
    }
    inline_parts.extend(sig_images);
    Ok((final_html, inline_parts))
}

fn split_header_addresses(header: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut angle_depth = 0usize;
    for ch in header.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            '<' if !in_quotes => {
                angle_depth += 1;
                current.push(ch);
            }
            '>' if !in_quotes => {
                angle_depth = angle_depth.saturating_sub(1);
                current.push(ch);
            }
            ',' if !in_quotes && angle_depth == 0 => {
                let addr = extract_envelope_address(current.trim());
                if !addr.is_empty() && addr.contains('@') {
                    out.push(addr.to_owned());
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    let addr = extract_envelope_address(current.trim());
    if !addr.is_empty() && addr.contains('@') {
        out.push(addr.to_owned());
    }
    out
}

pub struct MailSendTool {
    secrets: Arc<UserSecrets>,
    store: Option<Store>,
    artifacts: Option<Arc<ArtifactStore>>,
}

#[async_trait]
impl Tool for MailSendTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: MAIL_SEND_TOOL.into(),
            description: "Send an email through the member's own SMTP mailbox (with optional \
                          HTML body, CC, threading headers, and attachments). Always asks for \
                          approval; the approval names the recipients and subject."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "to": {"type": "array", "items": {"type": "string"}, "description": "Recipient addresses"},
                    "cc": {"type": "array", "items": {"type": "string"}, "description": "Optional CC recipient addresses"},
                    "subject": {"type": "string"},
                    "body": {"type": "string", "description": "Plain-text body"},
                    "body_html": {"type": "string", "description": "Optional HTML body (inline data:image/*;base64 images are extracted to cid: attachments)"},
                    "in_reply_to": {"type": "string", "description": "Optional Message-ID for In-Reply-To header"},
                    "references": {"type": "string", "description": "Optional space-separated Message-IDs for References header"},
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
        let to = arg_string_list(&args, "to");
        let cc = arg_string_list(&args, "cc");
        let subject = args
            .get("subject")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let raw_body = args
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let body_html = args.get("body_html").and_then(Value::as_str);
        let in_reply_to = args.get("in_reply_to").and_then(Value::as_str);
        let references = args.get("references").and_then(Value::as_str);

        let body = body_with_signature(&creds, raw_body.clone())?;
        let (final_html, inline_parts) = prepare_html_and_inline(
            &self.secrets,
            self.store.as_ref(),
            self.artifacts.as_ref(),
            &context.user_id,
            account.as_deref(),
            &creds,
            &raw_body,
            body_html,
        )?;

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
        let from = from_address(&creds);
        let message = build_message_with_inline(
            &from,
            &to,
            &cc,
            &subject,
            &body,
            final_html.as_deref(),
            in_reply_to,
            references,
            &inline_parts,
            &attachments,
        )?;
        let mut all_rcpts = Vec::with_capacity(to.len() + cc.len());
        all_rcpts.extend(to.iter().cloned());
        all_rcpts.extend(cc.iter().cloned());
        smtp_send(&creds, &from, &all_rcpts, &message).await?;
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
                "cc": cc,
                "subject": subject,
                "attachments": attachments.iter().map(|a| json!({"name": a.name, "bytes": a.bytes.len()})).collect::<Vec<_>>(),
            })),
            ..ToolOutput::default()
        })
    }
}

pub struct MailReplyTool {
    secrets: Arc<UserSecrets>,
    store: Option<Store>,
    artifacts: Option<Arc<ArtifactStore>>,
}

#[async_trait]
impl Tool for MailReplyTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: MAIL_REPLY_TOOL.into(),
            description: "Reply to an existing email (by UID) through the member's own SMTP \
                          mailbox, automatically setting Re: subject, In-Reply-To, and \
                          References headers plus HTML signature. Always asks for approval."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "uid": {"type": "integer", "description": "UID of the message to reply to"},
                    "body": {"type": "string", "description": "Plain-text reply body"},
                    "body_html": {"type": "string", "description": "Optional HTML reply body"},
                    "folder": {"type": "string", "description": "Folder containing the original message, default INBOX"},
                    "reply_all": {"type": "boolean", "description": "Include original To/CC recipients (excluding self)"},
                    "account": {"type": "string", "description": "Named mailbox connected in Integrations → Mail; omit for the default account"}
                },
                "required": ["uid", "body"],
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
        let raw_body = args
            .get("body")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("body is required".into()))?
            .to_owned();
        let body_html = args.get("body_html").and_then(Value::as_str);
        let folder = args
            .get("folder")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("INBOX")
            .to_owned();
        let reply_all = args
            .get("reply_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let target = imap_read_detail(&creds, &folder, uid, false).await?;
        let to = split_header_addresses(&target.from);
        if to.is_empty() {
            return Err(SeraError::Validation(
                "original message has no valid From address to reply to".into(),
            ));
        }

        let from = from_address(&creds);
        let self_addr = extract_envelope_address(&from).to_ascii_lowercase();
        let mut cc = Vec::new();
        if reply_all {
            for addr in split_header_addresses(&target.to)
                .into_iter()
                .chain(split_header_addresses(&target.cc))
            {
                let lc = addr.to_ascii_lowercase();
                if lc != self_addr
                    && !to.iter().any(|t| t.eq_ignore_ascii_case(&lc))
                    && !cc.iter().any(|c: &String| c.eq_ignore_ascii_case(&lc))
                {
                    cc.push(addr);
                }
            }
        }

        let orig_subj = target.subject.trim();
        let subject = if orig_subj
            .get(..3)
            .is_some_and(|p| p.eq_ignore_ascii_case("re:"))
        {
            orig_subj.to_owned()
        } else if orig_subj.is_empty() {
            "Re:".to_owned()
        } else {
            format!("Re: {orig_subj}")
        };

        let msg_id = target.message_id.trim();
        let in_reply_to = (!msg_id.is_empty()).then_some(msg_id);
        let mut refs: Vec<String> = target
            .references
            .iter()
            .map(|r| r.trim().to_owned())
            .filter(|r| !r.is_empty())
            .collect();
        if !msg_id.is_empty() && !refs.iter().any(|r| r == msg_id) {
            refs.push(msg_id.to_owned());
        }
        let references_str = if refs.is_empty() {
            None
        } else {
            Some(refs.join(" "))
        };

        let body = body_with_signature(&creds, raw_body.clone())?;
        let (final_html, inline_parts) = prepare_html_and_inline(
            &self.secrets,
            self.store.as_ref(),
            self.artifacts.as_ref(),
            &context.user_id,
            account.as_deref(),
            &creds,
            &raw_body,
            body_html,
        )?;

        let message = build_message_with_inline(
            &from,
            &to,
            &cc,
            &subject,
            &body,
            final_html.as_deref(),
            in_reply_to,
            references_str.as_deref(),
            &inline_parts,
            &[],
        )?;
        let mut all_rcpts = Vec::with_capacity(to.len() + cc.len());
        all_rcpts.extend(to.iter().cloned());
        all_rcpts.extend(cc.iter().cloned());
        smtp_send(&creds, &from, &all_rcpts, &message).await?;
        Ok(ToolOutput {
            content: format!(
                "Replied to {}: \"{}\" ({} bytes).",
                to.join(", "),
                clip(&subject, 120),
                message.len()
            ),
            data: Some(json!({
                "uid": uid,
                "to": to,
                "cc": cc,
                "subject": subject,
                "in_reply_to": in_reply_to,
            })),
            ..ToolOutput::default()
        })
    }
}

pub struct MailDraftTool {
    secrets: Arc<UserSecrets>,
    store: Option<Store>,
    artifacts: Option<Arc<ArtifactStore>>,
}

#[async_trait]
impl Tool for MailDraftTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: MAIL_DRAFT_TOOL.into(),
            description: "Save an email draft into the Drafts folder of the member's own IMAP \
                          mailbox, appending their HTML and plain-text signature. Does not send."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "to": {"type": "array", "items": {"type": "string"}, "description": "Recipient addresses"},
                    "cc": {"type": "array", "items": {"type": "string"}, "description": "Optional CC recipient addresses"},
                    "subject": {"type": "string"},
                    "body": {"type": "string", "description": "Plain-text body"},
                    "body_html": {"type": "string", "description": "Optional HTML body"},
                    "account": {"type": "string", "description": "Named mailbox connected in Integrations → Mail; omit for the default account"}
                },
                "required": ["to", "subject", "body"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let account = arg_account(&args);
        let creds = require_creds(&self.secrets, &context.user_id, account.as_deref())?;
        let to = arg_string_list(&args, "to");
        let cc = arg_string_list(&args, "cc");
        let subject = args
            .get("subject")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("subject is required".into()))?
            .to_owned();
        let raw_body = args
            .get("body")
            .and_then(Value::as_str)
            .ok_or_else(|| SeraError::Validation("body is required".into()))?
            .to_owned();
        let body_html = args.get("body_html").and_then(Value::as_str);

        let body = body_with_signature(&creds, raw_body.clone())?;
        let (final_html, inline_parts) = prepare_html_and_inline(
            &self.secrets,
            self.store.as_ref(),
            self.artifacts.as_ref(),
            &context.user_id,
            account.as_deref(),
            &creds,
            &raw_body,
            body_html,
        )?;

        let from = from_address(&creds);
        const PLACEHOLDER: &str = "draft-placeholder@serabot.invalid";
        let recipients: Vec<String> = if to.is_empty() {
            vec![PLACEHOLDER.to_owned()]
        } else {
            to.clone()
        };
        let raw = build_message_with_inline(
            &from,
            &recipients,
            &cc,
            &subject,
            &body,
            final_html.as_deref(),
            None,
            None,
            &inline_parts,
            &[],
        )?;
        let raw = if to.is_empty() {
            String::from_utf8_lossy(&raw)
                .replacen(&format!("To: {PLACEHOLDER}\r\n"), "", 1)
                .into_bytes()
        } else {
            raw
        };
        let draft_uid = imap_save_draft(&creds, "Drafts", &raw, None).await?;
        Ok(ToolOutput {
            content: format!(
                "Saved draft \"{}\" in Drafts{}.",
                clip(&subject, 120),
                draft_uid
                    .map(|u| format!(" (UID #{u})"))
                    .unwrap_or_default()
            ),
            data: Some(json!({
                "uid": draft_uid,
                "folder": "Drafts",
                "to": to,
                "cc": cc,
                "subject": subject,
            })),
            ..ToolOutput::default()
        })
    }
}

pub struct MailTriageTool {
    secrets: Arc<UserSecrets>,
}

#[async_trait]
impl Tool for MailTriageTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: MAIL_TRIAGE_TOOL.into(),
            description: "Triage the member's own IMAP mailbox in one call: returns connected \
                          account info, signature configuration status, folder list, INBOX unread \
                          count, and top unread message summaries. Read-only."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "account": {"type": "string", "description": "Named mailbox connected in Integrations → Mail; omit for the default account"},
                    "limit": {"type": "integer", "description": "Max unread messages to summarize, 1-30, default 15"}
                },
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let account = arg_account(&args);
        let creds = require_creds(&self.secrets, &context.user_id, account.as_deref())?;
        let sig_cfg = load_signature_config(&self.secrets, &context.user_id, account.as_deref())
            .unwrap_or_default();
        let has_signature_html = sig_cfg
            .signature_html
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty());
        let has_signature_logo = sig_cfg
            .signature_logo_hash
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty());
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| (n as u32).clamp(1, 30))
            .unwrap_or(15);

        let folders = imap_folders(&creds).await?;
        let unread_uids = imap_unread_uids(&creds, "INBOX").await?;
        let unread_count = unread_uids.len();
        let unread_messages = if unread_count > 0 {
            imap_search_preview(&creds, "INBOX", "is:unread", limit).await?
        } else {
            Vec::new()
        };

        let summaries: Vec<Value> = unread_messages
            .iter()
            .map(|m| {
                json!({
                    "uid": m.uid,
                    "from": m.from,
                    "subject": m.subject,
                    "date": m.date,
                    "snippet": m.snippet,
                })
            })
            .collect();

        let lines: Vec<String> = unread_messages
            .iter()
            .map(|m| {
                format!(
                    "#{} {} — {} ({}): {}",
                    m.uid,
                    clip(&m.from, 50),
                    clip(&m.subject, 80),
                    m.date,
                    clip(&m.snippet, 100)
                )
            })
            .collect();

        let account_label = account
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("default");
        let summary_text = format!(
            "Account: {} ({})\nSignature HTML: {}, Logo: {}\nFolders ({}): {}\nINBOX unread: {}\n{}",
            account_label,
            from_address(&creds),
            if has_signature_html {
                "configured"
            } else {
                "none"
            },
            if has_signature_logo {
                "configured"
            } else {
                "none"
            },
            folders.len(),
            folders.join(", "),
            unread_count,
            if lines.is_empty() {
                "No unread messages.".to_owned()
            } else {
                lines.join("\n")
            }
        );

        Ok(ToolOutput {
            content: summary_text,
            data: Some(json!({
                "account": account_label,
                "from": from_address(&creds),
                "username": creds.username,
                "signature_html_configured": has_signature_html,
                "signature_logo_configured": has_signature_logo,
                "folders": folders,
                "inbox_unread_count": unread_count,
                "unread": summaries,
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
    async fn smtp_send_handles_multiline_greeting_ehlo_serabot_local_and_auth_plain() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let expected_plain =
            base64::engine::general_purpose::STANDARD.encode(b"\0me@example.com\0app-password");
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = socket.into_split();
            let mut lines = BufReader::new(reader).lines();
            let mut saw_ehlo = None;
            let mut saw_auth = None;
            let mut data = String::new();
            writer
                .write_all(b"220-mail.hostinger.com ESMTP\r\n220 Ready\r\n")
                .await
                .unwrap();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.starts_with("EHLO") {
                    saw_ehlo = Some(line);
                    writer
                        .write_all(b"250-fake\r\n250-PIPELINING\r\n250 AUTH PLAIN LOGIN\r\n")
                        .await
                        .unwrap();
                } else if line.starts_with("AUTH PLAIN ") {
                    saw_auth = Some(line);
                    writer
                        .write_all(b"235 Authentication succeeded\r\n")
                        .await
                        .unwrap();
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
                    writer.write_all(b"250-queued\r\n250 ok\r\n").await.unwrap();
                } else if line == "QUIT" {
                    writer.write_all(b"221 bye\r\n").await.unwrap();
                    break;
                } else {
                    writer.write_all(b"500 unexpected\r\n").await.unwrap();
                }
            }
            (saw_ehlo, saw_auth, data)
        });
        let mut c = creds(Security::None);
        c.smtp_port = port;
        let message = build_message(
            "me@example.com",
            &["you@example.com".to_owned()],
            "Plain Auth",
            "Body\n",
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
        let (saw_ehlo, saw_auth, data) = server.await.unwrap();
        assert_eq!(saw_ehlo.as_deref(), Some("EHLO serabot.local"));
        assert_eq!(
            saw_auth.as_deref(),
            Some(format!("AUTH PLAIN {expected_plain}").as_str())
        );
        assert!(data.contains("Subject: Plain Auth"));
    }

    #[tokio::test]
    async fn smtp_test_handles_multiline_greeting_and_auth_plain() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let expected_plain =
            base64::engine::general_purpose::STANDARD.encode(b"\0me@example.com\0app-password");
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = socket.into_split();
            let mut lines = BufReader::new(reader).lines();
            writer
                .write_all(b"220-line1\r\n220-line2\r\n220 ready\r\n")
                .await
                .unwrap();
            let ehlo = lines.next_line().await.unwrap().unwrap();
            assert_eq!(ehlo, "EHLO serabot.local");
            writer
                .write_all(b"250-hello\r\n250 AUTH=LOGIN PLAIN\r\n")
                .await
                .unwrap();
            let auth = lines.next_line().await.unwrap().unwrap();
            assert_eq!(auth, format!("AUTH PLAIN {expected_plain}"));
            writer.write_all(b"235 ok\r\n").await.unwrap();
            let quit = lines.next_line().await.unwrap().unwrap();
            assert_eq!(quit, "QUIT");
        });
        let mut c = creds(Security::None);
        c.smtp_port = port;
        smtp_test(&c).await.unwrap();
        server.await.unwrap();
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

    #[test]
    fn parses_dovecot_and_hostinger_list_responses() {
        assert_eq!(
            parse_list_name("* LIST (\\HasNoChildren \\Sent) \".\" INBOX.Sent"),
            Some("INBOX.Sent".to_owned())
        );
        assert_eq!(
            parse_list_name("* LIST (\\HasNoChildren) \"/\" \"Sent Items\""),
            Some("Sent Items".to_owned())
        );
        assert_eq!(
            parse_list_name("* LIST (\\HasNoChildren) \".\" \"INBOX.My \\\"Folder\\\"\""),
            Some("INBOX.My \"Folder\"".to_owned())
        );
        assert_eq!(
            parse_list_name("* LIST () NIL INBOX"),
            Some("INBOX".to_owned())
        );
        assert_eq!(parse_list_name("* LIST (\\HasNoChildren) \".\" *"), None);
    }

    #[test]
    fn decodes_rfc2047_multiword_and_charsets() {
        // Adjacent encoded words collapse separating whitespace.
        assert_eq!(
            decode_rfc2047("=?UTF-8?B?SMOpbGxv?= =?utf-8?q?_World?="),
            "Héllo World"
        );
        // Non-encoded text keeps surrounding spaces.
        assert_eq!(
            decode_rfc2047("Re: =?iso-8859-1?Q?Caf=E9?= meeting"),
            "Re: Café meeting"
        );
        // Windows-1252 0x80 = Euro sign, 0x93/0x94 = curly quotes.
        assert_eq!(
            decode_rfc2047("=?windows-1252?Q?=80100_=93ok=94?="),
            "€100 “ok”"
        );
    }

    #[test]
    fn recursive_mime_parses_qp_sanitizes_html_and_rewrites_cid() {
        let png_bytes = b"\x89PNG\r\n\x1a\nfake";
        let png_b64 = base64::engine::general_purpose::STANDARD.encode(png_bytes);
        let raw = format!(
            "From: =?UTF-8?Q?Alice_M=C3=BCller?= <alice@example.com>\r\n\
             Subject: =?UTF-8?B?UmVwb3J0?= =?UTF-8?Q?_2026?=\r\n\
             Content-Type: multipart/mixed; boundary=\"mix\"\r\n\r\n\
             --mix\r\n\
             Content-Type: multipart/related; boundary=\"rel\"\r\n\r\n\
             --rel\r\n\
             Content-Type: multipart/alternative; boundary=\"alt\"\r\n\r\n\
             --alt\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\
             Content-Transfer-Encoding: quoted-printable\r\n\r\n\
             Hello =\r\nCaf=C3=A9\r\n\
             --alt\r\n\
             Content-Type: text/html; charset=utf-8\r\n\
             Content-Transfer-Encoding: quoted-printable\r\n\r\n\
             <html><head><style>body{{color:red}} @import url(https://evil.com); p{{width:expression(alert(1)); background:javascript:1}}</style></head><body>\
             <script>alert(1)</script>\
             <p onclick=\"evil()\">Hi</p>\
             <a href=\"javascript:alert(2)\">bad</a>\
             <a href=\"https://example.com\">good</a>\
             <img src=\"cid:logo@serabot\">\
             <img src=\"https://tracker.example.com/px.png\">\
             <div style=\"background-image: url(https://tracker.example.com/bg.png); color: blue\">x</div>\
             </body></html>\r\n\
             --alt--\r\n\
             --rel\r\n\
             Content-Type: image/png\r\n\
             Content-ID: <logo@serabot>\r\n\
             Content-Disposition: inline\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n\
             {png_b64}\r\n\
             --rel--\r\n\
             --mix\r\n\
             Content-Type: application/pdf\r\n\
             Content-Disposition: attachment; filename=\"report.pdf\"\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n\
             JVBERi0xLjQK\r\n\
             --mix--\r\n"
        );

        let blocked = parse_message_detail(1, vec![], &raw, false);
        assert_eq!(blocked.subject, "Report 2026");
        assert_eq!(blocked.from, "Alice Müller <alice@example.com>");
        assert_eq!(blocked.body_text, "Hello Café");
        assert!(blocked.has_remote_images);
        assert_eq!(blocked.attachments.len(), 1);
        assert_eq!(blocked.attachments[0].name, "report.pdf");
        let html = blocked.body_html.as_deref().unwrap();
        assert!(!html.contains("<script"));
        assert!(html.contains("<style>body{color:red}</style>"));
        assert!(!html.contains("@import"));
        assert!(!html.contains("expression("));
        assert!(!html.contains("onclick"));
        assert!(!html.contains("javascript:"));
        assert!(!html.contains("https://tracker.example.com/px.png"));
        assert!(!html.contains("https://tracker.example.com/bg.png"));
        assert!(html.contains(&format!("data:image/png;base64,{png_b64}")));
        assert!(html.contains("href=\"https://example.com\""));

        let allowed = parse_message_detail(1, vec![], &raw, true);
        let html_allowed = allowed.body_html.as_deref().unwrap();
        assert!(html_allowed.contains("https://tracker.example.com/px.png"));
    }

    #[test]
    fn builds_html_message_with_inline_logo_and_cc() {
        let msg = build_message_full(
            "me@example.com",
            &["to@example.com".into()],
            &["cc@example.com".into()],
            "HTML test",
            "Plain body",
            Some("<p>HTML body</p><img src=\"cid:signature-logo@serabot\">"),
            Some(("image/png", b"png-bytes")),
            &[],
        )
        .unwrap();
        let text = String::from_utf8(msg).unwrap();
        assert!(text.contains("Cc: cc@example.com"));
        assert!(!text.contains("Bcc:"));
        assert!(text.contains("multipart/related"));
        assert!(text.contains("multipart/alternative"));
        assert!(text.contains("Content-ID: <signature-logo@serabot>"));
        assert!(text.contains("<p>HTML body</p>"));
    }

    #[test]
    fn signature_config_roundtrips_without_password_reentry() {
        let store = crate::store::Store::open_in_memory(384).unwrap();
        let user = store
            .create_user("mail-sig", "sig@example.example", "Sig")
            .unwrap();
        let secrets =
            UserSecrets::with_master_key(store, [9u8; crate::user_secrets::KEY_BYTES]).unwrap();
        let c = creds(Security::None);
        store_credentials(&secrets, &user.id, None, &c).unwrap();
        store_signature_config(
            &secrets,
            &user.id,
            None,
            &MailSignatureConfig {
                signature: "Best, Alice".into(),
                signature_html: Some("<b>Best, Alice</b>".into()),
                signature_logo_hash: Some("a".repeat(64)),
            },
        )
        .unwrap();
        let accounts = list_accounts(&secrets, &user.id).unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].signature.as_deref(), Some("Best, Alice"));
        assert_eq!(
            accounts[0].signature_html.as_deref(),
            Some("<b>Best, Alice</b>")
        );
        assert_eq!(
            accounts[0].signature_logo_hash.as_deref(),
            Some("a".repeat(64).as_str())
        );
    }

    #[test]
    fn sanitize_style_sheet_preserves_safe_css_and_strips_dangerous_rules() {
        let css = "/* comment */ @charset \"utf-8\"; @import url(https://evil.com/a.css); \
                   .card { color: #222; position: fixed; behavior: url(x.htc); -moz-binding: url(y); \
                   background: url(https://tracker.example.com/bg.png); width: expression(1); } \
                   .overlay { background: url(//cdn.example.com/px.gif); } \
                   .bad { background: javascript:alert(1); color: vbscript:msgbox(1); } \
                   </style><script>alert(1)</script>";
        let mut has_remote = false;
        let cleaned = sanitize_style_sheet(css, false, &mut has_remote).unwrap();
        assert!(has_remote);
        assert!(cleaned.contains(".card{color: #222}"));
        assert!(!cleaned.contains("comment"));
        assert!(!cleaned.contains("@charset"));
        assert!(!cleaned.contains("@import"));
        assert!(!cleaned.contains("position: fixed"));
        assert!(!cleaned.contains("behavior:"));
        assert!(!cleaned.contains("-moz-binding"));
        assert!(!cleaned.contains("expression("));
        assert!(!cleaned.contains("javascript:"));
        assert!(!cleaned.contains("vbscript:"));
        assert!(!cleaned.contains("</style"));
        assert!(!cleaned.contains("https://tracker.example.com/bg.png"));
        assert!(!cleaned.contains("//cdn.example.com/px.gif"));

        // When only dangerous rules exist, returns None so no empty <style> is emitted.
        let mut has_remote2 = false;
        assert!(
            sanitize_style_sheet("@import 'x'; p{position:fixed}", false, &mut has_remote2)
                .is_none()
        );

        // Remote images preserved when allow_remote_images = true.
        let mut has_remote3 = false;
        let allowed = sanitize_style_sheet(
            ".bg{background:url(https://cdn.example.com/bg.png)}",
            true,
            &mut has_remote3,
        )
        .unwrap();
        assert!(has_remote3);
        assert!(allowed.contains("https://cdn.example.com/bg.png"));
    }

    #[test]
    fn extract_inline_data_images_rewrites_multiple_images_to_cid() {
        let png_b64 = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\n1");
        let jpg_b64 = base64::engine::general_purpose::STANDARD.encode(b"\xff\xd8\xff\xe0jpeg");
        let html = format!(
            "<p>One: <img alt=\"a\" src=\"DATA:IMAGE/PNG;BASE64,{png_b64}\"></p>\
             <p>Two: <img src='data:image/jpeg;base64,{jpg_b64}' width=\"10\"></p>\
             <p>Keep: <img src=\"https://example.com/x.png\"></p>"
        );
        let (rewritten, parts) = extract_inline_data_images(&html);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].content_id, "inline-0@serabot");
        assert_eq!(parts[0].content_type, "image/png");
        assert_eq!(parts[0].bytes, b"\x89PNG\r\n\x1a\n1");
        assert_eq!(parts[1].content_id, "inline-1@serabot");
        assert_eq!(parts[1].content_type, "image/jpeg");
        assert_eq!(parts[1].bytes, b"\xff\xd8\xff\xe0jpeg");
        assert!(rewritten.contains("src=\"cid:inline-0@serabot\""));
        assert!(rewritten.contains("src='cid:inline-1@serabot'"));
        assert!(rewritten.contains("https://example.com/x.png"));

        let raw = build_message_with_inline(
            "me@example.com",
            &["to@example.com".into()],
            &[],
            "Inline multi",
            "Plain",
            Some(&rewritten),
            None,
            None,
            &parts,
            &[],
        )
        .unwrap();
        let text = String::from_utf8(raw).unwrap();
        assert!(text.contains("Content-ID: <inline-0@serabot>"));
        assert!(text.contains("Content-ID: <inline-1@serabot>"));
        assert!(text.contains("multipart/related"));
    }

    #[test]
    fn build_message_with_inline_writes_in_reply_to_and_references() {
        let raw = build_message_with_inline(
            "me@example.com",
            &["to@example.com".into()],
            &[],
            "Re: Thread",
            "Reply text",
            None,
            Some("<msg-1@example.com>"),
            Some("<msg-0@example.com> <msg-1@example.com>"),
            &[],
            &[],
        )
        .unwrap();
        let text = String::from_utf8(raw).unwrap();
        assert!(text.contains("In-Reply-To: <msg-1@example.com>\r\n"));
        assert!(text.contains("References: <msg-0@example.com> <msg-1@example.com>\r\n"));
    }

    #[test]
    fn signature_html_accepts_64kib_and_rejects_oversize() {
        let store = crate::store::Store::open_in_memory(384).unwrap();
        let user = store
            .create_user("mail-sig-64k", "sig64k@example.example", "Sig64k")
            .unwrap();
        let secrets =
            UserSecrets::with_master_key(store, [11u8; crate::user_secrets::KEY_BYTES]).unwrap();
        let c = creds(Security::None);
        store_credentials(&secrets, &user.id, None, &c).unwrap();

        let ok_html = "a".repeat(MAX_SIGNATURE_HTML_CHARS);
        store_signature_config(
            &secrets,
            &user.id,
            None,
            &MailSignatureConfig {
                signature: String::new(),
                signature_html: Some(ok_html),
                signature_logo_hash: None,
            },
        )
        .unwrap();

        let too_big = "a".repeat(MAX_SIGNATURE_HTML_CHARS + 1);
        assert!(
            store_signature_config(
                &secrets,
                &user.id,
                None,
                &MailSignatureConfig {
                    signature: String::new(),
                    signature_html: Some(too_big),
                    signature_logo_hash: None,
                },
            )
            .is_err()
        );
    }

    #[test]
    fn mail_reply_draft_triage_and_send_tool_specs() {
        let store = crate::store::Store::open_in_memory(384).unwrap();
        let secrets = Arc::new(
            UserSecrets::with_master_key(store, [12u8; crate::user_secrets::KEY_BYTES]).unwrap(),
        );
        let mut reg = ToolRegistry::default();
        register(&mut reg, secrets).unwrap();

        let send = reg.get(MAIL_SEND_TOOL).unwrap().spec();
        assert!(send.external_effect);
        assert!(send.parameters["properties"]["body_html"].is_object());
        assert!(send.parameters["properties"]["cc"].is_object());
        assert!(send.parameters["properties"]["in_reply_to"].is_object());
        assert!(send.parameters["properties"]["references"].is_object());

        let reply = reg.get(MAIL_REPLY_TOOL).unwrap().spec();
        assert!(reply.external_effect);
        assert!(crate::gateway::is_destructive_tool(MAIL_REPLY_TOOL));
        assert_eq!(reply.parameters["required"], json!(["uid", "body"]));

        let draft = reg.get(MAIL_DRAFT_TOOL).unwrap().spec();
        assert!(!draft.external_effect);
        assert!(!crate::gateway::is_destructive_tool(MAIL_DRAFT_TOOL));
        assert_eq!(
            draft.parameters["required"],
            json!(["to", "subject", "body"])
        );

        let triage = reg.get(MAIL_TRIAGE_TOOL).unwrap().spec();
        assert!(!triage.external_effect);
        assert!(!crate::gateway::is_destructive_tool(MAIL_TRIAGE_TOOL));
    }

    #[test]
    fn load_inline_signature_images_resolves_uid_scoped_cid_refs_and_rejects_other_users() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open_in_memory(384).unwrap();
        let user_a = store
            .create_user("sig-img-a", "a@example.example", "Alice")
            .unwrap();
        let user_b = store
            .create_user("sig-img-b", "b@example.example", "Bob")
            .unwrap();
        let artifacts = ArtifactStore::new(dir.path()).unwrap();

        let meta_a = crate::maker::ArtifactMeta {
            title: "badge.png".into(),
            filename: "badge.png".into(),
            language: crate::domain::Language::En,
            source_artifact: None,
            created_at: chrono::Utc::now(),
        };
        let png_a = b"\x89PNG\r\n\x1a\nAAAA";
        let stored_a = artifacts.save(png_a, "badge.png", meta_a).unwrap();
        store
            .record_conversation_artifact(
                &user_a.id,
                "library",
                None,
                "upload",
                &stored_a.hash,
                &stored_a.filename,
                None,
            )
            .unwrap();

        let meta_b = crate::maker::ArtifactMeta {
            title: "secret.png".into(),
            filename: "secret.png".into(),
            language: crate::domain::Language::En,
            source_artifact: None,
            created_at: chrono::Utc::now(),
        };
        let png_b = b"\x89PNG\r\n\x1a\nBBBB";
        let stored_b = artifacts.save(png_b, "secret.png", meta_b).unwrap();
        store
            .record_conversation_artifact(
                &user_b.id,
                "library",
                None,
                "upload",
                &stored_b.hash,
                &stored_b.filename,
                None,
            )
            .unwrap();

        let sig_html = format!(
            "<table><tr><td><img src=\"cid:sig-img-{0}@serabot\"></td><td><img src=\"cid:sig-img-{0}@serabot\"></td></tr></table>",
            stored_a.hash
        );
        let cfg_a = MailSignatureConfig {
            signature: "Alice".into(),
            signature_html: Some(sig_html.clone()),
            signature_logo_hash: None,
        };
        let loaded = load_inline_signature_images(&store, &artifacts, &user_a.id, &cfg_a).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded[0].content_id,
            format!("sig-img-{}@serabot", stored_a.hash)
        );
        assert_eq!(loaded[0].content_type, "image/png");
        assert_eq!(loaded[0].bytes, png_a);

        let raw = build_message_with_inline(
            "a@example.example",
            &["to@example.example".into()],
            &[],
            "Sig test",
            "Alice",
            Some(&sig_html),
            None,
            None,
            &loaded,
            &[],
        )
        .unwrap();
        let text = String::from_utf8(raw).unwrap();
        assert!(text.contains(&format!("Content-ID: <sig-img-{}@serabot>", stored_a.hash)));

        // Cross-user reference must be rejected.
        let cfg_cross = MailSignatureConfig {
            signature: "Alice".into(),
            signature_html: Some(format!(
                "<img src=\"cid:sig-img-{}@serabot\">",
                stored_b.hash
            )),
            signature_logo_hash: None,
        };
        let err =
            load_inline_signature_images(&store, &artifacts, &user_a.id, &cfg_cross).unwrap_err();
        assert!(matches!(err, SeraError::Forbidden(_)));
    }
}
