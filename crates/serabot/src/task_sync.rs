//! Per-user sync between the Tasks tab and Google Tasks / Microsoft To Do /
//! Google Calendar / Outlook Calendar.
//!
//! Each user signs in once per provider family (Google or Microsoft) with
//! PKCE; the tokens live in that user's own envelope-encrypted credential store
//! under the provider ids below — never in the server-wide secrets. Sync is
//! opt-in per user (`user_task_prefs.sync_provider`): the chosen task provider
//! is pulled into `user_tasks` (linked by `remote_provider`/`remote_id`) and
//! new local tasks are pushed to it; the chosen calendar provider is read-only
//! and feeds the calendar view.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use secrecy::SecretString;
use serde_json::{Value, json};

use crate::error::{Result, SeraError};
use crate::personal_tasks::{
    self, PROVIDER_GOOGLE_CALENDAR, PROVIDER_GOOGLE_TASKS, PROVIDER_MICROSOFT_TODO,
    PROVIDER_OUTLOOK_CALENDAR,
};
use crate::store::Store;
use crate::user_secrets::{ProviderCredential, UserSecrets};

pub const GOOGLE_PROVIDER: &str = "tasks_google";
pub const MICROSOFT_PROVIDER: &str = "tasks_microsoft";

pub const GOOGLE_AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub const MICROSOFT_AUTHORIZE_URL: &str =
    "https://login.microsoftonline.com/common/oauth2/v2.0/authorize";
pub const MICROSOFT_TOKEN_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/token";

pub const GOOGLE_SCOPE: &str = "openid email https://www.googleapis.com/auth/tasks https://www.googleapis.com/auth/calendar.readonly";
pub const MICROSOFT_SCOPE: &str = "offline_access openid email Tasks.ReadWrite Calendars.Read";

const GOOGLE_TASKS_BASE: &str = "https://tasks.googleapis.com";
const GOOGLE_CALENDAR_BASE: &str = "https://www.googleapis.com/calendar/v3";
const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

const MAX_REMOTE_TASKS: usize = 100;
const MAX_EVENTS: usize = 250;
const MAX_BODY_BYTES: usize = 1024 * 1024;
const REFRESH_SKEW: Duration = Duration::seconds(60);

/// The sync providers this module can serve, mapped to the OAuth provider
/// family whose per-user sign-in spends them.
pub fn provider_family(sync_provider: &str) -> Option<&'static str> {
    match sync_provider {
        PROVIDER_GOOGLE_TASKS | PROVIDER_GOOGLE_CALENDAR => Some(GOOGLE_PROVIDER),
        PROVIDER_MICROSOFT_TODO | PROVIDER_OUTLOOK_CALENDAR => Some(MICROSOFT_PROVIDER),
        _ => None,
    }
}

pub fn validate_oauth_provider(provider: &str) -> Result<()> {
    if !matches!(provider, GOOGLE_PROVIDER | MICROSOFT_PROVIDER) {
        return Err(SeraError::Validation(
            "provider must be tasks_google or tasks_microsoft".into(),
        ));
    }
    Ok(())
}

fn authorize_url_for(provider: &str) -> &'static str {
    match provider {
        GOOGLE_PROVIDER => GOOGLE_AUTHORIZE_URL,
        _ => MICROSOFT_AUTHORIZE_URL,
    }
}

fn token_url_for(provider: &str) -> &'static str {
    match provider {
        GOOGLE_PROVIDER => GOOGLE_TOKEN_URL,
        _ => MICROSOFT_TOKEN_URL,
    }
}

fn scope_for(provider: &str) -> &'static str {
    match provider {
        GOOGLE_PROVIDER => GOOGLE_SCOPE,
        _ => MICROSOFT_SCOPE,
    }
}

fn pkce_ok(value: &str) -> bool {
    (43..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'~'))
}

fn state_ok(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && value.bytes().all(|b| b.is_ascii_graphic())
}

