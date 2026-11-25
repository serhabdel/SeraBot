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
        .route("/v1/mail/signature", put(update_signature))
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
        .route("/v1/mail/schedule", post(schedule_message))
        .route("/v1/mail/scheduled", get(list_scheduled))
        .route(
            "/v1/mail/scheduled/{id}",
            patch(update_scheduled).delete(delete_scheduled),
        )
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
    #[serde(default)]
    signature_html: Option<String>,
    #[serde(default)]
    signature_logo_hash: Option<String>,
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
    if let Some(hash) = body
        .signature_logo_hash
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        verify_logo_artifact(&state, &user.id, hash)?;
    }
    let sig_cfg = mail::MailSignatureConfig {
        signature: body.signature.clone().unwrap_or_default(),
        signature_html: body.signature_html.clone(),
        signature_logo_hash: body.signature_logo_hash.clone(),
    };
    mail::validate_signature_config(&sig_cfg)?;
    if !sig_cfg.signature.is_empty() {
        state
            .runtime
            .scan_owner_output(&user.id, &sig_cfg.signature)?;
    }
    if let Some(html) = sig_cfg.signature_html.as_deref() {
        state.runtime.scan_owner_output(&user.id, html)?;
    }
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
    if sig_cfg.signature_html.is_some()
        || sig_cfg.signature_logo_hash.is_some()
        || !sig_cfg.signature.is_empty()
    {
        mail::store_signature_config(&state.users, &user.id, account_label, &sig_cfg)?;
    }
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
    let sig_cfg = if connected.is_some() {
        Some(mail::load_signature_config(&state.users, &user.id, None)?)
    } else {
        None
    };
    let accounts = mail::list_accounts(&state.users, &user.id)?;
    Ok(Json(json!({
        "connected": connected.is_some(),
        "smtp_host": connected.as_ref().map(|c| c.smtp_host.clone()),
        "imap_host": connected.as_ref().map(|c| c.imap_host.clone()),
        "username": connected.as_ref().map(|c| c.username.clone()),
        "signature": sig_cfg.as_ref().map(|s| s.signature.clone()),
        "signature_html": sig_cfg.as_ref().and_then(|s| s.signature_html.clone()),
        "signature_logo_hash": sig_cfg.as_ref().and_then(|s| s.signature_logo_hash.clone()),
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
    #[serde(default)]
    allow_remote_images: Option<bool>,
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
    let messages = super::mail_extras::hide_snoozed(&state, &user.id, account, folder, messages);
    super::mail_extras::observe_senders(&state, &user.id, &messages);
    Ok(Json(json!({ "messages": messages })))
}

/// `GET /v1/mail/messages/{uid}?account=&folder=&allow_remote_images=`: one full message (read-only).
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
    let allow_remote = query.allow_remote_images.unwrap_or(false);
    let message = mail::imap_read_detail(&creds, folder, uid, allow_remote).await?;
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

pub(super) fn normalize_account(account: Option<&str>) -> Result<Option<&str>> {
    let label = account.map(str::trim).filter(|s| !s.is_empty());
    mail::account_provider(label)?;
    Ok(label)
}

pub(super) fn require_mail_creds(
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
    super::mail_extras::observe_senders(&state, &user.id, &messages);
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
    #[serde(default)]
    cc: Vec<String>,
    #[serde(default)]
    bcc: Vec<String>,
    subject: String,
    body: String,
    #[serde(default)]
    body_html: Option<String>,
    #[serde(default)]
    artifact_hashes: Vec<String>,
    #[serde(default)]
    attachment_hashes: Vec<String>,
}

/// `POST /v1/mail/send`: send an email from the UI with optional HTML body and owned artifact attachments.
async fn send_message(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(req): Json<SendRequest>,
) -> Result<Json<Value>> {
    let account = normalize_account(req.account.as_deref())?;
    let mut hashes = req.artifact_hashes;
    hashes.extend(req.attachment_hashes);
    let sent = mail::send_account_mail(
        &state.users,
        &state.store,
        &state.artifacts,
        state.runtime.policy(),
        mail::SendAccountMailParams {
            user_id: &user.id,
            account,
            to: &req.to,
            cc: &req.cc,
            bcc: &req.bcc,
            subject: &req.subject,
            body: &req.body,
            body_html: req.body_html.as_deref(),
            attachment_hashes: &hashes,
        },
    )
    .await?;
    super::mail_extras::remember_recipients(
        &state,
        &user.id,
        &[req.to.as_slice(), req.cc.as_slice(), req.bcc.as_slice()],
    );
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
            "to_count": sent.to_count,
            "subject_chars": req.subject.chars().count(),
            "attachments": sent.attachments,
        })),
    );
    Ok(Json(json!({
        "sent": true,
        "to_count": sent.to_count,
        "attachments": sent.attachments,
    })))
}

