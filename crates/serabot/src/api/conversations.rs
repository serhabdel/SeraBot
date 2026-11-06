use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HistoryQuery {
    limit: Option<usize>,
}

pub(super) async fn conversation_messages(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<Value>> {
    validate_conversation_id(&id)?;
    let messages = state
        .store
        .list_visible_messages(&user.id, &id, query.limit.unwrap_or(100).clamp(1, 200))?
        .into_iter()
        .map(|message| {
            let content = if state
                .runtime
                .scan_owner_output(&user.id, &message.content)
                .is_ok()
            {
                message.content
            } else {
                "[Content withheld by policy]".to_owned()
            };
            let mut body = json!({
                "id": message.id,
                "role": message.role,
                "content": content,
                "created_at": message.created_at,
            });
            if let Some(name) = message
                .name
                .as_deref()
                .filter(|name| crate::orchestration::safe_bot_label(name))
                && state.runtime.scan_owner_output(&user.id, name).is_ok()
            {
                body["name"] = json!(name);
            }
            if let Some(bot) = message
                .bot
                .as_deref()
                .filter(|bot| crate::orchestration::safe_bot_label(bot))
                && state.runtime.scan_owner_output(&user.id, bot).is_ok()
            {
                body["bot"] = json!(bot);
            }
            body
        })
        .collect::<Vec<_>>();
    // Opening the thread is the acting user reading it.
    state.store.mark_conversation_read(&user.id, &id)?;
    Ok(Json(json!({"messages": messages})))
}

/// Team rejections must be HTTP 400 with the offending value named, so they do
/// not use the generic `SeraError::Validation` mapping (422, message dropped).
pub(super) fn team_error(error: SeraError) -> Response {
    match error {
        SeraError::Validation(message) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": message, "code": "invalid_team"})),
        )
            .into_response(),
        other => other.into_response(),
    }
}

/// A conversation that may carry a team: it must exist for the acting user
/// and be a `room:`.
pub(super) fn team_conversation(
    state: &ApiState,
    acting: &ActingUser,
    id: &str,
) -> Result<StoredConversation> {
    validate_conversation_id(id)?;
    let record = state
        .store
        .get_conversation(&acting.id, id)?
        .ok_or_else(|| SeraError::NotFound(format!("conversation {id}")))?;
    if crate::store::parse_stored_conversation_kind(&record.kind)? != ConversationKind::Room {
        return Err(SeraError::Validation(
            "a team is only available on a room conversation".into(),
        ));
    }
    Ok(record)
}

pub(super) fn team_member_view(
    state: &ApiState,
    user_id: &str,
    bots: &[crate::config::ConfiguredBot],
    bot: &str,
) -> Value {
    let entry = bots.iter().find(|candidate| candidate.id == bot);
    json!({
        "bot": bot,
        "role": entry.map(|entry| entry.role.clone()).unwrap_or_else(|| "unknown".into()),
        "enabled": entry.is_some_and(|entry| entry.enabled),
        "live": state.runtime.bot_loaded(user_id, bot),
    })
}

pub(super) fn team_view(
    state: &ApiState,
    user_id: &str,
    conversation_id: &str,
    members: &[String],
    mode: &str,
    rules: &str,
) -> Result<Value> {
    let bots = state.runtime.fleet().configured(user_id)?;
    let members = members
        .iter()
        .map(|bot| team_member_view(state, user_id, &bots, bot))
        .collect::<Vec<_>>();
    Ok(json!({
        "conversation_id": conversation_id,
        "mode": mode,
        "members": members,
        "rules": rules,
        "max_members": crate::teams::MAX_TEAM_MEMBERS,
    }))
}

