use super::*;

pub(super) fn task_view(task: &Task) -> Value {
    json!({
        "id": task.id,
        "kind": task.kind,
        "bot": task.bot,
        "status": task.status,
        "attempts": task.attempts,
        "created_at": task.created_at,
        "run_after": task.run_after,
    })
}

pub(super) async fn task(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let task = state.store.get_task(&user.id, &id)?;
    Ok(Json(json!({"task": task_view(&task)})))
}

pub(super) async fn list_approvals(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let approvals = state
        .store
        .list_pending_approvals(&user.id, 100)?
        .into_iter()
        .map(|approval| {
            json!({
                "id": approval.id,
                "action_id": approval.action_id,
                "tool": approval.tool,
                "summary": approval.summary,
                "expires_at": approval.expires_at,
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({"approvals": approvals})))
}

#[derive(Debug, Deserialize)]
pub(super) struct DecideRequest {
    decision: String,
    #[serde(default)]
    note: Option<String>,
}

pub(super) async fn decide_approval(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<DecideRequest>,
) -> Result<Json<Value>> {
    if request
        .note
        .as_ref()
        .is_some_and(|note| note.chars().count() > 1000)
    {
        return Err(SeraError::Validation("note is too long".into()));
    }
    let decision = match request.decision.as_str() {
        "once" => Decision::Once,
        "always" => Decision::Always,
        "deny" => Decision::Deny,
        _ => {
            return Err(SeraError::Validation(
                "decision must be once, always, or deny".into(),
            ));
        }
    };
    let result = state
        .runtime
        .handle_approval(&user.id, &id, decision)
        .await?;
    Ok(Json(json!({
        "approval_id": result.approval.id,
        "approval_state": result.approval.state,
        "action_id": result.action.id,
        "action_status": result.action.status,
        "resume_task": result.resume_task,
    })))
}

#[derive(Debug, Deserialize)]
pub(super) struct EventQuery {
    /// An event id, or `latest` to start at the newest event.
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

/// Longest an idle stream waits before it re-checks the table, in case an
/// event was written by a path that does not wake the stream.
const EVENT_RECHECK: Duration = Duration::from_secs(1);
/// Comment frames keep proxies and mobile NATs from dropping an idle stream,
/// and let clients notice a dead one.
const EVENT_KEEP_ALIVE: Duration = Duration::from_secs(10);

/// `GET /v1/events/head`: the newest event id, so a fresh client can start
/// there instead of replaying the retained history.
pub(super) async fn events_head(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    Ok(Json(json!({"id": state.store.latest_event_id(&user.id)?})))
}

pub(super) async fn events(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    headers: HeaderMap,
    Query(query): Query<EventQuery>,
) -> Result<Sse<impl futures::Stream<Item = std::result::Result<SseEvent, Infallible>>>> {
    let last = match query.after.as_deref().map(str::trim) {
        Some("latest") | Some("head") => state.store.latest_event_id(&user.id)?,
        Some(raw) => {
            let parsed = raw
                .parse::<i64>()
                .map_err(|_| SeraError::Validation("after must be an event id or latest".into()))?;
            resume_cursor(&headers, Some(parsed))
        }
        None => resume_cursor(&headers, None),
    };
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let store = state.store.clone();
    let notifier = store.event_notifier();
    let user_id = user.id;
    let stream = futures::stream::unfold(
        (last, store, notifier, user_id),
        move |(last, store, notifier, user_id)| async move {
            loop {
                // Register before querying so a write between the query and
                // the wait still wakes this stream.
                let waiter = Arc::clone(&notifier);
                let notified = waiter.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let query_store = store.clone();
                let query_user = user_id.clone();
                let found = tokio::task::spawn_blocking(move || {
                    query_store.events_after(&query_user, last, limit)
                })
                .await;
                match found {
                    Ok(Ok(events)) if !events.is_empty() => {
                        let next = events.last().map(|event| event.id).unwrap_or(last);
                        let items = events
                            .into_iter()
                            .map(|event| {
                                Ok(SseEvent::default()
                                    .id(event.id.to_string())
                                    .event(event.kind)
                                    .data(event.payload.to_string()))
                            })
                            .collect::<Vec<_>>();
                        return Some((
                            futures::stream::iter(items),
                            (next, store, notifier, user_id),
                        ));
                    }
                    Ok(Ok(_)) => {
                        tokio::select! {
                            _ = &mut notified => {}
                            _ = tokio::time::sleep(EVENT_RECHECK) => {}
                        }
                    }
                    Ok(Err(error)) => {
                        warn!(%error, "event stream query failed");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                    Err(error) => {
                        warn!(%error, "event stream query task failed");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        },
    )
    .flatten();
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(EVENT_KEEP_ALIVE)))
}

/// `?after=` wins; otherwise honor the standard SSE reconnect header.
pub(super) fn resume_cursor(headers: &HeaderMap, after: Option<i64>) -> i64 {
    after
        .or_else(|| {
            headers
                .get("last-event-id")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<i64>().ok())
        })
        .unwrap_or(0)
        .max(0)
}

#[derive(Debug, Deserialize)]
pub(super) struct AuditQuery {
    #[serde(default)]
    limit: Option<usize>,
}

pub(super) async fn audit(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<AuditQuery>,
) -> Result<Json<Value>> {
    let limit = query.limit.unwrap_or(50).clamp(1, 500);
    // Members see only their own rows; the admin keeps the cross-user audit
    // view (spec: the admin sees everything in audit — audit rows only,
    // never another user's data).
    let entries = if user.is_admin() {
        state.store.list_audit(limit)?
    } else {
        state.store.list_audit_for_user(&user.id, limit)?
    };
    Ok(Json(json!({"audit": entries})))
}

pub(super) async fn artifact(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(hash): Path<String>,
) -> Result<Response> {
    // Only artifacts produced in the acting user's own conversations.
    if !state.store.user_owns_artifact(&user.id, &hash)? {
        return Err(SeraError::NotFound("artifact".into()));
    }
    let artifact = state.artifacts.load(&hash)?;
    let filename = artifact.filename.replace(['"', '\\', '\r', '\n'], "-");
    let mime = crate::util::mime_for_filename(&artifact.filename);
    Ok((
        [
            ("content-type", mime),
            ("x-content-type-options", "nosniff"),
            (
                "content-disposition",
                &format!("attachment; filename=\"{filename}\""),
            ),
        ],
        Body::from(artifact.bytes),
    )
        .into_response())
}

pub(super) async fn approve_artifact(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(hash): Path<String>,
) -> Result<Json<Value>> {
    state.artifacts.approve(&state.store, &user.id, &hash)?;
    Ok(Json(json!({
        "artifact_hash": hash,
        "content_approved": true,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StandingGrantsQuery {
    #[serde(default)]
    limit: Option<usize>,
}

fn standing_grant_view(grant: &crate::domain::StandingGrant) -> Value {
    json!({
        "id": grant.id,
        "tool": grant.tool,
        "canonical_hash": grant.canonical_hash,
        "destination": grant.destination,
        "policy_version": grant.policy_version,
        "approval_id": grant.approval_id,
        "revoked_at": grant
            .revoked_at
            .as_ref()
            .map(|at| crate::store::to_string(*at)),
        "created_at": crate::store::to_string(grant.created_at),
        "active": grant.revoked_at.is_none(),
    })
}

/// `GET /v1/standing-grants`: hashes and destinations only — never canonical args.
pub(super) async fn list_standing_grants(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<StandingGrantsQuery>,
) -> Result<Json<Value>> {
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let grants = state
        .store
        .list_standing_grants(&user.id, limit)?
        .into_iter()
        .map(|grant| standing_grant_view(&grant))
        .collect::<Vec<_>>();
    Ok(Json(json!({ "grants": grants, "limit": limit })))
}

/// `DELETE /v1/standing-grants/{id}`: revoke one grant for the acting user.
pub(super) async fn revoke_standing_grant_endpoint(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    crate::util::validate_id(&id)?;
    let (grant, revoked_now) = state
        .store
        .revoke_standing_grant(&user.id, &id)?
        .ok_or_else(|| SeraError::NotFound("standing_grant".into()))?;
    if revoked_now {
        state.store.record_audit(
            &user.id,
            "standing_grant.revoked",
            Some(&grant.tool),
            None,
            Some(&grant.approval_id),
            None,
            Some(&grant.policy_version),
            Some(&json!({
                "grant_id": grant.id,
                "canonical_hash": grant.canonical_hash,
                "destination": grant.destination,
            })),
        )?;
    }
    Ok(Json(json!({ "grant": standing_grant_view(&grant) })))
}
