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
