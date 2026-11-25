//! Mail workspace endpoints added in 0.74.14 (`/v1/mail/*`): snooze, contacts,
//! saved searches, drafts and unsubscribe. Everything is scoped to the acting
//! user, and anything that leaves the server goes through the same blocklist scan
//! and audit trail as the rest of the mail API.

use super::mail::{normalize_account, require_mail_creds};
use super::*;
use crate::mail;

pub(super) fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route(
            "/v1/mail/messages/{uid}/snooze",
            post(snooze_message).delete(unsnooze_message),
        )
        .route("/v1/mail/snoozed", get(list_snoozed))
        .route("/v1/mail/contacts", get(search_contacts))
        .route(
            "/v1/mail/saved-searches",
            get(list_saved_searches).post(create_saved_search),
        )
        .route(
            "/v1/mail/saved-searches/{id}",
            axum::routing::delete(delete_saved_search),
        )
        .route("/v1/mail/drafts", post(save_draft))
        .route("/v1/mail/unsubscribe", post(unsubscribe))
}

/// The folder name snoozes are stored under (an empty folder means the inbox).
pub(super) fn folder_key(folder: &str) -> String {
    let folder = folder.trim();
    if folder.is_empty() {
        "INBOX".to_owned()
    } else {
        folder.to_owned()
    }
}

/// Drop the messages the user snoozed from a folder listing.
pub(super) fn hide_snoozed(
    state: &ApiState,
    user_id: &str,
    account: Option<&str>,
    folder: &str,
    messages: Vec<mail::MailMessage>,
) -> Vec<mail::MailMessage> {
    let hidden = state
        .store
        .snoozed_uids(user_id, account.unwrap_or(""), &folder_key(folder))
        .unwrap_or_default();
    if hidden.is_empty() {
        return messages;
    }
    messages
        .into_iter()
        .filter(|m| !hidden.contains(&m.uid))
        .collect()
}

/// Remember the senders of a listing for recipient autocomplete (best effort).
pub(super) fn observe_senders(state: &ApiState, user_id: &str, messages: &[mail::MailMessage]) {
    let mut contacts: Vec<(String, String)> = Vec::new();
    for message in messages {
        contacts.extend(crate::mail_ux::parse_addresses(&message.from));
    }
    contacts.sort_by(|a, b| a.1.cmp(&b.1));
    contacts.dedup_by(|a, b| a.1 == b.1);
    let _ =
        state
            .store
            .upsert_mail_contacts(user_id, &contacts, false, chrono::Utc::now().timestamp());
}

/// Remember the people the user wrote to (best effort); this ranks them first.
pub(super) fn remember_recipients(state: &ApiState, user_id: &str, lists: &[&[String]]) {
    let mut contacts: Vec<(String, String)> = Vec::new();
    for list in lists {
        for entry in *list {
            contacts.extend(crate::mail_ux::parse_addresses(entry));
        }
    }
    contacts.sort_by(|a, b| a.1.cmp(&b.1));
    contacts.dedup_by(|a, b| a.1 == b.1);
    let _ =
        state
            .store
            .upsert_mail_contacts(user_id, &contacts, true, chrono::Utc::now().timestamp());
}

// ------------------------------------------------------------- snooze ----

#[derive(Deserialize)]
struct SnoozeRequest {
    #[serde(default)]
    account: Option<String>,
    #[serde(default)]
    folder: Option<String>,
    wake_at: i64,
    #[serde(default)]
    subject: String,
    #[serde(default)]
    from: String,
}

const MAX_SNOOZE_SECS: i64 = 366 * 24 * 3600;

fn checked_folder(folder: Option<&str>) -> Result<String> {
    let folder = folder_key(folder.unwrap_or(""));
    if folder.len() > 512 || folder.chars().any(char::is_control) {
        return Err(SeraError::Validation("invalid folder name".into()));
    }
    Ok(folder)
}

/// `POST /v1/mail/messages/{uid}/snooze`: hide a message until `wake_at`.
async fn snooze_message(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(uid): Path<u32>,
    Json(req): Json<SnoozeRequest>,
) -> Result<Json<Value>> {
    let account = normalize_account(req.account.as_deref())?;
    require_mail_creds(&state, &user.id, account)?;
    let now = chrono::Utc::now().timestamp();
    if req.wake_at <= now || req.wake_at > now + MAX_SNOOZE_SECS {
        return Err(SeraError::Validation(
            "wake_at must be in the future and within a year".into(),
        ));
    }
    let folder = checked_folder(req.folder.as_deref())?;
    let row = state.store.snooze_mail(
        &user.id,
        account.unwrap_or(""),
        &folder,
        uid,
        &req.subject,
        &req.from,
        req.wake_at,
    )?;
    let _ = state.store.record_audit(
        &user.id,
        "mail.snooze",
        Some("mail"),
        None,
        None,
        None,
        None,
        Some(&json!({
            "account": account,
            "folder": folder,
            "uid": uid,
            "wake_at": req.wake_at,
        })),
    );
    Ok(Json(json!({ "item": row })))
}

#[derive(Deserialize)]
struct FolderQuery {
    #[serde(default)]
    account: Option<String>,
    #[serde(default)]
    folder: Option<String>,
}

/// `DELETE /v1/mail/messages/{uid}/snooze?account=&folder=`: bring it back now.
async fn unsnooze_message(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(uid): Path<u32>,
    Query(query): Query<FolderQuery>,
) -> Result<Json<Value>> {
    let account = normalize_account(query.account.as_deref())?;
    let folder = checked_folder(query.folder.as_deref())?;
    let removed = state
        .store
        .unsnooze_mail(&user.id, account.unwrap_or(""), &folder, uid)?;
    Ok(Json(json!({ "removed": removed })))
}

