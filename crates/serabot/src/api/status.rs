use super::*;

pub(super) async fn status(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Response {
    let pending = state.store.pending_task_count(&user.id).unwrap_or(-1);
    let approvals = state
        .store
        .list_pending_approvals(&user.id, 50)
        .map(|items| items.len())
        .unwrap_or(0);
    let approval_items = state
        .store
        .list_pending_approvals(&user.id, 50)
        .unwrap_or_default()
        .into_iter()
        .map(|approval| {
            json!({
                "id": approval.id,
                "tool": approval.tool,
                "summary": approval.summary,
                "expires_at": approval.expires_at,
                "action_id": approval.action_id,
            })
        })
        .collect::<Vec<_>>();
    Json(json!({
        "ok": true,
        "pending_tasks": pending,
        "pending_approvals": approvals,
        "approvals": approval_items,
        "policy_version": "managed-by-server",
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
pub(super) struct UsageQuery {
    #[serde(default)]
    days: Option<i64>,
    #[serde(default)]
    bot: Option<String>,
    #[serde(default)]
    conversation_id: Option<String>,
    /// `all` (admin only) rolls up every user; anything else is rejected.
    #[serde(default)]
    scope: Option<String>,
}

/// `GET /v1/usage` rolls up `model.used` audit rows. `days` defaults to 7 and
/// is clamped to 1..=90; `bot` and `conversation_id` are optional filters.
/// `scope=all` (admin only) covers every user; the default is the caller.
pub(super) async fn usage(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<UsageQuery>,
) -> Result<Json<Value>> {
    let days = query.days.unwrap_or(7).clamp(1, 90);
    if let Some(bot) = &query.bot
        && !safe_bot_label(bot)
    {
        return Err(SeraError::Validation("invalid bot id".into()));
    }
    if let Some(conversation_id) = &query.conversation_id {
        validate_conversation_id(conversation_id)?;
    }
    let all_users = match query.scope.as_deref() {
        None | Some("") | Some("me") => false,
        Some("all") => {
            require_admin(&user)?;
            true
        }
        Some(_) => return Err(SeraError::Validation("scope must be me or all".into())),
    };
    let since = Utc::now() - chrono::Duration::days(days);
    let rollup = state.store.usage_rollup_for(
        (!all_users).then_some(user.id.as_str()),
        since,
        query.bot.as_deref(),
        query.conversation_id.as_deref(),
    )?;
    let body = json!({
        "totals": rollup.totals,
        "per_day": rollup.per_day,
        "per_bot": rollup.per_bot,
        "per_model": rollup.per_model,
        "tools": rollup.tools,
        "scope": if all_users { "all" } else { "me" },
    });
    // Same egress scan as the model catalog endpoints.
    state
        .runtime
        .scan_owner_output(&user.id, &body.to_string())?;
    Ok(Json(body))
}

/// `GET /v1/admin/status` (admin only): server health for the owner's Usage
/// tab. Numbers only: uptime, memory, CPU, disk, a storage breakdown of the
/// data directory and per-member counts and sizes. It never returns message
/// text, file names, documents or credentials. The directory walk is bounded
/// and cached (see `sysstatus`). Views are audited at most every 15 minutes
/// (the card polls); a refused member is always audited.
pub(super) async fn admin_status(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    if let Err(error) = require_admin(&user) {
        let _ = state.store.record_audit(
            &user.id,
            "admin.status.denied",
            None,
            None,
            None,
            None,
            Some(state.runtime.policy_version()),
            None,
        );
        return Err(error);
    }
    if state.sys_status.should_audit() {
        state.store.record_audit(
            &user.id,
            "admin.status.viewed",
            None,
            None,
            None,
            None,
            Some(state.runtime.policy_version()),
            None,
        )?;
    }
    let data_dir = state
        .store
        .path()
        .parent()
        .map(std::path::Path::to_path_buf)
        .filter(|path| path.is_dir());
    let now = Utc::now();
    let reliability = crate::sysstatus::reliability_counts(&state.store, &state.config, now)
        .map_err(|error| SeraError::Internal(format!("reliability counts failed: {error}")))?;
    let work = {
        let state = state.clone();
        let reliability = serde_json::to_value(&reliability)?;
        move || -> Result<Value> {
            use crate::sysstatus::{Category, Rules};
            let mut users = crate::sysstatus::user_totals(&state.store)?;
            let ids: std::collections::HashSet<String> =
                users.iter().map(|u| u.id.clone()).collect();
            let host = state
                .sys_status
                .host_snapshot(data_dir.as_deref().unwrap_or(std::path::Path::new(".")));
            let storage = match &data_dir {
                Some(root) => {
                    let root = root.canonicalize().unwrap_or_else(|_| root.clone());
                    let canon = |path: std::path::PathBuf| path.canonicalize().unwrap_or(path);
                    let rules = Rules::new(
                        &root,
                        &canon(state.store.path().to_path_buf()),
                        &canon(state.config.path(&state.config.documents.watch_dir)),
                        &canon(state.config.artifacts_dir()),
                    );
                    state.sys_status.storage(&root, &rules, &ids)
                }
                None => Default::default(),
            };
            for member in &mut users {
                member.file_bytes = storage.user_bytes.get(&member.id).copied().unwrap_or(0);
            }
            let categories: Vec<Value> = Category::ALL
                .iter()
                .map(|category| {
                    let (bytes, files) =
                        storage.categories.get(category).copied().unwrap_or((0, 0));
                    json!({
                        "id": category.id(),
                        "label": category.label(),
                        "bytes": bytes,
                        "files": files,
                    })
                })
                .collect();
            Ok(json!({
                "ok": true,
                "version": env!("CARGO_PKG_VERSION"),
                "reliability": reliability,
                "uptime_secs": state.sys_status.uptime_secs(),
                "process": {
                    "memory_bytes": host.process_rss_bytes,
                    "cpu_percent": host.process_cpu_percent,
                },
                "host": {
                    "memory_total_bytes": host.host_memory_total_bytes,
                    "memory_used_bytes": host.host_memory_used_bytes,
                    "cpu_percent": host.host_cpu_percent,
                    "cpu_cores": host.cpu_cores,
                },
                "disk": host.disk,
                "storage": {
                    "total_bytes": storage.total_bytes,
                    "total_files": storage.total_files,
                    "truncated": storage.truncated,
                    "scanned_at": storage.scanned_at,
                    "categories": categories,
                },
                "users": users,
            }))
        }
    };
    let body = tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| SeraError::Internal(format!("status task failed: {error}")))??;
    Ok(Json(body))
}