/// Only loopback redirects are accepted: the desktop console runs the browser
/// leg and the callback listener on the owner's machine.
pub fn validate_redirect_uri(uri: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(uri)
        .map_err(|_| SeraError::Validation("invalid redirect_uri".into()))?;
    let host = parsed.host_str().unwrap_or("");
    let loopback = parsed.scheme() == "http"
        && (host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost"))
        && parsed.port().is_some()
        && parsed.username().is_empty()
        && parsed.password().is_none();
    if !loopback {
        return Err(SeraError::Validation(
            "redirect_uri must be a loopback http URL".into(),
        ));
    }
    Ok(())
}

/// Build the provider's consent URL (PKCE S256 + state). The endpoint is
/// pinned to the provider's documented authorize URL.
pub fn authorize_url(
    provider: &str,
    client_id: &str,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
) -> Result<String> {
    validate_oauth_provider(provider)?;
    validate_redirect_uri(redirect_uri)?;
    if !pkce_ok(challenge) || !state_ok(state) {
        return Err(SeraError::Validation(
            "invalid PKCE challenge or state".into(),
        ));
    }
    if client_id.trim().is_empty() {
        return Err(SeraError::Config(
            "the sync client id is not configured".into(),
        ));
    }
    let mut url = reqwest::Url::parse(authorize_url_for(provider))
        .map_err(|_| SeraError::Config("invalid authorize URL".into()))?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("client_id", client_id.trim());
        query.append_pair("response_type", "code");
        query.append_pair("redirect_uri", redirect_uri);
        query.append_pair("scope", scope_for(provider));
        query.append_pair("code_challenge", challenge);
        query.append_pair("code_challenge_method", "S256");
        query.append_pair("state", state);
        if provider == GOOGLE_PROVIDER {
            query.append_pair("access_type", "offline");
            query.append_pair("include_granted_scopes", "true");
        }
    }
    Ok(url.to_string())
}

#[derive(Debug, Clone)]
pub struct TokenPair {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
}

#[allow(clippy::too_many_arguments)]
async fn token_request(
    http: &reqwest::Client,
    provider: &str,
    form: &[(&str, &str)],
) -> Result<TokenPair> {
    let url = reqwest::Url::parse(token_url_for(provider))
        .map_err(|_| SeraError::Config("invalid token URL".into()))?;
    let response = http
        .post(url)
        .form(form)
        .send()
        .await
        .map_err(|error| SeraError::External(format!("sync token request failed: {error}")))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| SeraError::External(format!("sync token read failed: {error}")))?
        .to_vec();
    if bytes.len() > MAX_BODY_BYTES {
        return Err(SeraError::External("sync token response too large".into()));
    }
    // The form carried the code/verifier/refresh token; only the short `error`
    // code may surface, never the form or the tokens.
    let parsed: Option<Value> = serde_json::from_slice(&bytes).ok();
    if !status.is_success() {
        let code = parsed
            .as_ref()
            .and_then(|value| value.get("error").and_then(Value::as_str))
            .unwrap_or("request_failed");
        return Err(SeraError::External(format!(
            "sync token request rejected (HTTP {}): {}",
            status.as_u16(),
            code.chars().take(200).collect::<String>()
        )));
    }
    let value =
        parsed.ok_or_else(|| SeraError::External("sync token response was not JSON".into()))?;
    let access = value
        .get("access_token")
        .and_then(Value::as_str)
        .unwrap_or("");
    if access.is_empty()
        || access.len() > MAX_BODY_BYTES
        || !access.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err(SeraError::External(
            "sync token response was rejected".into(),
        ));
    }
    let refresh = value
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|token| !token.is_empty());
    let expires_in = value
        .get("expires_in")
        .and_then(Value::as_u64)
        .filter(|seconds| *seconds <= 86_400 * 7);
    Ok(TokenPair {
        access_token: access.to_owned(),
        refresh_token: refresh,
        expires_in,
    })
}

pub async fn exchange_code(
    http: &reqwest::Client,
    provider: &str,
    client_id: &str,
    client_secret: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<TokenPair> {
    validate_oauth_provider(provider)?;
    validate_redirect_uri(redirect_uri)?;
    if !state_ok(code) || !pkce_ok(verifier) {
        return Err(SeraError::Validation(
            "invalid authorization code or verifier".into(),
        ));
    }
    token_request(
        http,
        provider,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", client_id.trim()),
            ("client_secret", client_secret),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", verifier),
        ],
    )
    .await
}

