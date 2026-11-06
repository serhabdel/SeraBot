//! Owner API for the live browser view. Every route acts on the session of
//! the authenticated user only: the user id comes from the bearer token, never
//! from the path or body.

use super::*;
use crate::browser::guard::validate_resolved;
use crate::browser::outline::key_def;
use crate::browser::{BrowserManager, Controller};

const MAX_INPUT_TEXT_CHARS: usize = 2000;
const MAX_COORD: f64 = 4000.0;
const MAX_SCROLL: f64 = 5000.0;

fn manager(state: &ApiState) -> Result<&Arc<BrowserManager>> {
    state
        .browser
        .as_ref()
        .ok_or_else(|| SeraError::NotFound("browser is not enabled".into()))
}

/// `GET /v1/browser`. A disabled browser reads as simply not active.
pub(super) async fn browser_status(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    match state.browser.as_ref() {
        Some(browser) => Ok(Json(browser.info(&user.id).await.to_json())),
        None => Ok(Json(json!({
            "active": false,
            "controller": "bot",
            "updated_at": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        }))),
    }
}

/// `GET /v1/browser/screenshot` returns a JPEG (<= 1280 px wide, <= 512 KiB).
pub(super) async fn browser_screenshot(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Response> {
    let bytes = manager(&state)?.screenshot(&user.id).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        bytes,
    )
        .into_response())
}

pub(super) async fn browser_takeover(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let browser = manager(&state)?;
    browser.set_controller(&user.id, Controller::User).await?;
    Ok(Json(browser.info(&user.id).await.to_json()))
}

pub(super) async fn browser_handback(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let browser = manager(&state)?;
    browser.set_controller(&user.id, Controller::Bot).await?;
    Ok(Json(browser.info(&user.id).await.to_json()))
}

pub(super) async fn browser_close(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let closed = manager(&state)?.close(&user.id).await;
    Ok(Json(json!({"closed": closed})))
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

/// `POST /v1/browser/input`, only while the user holds control.
pub(super) async fn browser_input(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(input): Json<BrowserInput>,
) -> Result<Json<Value>> {
    let browser = manager(&state)?;
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
    let lease = browser.user_lease(&user.id).await?;
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
    Ok(Json(browser.info(&user.id).await.to_json()))
}
