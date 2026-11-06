use super::*;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MemoryListQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    query: Option<String>,
}

pub(super) async fn list_memories(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<MemoryListQuery>,
) -> Result<Json<Value>> {
    let limit = query.limit.unwrap_or(100).clamp(1, memory::MAX_LIST_LIMIT);
    let memories = match query
        .query
        .as_deref()
        .map(str::trim)
        .filter(|needle| !needle.is_empty())
    {
        Some(needle) => {
            let needle = needle.to_lowercase();
            let mut matches = memory::list(&state.store, &user.id, memory::MAX_LIST_LIMIT)?
                .into_iter()
                .filter(|memory| {
                    memory.content.to_lowercase().contains(&needle)
                        || memory.kind.to_lowercase().contains(&needle)
                        || memory.source.to_lowercase().contains(&needle)
                })
                .collect::<Vec<_>>();
            matches.truncate(limit);
            matches
        }
        None => memory::list(&state.store, &user.id, limit)?,
    };
    let body = json!({"memories": memories});
    state
        .runtime
        .scan_owner_output(&user.id, &body.to_string())?;
    Ok(Json(body))
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

pub(super) async fn delete_memory(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(SeraError::Validation("invalid memory id".into()));
    }
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