async fn refresh_grant(
    http: &reqwest::Client,
    provider: &str,
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<TokenPair> {
    token_request(
        http,
        provider,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", client_id.trim()),
            ("client_secret", client_secret),
            ("refresh_token", refresh_token),
            ("scope", scope_for(provider)),
        ],
    )
    .await
}

/// Store a fresh sign-in for one user under the provider id.
pub fn store_sign_in(
    users: &UserSecrets,
    user_id: &str,
    provider: &str,
    pair: &TokenPair,
) -> Result<()> {
    validate_oauth_provider(provider)?;
    let refresh = pair.refresh_token.clone().unwrap_or_default();
    let credential = ProviderCredential {
        api_key: pair.access_token.clone(),
        refresh_token: refresh,
        expires_at: pair
            .expires_in
            .map(|secs| (Utc::now() + Duration::seconds(secs as i64)).timestamp()),
        generation: Some(crate::domain::new_id()),
    };
    users.set(user_id, provider, &credential.encode()?)
}

pub fn sign_in_status(users: &UserSecrets, user_id: &str, provider: &str) -> Result<bool> {
    validate_oauth_provider(provider)?;
    Ok(users.get(user_id, provider)?.is_some())
}

pub fn disconnect(users: &UserSecrets, user_id: &str, provider: &str) -> Result<bool> {
    validate_oauth_provider(provider)?;
    users.delete(user_id, provider)
}

fn credential(users: &UserSecrets, user_id: &str, provider: &str) -> Result<ProviderCredential> {
    let stored = users.get(user_id, provider)?.ok_or_else(|| {
        SeraError::Validation("task sync is not signed in for this account".into())
    })?;
    ProviderCredential::parse(&stored)
}

/// The user's access token, renewed once when expired. The rotation is written
/// back to the same user's credential only.
pub async fn access_token(
    users: &UserSecrets,
    user_id: &str,
    provider: &str,
    client_id: &str,
    client_secret: &str,
    http: &reqwest::Client,
) -> Result<SecretString> {
    let mut cred = credential(users, user_id, provider)?;
    let expired = cred.expires_at.is_none_or(|at| {
        Utc::now() + REFRESH_SKEW >= DateTime::from_timestamp(at, 0).unwrap_or_default()
    });
    if expired {
        if cred.refresh_token.is_empty() {
            return Err(SeraError::Validation(
                "task sync sign-in expired; sign in again".into(),
            ));
        }
        let pair = refresh_grant(
            http,
            provider,
            client_id,
            client_secret,
            &cred.refresh_token,
        )
        .await?;
        let refresh = pair
            .refresh_token
            .clone()
            .unwrap_or_else(|| cred.refresh_token.clone());
        cred.merge_rotation(&refresh, Some(&pair.access_token))?;
        cred.expires_at = pair
            .expires_in
            .map(|secs| (Utc::now() + Duration::seconds(secs as i64)).timestamp());
        users.set(user_id, provider, &cred.encode()?)?;
    }
    Ok(SecretString::from(cred.api_key))
}

async fn get_json(
    http: &reqwest::Client,
    url: &str,
    token: &SecretString,
    accept: &str,
) -> Result<Value> {
    use secrecy::ExposeSecret;
    let response = http
        .get(url)
        .bearer_auth(token.expose_secret())
        .header("accept", accept)
        .send()
        .await
        .map_err(|error| SeraError::External(format!("task sync request failed: {error}")))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| SeraError::External(format!("task sync read failed: {error}")))?
        .to_vec();
    if bytes.len() > MAX_BODY_BYTES {
        return Err(SeraError::External("task sync response too large".into()));
    }
    if !status.is_success() {
        return Err(SeraError::External(format!(
            "task sync request rejected (HTTP {})",
            status.as_u16()
        )));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| SeraError::External("task sync response was not JSON".into()))
}

