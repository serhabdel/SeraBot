//! Owner API for the live browser view and browser profiles. Every route acts
//! on the sessions and profiles of the authenticated user only: the user id
//! comes from the bearer token, never from the path or body.
//!
//! One Chrome per (user, profile). The 0.14 single-session routes are aliases
//! for the `default` profile; `/v1/browser/sessions/{id}/...` addresses any
//! profile by its id.

use super::*;
use crate::browser::guard::validate_resolved;
use crate::browser::outline::key_def;
use crate::browser::{BrowserManager, Controller};
use crate::browser_profiles::{
    BrowserProfile, DEFAULT_PROFILE, MAX_PROFILE_NAME_CHARS, remove_profile_dir, valid_profile_id,
};

const MAX_INPUT_TEXT_CHARS: usize = 2000;
const MAX_COORD: f64 = 4000.0;
const MAX_SCROLL: f64 = 5000.0;

fn manager(state: &ApiState) -> Result<&Arc<BrowserManager>> {
    state
        .browser
        .as_ref()
        .ok_or_else(|| SeraError::NotFound("browser is not enabled".into()))
}

/// A profile from a request path; unknown or malformed ids are a 404.
fn existing_profile(state: &ApiState, user: &ActingUser, id: &str) -> Result<BrowserProfile> {
    if !valid_profile_id(id) {
        return Err(SeraError::NotFound(format!("browser profile {id}")));
    }
    state
        .store
        .get_browser_profile(&user.id, id)?
        .ok_or_else(|| SeraError::NotFound(format!("browser profile {id}")))
}

async fn status_for(state: &ApiState, user: &ActingUser, profile: &str) -> Result<Json<Value>> {
    match state.browser.as_ref() {
        Some(browser) => Ok(Json(browser.info(&user.id, profile).await.to_json())),
        None => Ok(Json(json!({
            "active": false,
            "controller": "bot",
            "updated_at": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        }))),
    }
}

async fn screenshot_for(state: &ApiState, user: &ActingUser, profile: &str) -> Result<Response> {
    let bytes = manager(state)?.screenshot(&user.id, profile).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        bytes,
    )
        .into_response())
}

async fn controller_for(
    state: &ApiState,
    user: &ActingUser,
    profile: &str,
    controller: Controller,
) -> Result<Json<Value>> {
    let browser = manager(state)?;
    browser
        .set_controller(&user.id, profile, controller)
        .await?;
    session_reply(state, user, profile, browser.info(&user.id, profile).await.to_json())
}

/// The shape the desktop expects for a session: the live info plus the
/// profile `id` and `profile_name` (the list route adds the same two).
fn session_reply(
    state: &ApiState,
    user: &ActingUser,
    profile: &str,
    mut info: Value,
) -> Result<Json<Value>> {
    let name = state
        .store
        .get_browser_profile(&user.id, profile)?
        .map(|p| p.name)
        .unwrap_or_else(|| profile.to_owned());
    info["id"] = json!(profile);
    info["profile_name"] = json!(name);
    Ok(Json(info))
}

async fn close_for(state: &ApiState, user: &ActingUser, profile: &str) -> Result<Json<Value>> {
    let closed = manager(state)?.close(&user.id, profile).await;
    Ok(Json(json!({"closed": closed})))
}

// ---- 0.14 single-session routes: aliases for the default profile ----

/// `GET /v1/browser`. A disabled browser reads as simply not active.
pub(super) async fn browser_status(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    status_for(&state, &user, DEFAULT_PROFILE).await
}

