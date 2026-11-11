//! Several chats per bot (v0.35.0). A bot's primary chat is `dir:<bot>`; each
//! extra chat is a direct conversation bound to the same bot. A new chat starts
//! with the bot's latest rolling summary, not the transcript of an old chat.
//! Rename and archive use the existing `/v1/conversations/{id}` routes.

use super::conversations::normalize_conversation_title;
use super::*;
use crate::memory;
use crate::store::BotChatRow;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListBotChatsQuery {
    #[serde(default)]
    include_archived: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateBotChatRequest {
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BotChatSettingsRequest {
    refresh_every_turns: u32,
}

fn ensure_bot(state: &ApiState, user: &ActingUser, bot: &str) -> Result<()> {
    let known = state
        .runtime
        .fleet()
        .configured(&user.id)?
        .iter()
        .any(|configured| configured.id == bot);
    if known {
        Ok(())
    } else {
        Err(SeraError::NotFound(format!("bot {bot}")))
    }
}

fn stamp(value: chrono::DateTime<chrono::Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// One chat as the desktop lists it. `None` when an extra chat has no
/// conversation row (it is then left out of the list).
fn chat_view(
    state: &ApiState,
    user: &ActingUser,
    bot: &str,
    id: &str,
    primary: bool,
    binding: Option<&BotChatRow>,
) -> Result<Option<Value>> {
    let has_summary = memory::latest_summary(&state.store, &user.id, id)?.is_some();
    let carried_from = binding.and_then(|row| row.carried_from.clone());
    let Some(conversation) = state.store.get_conversation(&user.id, id)? else {
        if !primary {
            return Ok(None);
        }
        // The primary chat has no row until its first message.
        return Ok(Some(json!({
            "id": id, "bot": bot, "title": null, "primary": true, "archived": false,
            "carried_from": null, "created_at": null, "updated_at": null,
            "last_message_at": null, "turns": 0, "has_summary": has_summary,
        })));
    };
    let turns = state.store.count_user_turns(&user.id, id)?;
    Ok(Some(json!({
        "id": id,
        "bot": bot,
        "title": conversation.title,
        "primary": primary,
        "archived": conversation.archived_at.is_some(),
        "carried_from": carried_from,
        "created_at": stamp(conversation.created_at),
        "updated_at": stamp(conversation.updated_at),
        "last_message_at": conversation.last_message_at.map(stamp),
        "turns": turns,
        "has_summary": has_summary,
    })))
}

/// The newest rolling summary across a bot's chats, with the chat it came from.
fn latest_bot_summary(
    state: &ApiState,
    user: &ActingUser,
    bot: &str,
) -> Result<Option<(String, String)>> {
    let mut ids = vec![format!("dir:{bot}")];
    ids.extend(
        state
            .store
            .bot_chat_rows(&user.id, bot)?
            .into_iter()
            .map(|row| row.conversation_id),
    );
    let mut best: Option<(String, String, String)> = None;
    for id in ids {
        if let Some(found) = memory::latest_summary(&state.store, &user.id, &id)? {
            let newer = best
                .as_ref()
                .is_none_or(|(created, _, _)| found.created_at > *created);
            if newer {
                best = Some((found.created_at, id, found.summary));
            }
        }
    }
    Ok(best.map(|(_, id, summary)| (id, summary)))
}

/// `GET /v1/bots/{bot}/chats`: the primary chat, then the extra chats, with
/// the bot's memory refresh interval.
pub(super) async fn list_bot_chats(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(bot): Path<String>,
    Query(query): Query<ListBotChatsQuery>,
) -> Result<Json<Value>> {
    ensure_bot(&state, &user, &bot)?;
    let include_archived = query.include_archived.unwrap_or(false);
    let primary_id = format!("dir:{bot}");
    let mut chats = Vec::new();
    if let Some(view) = chat_view(&state, &user, &bot, &primary_id, true, None)? {
        chats.push(view);
    }
    for row in state.store.bot_chat_rows(&user.id, &bot)? {
        if let Some(view) = chat_view(&state, &user, &bot, &row.conversation_id, false, Some(&row))?
        {
            let archived = view["archived"].as_bool().unwrap_or(false);
            if include_archived || !archived {
                chats.push(view);
            }
        }
    }
    let every = state.store.bot_refresh_every(&user.id, &bot)?;
    Ok(Json(json!({
        "bot": bot,
        "refresh_every_turns": every,
        "chats": chats,
    })))
}

/// `POST /v1/bots/{bot}/chats`: a new chat with the bot. It starts with the
/// bot's latest summary (`carried_from` names the chat it came from).
pub(super) async fn create_bot_chat(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(bot): Path<String>,
    Json(request): Json<CreateBotChatRequest>,
) -> Result<Json<Value>> {
    ensure_bot(&state, &user, &bot)?;
    let title = match request.title.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => {
            let title = normalize_conversation_title(raw)?;
            state.runtime.scan_owner_output(&user.id, &title)?;
            Some(title)
        }
    };
    let carried = latest_bot_summary(&state, &user, &bot)?;
    // `chat-` and twelve lowercase hex digits: a valid, unguessable slug.
    let suffix: String = crate::domain::new_id()
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .take(12)
        .collect::<String>()
        .to_ascii_lowercase();
    let id = format!("dir:chat-{suffix}");
    let carried_from = carried.as_ref().map(|(from, _)| from.as_str());
    state
        .store
        .create_conversation_titled(&user.id, &id, "direct", None, title.as_deref())?;
    state
        .store
        .bind_bot_chat(&user.id, &id, &bot, carried_from)?;
    if let Some((_, summary)) = &carried {
        memory::store_summary(&state.store, &user.id, &id, Some(&bot), summary, None)?;
    }
    let binding = state
        .store
        .bot_chat_rows(&user.id, &bot)?
        .into_iter()
        .find(|row| row.conversation_id == id);
    let view = chat_view(&state, &user, &bot, &id, false, binding.as_ref())?
        .ok_or_else(|| SeraError::Internal("chat missing after insert".into()))?;
    state.store.record_audit(
        &user.id,
        "bot_chat.created",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"bot": bot, "carried": carried.is_some()})),
    )?;
    Ok(Json(json!({
        "chat": view,
        "carried_from": carried_from,
    })))
}

/// `PUT /v1/bots/{bot}/chat-settings`: user turns between memory refreshes
/// (5 to 500, default 40).
pub(super) async fn put_bot_chat_settings(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(bot): Path<String>,
    Json(request): Json<BotChatSettingsRequest>,
) -> Result<Json<Value>> {
    ensure_bot(&state, &user, &bot)?;
    state
        .store
        .set_bot_refresh_every(&user.id, &bot, request.refresh_every_turns)?;
    state.store.record_audit(
        &user.id,
        "bot_chat.settings_updated",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"bot": bot, "refresh_every_turns": request.refresh_every_turns})),
    )?;
    Ok(Json(json!({
        "bot": bot,
        "refresh_every_turns": request.refresh_every_turns,
    })))
}
