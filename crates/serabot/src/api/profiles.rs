//! Bot and group profiles: avatars, the one-step group create, and the bot
//! onboarding interview. Every handler acts only on the session user's rows.

use axum::body::Bytes;
use axum::extract::DefaultBodyLimit;

use super::*;
use crate::avatar::{self, AVATAR_MIME, MAX_UPLOAD_BYTES};
use crate::default_avatars::{DEFAULT_AVATAR_MIME, DEFAULT_AVATAR_VERSION, default_avatar};
use crate::onboarding::{self, Drafter, Interview, Line, Proposal, Stage};
use crate::profiles::{OnboardingRow, StoredAvatar};

/// The routes this module owns. They are merged into the protected router so
/// they sit behind the same token check and per-user rate limit.
pub(super) fn routes() -> Router<Arc<ApiState>> {
    // The avatar body is raw bytes; leave a little room over 1 MiB so the
    // handler's own size check produces the readable error.
    let avatars = Router::new()
        .route(
            "/v1/bots/{id}/avatar",
            get(get_bot_avatar)
                .put(put_bot_avatar)
                .delete(delete_bot_avatar),
        )
        .route(
            "/v1/conversations/{id}/avatar",
            get(get_group_avatar)
                .put(put_group_avatar)
                .delete(delete_group_avatar),
        )
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES + 4096))
        .layer(RequestBodyLimitLayer::new(MAX_UPLOAD_BYTES + 4096));
    Router::new()
        .merge(avatars)
        .route("/v1/groups", post(create_group))
        .route(
            "/v1/conversations/{id}/profile",
            get(group_profile).put(set_group_profile),
        )
        .route(
            "/v1/bots/{id}/onboarding",
            get(onboarding_state).post(onboarding_start),
        )
        .route("/v1/bots/{id}/onboarding/reply", post(onboarding_reply))
        .route("/v1/bots/{id}/onboarding/confirm", post(onboarding_confirm))
}

// ---------------------------------------------------------------------------
// Names to ids. The owner types a name; the id is derived, never asked for.
// ---------------------------------------------------------------------------

/// Lowercase ascii slug from a free-text name, at most `max` characters,
/// starting with a letter.
pub(super) fn slugify(name: &str, max: usize, fallback: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(character.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    if !slug.starts_with(|c: char| c.is_ascii_lowercase()) {
        slug = format!("{fallback}-{slug}");
    }
    slug.truncate(max);
    slug.trim_end_matches('-').to_owned()
}

/// `base`, `base-2`, `base-3`, ... the first one `taken` does not claim.
pub(super) fn unique_slug(
    base: &str,
    max: usize,
    taken: impl Fn(&str) -> Result<bool>,
) -> Result<String> {
    if !taken(base)? {
        return Ok(base.to_owned());
    }
    for n in 2..100 {
        let suffix = format!("-{n}");
        let mut stem = base.to_owned();
        stem.truncate(max.saturating_sub(suffix.len()));
        let candidate = format!("{}{suffix}", stem.trim_end_matches('-'));
        if !taken(&candidate)? {
            return Ok(candidate);
        }
    }
    Err(SeraError::InvalidState(
        "too many items share that name; pick another".into(),
    ))
}

fn clean_text(label: &str, text: &str, max: usize) -> Result<String> {
    let text = text.trim();
    if text.chars().count() > max {
        return Err(SeraError::Validation(format!(
            "{label} must be at most {max} characters"
        )));
    }
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(SeraError::Validation(format!(
            "{label} must not contain control characters"
        )));
    }
    Ok(text.to_owned())
}

// ---------------------------------------------------------------------------
// Avatars
// ---------------------------------------------------------------------------