/// `GET /v1/browser/screenshot` returns a JPEG (<= 1280 px wide, <= 512 KiB).
pub(super) async fn browser_screenshot(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Response> {
    screenshot_for(&state, &user, DEFAULT_PROFILE).await
}

pub(super) async fn browser_takeover(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    controller_for(&state, &user, DEFAULT_PROFILE, Controller::User).await
}

pub(super) async fn browser_handback(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    controller_for(&state, &user, DEFAULT_PROFILE, Controller::Bot).await
}

pub(super) async fn browser_close(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    close_for(&state, &user, DEFAULT_PROFILE).await
}

/// `POST /v1/browser/input`, only while the user holds control.
pub(super) async fn browser_input(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(input): Json<BrowserInput>,
) -> Result<Json<Value>> {
    input_for(&state, &user, DEFAULT_PROFILE, input).await
}

// ---- 0.16 per-profile session routes ----

/// `GET /v1/browser/sessions` — one entry per profile of the acting user.
pub(super) async fn sessions_list(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let Some(browser) = state.browser.as_ref() else {
        return Ok(Json(json!({"sessions": []})));
    };
    let profiles = state.store.list_browser_profiles(&user.id)?;
    let live = browser.user_sessions(&user.id).await;
    let sessions = profiles
        .into_iter()
        .map(|profile| {
            let mut entry = match live.iter().find(|(id, _)| *id == profile.id) {
                Some((_, info)) => info.to_json(),
                None => json!({
                    "active": false,
                    "controller": "bot",
                    "updated_at": profile
                        .last_used_at
                        .clone()
                        .unwrap_or_else(|| profile.created_at.clone()),
                }),
            };
            entry["id"] = json!(profile.id);
            entry["profile_name"] = json!(profile.name);
            entry
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({"sessions": sessions})))
}

pub(super) async fn session_screenshot(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Response> {
    existing_profile(&state, &user, &id)?;
    screenshot_for(&state, &user, &id).await
}

pub(super) async fn session_takeover(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    existing_profile(&state, &user, &id)?;
    controller_for(&state, &user, &id, Controller::User).await
}

pub(super) async fn session_handback(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    existing_profile(&state, &user, &id)?;
    controller_for(&state, &user, &id, Controller::Bot).await
}

pub(super) async fn session_close(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    existing_profile(&state, &user, &id)?;
    close_for(&state, &user, &id).await
}

pub(super) async fn session_input(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(input): Json<BrowserInput>,
) -> Result<Json<Value>> {
    existing_profile(&state, &user, &id)?;
    input_for(&state, &user, &id, input).await
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub(super) enum BrowserInput {
    Click { x: f64, y: f64 },
    Text { text: String },
    Key { key: String },
    Scroll { dy: f64 },
    Navigate { url: String },
}

fn coord(value: f64) -> Result<f64> {
    if value.is_finite() && (0.0..=MAX_COORD).contains(&value) {
        Ok(value)
    } else {
        Err(SeraError::Validation("coordinates out of range".into()))
    }
}

async fn input_for(
    state: &ApiState,
    user: &ActingUser,
    profile: &str,
    input: BrowserInput,
) -> Result<Json<Value>> {
    let browser = manager(state)?;
    // Validate before touching the session.
    match &input {
        BrowserInput::Click { x, y } => {
            coord(*x)?;
            coord(*y)?;
        }
        BrowserInput::Text { text } => {
            if text.is_empty() || text.chars().count() > MAX_INPUT_TEXT_CHARS {
                return Err(SeraError::Validation(
                    "text must be 1..2000 characters".into(),
                ));
            }
        }
        BrowserInput::Key { key } => {
            if key_def(key).is_none() {
                return Err(SeraError::Validation("unsupported key".into()));
            }
        }
        BrowserInput::Scroll { dy } => {
            if !dy.is_finite() || dy.abs() > MAX_SCROLL {
                return Err(SeraError::Validation("scroll out of range".into()));
            }
        }
        BrowserInput::Navigate { .. } => {}
    }
    let url = match &input {
        BrowserInput::Navigate { url } => Some(validate_resolved(url).await?),
        _ => None,
    };
    let lease = browser.user_lease(&user.id, profile).await?;
    let backend = lease.backend();
    match (&input, url) {
        (BrowserInput::Click { x, y }, _) => backend.click_at(*x, *y).await?,
        (BrowserInput::Text { text }, _) => backend.insert_text(text).await?,
        (BrowserInput::Key { key }, _) => backend.press(key).await?,
        (BrowserInput::Scroll { dy }, _) => backend.scroll(*dy).await?,
        (BrowserInput::Navigate { .. }, Some(url)) => {
            backend.navigate(url.as_str()).await?;
        }
        _ => {}
    }
    drop(lease);
    session_reply(state, user, profile, browser.info(&user.id, profile).await.to_json())
}

// ---- profiles ----

fn profile_json(profile: &BrowserProfile, bots: Vec<String>) -> Value {
    let mut value = json!({
        "id": profile.id,
        "name": profile.name,
        "is_default": profile.is_default,
        "bots": bots,
        "created_at": profile.created_at,
    });
    if let Some(last_used_at) = &profile.last_used_at {
        value["last_used_at"] = json!(last_used_at);
    }
    value
}

fn profile_bots(
    state: &ApiState,
    user: &ActingUser,
) -> Result<std::collections::HashMap<String, Vec<String>>> {
    let ids = state
        .runtime
        .fleet()
        .configured(&user.id)?
        .into_iter()
        .map(|bot| bot.id)
        .collect::<Vec<_>>();
    state.store.browser_profile_bots(&user.id, &ids)
}

/// `GET /v1/browser/profiles`
pub(super) async fn profiles_list(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let profiles = state.store.list_browser_profiles(&user.id)?;
    let mut bots = profile_bots(&state, &user)?;
    let list = profiles
        .iter()
        .map(|profile| profile_json(profile, bots.remove(&profile.id).unwrap_or_default()))
        .collect::<Vec<_>>();
    Ok(Json(json!({"profiles": list})))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProfileNameRequest {
    name: String,
}

fn scanned_profile_name(state: &ApiState, user: &ActingUser, name: &str) -> Result<()> {
    if name.chars().count() > MAX_PROFILE_NAME_CHARS * 4 {
        return Err(SeraError::Validation("the profile name is too long".into()));
    }
    state.runtime.scan_owner_output(&user.id, name)
}

/// `POST /v1/browser/profiles` `{"name"}`
pub(super) async fn profile_create(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<ProfileNameRequest>,
) -> Result<Json<Value>> {
    scanned_profile_name(&state, &user, &request.name)?;
    let profile = state
        .store
        .create_browser_profile(&user.id, &request.name)?;
    state.store.record_audit(
        &user.id,
        "browser.profile.created",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"id": profile.id})),
    )?;
    Ok(Json(profile_json(&profile, Vec::new())))
}

/// `PUT /v1/browser/profiles/{id}` `{"name"}`
pub(super) async fn profile_rename(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<ProfileNameRequest>,
) -> Result<Json<Value>> {
    existing_profile(&state, &user, &id)?;
    scanned_profile_name(&state, &user, &request.name)?;
    let profile = state
        .store
        .rename_browser_profile(&user.id, &id, &request.name)?;
    let mut bots = profile_bots(&state, &user)?;
    Ok(Json(profile_json(
        &profile,
        bots.remove(&profile.id).unwrap_or_default(),
    )))
}

/// `DELETE /v1/browser/profiles/{id}` — closes the session, removes the
/// Chrome directory and points the profile's bots back at `default`.
pub(super) async fn profile_delete(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    existing_profile(&state, &user, &id)?;
    if id == DEFAULT_PROFILE {
        return Err(SeraError::InvalidState(
            "the default browser profile cannot be deleted".into(),
        ));
    }
    let mut data_removed = false;
    if let Some(browser) = state.browser.as_ref() {
        browser.close(&user.id, &id).await;
        if let Some(base) = browser.base_dir() {
            // Removed before the row, so a failure leaves the profile to retry.
            data_removed = remove_profile_dir(base, &user.id, &id)?;
        }
    }
    let reassigned = state.store.delete_browser_profile(&user.id, &id)?;
    state.store.record_audit(
        &user.id,
        "browser.profile.deleted",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"id": id, "reassigned_bots": reassigned, "data_removed": data_removed})),
    )?;
    Ok(Json(json!({
        "deleted": true,
        "id": id,
        "reassigned_bots": reassigned,
    })))
}
