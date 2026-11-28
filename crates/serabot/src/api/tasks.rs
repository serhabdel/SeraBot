//! The Tasks tab API: per-user personal tasks (todo / calendar / Kanban) and
//! the opt-in sync with Google Tasks, Microsoft To Do, Google Calendar and
//! Outlook Calendar. Every handler is scoped to the acting user; sync tokens
//! live in that user's own credential store.

use super::*;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskListQuery {
    #[serde(default)]
    status: Option<String>,
    /// RFC 3339 or `YYYY-MM-DD`; bounds the due range for the calendar view.
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

pub(super) async fn list_tasks(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<TaskListQuery>,
) -> Result<Json<Value>> {
    let limit = query.limit.unwrap_or(200);
    let tasks = crate::personal_tasks::list(
        &state.store,
        &user.id,
        query.status.as_deref(),
        query.from.as_deref(),
        query.to.as_deref(),
        limit,
    )?;
    let prefs = crate::personal_tasks::get_prefs(&state.store, &user.id)?;
    let body = json!({
        "tasks": tasks,
        "limit": limit.clamp(1, crate::personal_tasks::MAX_LIST_LIMIT),
        "counts": crate::personal_tasks::counts(&state.store, &user.id)?,
        "sync": {
            "provider": prefs.sync_provider,
            "health": crate::personal_tasks::sync_health(&prefs),
            "last_run": prefs.last_sync_at,
            "last_ok": prefs.last_sync_ok,
            "last_error": prefs.last_sync_error,
        }
    });
    state
        .runtime
        .scan_owner_output(&user.id, &body.to_string())?;
    Ok(Json(body))
}

pub(super) async fn create_task(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<crate::personal_tasks::NewTask>,
) -> Result<Json<Value>> {
    state.runtime.scan_owner_output(&user.id, &request.title)?;
    if let Some(notes) = &request.notes {
        state.runtime.scan_owner_output(&user.id, notes)?;
    }
    let task = crate::personal_tasks::create(&state.store, &user.id, &request)?;
    state.store.record_audit(
        &user.id,
        "task.created",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"id": task.id, "status": task.status})),
    )?;
    Ok(Json(json!({"task": task})))
}

pub(super) async fn patch_task(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(patch): Json<crate::personal_tasks::TaskPatch>,
) -> Result<Json<Value>> {
    if let Some(title) = &patch.title {
        state.runtime.scan_owner_output(&user.id, title)?;
    }
    if let Some(notes) = &patch.notes {
        state.runtime.scan_owner_output(&user.id, notes)?;
    }
    let task = crate::personal_tasks::update(&state.store, &user.id, &id, &patch)?
        .ok_or_else(|| SeraError::NotFound(format!("task {id}")))?;
    state.store.record_audit(
        &user.id,
        "task.updated",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"id": id, "status": task.status})),
    )?;
    Ok(Json(json!({"task": task})))
}

pub(super) async fn delete_task(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    if !crate::personal_tasks::delete(&state.store, &user.id, &id)? {
        return Err(SeraError::NotFound(format!("task {id}")));
    }
    state.store.record_audit(
        &user.id,
        "task.deleted",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"id": id})),
    )?;
    Ok(Json(json!({"deleted": id})))
}

pub(super) async fn get_task_prefs(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let prefs = crate::personal_tasks::get_prefs(&state.store, &user.id)?;
    Ok(Json(json!({"prefs": prefs})))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskPrefsRequest {
    sync_provider: String,
}

pub(super) async fn put_task_prefs(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<TaskPrefsRequest>,
) -> Result<Json<Value>> {
    let prefs = crate::personal_tasks::set_prefs(&state.store, &user.id, &request.sync_provider)?;
    clear_calendar_cooldowns_for_user(&state, &user.id);
    state.store.record_audit(
        &user.id,
        "task.prefs_updated",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"sync_provider": prefs.sync_provider})),
    )?;
    Ok(Json(json!({"prefs": prefs})))
}

