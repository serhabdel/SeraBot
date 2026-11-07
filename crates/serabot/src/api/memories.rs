use super::*;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MemoryListQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
    /// Hybrid lexical search (`q` is the console's name; `query` is kept for
    /// older clients). A search ignores `offset`.
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    query: Option<String>,
    /// `explicit`, `extracted`, or `all` (default: any origin).
    #[serde(default)]
    origin: Option<String>,
    /// `active` (default), `superseded`, `pending`, or `all`.
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    bot: Option<String>,
}

pub(super) async fn list_memories(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<MemoryListQuery>,
) -> Result<Json<Value>> {
    let limit = query.limit.unwrap_or(100).clamp(1, memory::MAX_LIST_LIMIT);
    let filter = memory::MemoryFilter {
        origin: query.origin.clone(),
        status: query.status.clone(),
        bot: query.bot.clone(),
    };
    let needle = query
        .q
        .as_deref()
        .or(query.query.as_deref())
        .map(str::trim)
        .filter(|needle| !needle.is_empty());
    let (memories, offset) = match needle {
        Some(needle) => {
            if needle.chars().count() > 200 || needle.chars().any(char::is_control) {
                return Err(SeraError::Validation(
                    "search text must be at most 200 characters without control characters".into(),
                ));
            }
            let hits = memory::search(&state.store, &user.id, needle, &filter, limit)?;
            (
                hits.into_iter().map(|hit| hit.memory).collect::<Vec<_>>(),
                0,
            )
        }
        None => {
            let offset = query.offset.unwrap_or(0).min(100_000);
            (
                memory::list_page(&state.store, &user.id, &filter, offset, limit)?,
                offset,
            )
        }
    };
    let body = json!({"memories": memories, "offset": offset, "limit": limit});
    state
        .runtime
        .scan_owner_output(&user.id, &body.to_string())?;
    Ok(Json(body))
}

pub(super) async fn memory_stats(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let stats = memory::stats(&state.store, &user.id)?;
    Ok(Json(json!({"stats": stats})))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AddMemoryRequest {
    kind: String,
    content: String,
    #[serde(default)]
    source: Option<String>,
}

pub(super) async fn add_memory(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<AddMemoryRequest>,
) -> Result<Json<Value>> {
    state
        .runtime
        .scan_owner_output(&user.id, &request.content)?;
    let source = request
        .source
        .as_deref()
        .map(str::trim)
        .filter(|source| !source.is_empty())
        .unwrap_or("operator:desktop");
    let memory = memory::add(
        &state.store,
        &user.id,
        &request.kind,
        &request.content,
        source,
    )?;
    state.store.record_audit(
        &user.id,
        "memory.created",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({
            "id": memory.id,
            "kind": memory.kind,
            "source": memory.source,
        })),
    )?;
    Ok(Json(json!({"memory": memory})))
}

/// Owner edit of one memory: content, importance, or status (`active` or
/// `superseded`). Absent fields are unchanged. Provenance never changes.
pub(super) async fn patch_memory(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(patch): Json<memory::MemoryPatch>,
) -> Result<Json<Value>> {
    validate_memory_id(&id)?;
    if let Some(content) = &patch.content {
        state.runtime.scan_owner_output(&user.id, content)?;
    }
    let mut fields = Vec::new();
    if patch.content.is_some() {
        fields.push("content");
    }
    if patch.importance.is_some() {
        fields.push("importance");
    }
    if patch.status.is_some() {
        fields.push("status");
    }
    let memory = memory::update(&state.store, &user.id, &id, &patch)?;
    state.store.record_audit(
        &user.id,
        "memory.updated",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"id": id, "fields": fields})),
    )?;
    Ok(Json(json!({"memory": memory})))
}

pub(super) async fn delete_memory(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    validate_memory_id(&id)?;
    if !memory::delete(&state.store, &user.id, &id)? {
        return Err(SeraError::NotFound(format!("memory {id}")));
    }
    state.store.record_audit(
        &user.id,
        "memory.deleted",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"id": id})),
    )?;
    Ok(Json(json!({"deleted": id})))
}

fn validate_memory_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(SeraError::Validation("invalid memory id".into()));
    }
    Ok(())
}
