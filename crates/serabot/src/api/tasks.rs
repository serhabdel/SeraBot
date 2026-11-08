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
    let body =
        json!({"tasks": tasks, "limit": limit.clamp(1, crate::personal_tasks::MAX_LIST_LIMIT)});
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
    )?;
    let microsoft = crate::task_sync::sign_in_status(
        &state.users,
        &user.id,
        crate::task_sync::MICROSOFT_PROVIDER,
    )?;
    Ok(Json(json!({
        "prefs": prefs,
        "signed_in": {"tasks_google": google, "tasks_microsoft": microsoft},
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
    let (id_name, secret_name) = match provider {
        crate::task_sync::GOOGLE_PROVIDER => (
            &state.config.connectors.google_drive.client_id_secret,
            &state.config.connectors.google_drive.client_secret_secret,
        ),
        _ => (
            &state.config.connectors.outlook.client_id_secret,
            &state.config.connectors.outlook.client_secret_secret,
        ),
    };
    let client_id = state
        .secrets
        .get(id_name)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| SeraError::Config("task sync OAuth client id is not configured".into()))?;
    let client_secret = state.secrets.get(secret_name).unwrap_or_default();
    Ok((client_id, client_secret))
}

fn sync_http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| SeraError::External(error.to_string()))
}

#[derive(Debug, Deserialize)]
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
    Json(request): Json<SyncRunRequest>,
) -> Result<Json<Value>> {
    let prefs = crate::personal_tasks::get_prefs(&state.store, &user.id)?;
    let provider = request
        .provider
        .unwrap_or_else(|| prefs.sync_provider.clone());
    crate::personal_tasks::validate_sync_provider(&provider)?;
    let family = crate::task_sync::provider_family(&provider)
        .ok_or_else(|| SeraError::Validation("choose a task sync provider first".into()))?;
    let (client_id, client_secret) = sync_client_credentials(&state, family)?;
    let http = sync_http()?;
    let token = crate::task_sync::access_token(
        &state.users,
        &user.id,
        family,
        &client_id,
        &client_secret,
        &http,
    )
    .await?;
    let mut report = crate::task_sync::SyncReport::default();
    if matches!(
        provider.as_str(),
        crate::personal_tasks::PROVIDER_GOOGLE_TASKS
            | crate::personal_tasks::PROVIDER_MICROSOFT_TODO
    ) {
        let pulled =
            crate::task_sync::pull_tasks(&state.store, &user.id, &provider, &token, &http).await?;
        let pushed =
            crate::task_sync::push_new_tasks(&state.store, &user.id, &provider, &token, &http)
                .await?;
        report.imported = pulled.imported;
        report.updated = pulled.updated;
        report.unchanged = pulled.unchanged;
        report.pushed = pushed.pushed;
    }
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
    Ok(Json(json!({"provider": provider, "report": report})))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CalendarQuery {
    from: String,
    to: String,
}

/// Calendar events in `[from, to]` from the user's chosen calendar provider
/// (read-only), merged with the local tasks' due dates by the client.
pub(super) async fn calendar_events(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<CalendarQuery>,
) -> Result<Json<Value>> {
    let prefs = crate::personal_tasks::get_prefs(&state.store, &user.id)?;
    let provider = prefs.sync_provider;
    let family = crate::task_sync::provider_family(&provider)
        .ok_or_else(|| SeraError::Validation("choose a calendar provider first".into()))?;
    let (client_id, client_secret) = sync_client_credentials(&state, family)?;
    let http = sync_http()?;
    let token = crate::task_sync::access_token(
        &state.users,
        &user.id,
        family,
        &client_id,
        &client_secret,
        &http,
    )
    .await?;
    let events = crate::task_sync::calendar_events(
        &user.id,
        &provider,
        &query.from,
        &query.to,
        &token,
        &http,
    )
    .await?;
    let body = json!({"provider": provider, "events": events});
    state
        .runtime
        .scan_owner_output(&user.id, &body.to_string())?;
    Ok(Json(body))
}