/// Which sync sign-ins this account has, and the chosen provider.
pub(super) async fn sync_status(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let prefs = crate::personal_tasks::get_prefs(&state.store, &user.id)?;
    let google = crate::task_sync::sign_in_status(
        &state.users,
        &user.id,
        crate::task_sync::GOOGLE_PROVIDER,
    )? || state.personal.has_google_connection(&user.id)?;
    let microsoft = crate::task_sync::sign_in_status(
        &state.users,
        &user.id,
        crate::task_sync::MICROSOFT_PROVIDER,
    )? || state.personal.has_microsoft_connection(&user.id)?;
    Ok(Json(json!({
        "prefs": prefs,
        "signed_in": {"tasks_google": google, "tasks_microsoft": microsoft},
        "health": crate::personal_tasks::sync_health(&prefs),
        "last_run": prefs.last_sync_at,
        "last_ok": prefs.last_sync_ok,
        "last_error": prefs.last_sync_error,
        "counts": crate::personal_tasks::counts(&state.store, &user.id)?,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SyncOauthStartRequest {
    provider: String,
    redirect_uri: String,
    challenge: String,
    state: String,
}

/// Begin a per-user Google/Microsoft sign-in for task sync. PKCE only; the
/// desktop console holds the verifier and state, the server just builds the
/// pinned consent URL with the configured OAuth client.
pub(super) async fn sync_oauth_start(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<SyncOauthStartRequest>,
) -> Result<Json<Value>> {
    let (client_id, client_secret) = sync_client_credentials(&state, &request.provider)?;
    let url = crate::task_sync::authorize_url(
        &request.provider,
        &client_id,
        &request.redirect_uri,
        &request.challenge,
        &request.state,
    )?;
    let _ = client_secret;
    state.store.record_audit(
        &user.id,
        "task.sync_oauth_started",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"provider": request.provider})),
    )?;
    Ok(Json(json!({"flow": "pkce", "url": url})))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SyncOauthCompleteRequest {
    provider: String,
    code: String,
    verifier: String,
    redirect_uri: String,
}

pub(super) async fn sync_oauth_complete(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<SyncOauthCompleteRequest>,
) -> Result<Json<Value>> {
    let (client_id, client_secret) = sync_client_credentials(&state, &request.provider)?;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| SeraError::External(error.to_string()))?;
    let pair = crate::task_sync::exchange_code(
        &http,
        &request.provider,
        &client_id,
        &client_secret,
        &request.code,
        &request.redirect_uri,
        &request.verifier,
    )
    .await?;
    crate::task_sync::store_sign_in(&state.users, &user.id, &request.provider, &pair)?;
    clear_calendar_cooldowns_for_user(&state, &user.id);
    state.store.record_audit(
        &user.id,
        "task.sync_signed_in",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"provider": request.provider})),
    )?;
    Ok(Json(
        json!({"signed_in": true, "provider": request.provider}),
    ))
}

pub(super) async fn sync_oauth_disconnect(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(provider): Path<String>,
) -> Result<Json<Value>> {
    let removed = crate::task_sync::disconnect(&state.users, &user.id, &provider)?;
    clear_calendar_cooldowns_for_user(&state, &user.id);
    if removed {
        state.store.record_audit(
            &user.id,
            "task.sync_signed_out",
            None,
            None,
            None,
            None,
            Some(state.runtime.policy_version()),
            Some(&json!({"provider": provider})),
        )?;
    }
    Ok(Json(json!({"disconnected": removed, "provider": provider})))
}

