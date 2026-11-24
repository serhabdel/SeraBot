//! Per-user SMTP/IMAP mail connector API (`/v1/mail/*`, 0.74.1; named
//! accounts since 0.74.4: the default mailbox under provider `mail`, extras
//! under `mail.<label>`).
//!
//! Credentials are stored only in the per-user envelope-encrypted store
//! (`users`), never logged, and never returned by `GET /v1/mail/status`.
//! Connect runs a real SMTP login and IMAP login test and reports each side.

use super::*;
use crate::mail::{self, MailCredentials, Security};

pub(super) fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/v1/mail", get(status).delete(disconnect))
        .route("/v1/mail/connect", post(connect))
        .route("/v1/mail/folders", get(folders))
        .route("/v1/mail/messages", get(messages))
        .route("/v1/mail/search", get(search))
        .route(
            "/v1/mail/messages/{uid}",
            get(message).delete(delete_message),
        )
        .route("/v1/mail/messages/{uid}/flag", post(flag_message))
        .route("/v1/mail/messages/{uid}/move", post(move_message))
        .route(
            "/v1/mail/messages/{uid}/attachments/{index}",
            get(download_attachment),
        )
        .route(
            "/v1/mail/messages/{uid}/attachments/{index}/save-to-library",
            post(save_attachment_to_library),
        )
        .route("/v1/mail/send", post(send_message))
}

#[derive(Deserialize)]
struct ConnectRequest {
    /// Named account label; omitted means the default mailbox.
    #[serde(default)]
    account: Option<String>,
    smtp_host: String,
    #[serde(default = "mail::default_smtp_port")]
    smtp_port: u16,
    #[serde(default)]
    smtp_security: Security,
    username: String,
    password: String,
    imap_host: String,
    #[serde(default = "mail::default_imap_port")]
    imap_port: u16,
    #[serde(default = "mail::default_imap_ssl")]
    imap_security: Security,
    #[serde(default)]
    from: Option<String>,
    /// Signature appended to every sent message (0.74.5).
    #[serde(default)]
    signature: Option<String>,
}

async fn connect(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(body): Json<ConnectRequest>,
) -> Result<Json<Value>> {
    let account_label = body
        .account
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    mail::account_provider(account_label)?;
    let creds = MailCredentials {
        smtp_host: body.smtp_host,
        smtp_port: body.smtp_port,
        smtp_security: body.smtp_security,
        username: body.username,
        password: body.password,
        imap_host: body.imap_host,
        imap_port: body.imap_port,
        imap_security: body.imap_security,
        from: body.from,
        signature: body.signature,
    };
    mail::validate_credentials(&creds)?;
    mail::store_credentials(&state.users, &user.id, account_label, &creds)?;
    let smtp = mail::smtp_test(&creds).await.map(|_| "ok".to_owned());
    let imap = mail::imap_test(&creds).await.map(|_| "ok".to_owned());
    let _ = state.store.record_audit(
        &user.id,
        "mail.connect",
        Some("mail"),
        None,
        None,
        None,
        None,
        Some(&json!({
            "account": account_label,
            "smtp_host": creds.smtp_host,
            "smtp_port": creds.smtp_port,
            "imap_host": creds.imap_host,
            "imap_port": creds.imap_port,
            "smtp_test": smtp.as_deref().unwrap_or("failed"),
            "imap_test": imap.as_deref().unwrap_or("failed"),
        })),
    );
    Ok(Json(json!({
        "connected": true,
        "account": account_label,
        "smtp_host": creds.smtp_host,
        "imap_host": creds.imap_host,
        "username": creds.username,
        "smtp_test": match smtp { Ok(v) => json!(v), Err(e) => json!({"error": e.to_string()}) },
        "imap_test": match imap { Ok(v) => json!(v), Err(e) => json!({"error": e.to_string()}) },
    })))
}

async fn status(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let connected = mail::load_credentials(&state.users, &user.id, None)?;
    let accounts = mail::list_accounts(&state.users, &user.id)?;
    Ok(Json(json!({
        "connected": connected.is_some(),
        "smtp_host": connected.as_ref().map(|c| c.smtp_host.clone()),
        "imap_host": connected.as_ref().map(|c| c.imap_host.clone()),
        "username": connected.as_ref().map(|c| c.username.clone()),
        "accounts": accounts,
    })))
}

