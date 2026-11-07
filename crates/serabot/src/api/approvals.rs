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
    #[serde(default)]
    after: Option<i64>,
    #[serde(default)]
    limit: Option<usize>,
}

pub(super) async fn events(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    headers: HeaderMap,
    Query(query): Query<EventQuery>,
) -> Sse<impl futures::Stream<Item = std::result::Result<SseEvent, Infallible>>> {
    let last = resume_cursor(&headers, query.after);
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let store = state.store.clone();
    let user_id = user.id;
    let stream = futures::stream::unfold(
        (last, store, user_id),
        move |(last, store, user_id)| async move {
            loop {
                tokio::time::sleep(Duration::from_millis(180)).await;
                match store.events_after(&user_id, last, limit) {
                    Ok(events) if !events.is_empty() => {
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
                        return Some((futures::stream::iter(items), (next, store, user_id)));
                    }
                    Ok(_) => continue,
                    Err(error) => {
                        warn!(%error, "event stream query failed");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        },
    )
    .flatten();
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
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
