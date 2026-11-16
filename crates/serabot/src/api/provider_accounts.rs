//! Per-member AI provider accounts: several API keys or sign-ins per
//! provider, their sign-in flows, tests and quota snapshots.
//!
//! Every route acts on the calling member's own accounts. An account id that
//! belongs to someone else, or to no one, is a plain 404. No response, event or
//! audit row carries a key, a token, a code or a verifier; the listing shows
//! the identity a provider reported with the email masked.

use super::*;
use crate::accounts::{AccountPatch, AccountStatus, MAX_LABEL_CHARS, ProviderAccount};
use crate::auth_flows::{CompleteRequest, FlowKind, FlowStatus, StartRequest};
use crate::llm_failure::{FailKind, classify_llm};
use crate::providers::{
    AuthPlan, CredentialOwner, auth_plan, canonical_kind, open_provider_client, resolve_base_url,
    resolve_catalog,
};

/// An account's quota is read from its provider at most this often.
const QUOTA_READ_INTERVAL: Duration = Duration::from_secs(240);

/// Reads an account's remaining quota from its provider. Any failure means "no
/// reading", never an error the member has to act on.
#[async_trait::async_trait]
pub trait QuotaReader: Send + Sync {
    async fn read(
        &self,
        credential: &ProviderCredential,
        identity: &Value,
    ) -> Result<Option<Value>>;
}

/// `p***@example.com`: enough to tell two accounts apart, not enough to mail.
pub(super) fn mask_email(email: &str) -> String {
    match email.split_once('@') {
        Some((local, domain)) if !local.is_empty() && !domain.is_empty() => {
            let first: String = local.chars().take(1).collect();
            format!("{first}***@{domain}")
        }
        _ => "***".into(),
    }
}

/// The sign-in flows a member's page may offer for a provider. A plan sign-in
/// (Claude, Codex) shows only on a provider that has not turned `plan_oauth`
/// off, and Claude's only to the admin.
pub(super) fn signin_flows(
    state: &ApiState,
    is_admin: bool,
    provider_id: &str,
    kind: &str,
) -> Vec<&'static str> {
    if matches!(kind, "anthropic" | "openai-codex") {
        let enabled = state
            .config
            .llm
            .providers
            .get(provider_id)
            // Not saved yet: the directory offers the sign-in (it is on by default).
            .is_none_or(ProviderConfig::plan_oauth_enabled);
        if !enabled || (kind == "anthropic" && !is_admin) {
            return Vec::new();
        }
    }
    state.signin.registry().flows_for(kind)
}