/// The Google/Microsoft OAuth client the sync flow uses: the same app
/// credentials the per-user personal connectors are configured with.
fn sync_client_credentials(state: &ApiState, provider: &str) -> Result<(String, String)> {
    crate::task_sync::validate_oauth_provider(provider)?;
    if provider == crate::task_sync::GOOGLE_PROVIDER {
        let client_id = state
            .secrets
            .get(&state.config.connectors.google_drive.client_id_secret)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                SeraError::Config("task sync OAuth client id is not configured".into())
            })?;
        let client_secret = state
            .secrets
            .get(&state.config.connectors.google_drive.client_secret_secret)
            .unwrap_or_default();
        return Ok((client_id, client_secret));
    }

    let candidates: [(&str, Option<&str>); 4] = [
        (&state.config.auth.client_id_secret, None),
        (&state.config.onedrive.client_id_secret, None),
        (
            &state.config.connectors.outlook.client_id_secret,
            Some(
                state
                    .config
                    .connectors
                    .outlook
                    .client_secret_secret
                    .as_str(),
            ),
        ),
        (
            &state.config.connectors.outlook_calendar.client_id_secret,
            Some(
                state
                    .config
                    .connectors
                    .outlook_calendar
                    .client_secret_secret
                    .as_str(),
            ),
        ),
    ];
    for (id_name, secret_name) in candidates {
        if let Some(client_id) = state
            .secrets
            .get(id_name)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
        {
            let client_secret = secret_name
                .and_then(|name| state.secrets.get(name))
                .unwrap_or_default();
            return Ok((client_id, client_secret));
        }
    }
    Err(SeraError::Config(
        "task sync OAuth client id is not configured".into(),
    ))
}

async fn resolve_sync_token(
    state: &ApiState,
    user_id: &str,
    family: &str,
    client_id: &str,
    client_secret: &str,
    http: &reqwest::Client,
) -> Result<secrecy::SecretString> {
    match crate::task_sync::access_token(
        &state.users,
        user_id,
        family,
        client_id,
        client_secret,
        http,
    )
    .await
    {
        Ok(token) => Ok(token),
        Err(error)
            if error
                .to_string()
                .to_ascii_lowercase()
                .contains("not signed in") =>
        {
            let personal = match family {
                crate::task_sync::GOOGLE_PROVIDER => {
                    state.personal.google_sync_token(user_id).await?
                }
                crate::task_sync::MICROSOFT_PROVIDER => {
                    state.personal.microsoft_sync_token(user_id).await?
                }
                _ => return Err(error),
            };
            Ok(secrecy::SecretString::from(personal))
        }
        Err(error) => Err(error),
    }
}

fn sync_failure_text(error: &SeraError) -> String {
    match error {
        SeraError::Upstream { message, .. } => message.clone(),
        SeraError::Validation(text) if text.len() <= 160 && !text.chars().any(char::is_control) => {
            text.clone()
        }
        SeraError::External(text)
            if text.to_ascii_lowercase().contains("timed out")
                || text.to_ascii_lowercase().contains("timeout") =>
        {
            "Task sync timed out. Wait a moment and try again.".into()
        }
        _ => "Task sync failed. Check the connection in Tasks and try again.".into(),
    }
}

fn sync_http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| SeraError::External(error.to_string()))
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SyncRunRequest {
    /// Defaults to the stored `sync_provider`.
    #[serde(default)]
    provider: Option<String>,
}

