//! Sign-in for marketplace OAuth plugins (v0.51, M3): the server-side half of
//! `POST .../oauth/start` and `.../oauth/complete`.
//!
//! `start` runs discovery, picks the client identity and builds the consent
//! URL. The PKCE verifier and the discovered endpoints stay here, in memory,
//! keyed by the single-use `state` and bound to the acting user and plugin; they
//! are never sent to the caller. `complete` exchanges the code and seals the
//! tokens per user (see [`crate::mcp_oauth`]). A pending sign-in expires after
//! ten minutes and the table is capped, so abandoned attempts cannot pile up.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::{Result, SeraError};
use crate::marketplace::{App, AuthKind, OauthSpec, PluginManifest, Server};
use crate::mcp_oauth::{
    ClientRegistration, Discovery, OauthAccounts, OauthNet, authorize_request, callback_state_ok,
    discover, exchange_code, register,
};
use crate::store::Store;
use crate::user_secrets::UserSecrets;

/// The account every plugin has unless it allows several.
pub const DEFAULT_ACCOUNT: &str = "default";
const PENDING_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_PENDING: usize = 64;
const MAX_LABEL_CHARS: usize = 60;

/// An account id: short, lowercase, and safe inside a sealed-store key.
pub fn account_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// The first http app of `manifest` that signs in with OAuth (or the one named).
pub fn oauth_app<'a>(
    manifest: &'a PluginManifest,
    app_id: Option<&str>,
) -> Result<(&'a App, &'a OauthSpec)> {
    for app in &manifest.apps {
        if app_id.is_some_and(|wanted| wanted != app.id) {
            continue;
        }
        if !matches!(app.server, Server::Http { .. }) {
            continue;
        }
        for auth in &app.auth {
            if auth.kind == AuthKind::Oauth
                && let Some(spec) = &auth.oauth
            {
                return Ok((app, spec));
            }
        }
    }
    Err(SeraError::Validation(format!(
        "plugin {} has no app that signs in with OAuth",
        manifest.slug
    )))
}

struct Pending {
    user_id: String,
    slug: String,
    account_id: String,
    label: String,
    discovery: Discovery,
    client: ClientRegistration,
    verifier: String,
    scopes: Vec<String>,
    started: Instant,
}

/// What `start` hands the caller: where to send the browser, and the state the
/// redirect will carry back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    pub authorize_url: String,
    pub state: String,
    pub account_id: String,
}

/// What `complete` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completed {
    pub account_id: String,
}

#[derive(Clone)]
pub struct McpSignIn {
    net: OauthNet,
    accounts: OauthAccounts,
    store: Store,
    pending: Arc<Mutex<HashMap<String, Pending>>>,
}

impl McpSignIn {
    pub fn new(store: Store, secrets: Arc<UserSecrets>, net: OauthNet) -> Self {
        Self {
            accounts: OauthAccounts::new(store.clone(), secrets, net.clone()),
            net,
            store,
            pending: Arc::default(),
        }
    }

    pub fn accounts(&self) -> &OauthAccounts {
        &self.accounts
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Pending>> {
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// Begin a sign-in for `account_id` of `manifest`'s OAuth app.
    pub async fn start(
        &self,
        user_id: &str,
        manifest: &PluginManifest,
        app_id: Option<&str>,
        account_id: &str,
        label: Option<&str>,
    ) -> Result<Started> {
        let (app, spec) = oauth_app(manifest, app_id)?;
        if !account_id_ok(account_id) {
            return Err(SeraError::Validation("account id is not valid".into()));
        }
        let multiple = app.accounts.as_ref().is_some_and(|rules| rules.multiple);
        if !multiple && account_id != DEFAULT_ACCOUNT {
            return Err(SeraError::Validation("this app allows one account".into()));
        }
        let label: String = label
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or(&manifest.name)
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_LABEL_CHARS)
            .collect();
        let Server::Http { url } = &app.server else {
            return Err(SeraError::Validation("app is not an http server".into()));
        };
        self.net
            .check_mcp_egress(url, &manifest.permissions.egress)?;
        let discovery = discover(&self.net, url, spec.resource_metadata_url.as_deref()).await?;
        let client = register(&self.net, &discovery, spec).await?;
        let request = authorize_request(&discovery, &client, &spec.scopes)?;
        let mut pending = self.lock();
        pending.retain(|_, entry| entry.started.elapsed() < PENDING_TTL);
        if pending.len() >= MAX_PENDING {
            return Err(SeraError::Validation(
                "too many sign-ins in progress; try again shortly".into(),
            ));
        }
        pending.insert(
            request.state.clone(),
            Pending {
                user_id: user_id.to_owned(),
                slug: manifest.slug.clone(),
                account_id: account_id.to_owned(),
                label,
                discovery,
                client,
                verifier: request.verifier,
                scopes: spec.scopes.clone(),
                started: Instant::now(),
            },
        );
        Ok(Started {
            authorize_url: request.url,
            state: request.state,
            account_id: account_id.to_owned(),
        })
    }

    /// Finish a sign-in with the code and state the redirect carried. The state
    /// is single use and must belong to this user and plugin.
    pub async fn complete(
        &self,
        user_id: &str,
        slug: &str,
        code: &str,
        state: &str,
    ) -> Result<Completed> {
        let unknown = || SeraError::NotFound("sign-in not found or expired".into());
        if !callback_state_ok(state) {
            return Err(unknown());
        }
        let entry = {
            let mut pending = self.lock();
            pending.retain(|_, entry| entry.started.elapsed() < PENDING_TTL);
            match pending.get(state) {
                Some(entry) if entry.user_id == user_id && entry.slug == slug => {
                    pending.remove(state)
                }
                _ => None,
            }
        }
        .ok_or_else(unknown)?;
        let record = exchange_code(
            &self.net,
            &entry.discovery,
            &entry.client,
            code,
            &entry.verifier,
            &entry.scopes,
        )
        .await?;
        self.store
            .marketplace_account_upsert(user_id, slug, &entry.account_id, &entry.label)?;
        self.accounts
            .save_signed_in(user_id, slug, &entry.account_id, &record)?;
        Ok(Completed {
            account_id: entry.account_id,
        })
    }
}