fn verify_logo_artifact(state: &ApiState, user_id: &str, hash: &str) -> Result<()> {
    if !state.store.user_owns_artifact(user_id, hash)? {
        return Err(SeraError::Forbidden("artifact".into()));
    }
    let stored = state.artifacts.load(hash)?;
    let mime = mail::inline_image_mime_for_filename(&stored.filename);
    if !mail::is_safe_inline_image_mime(mime) {
        return Err(SeraError::Validation(
            "signature logo must be png, jpeg, gif, or webp".into(),
        ));
    }
    if stored.bytes.is_empty() || stored.bytes.len() > 512 * 1024 {
        return Err(SeraError::Validation(
            "signature logo must be 1 byte to 512 KiB".into(),
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct SignatureUpdateRequest {
    #[serde(default)]
    account: Option<String>,
    #[serde(default)]
    signature: String,
    #[serde(default)]
    signature_html: Option<String>,
    #[serde(default)]
    signature_logo_hash: Option<String>,
}

/// `PUT /v1/mail/signature`: update plain/HTML signature and optional logo artifact without password re-entry.
async fn update_signature(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(req): Json<SignatureUpdateRequest>,
) -> Result<Json<Value>> {
    let account = normalize_account(req.account.as_deref())?;
    state.runtime.scan_owner_output(&user.id, &req.signature)?;
    if let Some(html) = &req.signature_html {
        state.runtime.scan_owner_output(&user.id, html)?;
    }
    if let Some(hash) = req
        .signature_logo_hash
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        verify_logo_artifact(&state, &user.id, hash)?;
    }
    let cfg = mail::MailSignatureConfig {
        signature: req.signature,
        signature_html: req.signature_html,
        signature_logo_hash: req.signature_logo_hash,
    };
    mail::store_signature_config(&state.users, &user.id, account, &cfg)?;
    let _ = state.store.record_audit(
        &user.id,
        "mail.signature_updated",
        Some("mail"),
        None,
        None,
        None,
        None,
        Some(&json!({ "account": account })),
    );
    Ok(Json(json!({
        "ok": true,
        "account": account,
        "signature": cfg.signature,
        "signature_html": cfg.signature_html,
        "signature_logo_hash": cfg.signature_logo_hash,
    })))
}

#[derive(Deserialize)]
struct ScheduleRequest {
    #[serde(default)]
    account: Option<String>,
    to: Vec<String>,
    #[serde(default)]
    cc: Vec<String>,
    #[serde(default)]
    bcc: Vec<String>,
    subject: String,
    body: String,
    #[serde(default)]
    body_html: Option<String>,
    send_at: i64,
    /// `later` (default) or `undo`, the short hold behind Undo send.
    #[serde(default)]
    kind: Option<String>,
    /// Artifact hashes (Library files) to send with the message.
    #[serde(default)]
    attachment_hashes: Vec<String>,
}

/// Undo send holds a message for a few seconds; anything outside this window is refused.
const UNDO_MIN_SECS: i64 = 3;
const UNDO_MAX_SECS: i64 = 120;

fn validate_schedule_fields(
    to: &[String],
    cc: &[String],
    bcc: &[String],
    subject: &str,
    body: &str,
    body_html: Option<&str>,
    send_at: i64,
) -> Result<()> {
    if to.is_empty() || to.len() + cc.len() + bcc.len() > mail::MAX_RECIPIENTS {
        return Err(SeraError::Validation(format!(
            "1 to {} recipients are allowed",
            mail::MAX_RECIPIENTS
        )));
    }
    for addr in to.iter().chain(cc.iter()).chain(bcc.iter()) {
        if addr.is_empty()
            || addr.len() > 254
            || addr.chars().any(|c| c == '\r' || c == '\n' || c == ',')
        {
            return Err(SeraError::Validation(format!("invalid recipient {addr:?}")));
        }
    }
    if subject.chars().count() > mail::MAX_SUBJECT_CHARS {
        return Err(SeraError::Validation("subject is too long".into()));
    }
    if body.chars().count() > mail::MAX_BODY_CHARS {
        return Err(SeraError::Validation("body is too long".into()));
    }
    if let Some(html) = body_html
        && html.chars().count() > mail::MAX_BODY_CHARS
    {
        return Err(SeraError::Validation("HTML body is too long".into()));
    }
    if send_at <= 0 {
        return Err(SeraError::Validation(
            "send_at must be a positive unix timestamp".into(),
        ));
    }
    Ok(())
}

/// `POST /v1/mail/schedule`: queue an outgoing email for future delivery.
async fn schedule_message(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(req): Json<ScheduleRequest>,
) -> Result<Json<Value>> {
    let account = normalize_account(req.account.as_deref())?;
    let _creds = require_mail_creds(&state, &user.id, account)?;
    validate_schedule_fields(
        &req.to,
        &req.cc,
        &req.bcc,
        &req.subject,
        &req.body,
        req.body_html.as_deref(),
        req.send_at,
    )?;
    state.runtime.scan_owner_output(&user.id, &req.subject)?;
    state.runtime.scan_owner_output(&user.id, &req.body)?;
    if let Some(html) = &req.body_html {
        state.runtime.scan_owner_output(&user.id, html)?;
    }
    for rcpt in req.to.iter().chain(req.cc.iter()).chain(req.bcc.iter()) {
        state.runtime.scan_owner_output(&user.id, rcpt)?;
    }
    if req.attachment_hashes.len() > mail::MAX_ATTACHMENTS {
        return Err(SeraError::Validation(format!(
            "at most {} attachments are allowed",
            mail::MAX_ATTACHMENTS
        )));
    }
    for hash in &req.attachment_hashes {
        if !state.store.user_owns_artifact(&user.id, hash)? {
            return Err(SeraError::Forbidden("artifact".into()));
        }
    }
    let kind = match req.kind.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
        None | Some("later") => "later",
        Some("undo") => {
            let now = chrono::Utc::now().timestamp();
            if req.send_at < now + UNDO_MIN_SECS - 1 || req.send_at > now + UNDO_MAX_SECS {
                return Err(SeraError::Validation(format!(
                    "an undo hold lasts {UNDO_MIN_SECS} to {UNDO_MAX_SECS} seconds"
                )));
            }
            "undo"
        }
        Some(_) => {
            return Err(SeraError::Validation("kind must be later or undo".into()));
        }
    };
    let row = state.store.create_scheduled_mail_kind(
        &user.id,
        account.unwrap_or(""),
        &req.to,
        &req.cc,
        &req.bcc,
        &req.subject,
        &req.body,
        req.body_html.as_deref(),
        req.send_at,
        kind,
    )?;
    if !req.attachment_hashes.is_empty() {
        state
            .store
            .set_scheduled_mail_attachments(&user.id, &row.id, &req.attachment_hashes)?;
    }
    super::mail_extras::remember_recipients(
        &state,
        &user.id,
        &[req.to.as_slice(), req.cc.as_slice(), req.bcc.as_slice()],
    );
    let _ = state.store.record_audit(
        &user.id,
        "mail.schedule",
        Some("mail"),
        None,
        None,
        None,
        None,
        Some(&json!({ "id": row.id, "account": account, "send_at": row.send_at })),
    );
    Ok(Json(json!({ "item": row })))
}

#[derive(Deserialize)]
struct ScheduledListQuery {
    #[serde(default)]
    account: Option<String>,
}

/// `GET /v1/mail/scheduled?account=`: list the acting user's scheduled emails.
async fn list_scheduled(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<ScheduledListQuery>,
) -> Result<Json<Value>> {
    let filter = normalize_account(query.account.as_deref())?;
    let items = state.store.list_scheduled_mail(&user.id, filter)?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
struct UpdateScheduledRequest {
    #[serde(default)]
    to: Option<Vec<String>>,
    #[serde(default)]
    cc: Option<Vec<String>>,
    #[serde(default)]
    bcc: Option<Vec<String>>,
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    body_html: Option<Option<String>>,
    #[serde(default)]
    send_at: Option<i64>,
}

/// `PATCH /v1/mail/scheduled/{id}`: update a pending scheduled email (UID-scoped).
async fn update_scheduled(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(req): Json<UpdateScheduledRequest>,
) -> Result<Json<Value>> {
    let validate_rcpts = |list: &[String], label: &str| -> Result<()> {
        if list.len() > mail::MAX_RECIPIENTS {
            return Err(SeraError::Validation(format!(
                "too many {label} recipients"
            )));
        }
        for r in list {
            let trimmed = r.trim();
            if trimmed.is_empty()
                || trimmed.len() > 254
                || trimmed.contains(['\r', '\n', ','])
                || !trimmed.contains('@')
            {
                return Err(SeraError::Validation(format!("invalid {label} recipient")));
            }
            state.runtime.scan_owner_output(&user.id, r)?;
        }
        Ok(())
    };
    if let Some(to) = &req.to {
        if to.is_empty() {
            return Err(SeraError::Validation("invalid to recipients".into()));
        }
        validate_rcpts(to, "to")?;
    }
    if let Some(cc) = &req.cc {
        validate_rcpts(cc, "cc")?;
    }
    if let Some(bcc) = &req.bcc {
        validate_rcpts(bcc, "bcc")?;
    }
    let total_req_rcpts = req.to.as_ref().map_or(0, Vec::len)
        + req.cc.as_ref().map_or(0, Vec::len)
        + req.bcc.as_ref().map_or(0, Vec::len);
    if total_req_rcpts > mail::MAX_RECIPIENTS {
        return Err(SeraError::Validation(format!(
            "too many recipients (max {})",
            mail::MAX_RECIPIENTS
        )));
    }
    if let Some(subject) = &req.subject {
        if subject.chars().count() > mail::MAX_SUBJECT_CHARS {
            return Err(SeraError::Validation("subject is too long".into()));
        }
        state.runtime.scan_owner_output(&user.id, subject)?;
    }
    if let Some(body) = &req.body {
        if body.chars().count() > mail::MAX_BODY_CHARS {
            return Err(SeraError::Validation("body is too long".into()));
        }
        state.runtime.scan_owner_output(&user.id, body)?;
    }
    if let Some(Some(html)) = &req.body_html {
        if html.chars().count() > mail::MAX_BODY_CHARS {
            return Err(SeraError::Validation("HTML body is too long".into()));
        }
        state.runtime.scan_owner_output(&user.id, html)?;
    }
    if let Some(send_at) = req.send_at
        && send_at <= 0
    {
        return Err(SeraError::Validation(
            "send_at must be a positive unix timestamp".into(),
        ));
    }
    let item = state.store.update_scheduled_mail(
        &user.id,
        &id,
        crate::store::ScheduledMailPatch {
            to: req.to,
            cc: req.cc,
            bcc: req.bcc,
            subject: req.subject,
            body: req.body,
            body_html: req.body_html,
            send_at: req.send_at,
        },
    )?;
    Ok(Json(json!({ "item": item })))
}

/// `DELETE /v1/mail/scheduled/{id}`: cancel/delete a scheduled email (UID-scoped).
async fn delete_scheduled(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let deleted = state.store.delete_scheduled_mail(&user.id, &id)?;
    if !deleted {
        return Err(SeraError::NotFound(format!("scheduled mail {id}")));
    }
    let _ = state.store.record_audit(
        &user.id,
        "mail.schedule_cancelled",
        Some("mail"),
        None,
        None,
        None,
        None,
        Some(&json!({ "id": id })),
    );
    Ok(Json(json!({ "deleted": true })))
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