pub(super) async fn conversation_team(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Response {
    let body = (|| -> Result<Value> {
        team_conversation(&state, &user, &id)?;
        let (members, mode, rules) = match state.store.get_conversation_team(&user.id, &id)? {
            Some(team) => (team.members, team.mode, team.rules),
            // No team yet is an empty shape, never a 404.
            None => (
                Vec::new(),
                crate::teams::TeamMode::Selected.as_str().to_owned(),
                String::new(),
            ),
        };
        let body = team_view(&state, &user.id, &id, &members, &mode, &rules)?;
        state
            .runtime
            .scan_owner_output(&user.id, &body.to_string())?;
        Ok(body)
    })();
    match body {
        Ok(body) => Json(body).into_response(),
        Err(error) => team_error(error),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SetTeamRequest {
    members: Vec<String>,
    #[serde(default = "default_team_mode")]
    mode: String,
    #[serde(default)]
    rules: String,
}

pub(super) fn default_team_mode() -> String {
    crate::teams::TeamMode::Selected.as_str().to_owned()
}

pub(super) async fn set_conversation_team(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<SetTeamRequest>,
) -> Response {
    let body = (|| -> Result<Value> {
        team_conversation(&state, &user, &id)?;
        let mode = crate::teams::TeamMode::from_name(&request.mode)?;
        crate::teams::validate_members(&request.members)?;
        if request.rules.chars().count() > 4000 {
            return Err(SeraError::Validation(
                "team rules must be at most 4000 characters".into(),
            ));
        }
        if request
            .rules
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        {
            return Err(SeraError::Validation(
                "team rules must not contain control characters".into(),
            ));
        }
        // Rules reach every later model request in the room, so they are
        // scanned before they are stored, not only on the way out.
        state.runtime.scan_owner_output(&user.id, &request.rules)?;
        let bots = state.runtime.fleet().configured(&user.id)?;
        for member in &request.members {
            let entry = bots.iter().find(|candidate| candidate.id == *member);
            let Some(entry) = entry else {
                return Err(SeraError::Validation(format!("unknown bot {member}")));
            };
            if member == crate::orchestration::ORCHESTRATOR_BOT || entry.role == "orchestrator" {
                return Err(SeraError::Validation(format!(
                    "bot {member} cannot be a team member"
                )));
            }
            if !entry.enabled {
                return Err(SeraError::Validation(format!("bot {member} is disabled")));
            }
            if !state.runtime.bot_loaded(&user.id, member) {
                return Err(SeraError::Validation(format!("bot {member} is not loaded")));
            }
        }
        state.store.set_conversation_team(
            &user.id,
            &id,
            &request.members,
            mode.as_str(),
            &request.rules,
        )?;
        let details = json!({
            "conversation_id": id,
            "members": request.members,
            "mode": mode.as_str(),
            "rules": request.rules,
        });
        state.store.record_audit(
            &user.id,
            crate::teams::AUDIT_MEMBER_SET,
            None,
            None,
            None,
            None,
            Some(state.runtime.policy_version()),
            Some(&details),
        )?;
        state
            .store
            .append_event(&user.id, crate::teams::AUDIT_MEMBER_SET, &details)?;
        let body = team_view(
            &state,
            &user.id,
            &id,
            &request.members,
            mode.as_str(),
            &request.rules,
        )?;
        state
            .runtime
            .scan_owner_output(&user.id, &body.to_string())?;
        Ok(body)
    })();
    match body {
        Ok(body) => Json(body).into_response(),
        Err(error) => team_error(error),
    }
}

pub(super) async fn conversation_library(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    validate_conversation_id(&id)?;
    state
        .store
        .get_conversation(&user.id, &id)?
        .ok_or_else(|| SeraError::NotFound(format!("conversation {id}")))?;
    let entries = state
        .store
        .list_conversation_artifacts(&user.id, &id)?
        .into_iter()
        .map(|entry| {
            let meta = state.artifacts.meta(&entry.hash).ok();
            json!({
                "action_id": entry.action_id,
                "tool": entry.tool,
                "hash": entry.hash,
                "filename": meta.as_ref().map(|meta| meta.filename.clone()),
                "title": meta.as_ref().map(|meta| meta.title.clone()),
                "created_at": entry.created_at,
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({"artifacts": entries})))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateConversationRequest {
    kind: String,
    slug: String,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

pub(super) async fn create_conversation(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<CreateConversationRequest>,
) -> Result<Json<Value>> {
    let kind = ConversationKind::from_name(&request.kind)?;
    let id = scoped_conversation_id(kind, &request.slug)?;
    let parent_id = match request.parent_id.as_deref() {
        None => None,
        Some("") => {
            return Err(SeraError::Validation(
                "conversation parent id is empty".into(),
            ));
        }
        Some(parent) => Some(parent),
    };
    if matches!(kind, ConversationKind::Section) && parent_id.is_some() {
        return Err(SeraError::Validation(
            "section conversations cannot have a parent".into(),
        ));
    }
    if matches!(kind, ConversationKind::Group) && parent_id.is_none() {
        return Err(SeraError::Validation(
            "group conversations require a section parent".into(),
        ));
    }
    let title = match request.title.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => {
            let title = normalize_conversation_title(raw)?;
            state.runtime.scan_owner_output(&user.id, &title)?;
            Some(title)
        }
    };
    state.store.create_conversation_titled(
        &user.id,
        &id,
        kind.as_str(),
        parent_id,
        title.as_deref(),
    )?;
    let record = state
        .store
        .get_conversation(&user.id, &id)?
        .ok_or_else(|| SeraError::Internal("conversation missing after insert".into()))?;
    state.store.record_audit(
        &user.id,
        "conversation.created",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({
            "id": record.id,
            "kind": record.kind,
            "parent_id": record.parent_id,
            "title": record.title,
            "accepts_turns": kind.accepts_turns(),
        })),
    )?;
    Ok(Json(json!({"conversation": conversation_view(&record)?})))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ConversationListQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    include_archived: Option<bool>,
}

pub(super) async fn list_conversations(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Query(query): Query<ConversationListQuery>,
) -> Result<Json<Value>> {
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let cursor = match query.cursor.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(value) => Some(parse_conversation_cursor(value)?),
    };
    let records = state.store.list_conversations(
        &user.id,
        limit,
        cursor.as_ref().map(|(at, id)| (*at, id.as_str())),
        query.include_archived.unwrap_or(false),
    )?;
    let next_cursor = if records.len() == limit {
        records.last().map(conversation_cursor)
    } else {
        None
    };
    let ids = records
        .iter()
        .map(|record| record.id.as_str())
        .collect::<Vec<_>>();
    let previews = state.store.last_message_previews(&user.id, &ids)?;
    let conversations = records
        .iter()
        .map(|record| {
            let mut view = conversation_view(record)?;
            if let Some((role, content)) = previews.get(&record.id) {
                // Same policy scan as the transcript: a blocked message never
                // leaks through the list.
                let text = if state.runtime.scan_owner_output(&user.id, content).is_ok() {
                    preview_text(content)
                } else {
                    "[Content withheld by policy]".to_owned()
                };
                if !text.is_empty() {
                    view["last_message"] = json!({"role": role, "text": text});
                }
            }
            Ok(view)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Json(json!({
        "conversations": conversations,
        "next_cursor": next_cursor,
    })))
}

/// Plain-text list preview: whitespace collapsed, control characters and
/// markdown emphasis marks dropped, at most 120 characters.
pub(super) fn preview_text(content: &str) -> String {
    let cleaned: String = content
        .chars()
        .filter(|c| !matches!(c, '*' | '`' | '#') && (!c.is_control() || c.is_whitespace()))
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(120).collect()
}

pub(super) fn conversation_view(record: &StoredConversation) -> Result<Value> {
    // Legacy rows predate the kind column; they are turn-capable direct chats.
    let kind = crate::store::parse_stored_conversation_kind(&record.kind)?;
    Ok(json!({
        "id": record.id,
        "kind": record.kind,
        "parent_id": record.parent_id,
        "title": record.title,
        "created_at": record.created_at,
        "updated_at": record.updated_at,
        "last_message_at": record.last_message_at,
        "archived_at": record.archived_at,
        "unread_count": record.unread_count,
        "accepts_turns": kind.accepts_turns(),
    }))
}

pub(super) fn conversation_cursor(record: &StoredConversation) -> String {
    format!(
        "{}|{}",
        record
            .updated_at
            .to_rfc3339_opts(SecondsFormat::Millis, true),
        record.id
    )
}

pub(super) fn parse_conversation_cursor(cursor: &str) -> Result<(DateTime<Utc>, String)> {
    let invalid = || SeraError::Validation("invalid conversation cursor".into());
    let (at, id) = cursor.rsplit_once('|').ok_or_else(invalid)?;
    let at = DateTime::parse_from_rfc3339(at)
        .map_err(|_| invalid())?
        .with_timezone(&Utc);
    validate_conversation_id(id)?;
    Ok((at, id.to_owned()))
}

pub(super) fn normalize_conversation_title(raw: &str) -> Result<String> {
    let title = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        return Err(SeraError::Validation(
            "conversation title must not be empty".into(),
        ));
    }
    let length = title.chars().count();
    if length > 120 {
        return Err(SeraError::Validation(format!(
            "conversation title has {length} characters; maximum is 120"
        )));
    }
    Ok(title)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RenameConversationRequest {
    title: String,
}

pub(super) async fn rename_conversation(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<RenameConversationRequest>,
) -> Result<Json<Value>> {
    validate_conversation_id(&id)?;
    let title = normalize_conversation_title(&request.title)?;
    state.runtime.scan_owner_output(&user.id, &title)?;
    if !state.store.rename_conversation(&user.id, &id, &title)? {
        return Err(SeraError::NotFound(format!("conversation {id}")));
    }
    state.store.record_audit(
        &user.id,
        "conversation.renamed",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"id": id, "title": title})),
    )?;
    let record = state
        .store
        .get_conversation(&user.id, &id)?
        .ok_or_else(|| SeraError::NotFound(format!("conversation {id}")))?;
    Ok(Json(json!({"conversation": conversation_view(&record)?})))
}

pub(super) async fn archive_conversation(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    validate_conversation_id(&id)?;
    if !state.store.archive_conversation(&user.id, &id)? {
        return Err(SeraError::NotFound(format!("conversation {id}")));
    }
    state.store.record_audit(
        &user.id,
        "conversation.archived",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"id": id})),
    )?;
    Ok(Json(json!({"id": id, "archived": true})))
}

pub(super) async fn unarchive_conversation(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    validate_conversation_id(&id)?;
    if !state.store.unarchive_conversation(&user.id, &id)? {
        return Err(SeraError::NotFound(format!("conversation {id}")));
    }
    state.store.record_audit(
        &user.id,
        "conversation.unarchived",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"id": id})),
    )?;
    Ok(Json(json!({"id": id, "archived": false})))
}