/// Run one sync pass: pull the chosen provider's tasks (and push new local
/// ones for the task providers). The provider must be signed in.
pub(super) async fn run_sync(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    body: axum::body::Bytes,
) -> Result<Json<Value>> {
    let request = if body.trim_ascii().is_empty() {
        SyncRunRequest::default()
    } else {
        serde_json::from_slice::<SyncRunRequest>(&body)
            .map_err(|error| SeraError::Validation(format!("invalid sync run request: {error}")))?
    };
    let prefs = crate::personal_tasks::get_prefs(&state.store, &user.id)?;
    let provider = request
        .provider
        .unwrap_or_else(|| prefs.sync_provider.clone());
    crate::personal_tasks::validate_sync_provider(&provider)?;
    let family = crate::task_sync::provider_family(&provider)
        .ok_or_else(|| SeraError::Validation("choose a task sync provider first".into()))?;
    let (client_id, client_secret) = sync_client_credentials(&state, family)?;
    clear_calendar_cooldowns_for_user(&state, &user.id);
    let http = sync_http()?;
    let synced = async {
        let token =
            resolve_sync_token(&state, &user.id, family, &client_id, &client_secret, &http).await?;
        let mut report = crate::task_sync::SyncReport::default();
        if matches!(
            provider.as_str(),
            crate::personal_tasks::PROVIDER_GOOGLE_TASKS
                | crate::personal_tasks::PROVIDER_MICROSOFT_TODO
        ) {
            let pulled =
                crate::task_sync::pull_tasks(&state.store, &user.id, &provider, &token, &http)
                    .await?;
            let pushed =
                crate::task_sync::push_new_tasks(&state.store, &user.id, &provider, &token, &http)
                    .await?;
            report.imported = pulled.imported;
            report.updated = pulled.updated;
            report.unchanged = pulled.unchanged;
            report.pushed = pushed.pushed;
        }
        Ok::<_, SeraError>(report)
    }
    .await;
    if let Err(error) = &synced {
        let _ = crate::personal_tasks::record_sync_result(
            &state.store,
            &user.id,
            false,
            Some(&sync_failure_text(error)),
        );
    }
    let report = synced?;
    crate::personal_tasks::record_sync_result(&state.store, &user.id, true, None)?;
    state.store.record_audit(
        &user.id,
        "task.sync_ran",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"provider": provider, "report": report})),
    )?;
    Ok(Json(
        json!({"provider": provider, "report": report, "health": "ok"}),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CalendarQuery {
    from: String,
    to: String,
}

fn calendar_providers_for_user(
    state: &ApiState,
    user_id: &str,
    sync_provider: &str,
) -> Vec<&'static str> {
    let has_google = sync_provider == crate::personal_tasks::PROVIDER_GOOGLE_CALENDAR
        || state
            .personal
            .has_google_connection(user_id)
            .unwrap_or(false)
        || crate::task_sync::sign_in_status(
            &state.users,
            user_id,
            crate::task_sync::GOOGLE_PROVIDER,
        )
        .unwrap_or(false);
    let has_outlook = sync_provider == crate::personal_tasks::PROVIDER_OUTLOOK_CALENDAR
        || sync_provider == crate::personal_tasks::PROVIDER_MICROSOFT_TODO
        || state
            .personal
            .has_microsoft_connection(user_id)
            .unwrap_or(false)
        || crate::task_sync::sign_in_status(
            &state.users,
            user_id,
            crate::task_sync::MICROSOFT_PROVIDER,
        )
        .unwrap_or(false);

    let mut providers = Vec::new();
    if has_google {
        providers.push(crate::personal_tasks::PROVIDER_GOOGLE_CALENDAR);
    }
    if has_outlook {
        providers.push(crate::personal_tasks::PROVIDER_OUTLOOK_CALENDAR);
    }
    providers
}

const CALENDAR_NEGATIVE_COOLDOWN: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(super) struct CalendarProviderWarning {
    pub provider: String,
    pub status_code: Option<u16>,
    pub message: String,
    pub hint: String,
}

pub(super) fn clear_calendar_cooldowns_for_user(state: &ApiState, user_id: &str) {
    let mut map = state
        .calendar_cooldowns
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.retain(|(uid, _), _| uid != user_id);
}

fn get_cached_calendar_warning(
    state: &ApiState,
    user_id: &str,
    provider: &str,
    now: Instant,
) -> Option<CalendarProviderWarning> {
    let mut map = state
        .calendar_cooldowns
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let key = (user_id.to_owned(), provider.to_owned());
    if let Some((expires_at, warning)) = map.get(&key) {
        if *expires_at > now {
            return Some(warning.clone());
        }
        map.remove(&key);
    }
    None
}

fn cache_calendar_warning(
    state: &ApiState,
    user_id: &str,
    provider: &str,
    warning: &CalendarProviderWarning,
    now: Instant,
) {
    if matches!(warning.status_code, Some(401 | 403)) {
        let mut map = state
            .calendar_cooldowns
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        map.insert(
            (user_id.to_owned(), provider.to_owned()),
            (now + CALENDAR_NEGATIVE_COOLDOWN, warning.clone()),
        );
    }
}

fn extract_http_status(error: &str) -> Option<u16> {
    let bytes = error.as_bytes();
    let lower = error.to_ascii_lowercase();
    let mut start = 0;
    while let Some(pos) = lower[start..].find("http ") {
        let idx = start + pos + 5;
        let rest = &bytes[idx..];
        let digits: Vec<u8> = rest
            .iter()
            .copied()
            .take_while(u8::is_ascii_digit)
            .collect();
        if digits.len() == 3
            && let Ok(text) = std::str::from_utf8(&digits)
            && let Ok(code) = text.parse::<u16>()
            && (100..=599).contains(&code)
        {
            return Some(code);
        }
        start = idx;
    }
    None
}

fn calendar_provider_label(provider: &str) -> &str {
    match provider {
        crate::personal_tasks::PROVIDER_GOOGLE_CALENDAR => "Google Calendar",
        crate::personal_tasks::PROVIDER_OUTLOOK_CALENDAR => "Outlook Calendar",
        _ => provider,
    }
}

pub(super) fn classify_calendar_warning(provider: &str, error: &str) -> CalendarProviderWarning {
    let status_code = extract_http_status(error);
    let label = calendar_provider_label(provider);
    let message = match status_code {
        Some(401) => format!("{label} returned HTTP 401 (unauthorized)."),
        Some(403) => format!("{label} returned HTTP 403 (access forbidden)."),
        Some(429) => format!("{label} returned HTTP 429 (rate limited)."),
        Some(code) => format!("{label} returned HTTP {code}."),
        None => {
            let trimmed = error.trim();
            if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
                format!("{label} sync failed.")
            } else {
                let summary = truncate_chars(trimmed, 160);
                format!("{label} sync failed: {summary}")
            }
        }
    };
    let hint = match (provider, status_code) {
        (_, Some(401)) => "Your calendar sign-in expired. Reconnect it in Integrations.".into(),
        (crate::personal_tasks::PROVIDER_GOOGLE_CALENDAR, Some(403)) => {
            "Enable the Google Calendar API in Google Cloud Console or reconnect Google with calendar permissions in Integrations.".into()
        }
        (crate::personal_tasks::PROVIDER_OUTLOOK_CALENDAR, Some(403)) => {
            "Check Outlook Calendar permissions or reconnect Microsoft in Integrations.".into()
        }
        (_, Some(429)) => "Too many requests. Wait a few minutes before retrying.".into(),
        _ => "Check your calendar connection in Integrations and try again.".into(),
    };
    CalendarProviderWarning {
        provider: provider.to_owned(),
        status_code,
        message,
        hint,
    }
}

