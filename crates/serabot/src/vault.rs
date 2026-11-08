//! Per-user secret vault (0.14): named secrets any of the user's bots may use
//! through the server, never shown to the model.
//!
//! Storage: the value is sealed by [`UserSecrets`] under provider id
//! `vault:<name>` (envelope encryption, per-user DEK). The metadata (kind,
//! username, allowed domains, note, timestamps) lives in `vault_entries`
//! (migration 0015) and never holds a value. Every function takes the acting
//! user's id and addresses only that user's rows; there is no unscoped read.
//!
//! Model side: bots see names, kinds, usernames and domains through the
//! read-only `vault_list` tool. Use is by reference (`resolve_for_browser`),
//! and [`Redactor`] scrubs a user's values out of anything bound for the
//! model. Audit rows carry the entry name and host only.

use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use rusqlite::{OptionalExtension, params};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::cards::CardKind;
use crate::error::{Result, SeraError};
use crate::gateway::{Tool, ToolContext, ToolOutput, ToolRegistry, ToolSpec};
use crate::plans::clean_text;
use crate::policy::Policy;
use crate::user_secrets::UserSecrets;
use crate::vault_providers;
use std::sync::Arc;

/// Longest entry name.
pub const MAX_NAME_CHARS: usize = 48;
/// Longest accepted secret value, in characters.
pub const MAX_VALUE_CHARS: usize = 8192;
/// Values shorter than this are not redacted (too many false positives).
pub const MIN_REDACT_CHARS: usize = 4;
/// Entries per user.
pub const MAX_ENTRIES: usize = 64;
const MAX_DOMAINS: usize = 16;
const MAX_NOTE_CHARS: usize = 500;
const MAX_USERNAME_CHARS: usize = 256;

/// What a vault entry holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VaultKind {
    Password,
    ApiKey,
    Note,
}

impl VaultKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::ApiKey => "api_key",
            Self::Note => "note",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "password" => Ok(Self::Password),
            "api_key" => Ok(Self::ApiKey),
            "note" => Ok(Self::Note),
            _ => Err(SeraError::Validation(
                "kind must be password, api_key or note".into(),
            )),
        }
    }
}

/// Where an entry's value comes from. `Local` keeps the value sealed in
/// `user_credentials`. The other sources hold a reference to an item in the
/// owner's password manager; the value is read at fill time and never stored
/// (see `vault_providers`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VaultSource {
    #[default]
    Local,
    OnePassword,
    ProtonPass,
}

impl VaultSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::OnePassword => "onepassword",
            Self::ProtonPass => "protonpass",
        }
    }

    pub fn is_external(self) -> bool {
        self != Self::Local
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "local" => Ok(Self::Local),
            "onepassword" => Ok(Self::OnePassword),
            "protonpass" => Ok(Self::ProtonPass),
            _ => Err(SeraError::InvalidState(
                "a vault entry has an unknown source".into(),
            )),
        }
    }
}

/// Which part of an entry the browser wants to type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultField {
    /// The secret value (`password` and `value` are the same field).
    Value,
    /// The stored username (metadata, not secret).
    Username,
}

impl VaultField {
    /// Parse the browser tool's `field` argument: `password`, `value` or
    /// `username`.
    pub fn parse(field: &str) -> Result<Self> {
        match field {
            "password" | "value" => Ok(Self::Value),
            "username" => Ok(Self::Username),
            _ => Err(SeraError::Validation(
                "field must be password, value or username".into(),
            )),
        }
    }
}

/// One entry as shown to the owner and to the `vault_list` tool. Never a
/// value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VaultEntry {
    pub name: String,
    pub kind: VaultKind,
    pub source: VaultSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// The password manager item pointer for a non-local source. A pointer,
    /// not a value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    pub domains: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<String>,
}

/// A write request. `Debug` redacts the value.
#[derive(Clone)]
pub struct VaultInput {
    pub kind: VaultKind,
    pub username: Option<String>,
    /// The sealed value. Must be empty for a password-manager source.
    pub value: Zeroizing<String>,
    pub domains: Vec<String>,
    pub note: Option<String>,
    pub source: VaultSource,
    /// The item pointer. Required for a password-manager source, refused for
    /// a local one.
    pub reference: Option<String>,
}

impl std::fmt::Debug for VaultInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VaultInput")
            .field("kind", &self.kind)
            .field("username", &self.username)
            .field("value", &"<redacted>")
            .field("domains", &self.domains)
            .field("note", &self.note)
            .field("source", &self.source)
            .field("reference", &self.reference)
            .finish()
    }
}

/// `^[a-z0-9][a-z0-9-]{0,47}$`.
pub fn validate_name(name: &str) -> Result<()> {
    let bytes = name.as_bytes();
    let valid = !bytes.is_empty()
        && bytes.len() <= MAX_NAME_CHARS
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-');
    if !valid {
        return Err(SeraError::Validation(
            "name must match ^[a-z0-9][a-z0-9-]{0,47}$".into(),
        ));
    }
    Ok(())
}

/// A vault value: 1-8192 characters; tabs and newlines allowed, other
/// control characters not.
pub fn validate_value(value: &str) -> Result<()> {
    let chars = value.chars().count();
    if chars == 0 || chars > MAX_VALUE_CHARS {
        return Err(SeraError::Validation(format!(
            "value must be 1-{MAX_VALUE_CHARS} characters"
        )));
    }
    if value
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(SeraError::Validation(
            "value must not contain control characters".into(),
        ));
    }
    Ok(())
}

/// Normalize a host or an allow-list domain: trim, drop a trailing dot,
/// lower-case and IDN-encode (punycode). `None` for anything that is not a
/// plain host name or IP literal.
pub fn normalize_host(host: &str) -> Option<String> {
    let trimmed = host.trim().trim_end_matches('.');
    if trimmed.is_empty()
        || trimmed.len() > 253 * 4
        || trimmed.chars().any(|c| {
            c.is_whitespace() || c.is_control() || matches!(c, '/' | '\\' | '@' | '?' | '#' | '*')
        })
    {
        return None;
    }
    match url::Host::parse(trimmed).ok()? {
        url::Host::Domain(domain) => {
            let domain = domain.trim_end_matches('.').to_owned();
            (!domain.is_empty() && domain.len() <= 253).then_some(domain)
        }
        url::Host::Ipv4(ip) => Some(ip.to_string()),
        url::Host::Ipv6(ip) => Some(format!("[{ip}]")),
    }
}

/// Strip a trailing `:port` from a `host[:port]` string (not for IPv6
/// literals, which keep their brackets).
fn strip_port(host: &str) -> &str {
    let host = host.trim();
    if let Some((head, tail)) = host.rsplit_once(':')
        && !head.contains(':')
        && !tail.is_empty()
        && tail.bytes().all(|byte| byte.is_ascii_digit())
    {
        return head;
    }
    if let Some(end) = host.rfind("]:")
        && host.starts_with('[')
    {
        return &host[..=end];
    }
    host
}