#[derive(Deserialize)]
struct AccountOnlyQuery {
    #[serde(default)]
    account: Option<String>,
}

/// `GET /v1/mail/snoozed?account=`: the user's snoozed messages, soonest first.
async fn list_snoozed(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<AccountOnlyQuery>,
) -> Result<Json<Value>> {
    let account = normalize_account(query.account.as_deref())?;
    let items = state.store.list_snoozed_mail(&user.id, account)?;
    Ok(Json(json!({ "items": items })))
}

// ----------------------------------------------------------- contacts ----

#[derive(Deserialize)]
struct ContactsQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

/// `GET /v1/mail/contacts?q=&limit=`: recipient suggestions, most used first.
async fn search_contacts(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<ContactsQuery>,
) -> Result<Json<Value>> {
    let q = query.q.as_deref().unwrap_or("");
    if q.chars().count() > 100 {
        return Err(SeraError::Validation("q is too long".into()));
    }
    let items = state
        .store
        .search_mail_contacts(&user.id, q, query.limit.unwrap_or(8))?;
    Ok(Json(json!({ "items": items })))
}

// ----------------------------------------------------- saved searches ----

#[derive(Deserialize)]
struct SavedSearchRequest {
    name: String,
    query: String,
}

async fn list_saved_searches(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let items = state.store.list_saved_searches(&user.id)?;
    Ok(Json(json!({ "items": items })))
}

async fn create_saved_search(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(req): Json<SavedSearchRequest>,
) -> Result<Json<Value>> {
    let item = state
        .store
        .create_saved_search(&user.id, &req.name, &req.query)?;
    Ok(Json(json!({ "item": item })))
}

async fn delete_saved_search(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    if !state.store.delete_saved_search(&user.id, &id)? {
        return Err(SeraError::NotFound(format!("saved search {id}")));
    }
    Ok(Json(json!({ "deleted": true })))
}

// -------------------------------------------------------------- drafts ----

#[derive(Deserialize)]
struct DraftRequest {
    #[serde(default)]
    account: Option<String>,
    /// Defaults to the mailbox's Drafts folder.
    #[serde(default)]
    folder: Option<String>,
    /// The UID of the previous autosave, replaced once the new copy is stored.
    #[serde(default)]
    replace_uid: Option<u32>,
    #[serde(default)]
    to: Vec<String>,
    #[serde(default)]
    cc: Vec<String>,
    #[serde(default)]
    subject: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    body_html: Option<String>,
}

fn plain_addresses(entries: &[String]) -> Vec<String> {
    entries
        .iter()
        .flat_map(|entry| crate::mail_ux::parse_addresses(entry))
        .map(|(_, email)| email)
        .collect()
}

/// `POST /v1/mail/drafts`: save (or replace) a draft in the mailbox's Drafts folder.
/// Half-typed recipients are dropped rather than rejected so autosave never fails
/// on an unfinished address.
async fn save_draft(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(req): Json<DraftRequest>,
) -> Result<Json<Value>> {
    let account = normalize_account(req.account.as_deref())?;
    let creds = require_mail_creds(&state, &user.id, account)?;
    if req.subject.chars().count() > mail::MAX_SUBJECT_CHARS
        || req.body.chars().count() > mail::MAX_BODY_CHARS
        || req
            .body_html
            .as_deref()
            .is_some_and(|h| h.chars().count() > mail::MAX_BODY_CHARS)
    {
        return Err(SeraError::Validation("the draft is too long".into()));
    }
    state.runtime.scan_owner_output(&user.id, &req.subject)?;
    state.runtime.scan_owner_output(&user.id, &req.body)?;
    if let Some(html) = &req.body_html {
        state.runtime.scan_owner_output(&user.id, html)?;
    }
    let mut to = plain_addresses(&req.to);
    let mut cc = plain_addresses(&req.cc);
    to.truncate(mail::MAX_RECIPIENTS);
    cc.truncate(mail::MAX_RECIPIENTS.saturating_sub(to.len()));

    let folder = match req
        .folder
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty())
    {
        Some(folder) => folder.to_owned(),
        None => {
            let folders = mail::imap_folders(&creds).await?;
            crate::mail_ux::pick_folder(&folders, "drafts")
                .map(str::to_owned)
                .ok_or_else(|| SeraError::Validation("this mailbox has no Drafts folder".into()))?
        }
    };
    let raw = mail::build_draft_message(
        &mail::from_address(&creds),
        &to,
        &cc,
        &req.subject,
        &req.body,
        req.body_html.as_deref(),
    )?;
    let uid = mail::imap_save_draft(&creds, &folder, &raw, req.replace_uid).await?;
    Ok(Json(json!({ "folder": folder, "uid": uid })))
}

// --------------------------------------------------------- unsubscribe ----

#[derive(Deserialize)]
struct UnsubscribeRequest {
    #[serde(default)]
    account: Option<String>,
    /// The `mailto:` target from the message's `unsubscribe.mailto`, scheme removed.
    mailto: String,
}

/// `POST /v1/mail/unsubscribe`: send the request a `List-Unsubscribe: <mailto:...>`
/// header asks for. `https:` links are never fetched by the server; clients open
/// them in the user's browser.
async fn unsubscribe(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(req): Json<UnsubscribeRequest>,
) -> Result<Json<Value>> {
    let account = normalize_account(req.account.as_deref())?;
    mail::send_unsubscribe_mailto(
        &state.users,
        &state.store,
        state.runtime.policy(),
        &user.id,
        account,
        &req.mailto,
    )
    .await?;
    let _ = state.store.record_audit(
        &user.id,
        "mail.unsubscribe",
        Some("mail"),
        None,
        None,
        None,
        None,
        Some(&json!({ "account": account })),
    );
    Ok(Json(json!({ "sent": true })))
}