/// The risk a plan sign-in carries, for the banner the page shows beside it.
/// `None` for every sign-in that is the vendor's supported path.
pub(super) fn signin_risk(kind: &str, flows: &[&'static str]) -> Option<&'static str> {
    if flows.is_empty() {
        return None;
    }
    match kind {
        "anthropic" => Some(
            "This signs in through Claude's own client, not an API contract. Anthropic may restrict or suspend an account used this way. It is your own account and is never shared. API keys are the supported path.",
        ),
        "openai-codex" => Some(
            "This signs in through ChatGPT's own Codex client, not an API contract. OpenAI may restrict or suspend an account used this way. It is your own account and is never shared. API keys on the OpenAI provider are the supported path.",
        ),
        _ => None,
    }
}

/// A plan sign-in needs a provider that has not turned `plan_oauth` off.
/// Claude's is for the admin; Codex's is open to members (each account is its
/// owner's own). Every other provider's sign-in is open to members.
pub(super) fn plan_signin_gate(
    user: &ActingUser,
    profile: &ProviderConfig,
    kind: &str,
) -> Result<()> {
    if !matches!(kind, "anthropic" | "openai-codex") {
        return Ok(());
    }
    if !profile.plan_oauth_enabled() {
        return Err(SeraError::Rejected {
            code: "signin_unsupported",
            message: "This provider has no sign-in; add its API key".into(),
        });
    }
    if kind == "anthropic" && !user.is_admin() {
        return Err(SeraError::Forbidden(
            "Claude plan sign-in is only available to the server admin".into(),
        ));
    }
    Ok(())
}

/// The identity a provider reported, cut down to what the page shows.
fn masked_identity(identity: &Value) -> Value {
    let mut shown = serde_json::Map::new();
    let text = |key: &str| {
        identity
            .get(key)
            .and_then(Value::as_str)
            .map(|value| truncate_chars(value, 64))
    };
    if let Some(email) = identity.get("email").and_then(Value::as_str) {
        shown.insert("email".into(), json!(mask_email(email)));
    }
    for key in ["plan", "tier", "org"] {
        if let Some(value) = text(key) {
            shown.insert(key.into(), json!(value));
        }
    }
    Value::Object(shown)
}

/// One account as the API shows it.
pub(super) fn account_view(account: &ProviderAccount) -> Value {
    json!({
        "id": account.account_id,
        "label": account.label,
        "auth_kind": account.auth_kind.as_str(),
        "status": account.status.as_str(),
        "priority": account.priority,
        "weight": account.weight,
        "identity": masked_identity(&account.identity),
        "quota": account.quota,
        "cooldown_until": account.cooldown_until,
        "last_used_at": account.last_used_at,
        "created_at": account.created_at,
    })
}

/// The acting member's accounts for one provider, as the API shows them.
pub(super) fn provider_account_views(
    state: &ApiState,
    user_id: &str,
    provider: &str,
) -> Result<Vec<Value>> {
    Ok(state
        .accounts
        .list(user_id, provider)?
        .iter()
        .map(account_view)
        .collect())
}

/// Providers the member has accounts for but that the router no longer lists
/// (every account disabled), so the page can still show and re-enable them.
pub(super) fn parked_provider_views(
    state: &ApiState,
    user_id: &str,
    is_admin: bool,
    listed: &[Value],
) -> Result<Vec<Value>> {
    let mut parked = Vec::new();
    let mut settings: BTreeMap<String, ProviderConfig> = state.config.llm.providers.clone();
    settings.insert("default".to_owned(), state.config.llm.default_provider());
    let mut seen = HashSet::new();
    for account in state.store.provider_accounts(user_id, None)? {
        if !seen.insert(account.provider.clone())
            || listed
                .iter()
                .any(|view| view["id"].as_str() == Some(&account.provider))
        {
            continue;
        }
        let Some(profile) = settings.get(&account.provider) else {
            continue;
        };
        let kind = canonical_kind(&profile.kind)?;
        parked.push(json!({
            "id": account.provider,
            "kind": kind,
            "auth": if matches!(auth_plan(&kind, &profile.auth)?, AuthPlan::OAuth { .. }) { "oauth" } else { "api_key" },
            "base_url": profile.base_url,
            "models": profile.models,
            "routing_classes": Vec::<String>::new(),
            "data_egress_approved": profile.data_egress_approved,
            "catalog_fetch": profile.catalog_fetch,
            "catalog_verified": false,
            "model_source": "fallback",
            "status": "unchecked",
            "health": "unknown",
            "last_error": Value::Null,
            "last_checked": Value::Null,
            "model_count": profile.models.len(),
            "managed": false,
            "secrets": {},
            "credential": credential_view(state, user_id, &account.provider)?,
            "accounts": provider_account_views(state, user_id, &account.provider)?,
            "signin": signin_flows(state, is_admin, &account.provider, &kind),
            "signin_risk": signin_risk(&kind, &signin_flows(state, is_admin, &account.provider, &kind)),
        }));
    }
    Ok(parked)
}

/// The configured provider and its canonical kind, or a 404 / 422.
fn provider_context(state: &ApiState, id: &str) -> Result<(ProviderConfig, String)> {
    let profile = provider_profile(state, id)?;
    let kind = canonical_kind(&profile.kind)?;
    Ok((profile, kind))
}

/// One of the acting member's accounts, or a 404 that does not say whether the
/// id belongs to someone else.
fn find_account(
    state: &ApiState,
    user_id: &str,
    provider: &str,
    account_id: &str,
) -> Result<ProviderAccount> {
    state
        .accounts
        .get(user_id, provider, account_id)?
        .ok_or_else(|| SeraError::NotFound("account".into()))
}

pub(super) async fn list_provider_accounts(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let (_, kind) = provider_context(&state, &id)?;
    let flows = signin_flows(&state, user.is_admin(), &id, &kind);
    Ok(Json(json!({
        "accounts": provider_account_views(&state, &user.id, &id)?,
        "signin_risk": signin_risk(&kind, &flows),
        "signin": flows,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AddAccountRequest {
    #[serde(default)]
    label: String,
    api_key: String,
}

/// Whether a candidate key is accepted by the provider: one catalog call with
/// the key, made only when the provider fetches catalogs and is egress
/// approved. `Ok(false)` means "could not tell"; a key the provider refuses is
/// an error.
async fn verify_key(
    state: &ApiState,
    user_id: &str,
    id: &str,
    profile: &ProviderConfig,
    key: &str,
) -> Result<bool> {
    if !profile.catalog_fetch || !profile.data_egress_approved {
        return Ok(false);
    }
    let kind = canonical_kind(&profile.kind)?;
    let endpoint = resolve_base_url(&kind, &profile.base_url)?;
    let catalog = resolve_catalog(&kind, &endpoint)?;
    let credential = ProviderCredential {
        api_key: key.to_owned(),
        ..Default::default()
    };
    let client = open_provider_client(
        profile,
        &credential,
        &state.secrets,
        state.config.llm.timeout_secs,
        state.config.llm.max_output_tokens,
        CredentialOwner {
            users: &state.users,
            user_id,
            provider: id,
            account_id: "candidate",
            refresher: None,
        },
    )?;
    match client
        .fetch_model_catalog(&catalog.url, catalog.strict)
        .await
    {
        Ok(_) => Ok(true),
        Err(error) => match classify_llm(&error) {
            Ok(FailKind::CredentialsRejected | FailKind::AccountForbidden) => {
                Err(SeraError::Rejected {
                    code: "key_rejected",
                    message: "The provider did not accept this key".into(),
                })
            }
            _ => Ok(false),
        },
    }
}

/// Add an API key as a new account. A key the member already holds is
/// recognised and not stored twice.
pub(super) async fn add_provider_account(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<AddAccountRequest>,
) -> Result<Json<Value>> {
    let (profile, kind) = provider_context(&state, &id)?;
    if matches!(auth_plan(&kind, &profile.auth)?, AuthPlan::OAuth { .. }) {
        return Err(SeraError::Rejected {
            code: "signin_required",
            message: "This provider signs in with an account. Use Sign in.".into(),
        });
    }
    let key = request.api_key.trim();
    if crate::user_secrets::validate_value(key).is_err() {
        return Err(SeraError::Rejected {
            code: "bad_key",
            message: "That API key is not valid".into(),
        });
    }
    let label = match request.label.trim() {
        "" => "API key",
        label if label.chars().count() > MAX_LABEL_CHARS || label.chars().any(char::is_control) => {
            return Err(SeraError::Rejected {
                code: "bad_label",
                message: format!("The account label must be at most {MAX_LABEL_CHARS} characters"),
            });
        }
        label => label,
    };
    let verified = verify_key(&state, &user.id, &id, &profile, key).await?;
    let outcome = state.accounts.add_api_key(&user.id, &id, label, key)?;
    state.runtime.invalidate_model_credentials(Some(&user.id));
    audit_auth(
        &state,
        &user.id,
        "provider.account_added",
        json!({"provider": id, "account": outcome.account_id, "created": outcome.created, "verified": verified}),
    )?;
    Ok(Json(json!({
        "id": outcome.account_id,
        "created": outcome.created,
        "verified": verified,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PatchAccountRequest {
    label: Option<String>,
    priority: Option<i64>,
    weight: Option<i64>,
    disabled: Option<bool>,
}

/// Change an account's label, priority, weight or disabled state.
pub(super) async fn patch_provider_account(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path((id, account_id)): Path<(String, String)>,
    Json(request): Json<PatchAccountRequest>,
) -> Result<Json<Value>> {
    provider_context(&state, &id)?;
    if request.label.is_none()
        && request.priority.is_none()
        && request.weight.is_none()
        && request.disabled.is_none()
    {
        return Err(SeraError::Rejected {
            code: "nothing_to_change",
            message: "Say what to change: label, priority, weight or disabled".into(),
        });
    }
    if let Some(label) = &request.label {
        let label = label.trim();
        if label.is_empty()
            || label.chars().count() > MAX_LABEL_CHARS
            || label.chars().any(char::is_control)
        {
            return Err(SeraError::Rejected {
                code: "bad_label",
                message: format!("The account label must be 1 to {MAX_LABEL_CHARS} characters"),
            });
        }
    }
    find_account(&state, &user.id, &id, &account_id)?;
    let updated = state.accounts.update(
        &user.id,
        &id,
        &account_id,
        &AccountPatch {
            label: request.label,
            priority: request.priority,
            weight: request.weight,
            disabled: request.disabled,
        },
    )?;
    state.runtime.invalidate_model_credentials(Some(&user.id));
    let event = if request.disabled == Some(true) {
        "provider.account_disabled"
    } else {
        "provider.account_updated"
    };
    audit_auth(
        &state,
        &user.id,
        event,
        json!({"provider": id, "account": updated.account_id}),
    )?;
    Ok(Json(json!({"account": account_view(&updated)})))
}

#[derive(Debug, Deserialize)]
pub(super) struct DeleteAccountQuery {
    #[serde(default)]
    force: bool,
}

/// The bots that pin one account in their route. Bot routes arrive with the
/// route editor; until a bot can name an account, nothing pins one.
fn account_pins(
    _state: &ApiState,
    _user_id: &str,
    _provider: &str,
    _account_id: &str,
) -> Result<Vec<String>> {
    Ok(Vec::new())
}

/// Remove an account and its sealed credential. Refused while a bot pins the
/// account, unless `?force=true` (the bot then uses the provider's pool).
pub(super) async fn delete_provider_account(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path((id, account_id)): Path<(String, String)>,
    Query(query): Query<DeleteAccountQuery>,
) -> Result<Json<Value>> {
    provider_context(&state, &id)?;
    find_account(&state, &user.id, &id, &account_id)?;
    let pins = account_pins(&state, &user.id, &id, &account_id)?;
    if !pins.is_empty() && !query.force {
        return Err(SeraError::Rejected {
            code: "account_pinned",
            message: format!(
                "These bots use this account: {}. Remove it anyway to let them use any account.",
                pins.join(", ")
            ),
        });
    }
    if !state.accounts.remove(&user.id, &id, &account_id)? {
        return Err(SeraError::NotFound("account".into()));
    }
    state.runtime.invalidate_model_credentials(Some(&user.id));
    audit_auth(
        &state,
        &user.id,
        "provider.account_removed",
        json!({"provider": id, "account": account_id, "forced": query.force && !pins.is_empty()}),
    )?;
    Ok(Json(
        json!({"id": account_id, "deleted": true, "unpinned": pins}),
    ))
}

/// Ask the provider for its model list with one account's credential and
/// report how it went as a stable status word. The reply never carries the
/// credential, a response body or a provider error text.
pub(super) async fn test_provider_account(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path((id, account_id)): Path<(String, String)>,
) -> Result<Json<Value>> {
    let (profile, kind) = provider_context(&state, &id)?;
    let account = find_account(&state, &user.id, &id, &account_id)?;
    let reply = |ok: bool, status: &str, models: usize| {
        Json(json!({"id": account.account_id, "ok": ok, "status": status, "model_count": models}))
    };
    if !profile.data_egress_approved {
        return Ok(reply(false, "egress_not_approved", 0));
    }
    if !profile.catalog_fetch {
        return Ok(reply(false, "no_catalog", 0));
    }
    let credential = state
        .accounts
        .credential(&user.id, &id, &account_id)?
        .ok_or_else(|| SeraError::NotFound("account".into()))?;
    let endpoint = resolve_base_url(&kind, &profile.base_url)?;
    let catalog = resolve_catalog(&kind, &endpoint)?;
    let client = open_provider_client(
        &profile,
        &credential,
        &state.secrets,
        state.config.llm.timeout_secs,
        state.config.llm.max_output_tokens,
        CredentialOwner {
            users: &state.users,
            user_id: &user.id,
            provider: &id,
            account_id: &account_id,
            refresher: Some(&state.refresher),
        },
    )?;
    let outcome = client
        .fetch_model_catalog(&catalog.url, catalog.strict)
        .await;
    let (ok, status, models) = match outcome {
        Ok(models) => {
            if account.status == AccountStatus::NeedsReauth {
                state.accounts.set_status(
                    &user.id,
                    &id,
                    &account_id,
                    AccountStatus::Ok,
                    None,
                    None,
                )?;
            }
            (true, "ok", models.len())
        }
        Err(SeraError::Rejected {
            code: "needs_reauth",
            ..
        }) => (false, "needs_reauth", 0),
        Err(error) => match classify_llm(&error) {
            Ok(FailKind::CredentialsRejected | FailKind::AccountForbidden) => {
                state.accounts.set_status(
                    &user.id,
                    &id,
                    &account_id,
                    AccountStatus::NeedsReauth,
                    Some("the provider rejected this account"),
                    None,
                )?;
                (false, "key_rejected", 0)
            }
            _ => (false, "unreachable", 0),
        },
    };
    audit_auth(
        &state,
        &user.id,
        "provider.account_tested",
        json!({"provider": id, "account": account_id, "status": status}),
    )?;
    Ok(reply(ok, status, models))
}

#[derive(Debug, Deserialize)]
pub(super) struct QuotaQuery {
    #[serde(default)]
    refresh: Option<String>,
}

/// The cached quota snapshot. `?refresh=1` reads it again from the provider,
/// at most once per four minutes per account.
pub(super) async fn provider_account_quota(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path((id, account_id)): Path<(String, String)>,
    Query(query): Query<QuotaQuery>,
) -> Result<Json<Value>> {
    let (profile, kind) = provider_context(&state, &id)?;
    let account = find_account(&state, &user.id, &id, &account_id)?;
    let wants_refresh = matches!(query.refresh.as_deref(), Some("1" | "true"));
    let mut quota = account.quota.clone();
    let mut outcome = "cached";
    if wants_refresh {
        outcome = match state.quota_readers.get(&kind) {
            None => "unsupported",
            Some(reader) => {
                let key = (user.id.clone(), id.clone(), account.account_id.clone());
                let throttled = {
                    let mut reads = state
                        .quota_read_at
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let throttled = reads
                        .get(&key)
                        .is_some_and(|at| at.elapsed() < QUOTA_READ_INTERVAL);
                    if !throttled {
                        reads.insert(key, Instant::now());
                    }
                    throttled
                };
                if throttled {
                    "throttled"
                } else {
                    let mut credential = state
                        .accounts
                        .credential(&user.id, &id, &account_id)?
                        .ok_or_else(|| SeraError::NotFound("account".into()))?;
                    // A token that is about to lapse is renewed first, so the
                    // reading is not refused for an old bearer.
                    if !credential.refresh_token.is_empty()
                        && crate::accounts::AccountRefresher::is_near_expiry(
                            &credential,
                            Utc::now().timestamp(),
                        )
                        && let Ok(client) = open_provider_client(
                            &profile,
                            &credential,
                            &state.secrets,
                            state.config.llm.timeout_secs,
                            state.config.llm.max_output_tokens,
                            CredentialOwner {
                                users: &state.users,
                                user_id: &user.id,
                                provider: &id,
                                account_id: &account_id,
                                refresher: Some(&state.refresher),
                            },
                        )
                        && client.refresh_if_stale().await.is_ok()
                        && let Some(fresh) =
                            state.accounts.credential(&user.id, &id, &account_id)?
                    {
                        credential = fresh;
                    }
                    match reader.read(&credential, &account.identity).await {
                        Ok(Some(reading)) => {
                            if !account.is_default() {
                                state.store.set_provider_account_quota(
                                    &user.id,
                                    &id,
                                    &account.account_id,
                                    &reading,
                                )?;
                            }
                            quota = Some(reading);
                            "refreshed"
                        }
                        // A failed or empty read is "no reading", not an error.
                        Ok(None) | Err(_) => "no_reading",
                    }
                }
            }
        };
    }
    Ok(Json(
        json!({"id": account.account_id, "quota": quota, "refresh": outcome}),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SigninStartRequest {
    #[serde(default)]
    flow: Option<String>,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    challenge: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

/// Begin a sign-in: a browser (PKCE) or a device code.
pub(super) async fn provider_signin_start(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<SigninStartRequest>,
) -> Result<Json<Value>> {
    let (profile, kind) = provider_context(&state, &id)?;
    plan_signin_gate(&user, &profile, &kind)?;
    let flow = match request.flow.as_deref() {
        None => None,
        Some(word) => Some(FlowKind::parse(word).ok_or_else(|| SeraError::Rejected {
            code: "bad_flow",
            message: "The sign-in flow must be pkce or device".into(),
        })?),
    };
    let started = state
        .signin
        .start(
            &user.id,
            &id,
            &kind,
            StartRequest {
                flow,
                redirect_uri: request.redirect_uri,
                challenge: request.challenge,
                state: request.state,
            },
        )
        .await?;
    Ok(Json(serde_json::to_value(started)?))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SigninCompleteRequest {
    flow_id: String,
    code: String,
    redirect_uri: String,
    /// Only when the desktop made the challenge and kept the verifier.
    #[serde(default)]
    verifier: Option<String>,
}

/// Finish a browser sign-in with the code the redirect carried.
pub(super) async fn provider_signin_complete(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<SigninCompleteRequest>,
) -> Result<Json<Value>> {
    let (profile, kind) = provider_context(&state, &id)?;
    plan_signin_gate(&user, &profile, &kind)?;
    let outcome = state
        .signin
        .complete(
            &user.id,
            &id,
            &kind,
            CompleteRequest {
                flow_id: Some(request.flow_id),
                code: request.code,
                redirect_uri: Some(request.redirect_uri),
                verifier: request.verifier,
            },
        )
        .await?;
    state.runtime.invalidate_model_credentials(Some(&user.id));
    Ok(Json(json!({
        "account_id": outcome.account_id,
        "created": outcome.created,
    })))
}

/// Where one sign-in stands: `pending`, `ok`, `denied` or `expired`.
pub(super) async fn provider_signin_status(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path((id, flow_id)): Path<(String, String)>,
) -> Result<Json<Value>> {
    provider_context(&state, &id)?;
    let status = state.signin.status(&user.id, &id, &flow_id)?;
    if matches!(status, FlowStatus::Ok { .. }) {
        // A device flow finishes in the background; drop clients built before.
        state.runtime.invalidate_model_credentials(Some(&user.id));
    }
    let mut body = json!({"status": status.word()});
    match status {
        FlowStatus::Ok {
            account_id,
            created,
        } => {
            body["account_id"] = json!(account_id);
            body["created"] = json!(created);
        }
        FlowStatus::Denied { reason } => body["reason"] = json!(reason),
        _ => {}
    }
    Ok(Json(body))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderOauthStart {
    challenge: String,
    state: String,
    redirect_uri: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct ProviderOauthComplete {
    code: String,
    verifier: String,
    #[serde(default)]
    redirect_uri: String,
}

/// `POST /v1/providers/{id}/oauth/start`: the pre-accounts spelling of
/// `signin/start` with a browser flow whose verifier the desktop keeps.
pub(super) async fn provider_oauth_start(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<ProviderOauthStart>,
) -> Result<Json<Value>> {
    let (profile, kind) = provider_context(&state, &id)?;
    plan_signin_gate(&user, &profile, &kind)?;
    let started = state
        .signin
        .start(
            &user.id,
            &id,
            &kind,
            StartRequest {
                flow: Some(FlowKind::Pkce),
                redirect_uri: Some(request.redirect_uri),
                challenge: Some(request.challenge),
                state: Some(request.state),
            },
        )
        .await?;
    Ok(Json(serde_json::to_value(started)?))
}

/// `POST /v1/providers/{id}/oauth/complete`: finishes the member's newest open
/// browser flow for this provider. The account is stored sealed; the key is
/// never returned.
pub(super) async fn provider_oauth_complete(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<ProviderOauthComplete>,
) -> Result<Json<Value>> {
    let (profile, kind) = provider_context(&state, &id)?;
    plan_signin_gate(&user, &profile, &kind)?;
    let outcome = state
        .signin
        .complete(
            &user.id,
            &id,
            &kind,
            CompleteRequest {
                flow_id: None,
                code: request.code,
                redirect_uri: Some(request.redirect_uri).filter(|uri| !uri.is_empty()),
                verifier: Some(request.verifier),
            },
        )
        .await?;
    state.runtime.invalidate_model_credentials(Some(&user.id));
    Ok(Json(json!({
        "id": id,
        "configured": true,
        "account_id": outcome.account_id,
        "created": outcome.created,
    })))
}