/// Whether `host` is allowed by `domains`. An empty list allows any host.
/// `example.com` matches itself and any subdomain. Fails closed on a host
/// that cannot be normalized or an unparsable allow-list entry.
pub fn host_allowed(domains: &[String], host: &str) -> bool {
    if domains.is_empty() {
        return true;
    }
    let Some(host) = normalize_host(strip_port(host)) else {
        return false;
    };
    domains.iter().any(|domain| {
        let Some(domain) = normalize_host(domain) else {
            return false;
        };
        host == domain
            || (host.len() > domain.len()
                && host.ends_with(domain.as_str())
                && host.as_bytes()[host.len() - domain.len() - 1] == b'.')
    })
}

/// Whether an entry may be typed into a page on `host`. Unlike
/// [`host_allowed`], an empty list refuses: a fill needs a site the owner
/// named, so an entry without sites is never typed anywhere.
pub fn fill_host_allowed(domains: &[String], host: &str) -> bool {
    !domains.is_empty() && host_allowed(domains, host)
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn normalize_domains(domains: &[String]) -> Result<Vec<String>> {
    if domains.len() > MAX_DOMAINS {
        return Err(SeraError::Validation(format!(
            "at most {MAX_DOMAINS} domains per entry"
        )));
    }
    let mut out = Vec::new();
    for domain in domains {
        let normalized = normalize_host(domain)
            .filter(|host| !host.starts_with('['))
            .ok_or_else(|| SeraError::Validation("a domain is not a valid host name".into()))?;
        if !out.contains(&normalized) {
            out.push(normalized);
        }
    }
    Ok(out)
}

fn clean_optional(value: Option<&str>, max: usize, what: &str) -> Result<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    if value.chars().count() > max || value.chars().any(char::is_control) {
        return Err(SeraError::Validation(format!(
            "{what} must be at most {max} characters without control characters"
        )));
    }
    Ok(Some(value.to_owned()))
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<(VaultEntry, String)> {
    let kind: String = row.get(1)?;
    let domains_json: String = row.get(3)?;
    let source: String = row.get(8)?;
    Ok((
        VaultEntry {
            name: row.get(0)?,
            kind: VaultKind::Password, // replaced by the caller after parsing
            source: VaultSource::Local, // replaced by the caller after parsing
            username: row.get(2)?,
            reference: row.get(9)?,
            domains: Vec::new(),
            note: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
            last_used_at: row.get(7)?,
        },
        format!("{kind}\u{1f}{domains_json}\u{1f}{source}"),
    ))
}

fn finish_entry((mut entry, packed): (VaultEntry, String)) -> Result<VaultEntry> {
    let mut parts = packed.splitn(3, '\u{1f}');
    let kind = parts.next().unwrap_or_default();
    let domains_json = parts.next().unwrap_or("[]");
    let source = parts.next().unwrap_or("local");
    entry.kind = VaultKind::parse(kind)?;
    entry.source = VaultSource::parse(source)?;
    entry.domains = serde_json::from_str(domains_json)
        .map_err(|_| SeraError::InvalidState("a vault entry has invalid domains".into()))?;
    Ok(entry)
}

const COLUMNS: &str = "name, kind, username, domains_json, note, created_at, updated_at, last_used_at, source, reference";

/// The acting user's entries, sorted by name. Never a value.
pub fn list(users: &UserSecrets, user_id: &str) -> Result<Vec<VaultEntry>> {
    let connection = users.store().connection()?;
    let mut statement = connection.prepare(&format!(
        "SELECT {COLUMNS} FROM vault_entries WHERE user_id = ?1 ORDER BY name LIMIT {MAX_ENTRIES}"
    ))?;
    let rows = statement.query_map(params![user_id], row_to_entry)?;
    rows.map(|row| finish_entry(row?)).collect()
}

/// One of the acting user's entries.
pub fn get(users: &UserSecrets, user_id: &str, name: &str) -> Result<Option<VaultEntry>> {
    validate_name(name)?;
    let connection = users.store().connection()?;
    let row = connection
        .query_row(
            &format!("SELECT {COLUMNS} FROM vault_entries WHERE user_id = ?1 AND name = ?2"),
            params![user_id, name],
            row_to_entry,
        )
        .optional()?;
    row.map(finish_entry).transpose()
}

/// Create or replace the acting user's entry `name`. Re-putting keeps the
/// original `created_at`.
pub fn put(
    users: &UserSecrets,
    user_id: &str,
    name: &str,
    input: &VaultInput,
) -> Result<VaultEntry> {
    validate_name(name)?;
    let domains = normalize_domains(&input.domains)?;
    let username = clean_optional(input.username.as_deref(), MAX_USERNAME_CHARS, "username")?;
    let note = clean_optional(input.note.as_deref(), MAX_NOTE_CHARS, "note")?;
    if input.kind != VaultKind::Password && username.is_some() {
        return Err(SeraError::Validation(
            "only a password entry has a username".into(),
        ));
    }
    let reference = match input.source {
        VaultSource::Local => {
            validate_value(&input.value)?;
            if input.reference.is_some() {
                return Err(SeraError::Validation(
                    "a local entry holds a value, not a reference".into(),
                ));
            }
            None
        }
        source => {
            if !input.value.is_empty() {
                return Err(SeraError::Validation(
                    "a password manager entry holds a reference, not a value".into(),
                ));
            }
            if input.kind == VaultKind::Note {
                return Err(SeraError::Validation(
                    "a note cannot come from a password manager".into(),
                ));
            }
            let reference = input.reference.as_deref().ok_or_else(|| {
                SeraError::Validation("a password manager entry needs a reference".into())
            })?;
            vault_providers::validate_reference(source, reference)?;
            Some(reference.to_owned())
        }
    };
    let existing = get(users, user_id, name)?;
    if existing.is_none() && list(users, user_id)?.len() >= MAX_ENTRIES {
        return Err(SeraError::Validation(format!(
            "at most {MAX_ENTRIES} vault entries"
        )));
    }
    if input.source == VaultSource::Local {
        users.set_vault(user_id, name, &input.value)?;
    } else {
        // A reference holds no value: drop any sealed value left by an
        // earlier local entry of the same name.
        users.delete_vault(user_id, name)?;
    }
    let stamp = now();
    let created = existing
        .as_ref()
        .map(|entry| entry.created_at.clone())
        .unwrap_or_else(|| stamp.clone());
    let last_used = existing.and_then(|entry| entry.last_used_at);
    {
        let connection = users.store().connection()?;
        connection.execute(
            "INSERT INTO vault_entries(user_id, name, kind, username, domains_json, note,
                                       created_at, updated_at, last_used_at, source, reference)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(user_id, name) DO UPDATE SET
                 kind = excluded.kind, username = excluded.username,
                 domains_json = excluded.domains_json, note = excluded.note,
                 updated_at = excluded.updated_at, source = excluded.source,
                 reference = excluded.reference",
            params![
                user_id,
                name,
                input.kind.as_str(),
                username,
                serde_json::to_string(&domains)?,
                note,
                created,
                stamp,
                last_used,
                input.source.as_str(),
                reference,
            ],
        )?;
    }
    audit(users, user_id, "vault.set", name, None)?;
    get(users, user_id, name)?
        .ok_or_else(|| SeraError::Internal("the vault entry was not stored".into()))
}

