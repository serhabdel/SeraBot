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
    if plan_signin_blocked(state, is_admin, provider_id, kind).is_some() {
        return Vec::new();
    }
    state.signin.registry().flows_for(kind)
}

/// Why a plan sign-in (Claude, Codex, Copilot) is not offered, so the page can
/// say so instead of silently showing no button: `"off"` when the operator set
/// `plan_oauth = false` on this provider, `"admin_only"` for Claude's to a
/// member. `None` when it is offered or the kind has no plan sign-in.
pub(super) fn plan_signin_blocked(
    state: &ApiState,
    _is_admin: bool,
    provider_id: &str,
    kind: &str,
) -> Option<&'static str> {
    if kind == "anthropic" {
        return Some("off");
    }
    if !matches!(kind, "openai-codex" | "github-copilot") {
        return None;
    }
    // The copy last saved in the console counts at once; a provider that is
    // not saved yet offers the sign-in (it is on by default).
    let enabled = live_provider_profile(state, provider_id)
        .ok()
        .is_none_or(|profile| profile.plan_oauth_enabled());
    if !enabled { Some("off") } else { None }
}

/// What the server still needs before this provider's sign-in can work, for
/// the page to ask the admin instead of failing after the member approved:
/// `"client_secret"` when Antigravity's Google client secret is missing.
pub(super) fn signin_setup_missing(state: &ApiState, kind: &str) -> Option<&'static str> {
    state
        .signin
        .registry()
        .get(kind)
        .and_then(|provider| provider.setup_missing())
}

/// The sign-in state a provider card needs beyond its flows: why a plan
/// sign-in is not offered (`plan_signin`), what the server still lacks for a
/// sign-in to work (`signin_setup`), and whether bots pick up the provider only
/// after a restart (`pending_restart`). Shown to members too, so a card can say
/// "ask your admin" instead of failing.
pub(super) fn add_signin_status(
    view: &mut Value,
    state: &ApiState,
    is_admin: bool,
    provider_id: &str,
    kind: &str,
) {
    view["plan_signin"] = json!(plan_signin_blocked(state, is_admin, provider_id, kind));
    view["signin_setup"] = json!(signin_setup_missing(state, kind));
    view["pending_restart"] = json!(provider_pending_restart(state, provider_id));
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
        "github-copilot" => Some(
            "This signs in through GitHub Copilot's own editor client, not an API contract. GitHub may restrict or suspend an account used this way. It is your own account and is never shared. A GitHub account without a Copilot plan cannot use it.",
        ),
        _ => None,
    }
}

/// A plan sign-in needs a provider that has not turned `plan_oauth` off.
/// Third-party `claude.ai` OAuth is retired (2026); Codex and Copilot remain
/// gated by `plan_oauth`.
pub(super) fn plan_signin_gate(
    _user: &ActingUser,
    profile: &ProviderConfig,
    kind: &str,
) -> Result<()> {
    if kind == "anthropic" {
        return Err(SeraError::Rejected {
            code: "claude_oauth_retired",
            message: crate::auth_flows::claude::CLAUDE_OAUTH_SHUTDOWN_MESSAGE.into(),
        });
    }
    if !matches!(kind, "openai-codex" | "github-copilot") {
        return Ok(());
    }
    if !profile.plan_oauth_enabled() {
        return Err(SeraError::Rejected {
            code: "signin_unsupported",
            message: "This provider has no sign-in; add its API key".into(),
        });
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
        let mut view = json!({
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
        });
        add_signin_status(&mut view, state, is_admin, &account.provider, &kind);
        parked.push(view);
    }
    Ok(parked)
}

