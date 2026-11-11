//! Per-user watcher source overlay API (Big Wave B3).

use super::*;

/// Body of `PATCH /v1/sources/{name}`. Either field may be sent alone.
#[derive(Deserialize)]
pub(super) struct SourcePatchBody {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct AddRssBody {
    pub name: String,
    pub url: String,
}

/// `GET /v1/sources` — effective merged sources for the acting user. A feed URL
/// is shown to admins and to the member who added that feed; other members get `null`.
pub(super) async fn list_sources(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let sources =
        crate::sources::effective_sources_for_user(&state.config, &state.store, &user.id)?;
    let own: std::collections::HashSet<String> = state
        .store
        .list_user_sources(&user.id)?
        .into_iter()
        .filter(|row| row.origin == "member")
        .map(|row| row.name)
        .collect();
    let rows = sources
        .into_iter()
        .map(|source| {
            json!({
                "name": source.name,
                "kind": source.kind,
                "enabled": source.enabled,
                "url": super::monitoring::visible_feed_url(
                    &user,
                    own.contains(&source.name),
                    &source.url,
                ),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({"sources": rows})))
}

/// `PATCH /v1/sources/{name}`. The caller's own feed takes `enabled` and a new
/// `url` (checked against the host allowlist, as on add). An admin-defined
/// source takes `enabled` only, as an overlay for this user.
pub(super) async fn patch_source(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(name): Path<String>,
    Json(body): Json<SourcePatchBody>,
) -> Result<Json<Value>> {
    if body.enabled.is_none() && body.url.is_none() {
        return Err(SeraError::Validation("no source fields to update".into()));
    }
    let own = state
        .store
        .list_user_sources(&user.id)?
        .into_iter()
        .any(|row| row.origin == "member" && row.name == name);
    if own {
        let url = match body.url.as_deref() {
            Some(raw) => Some(crate::sources::validate_member_rss_url(&state.config, raw)?),
            None => None,
        };
        if !state
            .store
            .update_member_rss_source(&user.id, &name, body.enabled, url.as_deref())?
        {
            return Err(SeraError::NotFound(format!("source {name}")));
        }
        return Ok(Json(json!({"name": name, "enabled": body.enabled})));
    }
    if body.url.is_some() {
        return Err(SeraError::Validation(
            "only a feed you added can change its URL".into(),
        ));
    }
    let Some(enabled) = body.enabled else {
        return Err(SeraError::Validation("no source fields to update".into()));
    };
    let admin_sources = crate::config::load_sources(&state.config)?;
    let Some(admin) = admin_sources.iter().find(|s| s.name == name) else {
        return Err(SeraError::NotFound(format!("source {name}")));
    };
    state
        .store
        .upsert_user_source_overlay(&user.id, &name, enabled, &admin.kind)?;
    Ok(Json(json!({"name": name, "enabled": enabled})))
}

/// `POST /v1/sources/rss` — add a member RSS feed on the host allowlist.
pub(super) async fn add_member_rss(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(body): Json<AddRssBody>,
) -> Result<Json<Value>> {
    let url = crate::sources::validate_member_rss_url(&state.config, &body.url)?;
    if crate::config::load_sources(&state.config)?
        .iter()
        .any(|s| s.name == body.name)
    {
        return Err(SeraError::Validation(
            "name conflicts with an admin source".into(),
        ));
    }
    state
        .runtime
        .gateway()
        .scan_egress(&user.id, &body.name)
        .map_err(|_| SeraError::Validation("source name blocked by policy".into()))?;
    state
        .runtime
        .gateway()
        .scan_egress(&user.id, &url)
        .map_err(|_| SeraError::Validation("url blocked by policy".into()))?;
    match state
        .store
        .add_member_rss_source(&user.id, &body.name, &url)
    {
        Ok(()) => Ok(Json(
            json!({"name": body.name, "url": url, "kind": "rss", "enabled": true}),
        )),
        Err(SeraError::Database(rusqlite::Error::SqliteFailure(err, _)))
            if err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE =>
        {
            Err(SeraError::InvalidState("source already exists".into()))
        }
        Err(other) => Err(other),
    }
}

/// `DELETE /v1/sources/{name}` — remove a member-added RSS feed.
pub(super) async fn delete_member_source(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(name): Path<String>,
) -> Result<Json<Value>> {
    if !state.store.delete_member_rss_source(&user.id, &name)? {
        return Err(SeraError::NotFound(format!("source {name}")));
    }
    Ok(Json(json!({"deleted": name})))
}