/// Delete the acting user's entry. False when there was none.
pub fn delete(users: &UserSecrets, user_id: &str, name: &str) -> Result<bool> {
    validate_name(name)?;
    let removed_value = users.delete_vault(user_id, name)?;
    let removed_meta = {
        let connection = users.store().connection()?;
        connection.execute(
            "DELETE FROM vault_entries WHERE user_id = ?1 AND name = ?2",
            params![user_id, name],
        )? > 0
    };
    let removed = removed_value || removed_meta;
    if removed {
        audit(users, user_id, "vault.delete", name, None)?;
    }
    Ok(removed)
}

/// Audit row: entry name (and host for a resolve) only, never a value.
fn audit(
    users: &UserSecrets,
    user_id: &str,
    event: &str,
    name: &str,
    host: Option<&str>,
) -> Result<()> {
    let mut details = json!({"name": name});
    if let Some(host) = host {
        details["host"] = Value::String(host.to_owned());
    }
    users
        .store()
        .record_audit(user_id, event, None, None, None, None, None, Some(&details))?;
    Ok(())
}

/// Resolve vault secret `name` of `user_id` for typing into a page on `host`.
/// Fails closed: unknown name, a host outside the secret's allowed domains,
/// or a disabled vault are all errors. The value never leaves the server.
pub fn resolve_for_browser(
    users: &UserSecrets,
    user_id: &str,
    name: &str,
    host: &str,
) -> Result<Zeroizing<String>> {
    resolve_field_for_browser(users, user_id, name, host, VaultField::Value)
}

/// Like [`resolve_for_browser`], for a chosen field (`username` returns the
/// stored username, subject to the same domain check).
pub fn resolve_field_for_browser(
    users: &UserSecrets,
    user_id: &str,
    name: &str,
    host: &str,
    field: VaultField,
) -> Result<Zeroizing<String>> {
    let entry = authorize_fill(users, user_id, name, host, field)?;
    if entry.source.is_external() {
        return Err(SeraError::Validation(
            "this entry is read from a password manager when it is typed".into(),
        ));
    }
    let value = match field {
        VaultField::Username => username_of(&entry)?,
        VaultField::Value => local_value(users, user_id, name)?,
    };
    record_fill(users, user_id, name, host)?;
    Ok(value)
}

/// Checks that the acting user's entry `name` may be typed into a page on
/// `host` in `field`, and returns the entry. Nothing is decrypted or read
/// from a password manager. Fails closed: an unknown name, a host outside the
/// entry's sites (an empty list allows none), or a field the entry does not
/// hold. A refused host is audited.
pub fn authorize_fill(
    users: &UserSecrets,
    user_id: &str,
    name: &str,
    host: &str,
    field: VaultField,
) -> Result<VaultEntry> {
    validate_name(name)?;
    let entry = get(users, user_id, name)?
        .ok_or_else(|| SeraError::NotFound(format!("vault entry {name}")))?;
    if !fill_host_allowed(&entry.domains, host) {
        audit(users, user_id, "vault.resolve_denied", name, Some(host))?;
        return Err(SeraError::Forbidden(format!(
            "vault entry {name} is not allowed on this host"
        )));
    }
    match field {
        VaultField::Username => {
            username_of(&entry)?;
        }
        VaultField::Value if entry.kind == VaultKind::Note => {
            return Err(SeraError::Validation(
                "a note cannot be typed into a page".into(),
            ));
        }
        VaultField::Value => {}
    }
    Ok(entry)
}

/// The stored username of an entry, or NotFound when it has none.
pub(crate) fn username_of(entry: &VaultEntry) -> Result<Zeroizing<String>> {
    entry
        .username
        .clone()
        .map(Zeroizing::new)
        .ok_or_else(|| SeraError::NotFound(format!("vault entry {} has no username", entry.name)))
}

/// The sealed value of a local entry.
pub(crate) fn local_value(
    users: &UserSecrets,
    user_id: &str,
    name: &str,
) -> Result<Zeroizing<String>> {
    let secret = users
        .get_vault(user_id, name)?
        .ok_or_else(|| SeraError::NotFound(format!("vault entry {name}")))?;
    Ok(Zeroizing::new(secret.expose_secret().clone()))
}

/// Records a completed fill: the entry's last-used time and a `vault.resolve`
/// audit row with the name and host only.
pub(crate) fn record_fill(
    users: &UserSecrets,
    user_id: &str,
    name: &str,
    host: &str,
) -> Result<()> {
    {
        let connection = users.store().connection()?;
        connection.execute(
            "UPDATE vault_entries SET last_used_at = ?3 WHERE user_id = ?1 AND name = ?2",
            params![user_id, name, now()],
        )?;
    }
    audit(users, user_id, "vault.resolve", name, Some(host))
}

/// Replaces the acting user's vault values with `[vault:<name>]`.
///
/// Built per call from the user's own entries and from the values recently
/// read from their password managers, so a value is only ever scrubbed for
/// the user who owns it. Values shorter than [`MIN_REDACT_CHARS`] are skipped.
/// `Debug` shows names only.
pub struct Redactor {
    /// Longest value first, so a value that contains another wins.
    secrets: Vec<(String, Zeroizing<String>)>,
}

impl std::fmt::Debug for Redactor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.secrets.iter().map(|(name, _)| name.as_str()).collect();
        formatter
            .debug_struct("Redactor")
            .field("names", &names)
            .field("values", &"<redacted>")
            .finish()
    }
}