#[derive(Deserialize)]
struct DisconnectQuery {
    /// Named account label; omitted means the default mailbox.
    #[serde(default)]
    account: Option<String>,
}

#[derive(Deserialize)]
struct AccountQuery {
    /// Named account label; omitted means the default mailbox.
    #[serde(default)]
    account: Option<String>,
    #[serde(default)]
    folder: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

fn query_account(query: &AccountQuery) -> Result<Option<&str>> {
    let label = query
        .account
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    mail::account_provider(label)?;
    Ok(label)
}

/// `GET /v1/mail/folders?account=`: folder names of one mailbox (read-only).
async fn folders(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<AccountQuery>,
) -> Result<Json<Value>> {
    let account = query_account(&query)?;
    let creds = mail::load_credentials(&state.users, &user.id, account)?
        .ok_or_else(|| SeraError::Validation("mail is not connected; connect it first".into()))?;
    let folders = mail::imap_folders(&creds).await?;
    Ok(Json(json!({ "folders": folders })))
}

/// `GET /v1/mail/messages?account=&folder=&limit=`: newest messages (read-only).
async fn messages(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<AccountQuery>,
) -> Result<Json<Value>> {
    let account = query_account(&query)?;
    let creds = mail::load_credentials(&state.users, &user.id, account)?
        .ok_or_else(|| SeraError::Validation("mail is not connected; connect it first".into()))?;
    let folder = query.folder.as_deref().unwrap_or("");
    let limit = query.limit.unwrap_or(20);
    let messages = mail::imap_list(&creds, folder, limit).await?;
    Ok(Json(json!({ "messages": messages })))
}

/// `GET /v1/mail/messages/{uid}?account=&folder=`: one full message (read-only).
async fn message(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(uid): Path<u32>,
    Query(query): Query<AccountQuery>,
) -> Result<Json<Value>> {
    let account = query_account(&query)?;
    let creds = mail::load_credentials(&state.users, &user.id, account)?
        .ok_or_else(|| SeraError::Validation("mail is not connected; connect it first".into()))?;
    let folder = query.folder.as_deref().unwrap_or("");
    let message = mail::imap_read(&creds, folder, uid).await?;
    Ok(Json(json!({ "message": message })))
}

async fn disconnect(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<DisconnectQuery>,
) -> Result<Json<Value>> {
    let account_label = query
        .account
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    mail::account_provider(account_label)?;
    let removed = mail::delete_credentials(&state.users, &user.id, account_label)?;
    let _ = state.store.record_audit(
        &user.id,
        "mail.disconnect",
        Some("mail"),
        None,
        None,
        None,
        None,
        Some(&json!({ "account": account_label, "removed": removed })),
    );
    Ok(Json(
        json!({ "connected": false, "account": account_label }),
    ))
}

fn normalize_account(account: Option<&str>) -> Result<Option<&str>> {
    let label = account.map(str::trim).filter(|s| !s.is_empty());
    mail::account_provider(label)?;
    Ok(label)
}

fn require_mail_creds(
    state: &ApiState,
    user_id: &str,
    account: Option<&str>,
) -> Result<MailCredentials> {
    mail::load_credentials(&state.users, user_id, account)?
        .ok_or_else(|| SeraError::Validation("mail is not connected; connect it first".into()))
}

#[derive(Deserialize)]
struct SearchQuery {
    #[serde(default)]
    account: Option<String>,
    #[serde(default)]
    folder: Option<String>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

/// `GET /v1/mail/search?account=&folder=&q=&limit=`: search messages in one mailbox (read-only).
async fn search(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Value>> {
    let account = normalize_account(query.account.as_deref())?;
    let creds = require_mail_creds(&state, &user.id, account)?;
    let folder = query.folder.as_deref().unwrap_or("");
    let q = query.q.as_deref().unwrap_or("");
    let limit = query.limit.unwrap_or(20);
    let messages = mail::imap_search(&creds, folder, q, limit).await?;
    Ok(Json(json!({ "messages": messages })))
}

#[derive(Deserialize)]
struct FlagRequest {
    #[serde(default)]
    account: Option<String>,
    #[serde(default)]
    folder: Option<String>,
    #[serde(default)]
    seen: Option<bool>,
    #[serde(default)]
    flagged: Option<bool>,
}

/// `POST /v1/mail/messages/{uid}/flag`: toggle `\Seen` and/or `\Flagged`.
async fn flag_message(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(uid): Path<u32>,
    Json(body): Json<FlagRequest>,
) -> Result<Json<Value>> {
    let account = normalize_account(body.account.as_deref())?;
    if body.seen.is_none() && body.flagged.is_none() {
        return Err(SeraError::Validation(
            "at least one of seen or flagged is required".into(),
        ));
    }
    let creds = require_mail_creds(&state, &user.id, account)?;
    let folder = body.folder.as_deref().unwrap_or("");
    mail::imap_flag(&creds, folder, uid, body.seen, body.flagged).await?;
    let _ = state.store.record_audit(
        &user.id,
        "mail.flag",
        Some("mail"),
        None,
        None,
        None,
        None,
        Some(&json!({
            "account": account,
            "folder": folder,
            "uid": uid,
            "seen": body.seen,
            "flagged": body.flagged,
        })),
    );
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct MoveRequest {
    #[serde(default)]
    account: Option<String>,
    #[serde(default)]
    folder: Option<String>,
    dest: String,
}

/// `POST /v1/mail/messages/{uid}/move`: move one message to another folder.
async fn move_message(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(uid): Path<u32>,
    Json(body): Json<MoveRequest>,
) -> Result<Json<Value>> {
    let account = normalize_account(body.account.as_deref())?;
    let creds = require_mail_creds(&state, &user.id, account)?;
    let folder = body.folder.as_deref().unwrap_or("");
    mail::imap_move(&creds, folder, uid, &body.dest).await?;
    let _ = state.store.record_audit(
        &user.id,
        "mail.move",
        Some("mail"),
        None,
        None,
        None,
        None,
        Some(&json!({
            "account": account,
            "folder": folder,
            "uid": uid,
            "dest": body.dest,
        })),
    );
    Ok(Json(json!({ "ok": true })))
}

/// `DELETE /v1/mail/messages/{uid}?account=&folder=`: permanently delete one message.
async fn delete_message(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(uid): Path<u32>,
    Query(query): Query<AccountQuery>,
) -> Result<Json<Value>> {
    let account = query_account(&query)?;
    let creds = require_mail_creds(&state, &user.id, account)?;
    let folder = query.folder.as_deref().unwrap_or("");
    mail::imap_delete(&creds, folder, uid).await?;
    let _ = state.store.record_audit(
        &user.id,
        "mail.delete",
        Some("mail"),
        None,
        None,
        None,
        None,
        Some(&json!({
            "account": account,
            "folder": folder,
            "uid": uid,
        })),
    );
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct SendRequest {
    #[serde(default)]
    account: Option<String>,
    to: Vec<String>,
    subject: String,
    body: String,
    #[serde(default)]
    artifact_hashes: Vec<String>,
}

/// `POST /v1/mail/send`: send an email from the UI with optional owned artifact attachments.
async fn send_message(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(req): Json<SendRequest>,
) -> Result<Json<Value>> {
    let account = normalize_account(req.account.as_deref())?;
    state.runtime.scan_owner_output(&user.id, &req.subject)?;
    state.runtime.scan_owner_output(&user.id, &req.body)?;
    for rcpt in &req.to {
        state.runtime.scan_owner_output(&user.id, rcpt)?;
    }
    if req.artifact_hashes.len() > mail::MAX_ATTACHMENTS {
        return Err(SeraError::Validation(format!(
            "at most {} attachments are allowed",
            mail::MAX_ATTACHMENTS
        )));
    }
    let mut attachments = Vec::with_capacity(req.artifact_hashes.len());
    for hash in &req.artifact_hashes {
        if !state.store.user_owns_artifact(&user.id, hash)? {
            return Err(SeraError::Forbidden("artifact".into()));
        }
        let stored = state.artifacts.load(hash)?;
        if !state.runtime.policy().is_allowed(&stored.filename) {
            return Err(SeraError::PolicyViolation);
        }
        if stored.bytes.is_empty() || stored.bytes.len() > mail::MAX_ATTACHMENT_BYTES {
            return Err(SeraError::Validation(
                "each attachment must be 1 byte to 20 MiB".into(),
            ));
        }
        let content_type = crate::util::mime_for_filename(&stored.filename).to_owned();
        attachments.push(mail::Attachment {
            name: stored.filename,
            content_type,
            bytes: stored.bytes,
        });
    }
    let creds = require_mail_creds(&state, &user.id, account)?;
    let from = mail::from_address(&creds);
    let body = mail::body_with_signature(&creds, req.body)?;
    state.runtime.scan_owner_output(&user.id, &body)?;
    let msg = mail::build_message(&from, &req.to, &req.subject, &body, &attachments)?;
    mail::smtp_send(&creds, &from, &req.to, &msg).await?;
    let _ = state.store.record_audit(
        &user.id,
        "mail.send_ui",
        Some("mail"),
        None,
        None,
        None,
        None,
        Some(&json!({
            "account": account,
            "to_count": req.to.len(),
            "subject_chars": req.subject.chars().count(),
            "attachments": attachments.len(),
        })),
    );
    Ok(Json(json!({
        "sent": true,
        "to_count": req.to.len(),
        "attachments": attachments.len(),
    })))
}

/// `GET /v1/mail/messages/{uid}/attachments/{index}?account=&folder=`: download one attachment.
async fn download_attachment(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path((uid, index)): Path<(u32, usize)>,
    Query(query): Query<AccountQuery>,
) -> Result<Response> {
    let account = query_account(&query)?;
    let creds = require_mail_creds(&state, &user.id, account)?;
    let folder = query.folder.as_deref().unwrap_or("");
    let (raw_name, raw_ct, bytes) = mail::imap_read_attachment(&creds, folder, uid, index).await?;
    let filename: String = raw_name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '"' | '\\') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let filename = if filename.trim().is_empty() {
        "attachment.bin".to_owned()
    } else {
        filename
    };
    let content_type = if raw_ct.is_empty() || raw_ct.chars().any(|c| c.is_control()) {
        crate::util::mime_for_filename(&filename).to_owned()
    } else {
        raw_ct
    };
    Ok((
        [
            ("content-type", content_type),
            ("x-content-type-options", "nosniff".to_owned()),
            (
                "content-disposition",
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        axum::body::Body::from(bytes),
    )
        .into_response())
}

/// `POST /v1/mail/messages/{uid}/attachments/{index}/save-to-library?account=&folder=`:
/// save one email attachment into the member's Library (and mirror best-effort to OneDrive).
async fn save_attachment_to_library(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path((uid, index)): Path<(u32, usize)>,
    Query(query): Query<AccountQuery>,
) -> Result<Json<Value>> {
    let account = query_account(&query)?;
    let creds = require_mail_creds(&state, &user.id, account)?;
    let folder = query.folder.as_deref().unwrap_or("");
    let (att_name, _ct, bytes) = mail::imap_read_attachment(&creds, folder, uid, index).await?;
    if bytes.is_empty() {
        return Err(SeraError::Validation("attachment is empty".into()));
    }
    if !state.runtime.policy().is_allowed(&att_name) {
        return Err(SeraError::PolicyViolation);
    }
    let meta = crate::maker::ArtifactMeta {
        title: att_name.clone(),
        filename: att_name.clone(),
        language: crate::domain::Language::En,
        source_artifact: None,
        created_at: Utc::now(),
    };
    let stored = state.artifacts.save(&bytes, &att_name, meta)?;
    if !state.runtime.policy().is_allowed(&stored.filename) {
        return Err(SeraError::PolicyViolation);
    }
    state.store.record_conversation_artifact(
        &user.id,
        crate::library::LIBRARY_SCOPE,
        None,
        crate::library::LIBRARY_UPLOAD_TOOL,
        &stored.hash,
        &stored.filename,
        Some(&stored.meta.title),
    )?;
    let mime = crate::util::mime_for_filename(&stored.filename);
    let onedrive =
        crate::library::LibraryMirror::new((*state.config).clone(), state.onedrive.clone())
            .mirror(&user.id, &stored.filename, &stored.bytes, mime)
            .await;
    let _ = state.store.record_audit(
        &user.id,
        "mail.attachment_saved",
        Some("mail"),
        None,
        None,
        Some(&stored.hash),
        None,
        Some(&json!({
            "account": account,
            "folder": folder,
            "uid": uid,
            "index": index,
            "filename": stored.filename,
            "bytes": stored.bytes.len(),
            "onedrive": onedrive,
        })),
    );
    Ok(Json(json!({
        "hash": stored.hash,
        "filename": stored.filename,
        "size": stored.bytes.len(),
        "onedrive": onedrive,
    })))
}