async fn send_json(
    http: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    token: &SecretString,
    body: Option<Value>,
) -> Result<Value> {
    use secrecy::ExposeSecret;
    let mut request = http
        .request(method, url)
        .bearer_auth(token.expose_secret())
        .header("accept", "application/json");
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request
        .send()
        .await
        .map_err(|error| SeraError::External(format!("task sync request failed: {error}")))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| SeraError::External(format!("task sync read failed: {error}")))?
        .to_vec();
    if bytes.len() > MAX_BODY_BYTES {
        return Err(SeraError::External("task sync response too large".into()));
    }
    if !status.is_success() {
        return Err(SeraError::External(format!(
            "task sync request rejected (HTTP {})",
            status.as_u16()
        )));
    }
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| SeraError::External("task sync response was not JSON".into()))
}

// --- remote → local mapping -------------------------------------------------

/// One normalized remote task, whatever the provider.
struct RemoteTask {
    remote_id: String,
    title: String,
    notes: String,
    completed: bool,
    due: Option<String>,
}

fn normalize_due_rfc3339(value: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(value.trim()).ok().map(|dt| {
        dt.with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Secs, true)
    })
}

fn google_tasks(value: &Value) -> Vec<RemoteTask> {
    value
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .take(MAX_REMOTE_TASKS)
                .filter_map(|item| {
                    let id = item.get("id")?.as_str()?.to_owned();
                    let title = item
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim();
                    if title.is_empty() || title.chars().count() > personal_tasks::MAX_TITLE_CHARS {
                        return None;
                    }
                    Some(RemoteTask {
                        remote_id: id,
                        title: title.to_owned(),
                        notes: item
                            .get("notes")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .trim()
                            .chars()
                            .take(personal_tasks::MAX_NOTES_CHARS)
                            .collect(),
                        completed: item.get("status").and_then(Value::as_str) == Some("completed"),
                        due: item
                            .get("due")
                            .and_then(Value::as_str)
                            .and_then(normalize_due_rfc3339),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn microsoft_tasks(value: &Value) -> Vec<RemoteTask> {
    value
        .get("value")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .take(MAX_REMOTE_TASKS)
                .filter_map(|item| {
                    let id = item.get("id")?.as_str()?.to_owned();
                    let title = item
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim();
                    if title.is_empty() || title.chars().count() > personal_tasks::MAX_TITLE_CHARS {
                        return None;
                    }
                    let notes = item
                        .get("body")
                        .and_then(|body| body.get("content"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim()
                        .chars()
                        .take(personal_tasks::MAX_NOTES_CHARS)
                        .collect();
                    let due = item
                        .get("dueDateTime")
                        .and_then(|due| due.get("dateTime"))
                        .and_then(Value::as_str)
                        .and_then(|raw| {
                            // Graph sends a floating local time; treat it as UTC.
                            chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f")
                                .ok()
                                .map(|dt| dt.and_utc().to_rfc3339_opts(SecondsFormat::Secs, true))
                                .or_else(|| normalize_due_rfc3339(raw))
                        });
                    Some(RemoteTask {
                        remote_id: id,
                        title: title.to_owned(),
                        notes,
                        completed: item.get("status").and_then(Value::as_str) == Some("completed"),
                        due,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn remote_status(completed: bool) -> &'static str {
    if completed {
        personal_tasks::STATUS_DONE
    } else {
        personal_tasks::STATUS_TODO
    }
}

/// Pull the provider's tasks into the user's `user_tasks`: existing remote
/// links are updated, new remote tasks are inserted.
pub async fn pull_tasks(
    store: &Store,
    user_id: &str,
    sync_provider: &str,
    token: &SecretString,
    http: &reqwest::Client,
) -> Result<SyncReport> {
    pull_tasks_at(
        store,
        user_id,
        sync_provider,
        token,
        http,
        GOOGLE_TASKS_BASE,
        GRAPH_BASE,
    )
    .await
}

async fn pull_tasks_at(
    store: &Store,
    user_id: &str,
    sync_provider: &str,
    token: &SecretString,
    http: &reqwest::Client,
    google_base: &str,
    graph_base: &str,
) -> Result<SyncReport> {
    let remote: Vec<RemoteTask> = match sync_provider {
        PROVIDER_GOOGLE_TASKS => {
            let url = format!(
                "{google_base}/tasks/v1/lists/@default/tasks?showCompleted=true&showHidden=false&maxResults={MAX_REMOTE_TASKS}"
            );
            google_tasks(&get_json(http, &url, token, "application/json").await?)
        }
        PROVIDER_MICROSOFT_TODO => {
            let lists = get_json(
                http,
                &format!("{graph_base}/me/todo/lists"),
                token,
                "application/json",
            )
            .await?;
            let list_id = lists
                .get("value")
                .and_then(Value::as_array)
                .and_then(|lists| {
                    lists
                        .iter()
                        .find(|list| {
                            list.get("wellKnownListName").and_then(Value::as_str) == Some("default")
                        })
                        .or_else(|| lists.first())
                        .and_then(|list| list.get("id").and_then(Value::as_str))
                        .map(str::to_owned)
                });
            let Some(list_id) = list_id else {
                return Ok(SyncReport::default());
            };
            let url = format!("{graph_base}/me/todo/lists/{list_id}/tasks?$top={MAX_REMOTE_TASKS}");
            microsoft_tasks(&get_json(http, &url, token, "application/json").await?)
        }
        _ => return Err(SeraError::Validation("not a task sync provider".into())),
    };
    let mut report = SyncReport::default();
    for task in remote {
        match personal_tasks::find_remote(store, user_id, sync_provider, &task.remote_id)? {
            Some(existing) => {
                let status = remote_status(task.completed).to_owned();
                if existing.status != status
                    || existing.title != task.title
                    || existing.due != task.due
                {
                    personal_tasks::update(
                        store,
                        user_id,
                        &existing.id,
                        &personal_tasks::TaskPatch {
                            title: Some(task.title),
                            notes: Some(task.notes),
                            status: Some(status),
                            due: Some(task.due),
                            position: None,
                        },
                    )?;
                    report.updated += 1;
                } else {
                    report.unchanged += 1;
                }
            }
            None => {
                let created = personal_tasks::create(
                    store,
                    user_id,
                    &personal_tasks::NewTask {
                        title: task.title,
                        notes: Some(task.notes),
                        status: Some(remote_status(task.completed).to_owned()),
                        due: task.due,
                    },
                )?;
                personal_tasks::link_remote(
                    store,
                    user_id,
                    &created.id,
                    sync_provider,
                    &task.remote_id,
                )?;
                report.imported += 1;
            }
        }
    }
    Ok(report)
}

/// Push local tasks that have no remote counterpart yet: create them at the
/// provider and link the local row to the new remote id.
pub async fn push_new_tasks(
    store: &Store,
    user_id: &str,
    sync_provider: &str,
    token: &SecretString,
    http: &reqwest::Client,
) -> Result<SyncReport> {
    push_new_tasks_at(
        store,
        user_id,
        sync_provider,
        token,
        http,
        GOOGLE_TASKS_BASE,
        GRAPH_BASE,
    )
    .await
}

async fn push_new_tasks_at(
    store: &Store,
    user_id: &str,
    sync_provider: &str,
    token: &SecretString,
    http: &reqwest::Client,
    google_base: &str,
    graph_base: &str,
) -> Result<SyncReport> {
    let locals = personal_tasks::list(
        store,
        user_id,
        None,
        None,
        None,
        personal_tasks::MAX_LIST_LIMIT,
    )?
    .into_iter()
    .filter(|task| task.remote_provider.as_deref().is_none() || task.remote_id.as_deref().is_none())
    .collect::<Vec<_>>();
    let mut report = SyncReport::default();
    for task in locals {
        let remote_id = match sync_provider {
            PROVIDER_GOOGLE_TASKS => {
                let body = json!({
                    "title": task.title,
                    "notes": task.notes,
                    "status": if task.status == personal_tasks::STATUS_DONE { "completed" } else { "needsAction" },
                    "due": task.due,
                });
                let url = format!("{google_base}/tasks/v1/lists/@default/tasks");
                let created =
                    send_json(http, reqwest::Method::POST, &url, token, Some(body)).await?;
                created.get("id").and_then(Value::as_str).map(str::to_owned)
            }
            PROVIDER_MICROSOFT_TODO => {
                let lists = get_json(
                    http,
                    &format!("{graph_base}/me/todo/lists"),
                    token,
                    "application/json",
                )
                .await?;
                let list_id = lists
                    .get("value")
                    .and_then(Value::as_array)
                    .and_then(|lists| {
                        lists
                            .iter()
                            .find(|list| {
                                list.get("wellKnownListName").and_then(Value::as_str)
                                    == Some("default")
                            })
                            .or_else(|| lists.first())
                            .and_then(|list| list.get("id").and_then(Value::as_str))
                            .map(str::to_owned)
                    });
                let Some(list_id) = list_id else {
                    continue;
                };
                let body = json!({
                    "title": task.title,
                    "body": {"content": task.notes, "contentType": "text"},
                    "status": if task.status == personal_tasks::STATUS_DONE { "completed" } else { "notStarted" },
                    "dueDateTime": task.due.as_ref().map(|due| json!({"dateTime": due, "timeZone": "UTC"})),
                });
                let url = format!("{graph_base}/me/todo/lists/{list_id}/tasks");
                let created =
                    send_json(http, reqwest::Method::POST, &url, token, Some(body)).await?;
                created.get("id").and_then(Value::as_str).map(str::to_owned)
            }
            _ => return Err(SeraError::Validation("not a task sync provider".into())),
        };
        if let Some(remote_id) = remote_id {
            personal_tasks::link_remote(store, user_id, &task.id, sync_provider, &remote_id)?;
            report.pushed += 1;
        }
    }
    Ok(report)
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct SyncReport {
    pub imported: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub pushed: usize,
}

/// Read-only calendar events in `[from, to]` (RFC 3339) from the chosen
/// calendar provider, normalized for the calendar view.
pub async fn calendar_events(
    user_id: &str,
    sync_provider: &str,
    from: &str,
    to: &str,
    token: &SecretString,
    http: &reqwest::Client,
) -> Result<Vec<Value>> {
    calendar_events_at(
        user_id,
        sync_provider,
        from,
        to,
        token,
        http,
        GOOGLE_CALENDAR_BASE,
        GRAPH_BASE,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn calendar_events_at(
    _user_id: &str,
    sync_provider: &str,
    from: &str,
    to: &str,
    token: &SecretString,
    http: &reqwest::Client,
    google_base: &str,
    graph_base: &str,
) -> Result<Vec<Value>> {
    let from = personal_tasks::normalize_due(from)?;
    let to = personal_tasks::normalize_due(to)?;
    let raw = match sync_provider {
        PROVIDER_GOOGLE_CALENDAR => {
            let url = format!(
                "{google_base}/calendars/primary/events?singleEvents=true&orderBy=startTime&timeMin={}&timeMax={}&maxResults={MAX_EVENTS}",
                urlencoding(from.as_str()),
                urlencoding(to.as_str())
            );
            get_json(http, &url, token, "application/json").await?
        }
        PROVIDER_OUTLOOK_CALENDAR => {
            let url = format!(
                "{graph_base}/me/events?$select=id,subject,start,end,isAllDay&$orderby=start/dateTime&startDateTime={}&endDateTime={}&$top={MAX_EVENTS}",
                urlencoding(from.as_str()),
                urlencoding(to.as_str())
            );
            get_json(http, &url, token, "application/json").await?
        }
        _ => return Err(SeraError::Validation("not a calendar provider".into())),
    };
    let items = match sync_provider {
        PROVIDER_GOOGLE_CALENDAR => raw.get("items").and_then(Value::as_array),
        _ => raw.get("value").and_then(Value::as_array),
    };
    let mut events = Vec::new();
    if let Some(items) = items {
        for item in items.iter().take(MAX_EVENTS) {
            let (id, title, start, end, all_day) = match sync_provider {
                PROVIDER_GOOGLE_CALENDAR => {
                    let start = item.get("start");
                    let end = item.get("end");
                    let all_day = start.and_then(|s| s.get("date")).is_some();
                    (
                        item.get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        item.get("summary")
                            .and_then(Value::as_str)
                            .unwrap_or("(busy)")
                            .to_owned(),
                        start
                            .and_then(|s| s.get("dateTime").or_else(|| s.get("date")))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        end.and_then(|e| e.get("dateTime").or_else(|| e.get("date")))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        all_day,
                    )
                }
                _ => (
                    item.get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    item.get("subject")
                        .and_then(Value::as_str)
                        .unwrap_or("(busy)")
                        .to_owned(),
                    item.get("start")
                        .and_then(|s| s.get("dateTime"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    item.get("end")
                        .and_then(|e| e.get("dateTime"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    item.get("isAllDay")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                ),
            };
            if id.is_empty() || start.is_empty() {
                continue;
            }
            events.push(json!({
                "id": id,
                "title": title.chars().take(200).collect::<String>(),
                "start": start,
                "end": end,
                "all_day": all_day,
                "provider": sync_provider,
            }));
        }
    }
    Ok(events)
}

fn urlencoding(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use secrecy::SecretString;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn store() -> Store {
        let store = Store::open_in_memory(384).unwrap();
        let connection = store.connection().unwrap();
        for id in ["u1", "u2"] {
            connection
                .execute(
                    "INSERT INTO users(id, provider_subject, email, display_name, role, status, created_at)
                     VALUES (?1, ?2, ?3, ?4, 'member', 'active', ?5)",
                    rusqlite::params![
                        id,
                        format!("microsoft:{id}"),
                        format!("{id}@example.test"),
                        id.to_uppercase(),
                        crate::store::now_string()
                    ],
                )
                .unwrap();
        }
        store
    }

    fn token() -> SecretString {
        SecretString::from("access-token".to_owned())
    }

    #[test]
    fn authorize_url_is_pinned_and_carries_pkce() {
        let url = authorize_url(
            GOOGLE_PROVIDER,
            "client-123",
            "http://127.0.0.1:51122/callback",
            &"a".repeat(43),
            "state-1",
        )
        .unwrap();
        let parsed = reqwest::Url::parse(&url).unwrap();
        assert_eq!(parsed.host_str(), Some("accounts.google.com"));
        let query: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["response_type"], "code");
        assert!(query["scope"].contains("auth/tasks"));
        assert!(
            authorize_url(
                GOOGLE_PROVIDER,
                "client-123",
                "https://evil.example/cb",
                &"a".repeat(43),
                "state-1"
            )
            .is_err()
        );
        assert!(
            authorize_url(
                "tasks_icloud",
                "client-123",
                "http://127.0.0.1:51122/callback",
                &"a".repeat(43),
                "state-1"
            )
            .is_err()
        );
        let ms = authorize_url(
            MICROSOFT_PROVIDER,
            "client-123",
            "http://localhost:51122/callback",
            &"a".repeat(43),
            "state-1",
        )
        .unwrap();
        assert!(ms.contains("login.microsoftonline.com"));
        assert!(ms.contains("Tasks.ReadWrite"));
    }

    #[test]
    fn google_remote_tasks_are_normalized() {
        let value = json!({
            "items": [
                {"id": "g1", "title": "Call Anna", "notes": "urgent", "status": "needsAction", "due": "2026-11-09T10:00:00.000Z"},
                {"id": "g2", "title": "Done thing", "status": "completed"},
                {"id": "g3", "title": ""},
                {"title": "no id"}
            ]
        });
        let tasks = google_tasks(&value);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].remote_id, "g1");
        assert!(!tasks[0].completed);
        assert_eq!(tasks[0].due.as_deref(), Some("2026-11-09T10:00:00Z"));
        assert!(tasks[1].completed);
    }

    #[test]
    fn microsoft_remote_tasks_are_normalized() {
        let value = json!({
            "value": [
                {"id": "m1", "title": "Review PR", "body": {"content": "look"}, "status": "inProgress", "dueDateTime": {"dateTime": "2026-11-10T09:00:00.0000000", "timeZone": "UTC"}},
                {"id": "m2", "title": "Old", "status": "completed"}
            ]
        });
        let tasks = microsoft_tasks(&value);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].due.as_deref(), Some("2026-11-10T09:00:00Z"));
        assert!(!tasks[0].completed);
        assert!(tasks[1].completed);
    }

    #[tokio::test]
    async fn pull_imports_then_updates_remote_changes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/tasks/v1/lists/@default/tasks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    {"id": "g1", "title": "Call Anna", "status": "needsAction"},
                    {"id": "g2", "title": "Old", "status": "needsAction"}
                ]
            })))
            .mount(&server)
            .await;
        let store = store();
        let http = reqwest::Client::new();
        let report = pull_tasks_at(
            &store,
            "u1",
            PROVIDER_GOOGLE_TASKS,
            &token(),
            &http,
            &server.uri(),
            &server.uri(),
        )
        .await
        .unwrap();
        assert_eq!(report.imported, 2);
        // Second pull with a changed remote task updates the linked row.
        let server2 = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/tasks/v1/lists/@default/tasks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [{"id": "g1", "title": "Call Anna", "status": "completed"}]
            })))
            .mount(&server2)
            .await;
        let report = pull_tasks_at(
            &store,
            "u1",
            PROVIDER_GOOGLE_TASKS,
            &token(),
            &http,
            &server2.uri(),
            &server2.uri(),
        )
        .await
        .unwrap();
        assert_eq!(report.updated, 1);
        assert_eq!(report.imported, 0);
        let tasks = personal_tasks::list(&store, "u1", None, None, None, 100).unwrap();
        let g1 = tasks
            .iter()
            .find(|t| t.remote_id.as_deref() == Some("g1"))
            .unwrap();
        assert_eq!(g1.status, "done");
        // Rows stay scoped to the user.
        assert!(
            personal_tasks::list(&store, "u2", None, None, None, 100)
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn push_creates_remote_tasks_and_links_them() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/tasks/v1/lists/@default/tasks"))
            .and(body_string_contains("Buy milk"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "g-new"})))
            .mount(&server)
            .await;
        let store = store();
        let local = personal_tasks::create(
            &store,
            "u1",
            &personal_tasks::NewTask {
                title: "Buy milk".into(),
                notes: None,
                status: None,
                due: None,
            },
        )
        .unwrap();
        let report = push_new_tasks_at(
            &store,
            "u1",
            PROVIDER_GOOGLE_TASKS,
            &token(),
            &reqwest::Client::new(),
            &server.uri(),
            &server.uri(),
        )
        .await
        .unwrap();
        assert_eq!(report.pushed, 1);
        let linked = personal_tasks::get(&store, "u1", &local.id)
            .unwrap()
            .unwrap();
        assert_eq!(linked.remote_id.as_deref(), Some("g-new"));
        assert_eq!(
            linked.remote_provider.as_deref(),
            Some(PROVIDER_GOOGLE_TASKS)
        );
    }

    #[tokio::test]
    async fn calendar_events_are_normalized_from_google_and_graph() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/calendars/primary/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    {"id": "e1", "summary": "Standup", "start": {"dateTime": "2026-11-09T09:00:00Z"}, "end": {"dateTime": "2026-11-09T09:30:00Z"}},
                    {"id": "e2", "summary": "Holiday", "start": {"date": "2026-11-11"}, "end": {"date": "2026-11-12"}}
                ]
            })))
            .mount(&server)
            .await;
        let events = calendar_events_at(
            "u1",
            PROVIDER_GOOGLE_CALENDAR,
            "2026-11-01",
            "2026-11-30",
            &token(),
            &reqwest::Client::new(),
            &server.uri(),
            &server.uri(),
        )
        .await
        .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["title"], "Standup");
        assert_eq!(events[1]["all_day"], true);

        let graph = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "value": [
                    {"id": "m1", "subject": "1:1", "isAllDay": false,
                     "start": {"dateTime": "2026-11-09T10:00:00.0000000", "timeZone": "UTC"},
                     "end": {"dateTime": "2026-11-09T10:30:00.0000000", "timeZone": "UTC"}}
                ]
            })))
            .mount(&graph)
            .await;
        let events = calendar_events_at(
            "u1",
            PROVIDER_OUTLOOK_CALENDAR,
            "2026-11-01",
            "2026-11-30",
            &token(),
            &reqwest::Client::new(),
            &graph.uri(),
            &graph.uri(),
        )
        .await
        .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["title"], "1:1");
        assert_eq!(events[0]["provider"], PROVIDER_OUTLOOK_CALENDAR);
    }
}