impl Redactor {
    /// Load the acting user's values: their sealed local values, and the
    /// values recently read from a password manager (`resolved`). A decrypt
    /// failure is an error: the caller must not forward unredacted text.
    pub fn for_user(
        users: &UserSecrets,
        user_id: &str,
        resolved: &vault_providers::ResolvedValues,
    ) -> Result<Self> {
        let mut secrets = Vec::new();
        for entry in list(users, user_id)? {
            let Some(value) = users.get_vault(user_id, &entry.name)? else {
                continue;
            };
            let value = value.expose_secret();
            if value.chars().count() >= MIN_REDACT_CHARS {
                secrets.push((entry.name, Zeroizing::new(value.clone())));
            }
        }
        for (name, value) in resolved.live(user_id) {
            if value.chars().count() >= MIN_REDACT_CHARS {
                secrets.push((name, value));
            }
        }
        secrets.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
        Ok(Self { secrets })
    }

    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }

    /// Replace every occurrence in one left-to-right pass, so a placeholder
    /// is never re-scanned.
    pub fn redact(&self, text: &str) -> String {
        if self.secrets.is_empty() {
            return text.to_owned();
        }
        let mut out = String::with_capacity(text.len());
        let mut index = 0;
        while index < text.len() {
            let rest = &text[index..];
            if let Some((name, value)) = self
                .secrets
                .iter()
                .find(|(_, value)| rest.starts_with(value.as_str()))
            {
                out.push_str("[vault:");
                out.push_str(name);
                out.push(']');
                index += value.len();
            } else {
                let c = rest.chars().next().unwrap_or_default();
                out.push(c);
                index += c.len_utf8().max(1);
            }
        }
        out
    }

    /// Redact every string inside a JSON value, in place.
    pub fn redact_json(&self, value: &mut Value) {
        match value {
            Value::String(text) => *text = self.redact(text),
            Value::Array(items) => items.iter_mut().for_each(|item| self.redact_json(item)),
            Value::Object(map) => map.values_mut().for_each(|item| self.redact_json(item)),
            _ => {}
        }
    }
}

/// Read-only bot tool: names, kinds, usernames and domains. Never a value.
pub struct VaultListTool {
    users: std::sync::Arc<UserSecrets>,
}

impl VaultListTool {
    pub fn new(users: std::sync::Arc<UserSecrets>) -> Self {
        Self { users }
    }
}

#[async_trait]
impl Tool for VaultListTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "vault_list".into(),
            description: "List the owner's vault entries (name, kind, username, allowed domains). \
                Values are never shown; use an entry by name with the browser type tool."
                .into(),
            parameters: json!({"type": "object", "properties": {}, "additionalProperties": false}),
            external_effect: false,
        }
    }

    async fn execute(&self, _args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let entries = list(&self.users, &context.user_id)?;
        let view: Vec<Value> = entries
            .iter()
            .map(|entry| {
                let mut item = json!({
                    "name": entry.name,
                    "kind": entry.kind,
                    "source": entry.source,
                    "domains": entry.domains,
                });
                if let Some(username) = &entry.username {
                    item["username"] = Value::String(username.clone());
                }
                item
            })
            .collect();
        let content = if view.is_empty() {
            "The vault is empty.".to_owned()
        } else {
            serde_json::to_string_pretty(&json!({"entries": view}))?
        };
        Ok(ToolOutput {
            content,
            data: Some(json!({"entries": view})),
            ..ToolOutput::default()
        })
    }
}

/// The bot tool that asks the owner for a secret in a card.
pub const REQUEST_SECRET_TOOL: &str = "vault_request_secret";
const MAX_REASON_CHARS: usize = 300;
const REQUEST_SECRET_NOTE: &str = "The user will enter the value in a secure card; you will never see it. End your turn and wait.";
const SECRET_PENDING_NOTE: &str =
    "A secret request is already waiting for the user. End your turn and wait.";
const SECRET_EXISTS_NOTE: &str =
    "An entry with this name already exists in the vault. Use it with browser_type by name.";
const SECRET_NO_CARD_NOTE: &str = "Secret requests cannot be shown in this chat. Ask the owner to add the entry in the desktop Vault.";

/// Validate a model-supplied secret request into its card payload. The
/// payload is checked again by the card store; this keeps the model's errors
/// specific. Sites are required for a password, and for an API key unless the
/// key is only provided for an API connector.
pub fn parse_secret_request(args: &Value) -> Result<Value> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    validate_name(&name)?;
    let kind = match args.get("kind").and_then(Value::as_str) {
        Some("password") => VaultKind::Password,
        Some("api_key") => VaultKind::ApiKey,
        Some("note") => VaultKind::Note,
        _ => {
            return Err(SeraError::Validation(
                "kind must be password, api_key or note".into(),
            ));
        }
    };
    let purpose = match args.get("purpose").and_then(Value::as_str) {
        Some(purpose @ ("save" | "provide")) => purpose,
        _ => {
            return Err(SeraError::Validation(
                "purpose must be save or provide".into(),
            ));
        }
    };
    let username = match args.get("username") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => {
            clean_optional(Some(text.as_str()), MAX_USERNAME_CHARS, "username")?
        }
        Some(_) => return Err(SeraError::Validation("username must be text".into())),
    };
    if username.is_some() && kind != VaultKind::Password {
        return Err(SeraError::Validation(
            "only a password has a username".into(),
        ));
    }
    let listed: Vec<String> = match args.get("domains") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| SeraError::Validation("domains must be text".into()))
            })
            .collect::<Result<_>>()?,
        Some(_) => return Err(SeraError::Validation("domains must be a list".into())),
    };
    let domains = normalize_domains(&listed)?;
    let sites_required = match kind {
        VaultKind::Password => true,
        VaultKind::ApiKey => purpose == "save",
        VaultKind::Note => false,
    };
    if sites_required && domains.is_empty() {
        return Err(SeraError::Validation(
            "name at least one site the secret is for".into(),
        ));
    }
    let reason = clean_text(
        args.get("reason")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        MAX_REASON_CHARS,
    );
    if reason.is_empty() {
        return Err(SeraError::Validation("reason is required".into()));
    }
    let mut payload = json!({
        "purpose": purpose,
        "name": name,
        "kind": kind.as_str(),
        "domains": domains,
        "reason": reason,
    });
    if let Some(username) = username {
        payload["username"] = Value::String(username);
    }
    Ok(payload)
}

/// `vault_request_secret`: ask the owner to type a credential into a secure
/// card. The value never passes through the model. Works in autonomy mode
/// too: a secret always needs the human.
pub struct VaultRequestSecretTool {
    users: Arc<UserSecrets>,
    policy: Arc<Policy>,
}

impl VaultRequestSecretTool {
    pub fn new(users: Arc<UserSecrets>, policy: Arc<Policy>) -> Self {
        Self { users, policy }
    }