/// Calendar events in `[from, to]` from all connected calendar providers for
/// the acting user (read-only), merged with the local tasks' due dates by the
/// client.
pub(super) async fn calendar_events(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<CalendarQuery>,
) -> Result<Json<Value>> {
    let prefs = crate::personal_tasks::get_prefs(&state.store, &user.id)?;
    let provider = prefs.sync_provider;
    let providers = calendar_providers_for_user(&state, &user.id, &provider);
    if providers.is_empty() {
        return Ok(Json(json!({
            "provider": provider,
            "events": [],
            "provider_warnings": [],
        })));
    }
    let http = sync_http()?;
    let mut events = Vec::new();
    let mut warnings = Vec::new();
    for cal_provider in providers {
        if let Some(cached) =
            get_cached_calendar_warning(&state, &user.id, cal_provider, Instant::now())
        {
            warnings.push(cached);
            continue;
        }
        let Some(family) = crate::task_sync::provider_family(cal_provider) else {
            continue;
        };
        let (client_id, client_secret) = match sync_client_credentials(&state, family) {
            Ok(creds) => creds,
            Err(error) => {
                tracing::warn!(
                    user_id = %user.id,
                    provider = cal_provider,
                    %error,
                    "skipping calendar provider without OAuth credentials"
                );
                continue;
            }
        };
        let token =
            match resolve_sync_token(&state, &user.id, family, &client_id, &client_secret, &http)
                .await
            {
                Ok(token) => token,
                Err(error) => {
                    let warning = classify_calendar_warning(cal_provider, &error.to_string());
                    tracing::warn!(
                        user_id = %user.id,
                        provider = cal_provider,
                        status_code = ?warning.status_code,
                        %error,
                        "skipping calendar provider token resolution failure"
                    );
                    cache_calendar_warning(
                        &state,
                        &user.id,
                        cal_provider,
                        &warning,
                        Instant::now(),
                    );
                    warnings.push(warning);
                    continue;
                }
            };
        match crate::task_sync::calendar_events(
            &user.id,
            cal_provider,
            &query.from,
            &query.to,
            &token,
            &http,
        )
        .await
        {
            Ok(mut fetched) => events.append(&mut fetched),
            Err(error) => {
                let warning = classify_calendar_warning(cal_provider, &error.to_string());
                tracing::warn!(
                    user_id = %user.id,
                    provider = cal_provider,
                    status_code = ?warning.status_code,
                    %error,
                    "calendar provider fetch failed"
                );
                cache_calendar_warning(&state, &user.id, cal_provider, &warning, Instant::now());
                warnings.push(warning);
            }
        }
    }
    events.sort_by(|a, b| {
        let sa = a.get("start").and_then(Value::as_str).unwrap_or("");
        let sb = b.get("start").and_then(Value::as_str).unwrap_or("");
        sa.cmp(sb)
    });
    let body = json!({
        "provider": provider,
        "events": events,
        "provider_warnings": warnings,
    });
    state
        .runtime
        .scan_owner_output(&user.id, &body.to_string())?;
    Ok(Json(body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{Gateway, ToolRegistry};
    use crate::llm::LlmClient;
    use crate::policy::Policy;
    use crate::runtime::NullNotifier;
    use tower::ServiceExt;

    fn test_state_and_router() -> (ApiState, Router, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let mut config = Config::default();
        config.api.enabled = true;
        config.api.token_hash = sha256_hex("test-token-that-is-long-enough-123");
        config.llm.model = "test".into();
        config.llm.base_url = "https://example.com/v1".into();
        config.base_dir = dir.path().to_path_buf();
        let db_path = dir.path().join("serabot.db");
        config.database.path = db_path.to_string_lossy().into_owned();
        let config = Arc::new(config);
        let store = Store::open(&db_path, 384, 5000).unwrap();
        let gateway = Gateway::new(
            store.clone(),
            Arc::new(Policy::empty()),
            ToolRegistry::default(),
            1,
            false,
        );
        let llm = LlmClient::new("https://example.com/v1", "test", 5, 50)
            .unwrap()
            .with_model("test");
        let runtime = Runtime::new(
            config.clone(),
            store.clone(),
            gateway,
            crate::model_router::ModelRouter::new(
                &config.llm,
                BTreeMap::from([("default".into(), llm)]),
            )
            .unwrap(),
            vec![crate::config::LoadedBot {
                config: BotConfig {
                    title: None,
                    description: None,
                    name: "main".into(),
                    trigger: "command".into(),
                    model: None,
                    system_prompt: "prompts/main.txt".into(),
                    skills: vec![],
                    tools: vec![],
                    require_approval: vec![],
                    enabled: true,
                    temperature: None,
                    thinking: None,
                    role: crate::orchestration::BotRole::Orchestrator,
                    delegates: vec![],
                    route: None,
                    fallbacks: Vec::new(),
                    failover: Default::default(),
                    catalog_only: false,
                },
                prompt: "test".into(),
            }],
            Arc::new(NullNotifier),
            tokio_util::sync::CancellationToken::new(),
        )
        .unwrap();
        let artifacts = Arc::new(ArtifactStore::new(dir.path().join("artifacts")).unwrap());
        let secrets = SharedSecrets::new(crate::secrets::SecretStore::from_map(BTreeMap::new()));
        let users = Arc::new(
            crate::user_secrets::UserSecrets::with_master_key(store.clone(), [7u8; 32]).unwrap(),
        );
        let state = ApiState::new(runtime, store, config, artifacts, secrets, users, None).unwrap();
        let app = router(state.clone());
        (state, app, dir)
    }

    #[tokio::test]
    async fn run_sync_accepts_bodyless_post_without_http_415() {
        let (_state, app, _dir) = test_state_and_router();
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/tasks/sync/run")
            .header("Authorization", "Bearer test-token-that-is-long-enough-123")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_ne!(
            resp.status(),
            axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        assert_eq!(resp.status(), axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn calendar_providers_include_connected_accounts_even_when_sync_provider_is_none_or_todo()
    {
        let (state, _app, _dir) = test_state_and_router();
        let uid = state
            .store
            .create_user("sub-cal-test", "cal@example.example", "Cal Test")
            .unwrap()
            .id;
        assert!(calendar_providers_for_user(&state, &uid, "none").is_empty());
        assert_eq!(
            calendar_providers_for_user(&state, &uid, "microsoft_todo"),
            vec![crate::personal_tasks::PROVIDER_OUTLOOK_CALENDAR]
        );

        state
            .users
            .set(&uid, "conn.google", "{\"refresh_token\":\"rt\"}")
            .unwrap();
        assert_eq!(
            calendar_providers_for_user(&state, &uid, "none"),
            vec![crate::personal_tasks::PROVIDER_GOOGLE_CALENDAR]
        );

        state
            .users
            .set(&uid, "conn.outlook_calendar", "{\"refresh_token\":\"rt2\"}")
            .unwrap();
        assert_eq!(
            calendar_providers_for_user(&state, &uid, "none"),
            vec![
                crate::personal_tasks::PROVIDER_GOOGLE_CALENDAR,
                crate::personal_tasks::PROVIDER_OUTLOOK_CALENDAR,
            ]
        );
    }

    #[test]
    fn classify_calendar_warning_handles_google_403_and_401() {
        let w403 = classify_calendar_warning(
            crate::personal_tasks::PROVIDER_GOOGLE_CALENDAR,
            "task sync request rejected (HTTP 403)",
        );
        assert_eq!(w403.provider, "google_calendar");
        assert_eq!(w403.status_code, Some(403));
        assert!(w403.message.contains("403"));
        assert_eq!(
            w403.hint,
            "Enable the Google Calendar API in Google Cloud Console or reconnect Google with calendar permissions in Integrations."
        );

        let w401 = classify_calendar_warning(
            crate::personal_tasks::PROVIDER_GOOGLE_CALENDAR,
            "sync token request rejected (HTTP 401): invalid_grant",
        );
        assert_eq!(w401.provider, "google_calendar");
        assert_eq!(w401.status_code, Some(401));
        assert!(w401.message.contains("401"));
        assert_eq!(
            w401.hint,
            "Your calendar sign-in expired. Reconnect it in Integrations."
        );
    }

    #[test]
    fn calendar_cooldown_cache_hit_expiry_and_clear() {
        let (state, _app, _dir) = test_state_and_router();
        let now = Instant::now();
        let w403 = classify_calendar_warning(
            crate::personal_tasks::PROVIDER_GOOGLE_CALENDAR,
            "task sync request rejected (HTTP 403)",
        );
        let w429 = classify_calendar_warning(
            crate::personal_tasks::PROVIDER_OUTLOOK_CALENDAR,
            "task sync request rejected (HTTP 429)",
        );

        // Non-401/403 warnings are not cached.
        cache_calendar_warning(&state, "u1", "outlook_calendar", &w429, now);
        assert!(get_cached_calendar_warning(&state, "u1", "outlook_calendar", now).is_none());

        // 403 is cached for 300s.
        cache_calendar_warning(&state, "u1", "google_calendar", &w403, now);
        cache_calendar_warning(&state, "u2", "google_calendar", &w403, now);
        assert_eq!(
            get_cached_calendar_warning(
                &state,
                "u1",
                "google_calendar",
                now + Duration::from_secs(299)
            ),
            Some(w403.clone())
        );

        // Clear for u1 only; u2 remains cached.
        clear_calendar_cooldowns_for_user(&state, "u1");
        assert!(get_cached_calendar_warning(&state, "u1", "google_calendar", now).is_none());
        assert_eq!(
            get_cached_calendar_warning(&state, "u2", "google_calendar", now),
            Some(w403)
        );

        // Expired after 300s.
        assert!(
            get_cached_calendar_warning(
                &state,
                "u2",
                "google_calendar",
                now + Duration::from_secs(300)
            )
            .is_none()
        );
    }
}
