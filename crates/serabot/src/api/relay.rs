//! `/v1/relay/net` (0.66): the member's desktop network relay. Settings and
//! status for the console, and the long-poll exchange the desktop app runs.
//! Every route acts on the caller's own relay only. Design:
//! `docs/specs/desktop-network-relay.md`.

use super::*;
use crate::relay_net::{Frame, RELAY_PROVIDERS, RelaySettings};

/// Frames one exchange may carry from the desktop.
const MAX_INBOUND_FRAMES: usize = 512;

fn hub() -> Result<Arc<crate::relay_net::RelayHub>> {
    crate::relay_net::hub().ok_or_else(|| {
        SeraError::Validation("the network relay is not running on this server".into())
    })
}

fn view(state: &ApiState, user_id: &str) -> Result<Value> {
    let settings = state.store.relay_settings(user_id)?;
    let status = crate::relay_net::hub()
        .map(|hub| hub.status(user_id))
        .unwrap_or_default();
    let providers: Vec<Value> = RELAY_PROVIDERS
        .iter()
        .map(|(kind, label, hosts)| {
            json!({
                "kind": kind,
                "label": label,
                "hosts": hosts,
                "selected": settings.providers.iter().any(|p| p == kind),
            })
        })
        .collect();
    Ok(json!({
        "enabled": settings.enabled,
        "providers": providers,
        "status": status,
        "available": crate::relay_net::hub().is_some(),
    }))
}

/// `GET /v1/relay/net`: the caller's settings, the providers that can be
/// relayed, and the live status.
pub(super) async fn relay_get(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    Ok(Json(view(&state, &user.id)?))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RelayPut {
    enabled: bool,
    #[serde(default)]
    providers: Vec<String>,
}

/// `PUT /v1/relay/net`: turn the relay on or off and choose providers. The
/// caller's cached model clients are dropped so the change applies to the
/// next request.
pub(super) async fn relay_put(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(body): Json<RelayPut>,
) -> Result<Json<Value>> {
    if body.providers.len() > RELAY_PROVIDERS.len() * 2 {
        return Err(SeraError::Validation("too many providers".into()));
    }
    let settings = RelaySettings {
        enabled: body.enabled,
        providers: body.providers,
    }
    .normalized();
    state.store.set_relay_settings(&user.id, &settings)?;
    state.runtime.router().invalidate_user(&user.id);
    state.store.record_audit(
        &user.id,
        "relay.settings",
        None,
        None,
        None,
        None,
        None,
        Some(&json!({"enabled": settings.enabled, "providers": settings.providers})),
    )?;
    Ok(Json(view(&state, &user.id)?))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExchangeBody {
    #[serde(default)]
    frames: Vec<Frame>,
    #[serde(default)]
    wait: bool,
}

/// `POST /v1/relay/net/exchange`: the desktop hands over its frames and gets
/// the ones waiting for it, waiting up to 20 s when `wait` is set. With the
/// relay off, it returns at once with `enabled: false`.
pub(super) async fn relay_exchange(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(body): Json<ExchangeBody>,
) -> Result<Json<Value>> {
    if body.frames.len() > MAX_INBOUND_FRAMES {
        return Err(SeraError::Validation("too many relay frames".into()));
    }
    let settings = state.store.relay_settings(&user.id)?;
    if !settings.enabled {
        return Ok(Json(json!({"enabled": false, "frames": []})));
    }
    let hub = hub()?;
    let frames = hub.exchange(&user.id, body.frames, body.wait).await;
    Ok(Json(json!({"enabled": true, "frames": frames})))
}