    /// The owner-visible text of a request must pass the content policy.
    fn policy_allows(&self, payload: &Value) -> bool {
        let mut texts = vec![
            payload["name"].as_str().unwrap_or_default().to_owned(),
            payload["reason"].as_str().unwrap_or_default().to_owned(),
        ];
        texts.extend(payload["username"].as_str().map(str::to_owned));
        texts.extend(
            payload["domains"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
        texts.iter().all(|text| self.policy.is_allowed(text))
    }
}

fn secret_reply(value: Value) -> Result<ToolOutput> {
    Ok(ToolOutput {
        content: serde_json::to_string(&value)?,
        data: Some(value),
        ..ToolOutput::default()
    })
}

#[async_trait]
impl Tool for VaultRequestSecretTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: REQUEST_SECRET_TOOL.into(),
            description: "Ask the owner to enter a password or API key in a secure card in this chat. The value goes straight into the vault; you never see it. Use purpose save to store a new login, or provide to use an existing vault entry. Give the sites it is for (required for a password). It returns at once: end your turn and wait. Use the saved entry later with browser_type and secret=<name>.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "Vault name: lowercase letters, digits and hyphens, at most 48 characters"},
                    "kind": {"type": "string", "enum": ["password", "api_key", "note"]},
                    "username": {"type": "string", "description": "Login name, for a password (not secret)"},
                    "domains": {"type": "array", "items": {"type": "string"}, "maxItems": MAX_DOMAINS, "description": "Sites the secret is for, such as example.com"},
                    "reason": {"type": "string", "description": "Why it is needed, at most 300 characters"},
                    "purpose": {"type": "string", "enum": ["save", "provide"]}
                },
                "required": ["name", "kind", "reason", "purpose"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: Value, context: &ToolContext) -> Result<ToolOutput> {
        let conversation_id = context.conversation_id.as_deref().ok_or_else(|| {
            SeraError::Validation("vault_request_secret needs a conversation".into())
        })?;
        let user = context.user_id.as_str();
        // A Telegram chat has no card view, so no secret can reach the owner there.
        if matches!(
            self.users.store().conversation_origin(user, conversation_id),
            Ok(Some(origin)) if origin == crate::telegram::ORIGIN_TELEGRAM
        ) {
            return secret_reply(json!({"requested": false, "note": SECRET_NO_CARD_NOTE}));
        }
        let mut payload = parse_secret_request(&args)?;
        if !self.policy_allows(&payload) {
            return Err(SeraError::Validation(
                "secret request is withheld by content policy".into(),
            ));
        }
        let name = payload["name"].as_str().unwrap_or_default().to_owned();
        let existing = get(&self.users, user, &name)?;
        if payload["purpose"] == "provide" && existing.is_some() {
            return secret_reply(
                json!({"requested": false, "exists": true, "note": SECRET_EXISTS_NOTE}),
            );
        }
        if let Some(entry) = &existing
            && payload["purpose"] == "save"
        {
            // A password manager entry is never overwritten by a card.
            if entry.source.is_external() {
                return Err(SeraError::Validation(
                    "a password manager entry with this name already exists; choose another name"
                        .into(),
                ));
            }
            // The card says it replaces the entry, so the owner sees that.
            payload["replaces"] = json!(true);
        }
        let store = self.users.store();
        if let Some(pending) = store.pending_secret_request(user, conversation_id)? {
            return secret_reply(json!({
                "requested": true,
                "card_id": pending.id,
                "pending": true,
                "note": SECRET_PENDING_NOTE,
            }));
        }
        let card = store.create_card(
            user,
            conversation_id,
            &context.bot,
            CardKind::SecretRequest,
            &payload,
        )?;
        secret_reply(json!({"requested": true, "card_id": card.id, "note": REQUEST_SECRET_NOTE}))
    }
}