fn image_response(avatar: crate::profiles::StoredAvatar) -> Response {
    (
        [
            (header::CONTENT_TYPE, avatar.mime),
            (header::CACHE_CONTROL, "private, no-cache".to_owned()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
            (header::ETAG, format!("\"{}\"", avatar.updated_at)),
        ],
        avatar.data,
    )
        .into_response()
}

/// Decode on a blocking thread: a 1 MiB image takes real CPU to resize.
async fn normalize(bytes: Bytes) -> Result<Vec<u8>> {
    tokio::task::spawn_blocking(move || avatar::normalize_avatar(&bytes))
        .await
        .map_err(|_| SeraError::Internal("avatar worker failed".into()))?
}

fn owned_bot(state: &ApiState, user: &ActingUser, id: &str) -> Result<()> {
    if !safe_bot_label(id) {
        return Err(SeraError::NotFound(format!("bot {id}")));
    }
    state
        .runtime
        .fleet()
        .row(&user.id, id)?
        .map(|_| ())
        .ok_or_else(|| SeraError::NotFound(format!("bot {id}")))
}

fn owned_conversation(state: &ApiState, user: &ActingUser, id: &str) -> Result<()> {
    validate_conversation_id(id)?;
    state
        .store
        .get_conversation(&user.id, id)?
        .map(|_| ())
        .ok_or_else(|| SeraError::NotFound(format!("conversation {id}")))
}

/// `PUT /v1/bots/{id}/avatar` - raw png, jpeg or webp bytes (at most 1 MiB).
pub(super) async fn put_bot_avatar(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<Value>> {
    owned_bot(&state, &user, &id)?;
    let png = normalize(body).await?;
    state
        .store
        .set_bot_avatar(&user.id, &id, AVATAR_MIME, &png)?;
    state.store.record_audit(
        &user.id,
        "bot.avatar_set",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"id": id, "bytes": png.len()})),
    )?;
    let version = state
        .store
        .get_bot_avatar(&user.id, &id)?
        .map(|stored| stored.updated_at);
    Ok(Json(json!({"avatar": {"bot": id, "version": version}})))
}

