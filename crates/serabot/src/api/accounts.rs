use super::*;

/// The providers list rendered by web, desktop, and mobile clients. Public
/// and derived from config; `google`/`github` always report disabled in v1.
pub(super) async fn auth_providers(State(state): State<Arc<ApiState>>) -> Json<Value> {
    let providers: Vec<Value> = auth::provider_report(&state.config.auth)
        .into_iter()
        .map(|provider| json!({"id": provider.id, "enabled": provider.enabled}))
        .collect();
    Json(json!({"providers": providers}))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LoginStartRequest {
    provider: String,
    #[serde(default)]
    invite_code: Option<String>,
    /// Native-client handoff: all three of `return_to`, `handoff_challenge`
    /// and `client_state` together, or none (legacy web page flow).
    #[serde(default)]
    return_to: Option<String>,
    #[serde(default)]
    handoff_challenge: Option<String>,
    #[serde(default)]
    client_state: Option<String>,
}

pub(super) fn bad_request(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": message}))).into_response()
}

/// Start a browser sign-in. Public by definition (no session yet); the
/// invite code, when presented, is validated but consumed only at the
/// callback — inside the transaction that creates the account.
pub(super) async fn auth_login_start(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<LoginStartRequest>,
) -> Response {
    let service = match auth_service(&state) {
        Ok(service) => service,
        Err(error) => return error.into_response(),
    };
    if request.provider.len() > 64 {
        return SeraError::Validation("provider must be a known provider id".into())
            .into_response();
    }
    let handoff = match auth::HandoffRequest::parse(
        request.return_to.as_deref(),
        request.handoff_challenge.as_deref(),
        request.client_state.as_deref(),
    ) {
        Ok(handoff) => handoff,
        Err(error) => {
            warn!(%error, "login start rejected its handoff fields");
            return bad_request("invalid return_to, handoff_challenge, or client_state");
        }
    };
    let invite = request
        .invite_code
        .as_deref()
        .map(str::trim)
        .filter(|code| !code.is_empty());
    match service
        .login_start_with_handoff(&request.provider, invite, handoff)
        .await
    {
        Ok(url) => Json(json!({"url": url})).into_response(),
        Err(error) => error.into_response(),
    }
}

/// The `/login` page's Microsoft button target: start the flow and hand the
/// browser to the provider. Failures render the same minimal error page as
/// the callback (fail closed, no provider detail).
pub(super) async fn auth_login_microsoft(State(state): State<Arc<ApiState>>) -> Response {
    let Some(service) = state.auth.as_ref() else {
        return auth_error_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Sign-in is not enabled on this deployment.",
        );
    };
    match service.login_start("microsoft", None).await {
        Ok(url) => (
            StatusCode::FOUND,
            [
                (header::LOCATION, url.as_str()),
                (header::CACHE_CONTROL, "no-store"),
            ],
        )
            .into_response(),
        Err(error) => {
            warn!(%error, "web login start failed");
            auth_error_page(
                StatusCode::BAD_REQUEST,
                "Sign-in could not be started. Check that sign-in is configured, then try again.",
            )
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct CallbackParams {
    #[serde(default)]
    state: String,
    #[serde(default)]
    code: Option<String>,
    /// Set by the provider when the user declined or consent failed.
    #[serde(default)]
    error: Option<String>,
}

/// A `302` back to the native client (or any redirect target). Never cached
/// and never leaks the one-time code through the referrer.
pub(super) fn redirect_response(location: &str) -> Response {
    match axum::http::HeaderValue::from_str(location) {
        Ok(value) => (
            StatusCode::FOUND,
            [
                (header::LOCATION, value),
                (
                    header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("no-store"),
                ),
                (
                    header::REFERRER_POLICY,
                    axum::http::HeaderValue::from_static("no-referrer"),
                ),
            ],
        )
            .into_response(),
        Err(_) => auth_error_page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Sign-in could not be completed. Start again from the client.",
        ),
    }
}

/// The OIDC redirect target. A native client (login started with
/// `return_to`) is sent back to its allowlisted `return_to` with a one-time
/// `handoff` code or a short `error` code; a plain web sign-in only sees a
/// "signed in, return to the app" page. No session token is ever rendered.
pub(super) async fn auth_callback(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<CallbackParams>,
) -> Response {
    let Some(service) = state.auth.as_ref() else {
        return auth_error_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Sign-in is not enabled on this deployment.",
        );
    };
    let code = params.code.as_deref().filter(|code| !code.is_empty());
    let Some(code) = code else {
        // The provider returned an error (e.g. the user declined) or the
        // request is malformed. Consume the pending login either way.
        let handoff = service.callback_denied(&params.state).await;
        let provider_error: String = params
            .error
            .as_deref()
            .unwrap_or("none")
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(64)
            .collect();
        warn!(
            provider_error,
            "auth callback without an authorization code"
        );
        return match handoff {
            Some(request) => redirect_response(&auth::handoff_redirect(
                &request,
                Err(auth::SignInFailure::Denied),
            )),
            None => auth_error_page(
                StatusCode::BAD_REQUEST,
                "Sign-in was not completed. Start again from the client.",
            ),
        };
    };
    let result = service.callback_login(&params.state, code).await;
    match (result.outcome, result.handoff) {
        (Ok(user), Some(request)) => match service.issue_handoff(&user, &request) {
            Ok(handoff) => redirect_response(&auth::handoff_redirect(&request, Ok(&handoff))),
            Err(error) => {
                warn!(%error, "handoff issue failed");
                redirect_response(&auth::handoff_redirect(
                    &request,
                    Err(auth::SignInFailure::Server),
                ))
            }
        },
        (Ok(_user), None) => html_response(StatusCode::OK, signed_in_page()),
        (Err((failure, error)), Some(request)) => {
            warn!(%error, code = failure.code(), "auth callback failed");
            redirect_response(&auth::handoff_redirect(&request, Err(failure)))
        }
        (Err((_, error)), None) => {
            warn!(%error, "auth callback failed");
            auth_error_page(
                StatusCode::BAD_REQUEST,
                "Sign-in could not be completed. The link may have expired or been used; start again from the client.",
            )
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HandoffRedeemRequest {
    handoff: String,
    verifier: String,
}

/// Redeem a one-time handoff code for a session. Public: the PKCE verifier,
/// known only to the client that started the sign-in, is the credential. Any
/// failure is the same generic 400 and burns the code.
pub(super) async fn auth_handoff(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<HandoffRedeemRequest>,
) -> Response {
    let service = match auth_service(&state) {
        Ok(service) => service,
        Err(error) => return error.into_response(),
    };
    match service.redeem_handoff(&request.handoff, &request.verifier) {
        Ok(grant) => Json(json!({
            "token": grant.token,
            "expires_at": grant.expires_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            "user": me_view(&grant.user),
        }))
        .into_response(),
        Err(SeraError::Validation(_)) => bad_request("invalid handoff"),
        Err(error) => error.into_response(),
    }
}

pub(super) fn me_view(user: &User) -> Value {
    json!({
        "id": user.id,
        "email": user.email,
        "display_name": user.display_name,
        "role": user.role.as_str(),
    })
}

/// The signed-in caller, in the same shape the handoff returns.
pub(super) async fn auth_me(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    let account = state
        .store
        .get_user(&user.id)?
        .ok_or_else(|| SeraError::NotFound("user".into()))?;
    Ok(Json(me_view(&account)))
}

/// Revoke the caller's own session. The static config token has no session
/// row, so it cannot be revoked here — rotating it is a config change.
pub(super) async fn auth_logout(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    if user.session_id.is_none() {
        return Err(SeraError::InvalidState(
            "this token is the server configuration token and cannot be revoked here".into(),
        ));
    }
    state.store.revoke_session(&user.token_hash)?;
    audit_auth(&state, &user.id, "auth.logout", json!({}))?;
    Ok(Json(json!({"revoked": true})))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MintInviteRequest {
    uses: u32,
}

/// Mint an invitation (admin only). The raw code is returned exactly once;
/// the server keeps only its SHA-256 hash.
pub(super) async fn auth_invites(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Json(request): Json<MintInviteRequest>,
) -> Result<Json<Value>> {
    require_admin(&user)?;
    let service = auth_service(&state)?;
    if request.uses == 0 || request.uses > 100 {
        return Err(SeraError::Validation(
            "uses must be between 1 and 100".into(),
        ));
    }
    let code = service.mint_invite(request.uses, &user.id)?;
    audit_auth(
        &state,
        &user.id,
        "auth.invite_minted",
        json!({"uses": request.uses}),
    )?;
    Ok(Json(json!({"code": code})))
}

/// List accounts (admin only). Member sessions get 403 and never see the
/// user directory.
pub(super) async fn list_users(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
) -> Result<Json<Value>> {
    require_admin(&user)?;
    let users = state.store.list_users()?;
    let users: Vec<Value> = users.iter().map(user_view).collect();
    Ok(Json(json!({"users": users})))
}

pub(super) fn user_view(user: &User) -> Value {
    json!({
        "id": user.id,
        "provider_subject": user.provider_subject,
        "email": user.email,
        "display_name": user.display_name,
        "role": user.role.as_str(),
        "status": user.status.as_str(),
        "created_at": user.created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
    })
}

pub(super) async fn disable_user(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    change_user_status(&state, &user, &id, UserStatus::Disabled)
}

pub(super) async fn enable_user(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<ActingUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    change_user_status(&state, &user, &id, UserStatus::Active)
}

/// Enable or disable an account (admin only). A disabled user's sessions
/// stop authorizing immediately. The admin account itself cannot be
/// disabled: it owns recovery, and locking it out would strand the
/// deployment with no admin path at all.
pub(super) fn change_user_status(
    state: &ApiState,
    acting: &ActingUser,
    id: &str,
    status: UserStatus,
) -> Result<Json<Value>> {
    require_admin(acting)?;
    crate::util::validate_id(id)?;
    let target = state
        .store
        .get_user(id)?
        .ok_or_else(|| SeraError::NotFound(format!("user {id}")))?;
    if status == UserStatus::Disabled && target.role == UserRole::Admin {
        return Err(SeraError::Validation(
            "the admin account cannot be disabled".into(),
        ));
    }
    state.store.set_user_status(id, status)?;
    audit_auth(
        state,
        &acting.id,
        if status == UserStatus::Disabled {
            "auth.user_disabled"
        } else {
            "auth.user_enabled"
        },
        json!({"user_id": id, "status": status.as_str()}),
    )?;
    Ok(Json(json!({"id": id, "status": status.as_str()})))
}

/// The auth service, or an explicit fail-closed error while sign-in is off.
pub(super) fn auth_service(state: &ApiState) -> Result<&AuthService> {
    state
        .auth
        .as_deref()
        .ok_or_else(|| SeraError::Forbidden("sign-in is disabled".into()))
}

/// Record an auth audit event under the acting user's id.
pub(super) fn audit_auth(state: &ApiState, actor: &str, event: &str, details: Value) -> Result<()> {
    state
        .store
        .record_audit(actor, event, None, None, None, None, None, Some(&details))?;
    Ok(())
}

/// Minimal page shell matching the landing aesthetic: inline CSS only, no
/// scripts, no external origins. `body` is server-controlled text only —
/// never provider- or user-supplied content.
pub(super) fn auth_page(title: &str, body: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="dark light">
<title>{title}</title>
<style>
  :root {{ color-scheme: dark light; --bg: #101418; --panel: #171d23; --border: #2b343d; --text: #e6edf3; --muted: #9aa7b4; --accent: #4d9fff; }}
  * {{ box-sizing: border-box; }}
  body {{ margin: 0; min-height: 100vh; background: var(--bg); color: var(--text); font: 16px/1.55 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif; display: flex; justify-content: center; padding: 40px 20px; }}
  main {{ width: 100%; max-width: 640px; }}
  .eyebrow {{ margin: 0 0 8px; color: var(--muted); font-size: 13px; letter-spacing: 0.08em; text-transform: uppercase; }}
  h1 {{ margin: 0 0 12px; font-size: 40px; line-height: 1.1; letter-spacing: -0.02em; }}
  p {{ margin: 0 0 12px; }}
  .lede {{ color: var(--muted); }}
  section {{ margin-top: 20px; padding: 20px; background: var(--panel); border: 1px solid var(--border); border-radius: 12px; }}
  code {{ display: block; margin: 12px 0; padding: 12px; border: 1px solid var(--border); border-radius: 8px; background: #0d1116; font-size: 13px; word-break: break-all; user-select: all; }}
  .meta {{ color: var(--muted); font-size: 14px; }}
</style>
</head>
<body>
<main>
<header>
<p class="eyebrow">SeraBot</p>
<h1>{title}</h1>
</header>
<section>
{body}
</section>
</main>
</body>
</html>
"#
    )
}

/// The page shown after a plain web sign-in. It carries no credential: the
/// desktop and mobile apps receive their session through the handoff.
pub(super) fn signed_in_page() -> String {
    auth_page(
        "Signed in",
        "<p class=\"lede\">Sign-in succeeded. You can close this tab and return to the SeraBot app.</p>\n<p class=\"meta\">If the app does not pick it up, start sign-in again from the app.</p>",
    )
}

/// A plain, link-free error page for failed sign-ins.
pub(super) fn auth_error_page(status: StatusCode, message: &str) -> Response {
    html_response(
        status,
        auth_page("Sign-in", &format!("<p class=\"lede\">{message}</p>")),
    )
}

pub(super) fn html_response(status: StatusCode, page: String) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CONTENT_SECURITY_POLICY, crate::web::CSP),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        page,
    )
        .into_response()
}