/// Register the vault tools with the gateway registry.
pub fn register_vault_tools(
    registry: &mut ToolRegistry,
    users: Arc<UserSecrets>,
    policy: Arc<Policy>,
) -> Result<()> {
    registry.register(VaultListTool::new(users.clone()))?;
    registry.register(VaultRequestSecretTool::new(users, policy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::user_secrets::KEY_BYTES;

    fn setup() -> (Store, UserSecrets, String, String) {
        let store = Store::open_in_memory(384).unwrap();
        let users = UserSecrets::with_master_key(store.clone(), [3u8; KEY_BYTES]).unwrap();
        let a = store
            .create_user("subject-a", "a@example.example", "Synthetic A")
            .unwrap()
            .id;
        let b = store
            .create_user("subject-b", "b@example.example", "Synthetic B")
            .unwrap()
            .id;
        (store, users, a, b)
    }

    fn password(value: &str, domains: &[&str]) -> VaultInput {
        VaultInput {
            kind: VaultKind::Password,
            username: Some("synthetic-user".into()),
            value: Zeroizing::new(value.into()),
            domains: domains.iter().map(|d| (*d).to_owned()).collect(),
            note: Some("synthetic note".into()),
            source: VaultSource::Local,
            reference: None,
        }
    }

    fn domains(list: &[&str]) -> Vec<String> {
        list.iter().map(|d| (*d).to_owned()).collect()
    }

    #[test]
    fn name_validation() {
        for good in ["a", "0", "site-login", &"a".repeat(48)] {
            validate_name(good).unwrap();
        }
        for bad in ["", "-a", "A", "a_b", "a b", "a:b", "é", &"a".repeat(49)] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn domain_match_exact_subdomain_and_mismatch() {
        let allow = domains(&["example.com"]);
        assert!(host_allowed(&allow, "example.com"));
        assert!(host_allowed(&allow, "login.example.com"));
        assert!(host_allowed(&allow, "a.b.example.com"));
        assert!(!host_allowed(&allow, "notexample.com"));
        assert!(!host_allowed(&allow, "example.com.evil.example"));
        assert!(!host_allowed(&allow, "com"));
        assert!(!host_allowed(&allow, ""));
        assert!(!host_allowed(&allow, "exa mple.com"));
    }

    #[test]
    fn domain_match_normalizes_case_dot_port_and_idn() {
        let allow = domains(&["Example.COM"]);
        assert!(host_allowed(&allow, "LOGIN.EXAMPLE.COM"));
        assert!(host_allowed(&allow, "example.com."));
        assert!(host_allowed(&allow, "example.com:8443"));
        let idn = domains(&["bücher.example"]);
        assert!(host_allowed(&idn, "xn--bcher-kva.example"));
        assert!(host_allowed(&idn, "shop.BÜCHER.example"));
        assert!(!host_allowed(&idn, "bucher.example"));
        let punycode = domains(&["xn--bcher-kva.example"]);
        assert!(host_allowed(&punycode, "bücher.example"));
    }

    #[test]
    fn empty_allow_list_means_any_host_and_bad_entries_fail_closed() {
        assert!(host_allowed(&[], "anything.example"));
        assert!(host_allowed(&[], ""));
        assert!(!host_allowed(&domains(&["*"]), "example.com"));
        assert!(!host_allowed(
            &domains(&["https://example.com"]),
            "example.com"
        ));
    }

    #[test]
    fn put_normalizes_domains_and_rejects_bad_ones() {
        let (_, users, a, _) = setup();
        let entry = put(
            &users,
            &a,
            "site",
            &password("synthetic-pw-1", &["Example.COM", "example.com"]),
        )
        .unwrap();
        assert_eq!(entry.domains, vec!["example.com".to_owned()]);
        for bad in ["*.example.com", "http://example.com", "exa mple", "a/b", ""] {
            assert!(
                put(&users, &a, "bad", &password("synthetic-pw-1", &[bad])).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn put_list_get_replace_and_delete() {
        let (_, users, a, _) = setup();
        let first = put(
            &users,
            &a,
            "site",
            &password("synthetic-pw-1", &["example.com"]),
        )
        .unwrap();
        assert_eq!(first.kind, VaultKind::Password);
        assert_eq!(first.username.as_deref(), Some("synthetic-user"));
        assert!(first.last_used_at.is_none());
        let mut replacement = password("synthetic-pw-2", &[]);
        replacement.username = None;
        let second = put(&users, &a, "site", &replacement).unwrap();
        assert_eq!(second.created_at, first.created_at);
        assert!(second.username.is_none());
        assert!(second.domains.is_empty());
        assert_eq!(list(&users, &a).unwrap().len(), 1);
        // No sites: a fill is refused, even on a host that would otherwise match.
        assert!(resolve_for_browser(&users, &a, "site", "any.example").is_err());
        let third = put(
            &users,
            &a,
            "site",
            &password("synthetic-pw-3", &["any.example"]),
        )
        .unwrap();
        assert_eq!(third.domains, vec!["any.example".to_owned()]);
        let value = resolve_for_browser(&users, &a, "site", "any.example").unwrap();
        assert_eq!(value.as_str(), "synthetic-pw-3");
        assert!(delete(&users, &a, "site").unwrap());
        assert!(!delete(&users, &a, "site").unwrap());
        assert!(list(&users, &a).unwrap().is_empty());
        assert!(resolve_for_browser(&users, &a, "site", "x.example").is_err());
    }

    #[test]
    fn value_and_kind_rules() {
        let (_, users, a, _) = setup();
        assert!(put(&users, &a, "k", &password("", &[])).is_err());
        assert!(put(&users, &a, "k", &password(&"x".repeat(8193), &[])).is_err());
        assert!(put(&users, &a, "k", &password("bad\u{0}value", &[])).is_err());
        put(&users, &a, "k", &password(&"x".repeat(8192), &[])).unwrap();
        let note = VaultInput {
            kind: VaultKind::Note,
            username: None,
            value: Zeroizing::new("line one\nline two".into()),
            domains: vec![],
            note: None,
            source: VaultSource::Local,
            reference: None,
        };
        put(&users, &a, "memo", &note).unwrap();
        assert!(resolve_for_browser(&users, &a, "memo", "example.com").is_err());
        let with_user = VaultInput {
            username: Some("u".into()),
            ..note.clone()
        };
        assert!(put(&users, &a, "memo2", &with_user).is_err());
    }

    #[test]
    fn resolve_denies_other_hosts_and_returns_username() {
        let (_, users, a, _) = setup();
        put(
            &users,
            &a,
            "site",
            &password("synthetic-pw-1", &["example.com"]),
        )
        .unwrap();
        assert!(resolve_for_browser(&users, &a, "site", "login.example.com").is_ok());
        assert!(matches!(
            resolve_for_browser(&users, &a, "site", "evil.example"),
            Err(SeraError::Forbidden(_))
        ));
        let user =
            resolve_field_for_browser(&users, &a, "site", "example.com", VaultField::Username)
                .unwrap();
        assert_eq!(user.as_str(), "synthetic-user");
        assert!(
            resolve_field_for_browser(&users, &a, "site", "evil.example", VaultField::Username)
                .is_err()
        );
        assert!(list(&users, &a).unwrap()[0].last_used_at.is_some());
        assert!(VaultField::parse("password").is_ok());
        assert!(VaultField::parse("other").is_err());
    }

    #[test]
    fn users_cannot_see_resolve_or_delete_each_others_entries() {
        let (_, users, a, b) = setup();
        put(
            &users,
            &a,
            "shared-name",
            &password("synthetic-pw-a", &["x.example"]),
        )
        .unwrap();
        assert!(list(&users, &b).unwrap().is_empty());
        assert!(get(&users, &b, "shared-name").unwrap().is_none());
        assert!(matches!(
            resolve_for_browser(&users, &b, "shared-name", "example.com"),
            Err(SeraError::NotFound(_))
        ));
        assert!(!delete(&users, &b, "shared-name").unwrap());
        assert_eq!(list(&users, &a).unwrap().len(), 1);
        // The same name for B is a separate entry with its own value.
        put(
            &users,
            &b,
            "shared-name",
            &password("synthetic-pw-b", &["x.example"]),
        )
        .unwrap();
        assert_eq!(
            resolve_for_browser(&users, &a, "shared-name", "x.example")
                .unwrap()
                .as_str(),
            "synthetic-pw-a"
        );
        assert_eq!(
            resolve_for_browser(&users, &b, "shared-name", "x.example")
                .unwrap()
                .as_str(),
            "synthetic-pw-b"
        );
        assert!(delete(&users, &b, "shared-name").unwrap());
        assert_eq!(list(&users, &a).unwrap().len(), 1);
    }

    #[test]
    fn nothing_in_the_database_or_audit_holds_the_value() {
        let (store, users, a, _) = setup();
        let secret = "synthetic-unique-secret-xyz";
        put(&users, &a, "site", &password(secret, &["example.com"])).unwrap();
        resolve_for_browser(&users, &a, "site", "example.com").unwrap();
        let _ = resolve_for_browser(&users, &a, "site", "evil.example");
        delete(&users, &a, "site").unwrap();
        let connection = store.connection().unwrap();
        for table in ["vault_entries", "audit_log"] {
            let mut statement = connection
                .prepare(&format!("SELECT * FROM {table}"))
                .unwrap();
            let columns = statement.column_count();
            let mut rows = statement.query([]).unwrap();
            while let Some(row) = rows.next().unwrap() {
                for column in 0..columns {
                    if let Ok(text) = row.get::<_, String>(column) {
                        assert!(!text.contains(secret), "{table} leaked the value");
                    }
                }
            }
        }
        let events: Vec<String> = connection
            .prepare("SELECT event FROM audit_log WHERE event LIKE 'vault.%' ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            events,
            [
                "vault.set",
                "vault.resolve",
                "vault.resolve_denied",
                "vault.delete"
            ]
        );
        let details: String = connection
            .query_row(
                "SELECT details_json FROM audit_log WHERE event = 'vault.resolve'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&details).unwrap(),
            json!({"name": "site", "host": "example.com"})
        );
    }

    #[test]
    fn vault_entries_are_not_listed_as_ai_providers() {
        let (_, users, a, _) = setup();
        put(&users, &a, "site", &password("synthetic-pw-1", &[])).unwrap();
        assert!(users.providers_for(&a).unwrap().is_empty());
        // The public credential API cannot reach a vault id.
        assert!(users.get(&a, "vault:site").is_err());
        assert!(users.set(&a, "vault:site", "x").is_err());
        assert!(users.delete(&a, "vault:site").is_err());
    }

    #[test]
    fn redactor_replaces_only_the_owners_values() {
        let (_, users, a, b) = setup();
        let none = vault_providers::ResolvedValues::default();
        put(&users, &a, "site", &password("synthetic-pw-a", &[])).unwrap();
        put(&users, &a, "tiny", &password("abc", &[])).unwrap();
        put(&users, &b, "other", &password("synthetic-pw-b", &[])).unwrap();
        let redactor = Redactor::for_user(&users, &a, &none).unwrap();
        assert_eq!(
            redactor.redact("login synthetic-pw-a then synthetic-pw-b, abc, synthetic-pw-a"),
            "login [vault:site] then synthetic-pw-b, abc, [vault:site]"
        );
        let mut value = json!({"a": ["x synthetic-pw-a"], "n": 1});
        redactor.redact_json(&mut value);
        assert_eq!(value, json!({"a": ["x [vault:site]"], "n": 1}));
        let debug = format!("{redactor:?}");
        assert!(debug.contains("site") && !debug.contains("synthetic-pw-a"));
        assert_eq!(
            Redactor::for_user(&users, &b, &none).unwrap().redact("abc"),
            "abc"
        );
    }

    #[test]
    fn redactor_prefers_the_longest_value_and_handles_multibyte() {
        let (_, users, a, _) = setup();
        let none = vault_providers::ResolvedValues::default();
        put(&users, &a, "short", &password("hunter22", &[])).unwrap();
        put(&users, &a, "long", &password("hunter22-extended", &[])).unwrap();
        let redactor = Redactor::for_user(&users, &a, &none).unwrap();
        assert_eq!(
            redactor.redact("é hunter22-extended ü hunter22"),
            "é [vault:long] ü [vault:short]"
        );
        assert!(
            Redactor::for_user(&users, "nobody", &none)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn debug_never_prints_a_value() {
        let input = password("synthetic-debug-secret", &[]);
        assert!(!format!("{input:?}").contains("synthetic-debug-secret"));
    }

    #[tokio::test]
    async fn vault_list_tool_shows_metadata_only_for_the_acting_user() {
        let (_, users, a, b) = setup();
        let users = std::sync::Arc::new(users);
        put(
            &users,
            &a,
            "site",
            &password("synthetic-tool-secret", &["example.com"]),
        )
        .unwrap();
        put(&users, &b, "theirs", &password("synthetic-b-secret", &[])).unwrap();
        let tool = VaultListTool::new(users);
        assert!(!tool.spec().external_effect);
        let context = ToolContext {
            user_id: a.clone(),
            actor: a,
            bot: "main".into(),
            task_id: None,
            turn_id: None,
            conversation_id: None,
            allowed_tools: Default::default(),
            require_approval: Default::default(),
        };
        let output = tool.execute(json!({}), &context).await.unwrap();
        assert!(output.content.contains("site") && output.content.contains("synthetic-user"));
        assert!(!output.content.contains("theirs"));
        let all = format!("{} {:?}", output.content, output.data);
        assert!(!all.contains("synthetic-tool-secret") && !all.contains("synthetic-b-secret"));
        assert!(!all.contains("synthetic note"));
    }

    #[test]
    fn a_reference_entry_stores_a_pointer_and_no_value() {
        let (_, users, a, _) = setup();
        let reference = VaultInput {
            kind: VaultKind::Password,
            username: Some("synthetic-user".into()),
            value: Zeroizing::new(String::new()),
            domains: domains(&["bank.example"]),
            note: None,
            source: VaultSource::OnePassword,
            reference: Some("op://Vault/Bank/password".into()),
        };
        let entry = put(&users, &a, "bank", &reference).unwrap();
        assert_eq!(entry.source, VaultSource::OnePassword);
        assert_eq!(entry.reference.as_deref(), Some("op://Vault/Bank/password"));
        assert!(users.get_vault(&a, "bank").unwrap().is_none());
        // A reference refuses a value, a note, a bad pointer, and a local source with a pointer.
        let with_value = VaultInput {
            value: Zeroizing::new("synthetic-value".into()),
            ..reference.clone()
        };
        assert!(put(&users, &a, "bank", &with_value).is_err());
        let note = VaultInput {
            kind: VaultKind::Note,
            ..reference.clone()
        };
        assert!(put(&users, &a, "bank", &note).is_err());
        let bad_pointer = VaultInput {
            reference: Some("op://Vault/Bank".into()),
            ..reference.clone()
        };
        assert!(put(&users, &a, "bank", &bad_pointer).is_err());
        let local_with_pointer = VaultInput {
            source: VaultSource::Local,
            value: Zeroizing::new("synthetic-value".into()),
            ..reference.clone()
        };
        assert!(put(&users, &a, "bank", &local_with_pointer).is_err());
        // Switching a local entry to a reference drops its sealed value.
        put(
            &users,
            &a,
            "site",
            &password("synthetic-pw-1", &["example.com"]),
        )
        .unwrap();
        let switched = VaultInput {
            source: VaultSource::ProtonPass,
            reference: Some("pass://Personal/site/password".into()),
            ..reference
        };
        put(&users, &a, "site", &switched).unwrap();
        assert!(users.get_vault(&a, "site").unwrap().is_none());
        assert_eq!(
            get(&users, &a, "site").unwrap().unwrap().source,
            VaultSource::ProtonPass
        );
    }

    #[test]
    fn fill_refuses_an_empty_site_list_and_a_note() {
        assert!(!fill_host_allowed(&[], "example.com"));
        assert!(!fill_host_allowed(&[], ""));
        assert!(fill_host_allowed(
            &domains(&["example.com"]),
            "a.example.com"
        ));
        let (_, users, a, _) = setup();
        put(
            &users,
            &a,
            "memo",
            &VaultInput {
                kind: VaultKind::Note,
                username: None,
                value: Zeroizing::new("synthetic memo".into()),
                domains: domains(&["example.com"]),
                note: None,
                source: VaultSource::Local,
                reference: None,
            },
        )
        .unwrap();
        assert!(matches!(
            authorize_fill(&users, &a, "memo", "example.com", VaultField::Value),
            Err(SeraError::Validation(_))
        ));
    }

    #[test]
    fn secret_request_rules_need_sites_except_where_allowed() {
        let ok = [
            json!({"name": "bank", "kind": "password", "domains": ["bank.example"], "reason": "log in", "purpose": "save", "username": "ada"}),
            json!({"name": "api-key", "kind": "api_key", "reason": "connect", "purpose": "provide"}),
            json!({"name": "memo", "kind": "note", "reason": "keep", "purpose": "save"}),
        ];
        for args in ok {
            parse_secret_request(&args).unwrap_or_else(|error| panic!("{args}: {error}"));
        }
        let bad = [
            json!({"name": "bank", "kind": "password", "reason": "log in", "purpose": "save"}),
            json!({"name": "api-key", "kind": "api_key", "reason": "connect", "purpose": "save"}),
            json!({"name": "Bad_Name", "kind": "password", "domains": ["a.example"], "reason": "r", "purpose": "save"}),
            json!({"name": "bank", "kind": "file", "domains": ["a.example"], "reason": "r", "purpose": "save"}),
            json!({"name": "bank", "kind": "password", "domains": ["a.example"], "reason": "r", "purpose": "steal"}),
            json!({"name": "bank", "kind": "password", "domains": ["https://a.example"], "reason": "r", "purpose": "save"}),
            json!({"name": "bank", "kind": "password", "domains": ["a.example"], "reason": "  ", "purpose": "save"}),
            json!({"name": "bank", "kind": "api_key", "domains": ["a.example"], "username": "u", "reason": "r", "purpose": "save"}),
        ];
        for args in bad {
            assert!(parse_secret_request(&args).is_err(), "{args}");
        }
    }

    fn secret_context(user: &str, conversation: &str) -> ToolContext {
        ToolContext {
            user_id: user.into(),
            actor: user.into(),
            bot: "main".into(),
            task_id: None,
            turn_id: None,
            conversation_id: Some(conversation.into()),
            allowed_tools: Default::default(),
            require_approval: Default::default(),
        }
    }

    #[tokio::test]
    async fn request_secret_card_is_one_per_conversation_and_never_holds_a_value() {
        let (store, users, a, _) = setup();
        let users = Arc::new(users);
        let tool = VaultRequestSecretTool::new(users.clone(), Arc::new(Policy::empty()));
        let args = json!({
            "name": "vendor-portal",
            "kind": "password",
            "username": "ada",
            "domains": ["vendor.example"],
            "reason": "Log in to the vendor portal.",
            "purpose": "save",
        });
        let first = tool
            .execute(args.clone(), &secret_context(&a, "dir:main"))
            .await
            .unwrap()
            .data
            .unwrap();
        assert_eq!(first["requested"], true);
        let card_id = first["card_id"].as_str().unwrap().to_owned();
        let second = tool
            .execute(
                json!({"name": "other", "kind": "password", "domains": ["x.example"], "reason": "r", "purpose": "save"}),
                &secret_context(&a, "dir:main"),
            )
            .await
            .unwrap()
            .data
            .unwrap();
        assert_eq!(second["card_id"], card_id);
        assert_eq!(second["pending"], true);
        let cards = store.list_cards(&a, "dir:main").unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].kind, CardKind::SecretRequest);
        assert_eq!(cards[0].payload["name"], "vendor-portal");
        let stored = cards[0].payload.to_string();
        assert!(!stored.contains("synthetic"));
    }

    #[tokio::test]
    async fn provide_of_an_existing_entry_is_refused_and_telegram_chats_are_too() {
        let (store, users, a, _) = setup();
        let users = Arc::new(users);
        put(
            &users,
            &a,
            "vendor",
            &password("synthetic-existing", &["vendor.example"]),
        )
        .unwrap();
        let tool = VaultRequestSecretTool::new(users.clone(), Arc::new(Policy::empty()));
        let out = tool
            .execute(
                json!({"name": "vendor", "kind": "password", "domains": ["vendor.example"], "reason": "use it", "purpose": "provide"}),
                &secret_context(&a, "dir:main"),
            )
            .await
            .unwrap()
            .data
            .unwrap();
        assert_eq!(out["requested"], false);
        assert_eq!(out["exists"], true);
        assert!(store.list_cards(&a, "dir:main").unwrap().is_empty());

        store
            .set_conversation_origin(&a, "20", crate::telegram::ORIGIN_TELEGRAM)
            .unwrap();
        let out = tool
            .execute(
                json!({"name": "fresh", "kind": "password", "domains": ["fresh.example"], "reason": "r", "purpose": "save"}),
                &secret_context(&a, "20"),
            )
            .await
            .unwrap()
            .data
            .unwrap();
        assert_eq!(out["requested"], false);
        assert!(store.list_cards(&a, "20").unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_save_over_an_existing_name_is_a_replacement_but_never_a_password_manager_entry() {
        let (store, users, a, _) = setup();
        let users = Arc::new(users);
        put(
            &users,
            &a,
            "vendor",
            &password("synthetic-existing", &["vendor.example"]),
        )
        .unwrap();
        put(
            &users,
            &a,
            "bank-login",
            &VaultInput {
                kind: VaultKind::Password,
                username: None,
                value: Zeroizing::new(String::new()),
                domains: vec!["bank.example".into()],
                note: None,
                source: VaultSource::OnePassword,
                reference: Some("op://Vault/Bank/password".into()),
            },
        )
        .unwrap();
        let tool = VaultRequestSecretTool::new(users.clone(), Arc::new(Policy::empty()));
        let save = |name: &str, site: &str| json!({"name": name, "kind": "password", "domains": [site], "reason": "log in", "purpose": "save"});
        // A local entry of that name: the card says it replaces the entry.
        let out = tool
            .execute(
                save("vendor", "vendor.example"),
                &secret_context(&a, "dir:replace"),
            )
            .await
            .unwrap()
            .data
            .unwrap();
        let card = store
            .get_card(&a, out["card_id"].as_str().unwrap())
            .unwrap();
        assert_eq!(card.payload["replaces"], true);
        // A password manager entry is never offered for replacement.
        assert!(matches!(
            tool.execute(
                save("bank-login", "bank.example"),
                &secret_context(&a, "dir:external")
            )
            .await,
            Err(SeraError::Validation(_))
        ));
        assert!(store.list_cards(&a, "dir:external").unwrap().is_empty());
        // A new name is not a replacement.
        let out = tool
            .execute(
                save("fresh", "fresh.example"),
                &secret_context(&a, "dir:fresh"),
            )
            .await
            .unwrap()
            .data
            .unwrap();
        let card = store
            .get_card(&a, out["card_id"].as_str().unwrap())
            .unwrap();
        assert!(card.payload.get("replaces").is_none());
    }

    #[tokio::test]
    async fn request_secret_needs_a_conversation_and_clean_policy_text() {
        let (store, users, a, _) = setup();
        let users = Arc::new(users);
        let tool = VaultRequestSecretTool::new(users, Arc::new(Policy::from_text("forbiddenterm")));
        let args = json!({"name": "bank", "kind": "password", "domains": ["bank.example"], "reason": "r", "purpose": "save"});
        let mut no_conversation = secret_context(&a, "dir:main");
        no_conversation.conversation_id = None;
        assert!(tool.execute(args.clone(), &no_conversation).await.is_err());
        let blocked = json!({"name": "bank", "kind": "password", "domains": ["bank.example"], "reason": "forbiddenterm", "purpose": "save"});
        assert!(matches!(
            tool.execute(blocked, &secret_context(&a, "dir:main")).await,
            Err(SeraError::Validation(_))
        ));
        assert!(store.list_cards(&a, "dir:main").unwrap().is_empty());
    }

    #[test]
    fn the_request_tool_is_not_a_destructive_tool() {
        assert!(!crate::gateway::is_destructive_tool(REQUEST_SECRET_TOOL));
    }
}