/// The configured provider and its canonical kind, or a 404 / 422.
fn provider_context(state: &ApiState, id: &str) -> Result<(ProviderConfig, String)> {
    let profile = live_provider_profile(state, id)?;
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
    let mut body = json!({
        "accounts": provider_account_views(&state, &user.id, &id)?,
        "signin_risk": signin_risk(&kind, &flows),
        "signin": flows,
    });
    add_signin_status(&mut body, &state, user.is_admin(), &id, &kind);
    Ok(Json(body))
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

/// The loopback gateway a `claude-agent-sdk` sign-in is relayed to, with the
/// gateway bearer token. The token comes from the provider's stored credential
/// (the per-user key) when there is one, else from the server-level
/// `[claude_gateway] token_secret` in the age store — so "Sign in with
/// browser" works on a provider saved without a key. `None` for every other
/// kind.
fn gateway_endpoint(
    state: &ApiState,
    user: &ActingUser,
    id: &str,
    profile: &ProviderConfig,
    kind: &str,
) -> Result<Option<crate::auth_flows::gateway::GatewayEndpoint>> {
    use crate::auth_flows::gateway::GatewayEndpoint;
    if kind != "claude-agent-sdk" {
        return Ok(None);
    }
    let base_url = crate::providers::resolve_base_url(kind, &profile.base_url)?;
    let token = match state.users.resolve_with_owner(&user.id, id)? {
        Some((_, secret)) => {
            let credential = crate::user_secrets::ProviderCredential::parse(&secret)?;
            if credential.api_key.trim().is_empty() {
                None
            } else {
                Some(credential.api_key)
            }
        }
        None => None,
    };
    let token = token.or_else(|| state.secrets.get(&state.config.claude_gateway.token_secret));
    let Some(token) = token else {
        return Err(SeraError::Rejected {
            code: "gateway_token_missing",
            message: "The Claude Agent SDK gateway token is not set yet. Generate or save a gateway token on this card first.".into(),
        });
    };
    Ok(Some(GatewayEndpoint::validated(
        &base_url,
        zeroize::Zeroizing::new(token),
    )?))
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
    let request = StartRequest {
        flow,
        redirect_uri: request.redirect_uri,
        challenge: request.challenge,
        state: request.state,
    };
    let started = match gateway_endpoint(&state, &user, &id, &profile, &kind)? {
        Some(gateway) => {
            state
                .signin
                .start_with_gateway(&user.id, &id, &kind, request, gateway)
                .await?
        }
        None => state.signin.start(&user.id, &id, &kind, request).await?,
    };
    Ok(Json(serde_json::to_value(started)?))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SigninCompleteRequest {
    flow_id: String,
    code: String,
    #[serde(default)]
    redirect_uri: Option<String>,
    /// Only when the desktop made the challenge and kept the verifier.
    #[serde(default)]
    verifier: Option<String>,
}

/// Finish a browser sign-in with the code the redirect carried (or pasted code).
pub(super) async fn provider_signin_complete(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<SigninCompleteRequest>,
) -> Result<Json<Value>> {
    let (profile, kind) = provider_context(&state, &id)?;
    plan_signin_gate(&user, &profile, &kind)?;
    let request = CompleteRequest {
        flow_id: Some(request.flow_id),
        code: request.code,
        redirect_uri: request.redirect_uri.filter(|uri| !uri.trim().is_empty()),
        verifier: request.verifier,
    };
    let outcome = match gateway_endpoint(&state, &user, &id, &profile, &kind)? {
        Some(gateway) => {
            state
                .signin
                .complete_with_gateway(&user.id, &id, &kind, request, gateway)
                .await?
        }
        None => state.signin.complete(&user.id, &id, &kind, request).await?,
    };
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
    let (profile, kind) = provider_context(&state, &id)?;
    let gateway = if kind == "claude-agent-sdk" {
        gateway_endpoint(&state, &user, &id, &profile, &kind)
            .ok()
            .flatten()
    } else {
        None
    };
    let status = state
        .signin
        .status_with_gateway(&user.id, &id, &flow_id, gateway.as_ref())
        .await?;
    if matches!(status, FlowStatus::Ok { .. }) {
        // A device or gateway flow finishes in the background; drop clients built before.
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

/// `GET /v1/providers/{id}/gateway-status`: probes `/health` and, if token
/// exists, `/v1/auth/status`.
pub(super) async fn provider_gateway_status(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let (profile, kind) = provider_context(&state, &id)?;
    if kind != "claude-agent-sdk" {
        return Err(SeraError::Rejected {
            code: "not_gateway_provider",
            message: "This provider is not a Claude Agent SDK gateway.".into(),
        });
    }
    let base_url = crate::providers::resolve_base_url(&kind, &profile.base_url)?;
    let health = crate::auth_flows::gateway::gateway_health(&base_url)
        .await
        .ok();
    let reachable = health.is_some();
    let endpoint = gateway_endpoint(&state, &user, &id, &profile, &kind)
        .ok()
        .flatten();
    let token_configured = endpoint.is_some();
    let auth_status = if let (true, Some(ep)) = (reachable, endpoint.as_ref()) {
        crate::auth_flows::gateway::gateway_auth_status(ep)
            .await
            .ok()
    } else {
        None
    };
    let auth_ok = auth_status.is_some();
    let pick_bool = |key: &str| {
        auth_status
            .as_ref()
            .and_then(|v| v.get(key))
            .or_else(|| health.as_ref().and_then(|v| v.get(key)))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    let cli_version = auth_status
        .as_ref()
        .and_then(|v| v.get("cli_version"))
        .or_else(|| health.as_ref().and_then(|v| v.get("cli_version")))
        .cloned()
        .unwrap_or(Value::Null);
    let default_cli_login = auth_status
        .as_ref()
        .and_then(|v| v.get("default_cli_login"))
        .or_else(|| health.as_ref().and_then(|v| v.get("default_cli_login")))
        .cloned()
        .unwrap_or(Value::Null);
    let profiles = auth_status
        .as_ref()
        .and_then(|v| v.get("profiles"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    Ok(Json(json!({
        "reachable": reachable,
        "token_configured": token_configured,
        "auth_ok": auth_ok,
        "cli_installed": pick_bool("cli_installed"),
        "cli_version": cli_version,
        "sdk_installed": pick_bool("sdk_installed"),
        "default_cli_login": default_cli_login,
        "profiles": profiles,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GatewayTokenRequest {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    generate: bool,
}

/// `POST /v1/providers/{id}/gateway-token`: admin-only; saves or generates the
/// gateway bearer token in the age secret store and invalidates cached clients.
pub(super) async fn provider_gateway_token(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
    Json(request): Json<GatewayTokenRequest>,
) -> Result<Json<Value>> {
    require_admin(&user)?;
    let (profile, kind) = provider_context(&state, &id)?;
    if kind != "claude-agent-sdk" {
        return Err(SeraError::Rejected {
            code: "not_gateway_provider",
            message: "This provider is not a Claude Agent SDK gateway.".into(),
        });
    }
    let (token, generated) = if request.generate {
        let mut bytes = [0u8; 24];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        (format!("gw_{}", hex::encode(bytes)), true)
    } else if let Some(raw) = request.token {
        let trimmed = raw.trim();
        if crate::user_secrets::validate_value(trimmed).is_err() {
            return Err(SeraError::Rejected {
                code: "bad_token",
                message: "That gateway token is not valid".into(),
            });
        }
        (trimmed.to_owned(), false)
    } else {
        return Err(SeraError::Rejected {
            code: "bad_token",
            message: "Provide a token or set generate = true".into(),
        });
    };

    let primary_secret = state.config.claude_gateway.token_secret.trim();
    let provider_secret = profile.api_key_secret.trim();
    let mut updates: Vec<(&str, &str)> = Vec::new();
    if !primary_secret.is_empty() {
        updates.push((primary_secret, token.as_str()));
    }
    if !provider_secret.is_empty() && provider_secret != primary_secret {
        updates.push((provider_secret, token.as_str()));
    }
    if updates.is_empty() {
        updates.push(("claude_gateway_token", token.as_str()));
    }
    state.secrets.persist(&updates)?;
    state.runtime.invalidate_model_credentials(None);
    audit_auth(
        &state,
        &user.id,
        "provider.gateway_token_updated",
        json!({"provider": id, "generated": generated}),
    )?;

    if generated {
        let base_url = crate::providers::resolve_base_url(&kind, &profile.base_url)?;
        let parsed = url::Url::parse(&base_url).ok();
        let host = parsed
            .as_ref()
            .and_then(|u| u.host_str())
            .unwrap_or("127.0.0.1");
        let port = parsed.as_ref().and_then(|u| u.port()).unwrap_or(8791);
        let startup_command = format!(
            "SERABOT_CLAUDE_GATEWAY_TOKEN={token} serabot-claude-gateway --host {host} --port {port}"
        );
        Ok(Json(json!({
            "ok": true,
            "token": token,
            "startup_command": startup_command,
        })))
    } else {
        Ok(Json(json!({ "ok": true })))
    }
}

/// `POST /v1/providers/{id}/gateway-import-cli`: imports the existing `claude`
/// CLI login from the gateway into the member's `"default"` profile account.
pub(super) async fn provider_gateway_import_cli(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let (profile, kind) = provider_context(&state, &id)?;
    let endpoint = gateway_endpoint(&state, &user, &id, &profile, &kind)?.ok_or_else(|| {
        SeraError::Rejected {
            code: "not_gateway_provider",
            message: "This provider is not a Claude Agent SDK gateway.".into(),
        }
    })?;
    let outcome = state
        .signin
        .import_gateway_cli_account(&user.id, &id, &endpoint)
        .await?;
    state.runtime.invalidate_model_credentials(Some(&user.id));
    Ok(Json(json!({
        "account_id": outcome.account_id,
        "created": outcome.created,
    })))
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