/// `GET /v1/bots/{id}/avatar` - the owner's upload (256x256 png), else the
/// bundled default for a shipped bot, else 404.
pub(super) async fn get_bot_avatar(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Response> {
    owned_bot(&state, &user, &id)?;
    if let Some(stored) = state.store.get_bot_avatar(&user.id, &id)? {
        return Ok(image_response(stored));
    }
    let data =
        default_avatar(&id).ok_or_else(|| SeraError::NotFound(format!("avatar for bot {id}")))?;
    Ok(image_response(StoredAvatar {
        mime: DEFAULT_AVATAR_MIME.to_owned(),
        data: data.to_vec(),
        updated_at: DEFAULT_AVATAR_VERSION.to_owned(),
    }))
}

/// `DELETE /v1/bots/{id}/avatar`
pub(super) async fn delete_bot_avatar(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    owned_bot(&state, &user, &id)?;
    let removed = state.store.delete_bot_avatar(&user.id, &id)?;
    if removed {
        state.store.record_audit(
            &user.id,
            "bot.avatar_removed",
            None,
            None,
            None,
            None,
            Some(state.runtime.policy_version()),
            Some(&json!({"id": id})),
        )?;
    }
    Ok(Json(json!({"deleted": removed, "id": id})))
}

pub(super) async fn put_group_avatar(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<Value>> {
    owned_conversation(&state, &user, &id)?;
    let png = normalize(body).await?;
    state
        .store
        .set_conversation_avatar(&user.id, &id, AVATAR_MIME, &png)?;
    let version = state
        .store
        .get_conversation_avatar(&user.id, &id)?
        .map(|stored| stored.updated_at);
    Ok(Json(
        json!({"avatar": {"conversation_id": id, "version": version}}),
    ))
}

pub(super) async fn get_group_avatar(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Response> {
    owned_conversation(&state, &user, &id)?;
    let stored = state
        .store
        .get_conversation_avatar(&user.id, &id)?
        .ok_or_else(|| SeraError::NotFound(format!("avatar for {id}")))?;
    Ok(image_response(stored))
}

pub(super) async fn delete_group_avatar(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    owned_conversation(&state, &user, &id)?;
    let removed = state.store.delete_conversation_avatar(&user.id, &id)?;
    Ok(Json(json!({"deleted": removed, "id": id})))
}

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

const MAX_GROUP_DESCRIPTION_CHARS: usize = 500;
const MAX_GROUP_RULES_CHARS: usize = 4000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateGroupRequest {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    rules: String,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    model: Option<ModelSelection>,
}

fn validate_member_bots(state: &ApiState, user: &ActingUser, members: &[String]) -> Result<()> {
    if members.is_empty() {
        return Ok(());
    }
    crate::teams::validate_members(members)?;
    let bots = state.runtime.fleet().configured(&user.id)?;
    for member in members {
        let Some(entry) = bots.iter().find(|candidate| candidate.id == *member) else {
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
    }
    Ok(())
}

/// `POST /v1/groups` - create a group chat in one step: name, optional
/// description, members, rules and model. The id is generated from the name.
pub(super) async fn create_group(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<CreateGroupRequest>,
) -> Response {
    match create_group_inner(&state, &user, request) {
        Ok(body) => Json(body).into_response(),
        Err(error) => team_error(error),
    }
}

fn create_group_inner(
    state: &ApiState,
    user: &ActingUser,
    request: CreateGroupRequest,
) -> Result<Value> {
    let title = normalize_conversation_title(request.name.trim())?;
    let description = clean_text(
        "description",
        &request.description,
        MAX_GROUP_DESCRIPTION_CHARS,
    )?;
    let rules = clean_text("rules", &request.rules, MAX_GROUP_RULES_CHARS)?;
    let mode = crate::teams::TeamMode::from_name(request.mode.as_deref().unwrap_or("selected"))?;
    for text in [&title, &description, &rules] {
        state.runtime.scan_owner_output(&user.id, text)?;
    }
    validate_member_bots(state, user, &request.members)?;
    if let Some(selection) = &request.model {
        state
            .runtime
            .router()
            .validate_selection(&user.id, selection)?;
    }
    let base = slugify(&title, 32, "group");
    let slug = unique_slug(&base, 32, |candidate| {
        let id = scoped_conversation_id(ConversationKind::Room, candidate)?;
        state.store.conversation_exists(&id)
    })?;
    let id = scoped_conversation_id(ConversationKind::Room, &slug)?;
    state.store.create_conversation_titled(
        &user.id,
        &id,
        ConversationKind::Room.as_str(),
        None,
        Some(&title),
    )?;
    if !request.members.is_empty() || !rules.is_empty() {
        // A rules-only group still needs a team row to carry the rules; the
        // member list may be empty only through this create path.
        state.store.set_conversation_team(
            &user.id,
            &id,
            &request.members,
            mode.as_str(),
            &rules,
        )?;
    }
    if !description.is_empty() {
        state
            .store
            .set_conversation_description(&user.id, &id, &description)?;
    }
    if let Some(selection) = request.model.clone() {
        state.runtime.select_model(&user.id, &id, selection)?;
    }
    let record = state
        .store
        .get_conversation(&user.id, &id)?
        .ok_or_else(|| SeraError::Internal("group missing after insert".into()))?;
    let details = json!({
        "conversation_id": id,
        "members": request.members,
        "mode": mode.as_str(),
        "rules": rules,
    });
    state.store.record_audit(
        &user.id,
        "group.created",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&details),
    )?;
    if !request.members.is_empty() {
        state
            .store
            .append_event(&user.id, crate::teams::AUDIT_MEMBER_SET, &details)?;
    }
    let body = json!({
        "group": conversation_view(&record)?,
        "members": request.members,
        "description": description,
    });
    state
        .runtime
        .scan_owner_output(&user.id, &body.to_string())?;
    Ok(body)
}

fn profile_view(state: &ApiState, user: &ActingUser, id: &str) -> Result<Value> {
    let profile = state.store.get_conversation_profile(&user.id, id)?;
    Ok(json!({
        "conversation_id": id,
        "description": profile.as_ref().map(|p| p.description.clone()).unwrap_or_default(),
        "has_avatar": profile.as_ref().is_some_and(|p| p.has_avatar),
        "version": profile.map(|p| p.updated_at),
    }))
}

pub(super) async fn group_profile(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    owned_conversation(&state, &user, &id)?;
    let body = profile_view(&state, &user, &id)?;
    state
        .runtime
        .scan_owner_output(&user.id, &body.to_string())?;
    Ok(Json(body))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SetGroupProfileRequest {
    description: String,
}

pub(super) async fn set_group_profile(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<SetGroupProfileRequest>,
) -> Result<Json<Value>> {
    owned_conversation(&state, &user, &id)?;
    let description = clean_text(
        "description",
        &request.description,
        MAX_GROUP_DESCRIPTION_CHARS,
    )?;
    state.runtime.scan_owner_output(&user.id, &description)?;
    state
        .store
        .set_conversation_description(&user.id, &id, &description)?;
    Ok(Json(profile_view(&state, &user, &id)?))
}

// ---------------------------------------------------------------------------
// Onboarding interview
// ---------------------------------------------------------------------------

/// The production drafter: one bounded runtime call under the acting user.
#[async_trait::async_trait]
impl Drafter for Runtime {
    async fn complete(
        &self,
        user_id: &str,
        bot: &str,
        system: &str,
        input: &str,
    ) -> Result<String> {
        self.bounded_completion(user_id, bot, system, input).await
    }
}

fn drafter(state: &ApiState) -> Arc<dyn Drafter> {
    match &state.onboarding_drafter {
        Some(custom) => custom.clone(),
        None => Arc::new(state.runtime.clone()),
    }
}

fn to_row(interview: &Interview, group_id: Option<String>) -> Result<OnboardingRow> {
    Ok(OnboardingRow {
        state: interview.stage.as_str().to_owned(),
        transcript_json: serde_json::to_string(&interview.lines)?,
        proposal_json: interview
            .proposal
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?,
        rounds: i64::from(interview.revisions),
        group_id,
    })
}

fn from_row(row: &OnboardingRow) -> Result<Interview> {
    let lines: Vec<Line> = serde_json::from_str(&row.transcript_json)
        .map_err(|_| SeraError::InvalidState("stored interview is invalid".into()))?;
    let proposal: Option<Proposal> = row
        .proposal_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| SeraError::InvalidState("stored proposal is invalid".into()))?;
    Ok(Interview {
        stage: Stage::from_name(&row.state)?,
        lines,
        proposal,
        revisions: u32::try_from(row.rounds).unwrap_or(0),
    })
}

fn interview_view(bot: &str, interview: &Interview) -> Value {
    json!({
        "bot": bot,
        "name": onboarding::display_name(bot),
        "state": interview.stage.as_str(),
        "messages": interview.lines,
        "proposal": interview.proposal,
        "can_confirm": interview.stage == Stage::Proposed,
        "done": interview.stage == Stage::Confirmed,
    })
}

/// Called by `POST /v1/bots`: the interview exists from the moment the bot
/// does, so the app can open it with one GET.
pub(super) fn begin_interview(
    state: &ApiState,
    user: &ActingUser,
    bot: &str,
    group_id: Option<String>,
) -> Result<()> {
    state
        .store
        .put_onboarding(&user.id, bot, &new_interview_row(bot, group_id)?)
}

/// The stored shape of a fresh interview for `bot`.
pub(super) fn new_interview_row(bot: &str, group_id: Option<String>) -> Result<OnboardingRow> {
    to_row(&Interview::start(&onboarding::display_name(bot)), group_id)
}

fn load_interview(
    state: &ApiState,
    user: &ActingUser,
    bot: &str,
) -> Result<(Interview, Option<String>)> {
    owned_bot(state, user, bot)?;
    let row = state
        .store
        .get_onboarding(&user.id, bot)?
        .ok_or_else(|| SeraError::NotFound(format!("onboarding for bot {bot}")))?;
    Ok((from_row(&row)?, row.group_id))
}

pub(super) async fn onboarding_state(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let (interview, _) = load_interview(&state, &user, &id)?;
    let body = interview_view(&id, &interview);
    state
        .runtime
        .scan_owner_output(&user.id, &body.to_string())?;
    Ok(Json(body))
}

/// `POST /v1/bots/{id}/onboarding` - (re)start the interview for an existing
/// bot. A running interview is returned as it is; a finished one starts over.
pub(super) async fn onboarding_start(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    owned_bot(&state, &user, &id)?;
    if let Some(row) = state.store.get_onboarding(&user.id, &id)? {
        let interview = from_row(&row)?;
        if interview.stage != Stage::Confirmed {
            return Ok(Json(interview_view(&id, &interview)));
        }
    }
    begin_interview(&state, &user, &id, None)?;
    let (interview, _) = load_interview(&state, &user, &id)?;
    Ok(Json(interview_view(&id, &interview)))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OnboardingReplyRequest {
    text: String,
}

pub(super) async fn onboarding_reply(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<OnboardingReplyRequest>,
) -> Result<Json<Value>> {
    let (mut interview, group_id) = load_interview(&state, &user, &id)?;
    let answer = onboarding::check_answer(&request.text)?;
    // The owner's words reach a model next, so they pass the policy first.
    state.runtime.scan_owner_output(&user.id, &answer)?;
    let drafter = drafter(&state);
    interview
        .reply(&answer, drafter.as_ref(), &user.id, &id)
        .await?;
    // Nothing the model produced is stored or shown unscanned.
    let view = interview_view(&id, &interview);
    state
        .runtime
        .scan_owner_output(&user.id, &view.to_string())?;
    state
        .store
        .put_onboarding(&user.id, &id, &to_row(&interview, group_id)?)?;
    Ok(Json(view))
}

/// `POST /v1/bots/{id}/onboarding/confirm` - save the proposal into the bot
/// through the ordinary bot update path (prompt + enabled), then add the bot
/// to the group picked at creation, if any.
pub(super) async fn onboarding_confirm(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let (mut interview, group_id) = load_interview(&state, &user, &id)?;
    let proposal = interview.confirm()?;
    let prompt = onboarding::final_prompt(&onboarding::display_name(&id), &proposal);
    let update = UpdateBotRequest {
        enabled: Some(true),
        prompt: Some(prompt),
        tools: None,
        skills: None,
        require_approval: None,
        display_name: None,
        description: None,
        model: None,
        browser_profile: None,
    };
    let _saved = update_bot(
        State(state.clone()),
        Extension(user.clone()),
        Path(id.clone()),
        Json(update),
    )
    .await?;
    let joined = match group_id.as_deref() {
        Some(group) => join_group(&state, &user, group, &id).is_ok(),
        None => false,
    };
    state
        .store
        .put_onboarding(&user.id, &id, &to_row(&interview, group_id)?)?;
    state.store.record_audit(
        &user.id,
        "bot.onboarded",
        None,
        None,
        None,
        None,
        Some(state.runtime.policy_version()),
        Some(&json!({"id": id, "joined_group": joined})),
    )?;
    let mut view = interview_view(&id, &interview);
    view["joined_group"] = json!(joined);
    Ok(Json(view))
}

/// Add `bot` to the room's team, keeping its mode and rules.
fn join_group(state: &ApiState, user: &ActingUser, group: &str, bot: &str) -> Result<()> {
    team_conversation(state, user, group)?;
    let (mut members, mode, rules) = match state.store.get_conversation_team(&user.id, group)? {
        Some(team) => (team.members, team.mode, team.rules),
        None => (
            Vec::new(),
            crate::teams::TeamMode::Selected.as_str().to_owned(),
            String::new(),
        ),
    };
    if members.iter().any(|member| member == bot) {
        return Ok(());
    }
    members.push(bot.to_owned());
    crate::teams::validate_members(&members)?;
    state
        .store
        .set_conversation_team(&user.id, group, &members, &mode, &rules)?;
    let details =
        json!({"conversation_id": group, "members": members, "mode": mode, "rules": rules});
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
    Ok(())
}
