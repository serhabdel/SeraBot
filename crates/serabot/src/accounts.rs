//! Per-user AI provider accounts.
//!
//! A member can hold several accounts per provider (API keys, OAuth sign-ins).
//! The rows live in `provider_accounts` and hold only non-secret state: label,
//! status, priority, the identity the provider reported (email, plan) and a
//! fingerprint used to recognise a repeat sign-in. Each account's credential is
//! a JSON [`ProviderCredential`] sealed in `user_credentials` under the
//! reserved provider id `acct:<provider>:<account_id>`; the AEAD associated
//! data binds the blob to the user and that id, so it cannot be moved to
//! another account or another user.
//!
//! Every method takes the acting user's id first and filters on it. There is no
//! path that lists or opens another user's account, and no value returned here
//! carries a secret except [`Accounts::credential`], which is
//! plumbing for building a client.
//!
//! # The legacy `default` account
//!
//! A `(user, provider)` credential written before accounts existed has no row
//! here. It reads as the virtual account [`DEFAULT_ACCOUNT`] and keeps working
//! untouched. The first edit (label, priority, disable) moves it into the
//! account scheme inside one transaction: the sealed blob is re-sealed under
//! the account's reserved id, the account row is inserted and the legacy row is
//! deleted together. The edit returns the account under its new id.

use std::sync::Arc;

use rusqlite::{OptionalExtension, Row, TransactionBehavior, params};
use serde::Serialize;
use serde_json::{Value, json};

use crate::error::{Result, SeraError};
use crate::store::{Store, now_string};
use crate::user_secrets::{
    ProviderCredential, UserSecrets, account_provider_id, new_account_id, valid_account_id,
};

/// The id of the account a pre-accounts credential reads as.
pub const DEFAULT_ACCOUNT: &str = "default";
/// Most accounts one user may hold for one provider.
pub const MAX_ACCOUNTS_PER_PROVIDER: usize = 10;
/// Longest label, in characters.
pub const MAX_LABEL_CHARS: usize = 48;
/// Largest `identity_json`, in bytes.
const MAX_IDENTITY_BYTES: usize = 2048;

/// How an account signed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    ApiKey,
    Oauth,
    Device,
    Minted,
}

impl AuthKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::Oauth => "oauth",
            Self::Device => "device",
            Self::Minted => "minted",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "api_key" => Self::ApiKey,
            "oauth" => Self::Oauth,
            "device" => Self::Device,
            "minted" => Self::Minted,
            _ => return None,
        })
    }
}

/// Whether an account can be used right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    Ok,
    NeedsReauth,
    RateLimited,
    QuotaExhausted,
    Disabled,
}

impl AccountStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::NeedsReauth => "needs_reauth",
            Self::RateLimited => "rate_limited",
            Self::QuotaExhausted => "quota_exhausted",
            Self::Disabled => "disabled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "ok" => Self::Ok,
            "needs_reauth" => Self::NeedsReauth,
            "rate_limited" => Self::RateLimited,
            "quota_exhausted" => Self::QuotaExhausted,
            "disabled" => Self::Disabled,
            _ => return None,
        })
    }
}

/// One account's non-secret state.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderAccount {
    pub provider: String,
    #[serde(rename = "id")]
    pub account_id: String,
    pub label: String,
    pub auth_kind: AuthKind,
    pub status: AccountStatus,
    pub priority: i64,
    pub weight: i64,
    /// What the provider reported about the account (email, plan, project).
    /// Non-secret by construction; the API layer masks it for display.
    pub identity: Value,
    pub quota: Option<Value>,
    pub cooldown_until: Option<String>,
    pub last_error: Option<String>,
    pub last_used_at: Option<String>,
    pub session_generation: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl ProviderAccount {
    /// Whether this is the virtual pre-accounts credential.
    pub fn is_default(&self) -> bool {
        self.account_id == DEFAULT_ACCOUNT
    }
}

const COLUMNS: &str = "provider, account_id, label, auth_kind, status, priority, weight,
    identity_json, quota_json, cooldown_until, last_error, last_used_at, session_generation,
    created_at, updated_at";

fn account_from_row(row: &Row<'_>) -> rusqlite::Result<ProviderAccount> {
    let kind: String = row.get(3)?;
    let status: String = row.get(4)?;
    let identity: String = row.get(7)?;
    let quota: Option<String> = row.get(8)?;
    Ok(ProviderAccount {
        provider: row.get(0)?,
        account_id: row.get(1)?,
        label: row.get(2)?,
        auth_kind: AuthKind::parse(&kind).unwrap_or(AuthKind::ApiKey),
        status: AccountStatus::parse(&status).unwrap_or(AccountStatus::NeedsReauth),
        priority: row.get(5)?,
        weight: row.get(6)?,
        identity: serde_json::from_str(&identity).unwrap_or_else(|_| json!({})),
        quota: quota.and_then(|text| serde_json::from_str(&text).ok()),
        cooldown_until: row.get(9)?,
        last_error: row.get(10)?,
        last_used_at: row.get(11)?,
        session_generation: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}

/// Everything a new account row needs besides its sealed credential.
pub(crate) struct NewAccount<'a> {
    pub provider: &'a str,
    pub account_id: &'a str,
    pub label: &'a str,
    pub auth_kind: AuthKind,
    pub fingerprint: &'a str,
    pub identity: &'a Value,
    pub priority: i64,
}

/// The result of saving an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddOutcome {
    pub account_id: String,
    /// False when the sign-in matched an existing account and refreshed it.
    pub created: bool,
}

impl Store {
    /// The acting user's accounts, best first (priority, then oldest). Pass a
    /// provider to narrow. The legacy `default` account is not a row here;
    /// see [`Accounts::list`].
    pub fn provider_accounts(
        &self,
        user_id: &str,
        provider: Option<&str>,
    ) -> Result<Vec<ProviderAccount>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(&format!(
            "SELECT {COLUMNS} FROM provider_accounts
             WHERE user_id = ?1 AND (?2 IS NULL OR provider = ?2)
             ORDER BY provider, priority DESC, created_at, account_id LIMIT 512"
        ))?;
        let rows = statement.query_map(params![user_id, provider], account_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Ids of the acting user's accounts for `provider` that are not disabled,
    /// best first, with `default` last when a pre-accounts credential exists.
    /// Ids only: nothing is opened.
    pub fn usable_account_ids(&self, user_id: &str, provider: &str) -> Result<Vec<String>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT account_id FROM provider_accounts
             WHERE user_id = ?1 AND provider = ?2 AND status != 'disabled'
             ORDER BY priority DESC, created_at, account_id LIMIT 32",
        )?;
        let mut ids = statement
            .query_map(params![user_id, provider], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let legacy: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM user_credentials WHERE user_id = ?1 AND provider = ?2)",
            params![user_id, provider],
            |row| row.get(0),
        )?;
        if legacy {
            ids.push(DEFAULT_ACCOUNT.into());
        }
        Ok(ids)
    }

    pub fn provider_account(
        &self,
        user_id: &str,
        provider: &str,
        account_id: &str,
    ) -> Result<Option<ProviderAccount>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM provider_accounts
                     WHERE user_id = ?1 AND provider = ?2 AND account_id = ?3"
                ),
                params![user_id, provider, account_id],
                account_from_row,
            )
            .optional()?)
    }

    fn provider_account_by_fingerprint(
        &self,
        user_id: &str,
        provider: &str,
        fingerprint: &str,
    ) -> Result<Option<ProviderAccount>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM provider_accounts
                     WHERE user_id = ?1 AND provider = ?2 AND fingerprint = ?3"
                ),
                params![user_id, provider, fingerprint],
                account_from_row,
            )
            .optional()?)
    }

    /// Insert an account (or refresh the one with the same fingerprint) and
    /// write its sealed credential in one transaction. With `move_legacy` the
    /// pre-accounts `(user, provider)` credential row is deleted in the same
    /// transaction, so a failure leaves the old credential in place.
    pub(crate) fn save_provider_account(
        &self,
        user_id: &str,
        new: &NewAccount<'_>,
        secret_id: &str,
        sealed: &(Vec<u8>, [u8; crate::user_secrets::NONCE_BYTES]),
        move_legacy: bool,
    ) -> Result<bool> {
        let now = now_string();
        let identity = serde_json::to_string(new.identity)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if move_legacy {
            let removed = transaction.execute(
                "DELETE FROM user_credentials WHERE user_id = ?1 AND provider = ?2",
                params![user_id, new.provider],
            )?;
            if removed != 1 {
                return Err(SeraError::InvalidState(
                    "the default account was already changed; reload and try again".into(),
                ));
            }
        }
        let existing: Option<String> = transaction
            .query_row(
                "SELECT account_id FROM provider_accounts
                 WHERE user_id = ?1 AND provider = ?2 AND fingerprint = ?3",
                params![user_id, new.provider, new.fingerprint],
                |row| row.get(0),
            )
            .optional()?;
        let created = match existing {
            Some(id) if id != new.account_id => {
                return Err(SeraError::InvalidState(
                    "the account list changed; try again".into(),
                ));
            }
            Some(_) => {
                // The same identity signed in again: refresh it in place.
                transaction.execute(
                    "UPDATE provider_accounts
                     SET auth_kind = ?4, identity_json = ?5, status = 'ok', last_error = NULL,
                         cooldown_until = NULL, updated_at = ?6
                     WHERE user_id = ?1 AND provider = ?2 AND account_id = ?3",
                    params![
                        user_id,
                        new.provider,
                        new.account_id,
                        new.auth_kind.as_str(),
                        identity,
                        now
                    ],
                )?;
                false
            }
            None => {
                let count: i64 = transaction.query_row(
                    "SELECT (SELECT COUNT(*) FROM provider_accounts
                             WHERE user_id = ?1 AND provider = ?2)
                          + (SELECT COUNT(*) FROM user_credentials
                             WHERE user_id = ?1 AND provider = ?2)",
                    params![user_id, new.provider],
                    |row| row.get(0),
                )?;
                if count as usize >= MAX_ACCOUNTS_PER_PROVIDER {
                    return Err(SeraError::Rejected {
                        code: "account_limit",
                        message: format!(
                            "You can add up to {MAX_ACCOUNTS_PER_PROVIDER} accounts for one provider"
                        ),
                    });
                }
                transaction.execute(
                    "INSERT INTO provider_accounts(
                         user_id, provider, account_id, label, auth_kind, fingerprint,
                         identity_json, priority, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
                    params![
                        user_id,
                        new.provider,
                        new.account_id,
                        new.label,
                        new.auth_kind.as_str(),
                        new.fingerprint,
                        identity,
                        new.priority,
                        now
                    ],
                )?;
                true
            }
        };
        transaction.execute(
            "INSERT INTO user_credentials(user_id, provider, ciphertext, nonce, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(user_id, provider) DO UPDATE SET
                 ciphertext = excluded.ciphertext,
                 nonce = excluded.nonce,
                 updated_at = excluded.updated_at",
            params![user_id, secret_id, sealed.0, sealed.1.as_slice(), now],
        )?;
        transaction.commit()?;
        Ok(created)
    }

    /// Change an account's label, priority, weight or disabled state.
    pub(crate) fn patch_provider_account(
        &self,
        user_id: &str,
        provider: &str,
        account_id: &str,
        patch: &AccountPatch,
    ) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE provider_accounts SET
                 label = COALESCE(?4, label),
                 priority = COALESCE(?5, priority),
                 weight = COALESCE(?6, weight),
                 status = CASE
                     WHEN ?7 = 1 THEN 'disabled'
                     WHEN ?7 = 0 AND status = 'disabled' THEN 'ok'
                     ELSE status END,
                 updated_at = ?8
             WHERE user_id = ?1 AND provider = ?2 AND account_id = ?3",
            params![
                user_id,
                provider,
                account_id,
                patch.label,
                patch.priority,
                patch.weight,
                patch.disabled.map(i64::from),
                now_string()
            ],
        )?;
        Ok(changed == 1)
    }

    /// Record why an account is (or stops being) unusable.
    pub fn set_provider_account_status(
        &self,
        user_id: &str,
        provider: &str,
        account_id: &str,
        status: AccountStatus,
        last_error: Option<&str>,
        cooldown_until: Option<&str>,
    ) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE provider_accounts
             SET status = ?4, last_error = ?5, cooldown_until = ?6, updated_at = ?7
             WHERE user_id = ?1 AND provider = ?2 AND account_id = ?3",
            params![
                user_id,
                provider,
                account_id,
                status.as_str(),
                last_error,
                cooldown_until,
                now_string()
            ],
        )?;
        Ok(changed == 1)
    }

    /// Delete an account row and its sealed credential together. Returns
    /// false when the acting user has no such account.
    pub(crate) fn delete_provider_account(
        &self,
        user_id: &str,
        provider: &str,
        account_id: &str,
        secret_id: &str,
    ) -> Result<bool> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let removed = transaction.execute(
            "DELETE FROM provider_accounts
             WHERE user_id = ?1 AND provider = ?2 AND account_id = ?3",
            params![user_id, provider, account_id],
        )?;
        transaction.execute(
            "DELETE FROM user_credentials WHERE user_id = ?1 AND provider = ?2",
            params![user_id, secret_id],
        )?;
        transaction.commit()?;
        Ok(removed == 1)
    }
}

/// An edit to an account. `None` leaves a field alone.
#[derive(Debug, Clone, Default)]
pub struct AccountPatch {
    pub label: Option<String>,
    pub priority: Option<i64>,
    pub weight: Option<i64>,
    pub disabled: Option<bool>,
}

/// A sign-in or a pasted key, ready to store.
pub struct AddAccount {
    pub provider: String,
    pub label: String,
    pub auth_kind: AuthKind,
    /// Derived from a stable identity (see [`identity_fingerprint`]) or, for an
    /// API key, [`UserSecrets::key_fingerprint`]. Never a hash of a token.
    pub fingerprint: String,
    /// Non-secret facts the provider reported: email, plan, project id, tier.
    pub identity: Value,
    pub credential: ProviderCredential,
    pub priority: i64,
}

/// The fingerprint of a provider identity (an account id or a lower-cased
/// email): `sha256("<provider>:<identity>")`.
pub fn identity_fingerprint(provider: &str, identity: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(format!("{provider}:{identity}").as_bytes()))
}

/// The account service: store rows plus sealed credentials.
pub struct Accounts {
    store: Store,
    secrets: Arc<UserSecrets>,
}

impl Accounts {
    pub fn new(store: Store, secrets: Arc<UserSecrets>) -> Self {
        Self { store, secrets }
    }

    /// The acting user's accounts for `provider`, best first, including the
    /// virtual `default` account when a pre-accounts credential exists.
    pub fn list(&self, user_id: &str, provider: &str) -> Result<Vec<ProviderAccount>> {
        let mut accounts = self.store.provider_accounts(user_id, Some(provider))?;
        if let Some(default) = self.default_account(user_id, provider)? {
            accounts.push(default);
        }
        accounts.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.created_at.cmp(&b.created_at))
        });
        Ok(accounts)
    }

    /// One account, or `None` when the acting user has no such account (another
    /// user's account id is indistinguishable from a missing one).
    pub fn get(
        &self,
        user_id: &str,
        provider: &str,
        account_id: &str,
    ) -> Result<Option<ProviderAccount>> {
        if account_id == DEFAULT_ACCOUNT {
            return self.default_account(user_id, provider);
        }
        if !valid_account_id(account_id) {
            return Ok(None);
        }
        self.store.provider_account(user_id, provider, account_id)
    }

    /// Open one account's credential to build a client. `None` when the account
    /// or its sealed blob is missing; a corrupt or moved blob is an error.
    pub fn credential(
        &self,
        user_id: &str,
        provider: &str,
        account_id: &str,
    ) -> Result<Option<ProviderCredential>> {
        let secret = if account_id == DEFAULT_ACCOUNT {
            self.secrets.get(user_id, provider)?
        } else {
            if self
                .store
                .provider_account(user_id, provider, account_id)?
                .is_none()
            {
                return Ok(None);
            }
            self.secrets.get_account(user_id, provider, account_id)?
        };
        secret
            .map(|secret| ProviderCredential::parse(&secret))
            .transpose()
    }

    /// Add a sign-in or key. A repeat of a known fingerprint refreshes that
    /// account in place (credential, identity, status) and keeps its label,
    /// priority and id.
    pub fn add(&self, user_id: &str, request: AddAccount) -> Result<AddOutcome> {
        let label = clean_label(&request.label)?;
        let identity = clean_identity(&request.identity)?;
        let encoded = request.credential.encode()?;
        if request.fingerprint.is_empty() || request.fingerprint.len() > 128 {
            return Err(SeraError::Validation("invalid account fingerprint".into()));
        }
        let existing = self.store.provider_account_by_fingerprint(
            user_id,
            &request.provider,
            &request.fingerprint,
        )?;
        let account_id = existing
            .map(|account| account.account_id)
            .unwrap_or_else(new_account_id);
        let secret_id = account_provider_id(&request.provider, &account_id)?;
        let sealed =
            self.secrets
                .seal_account(user_id, &request.provider, &account_id, &encoded)?;
        let created = self.store.save_provider_account(
            user_id,
            &NewAccount {
                provider: &request.provider,
                account_id: &account_id,
                label: &label,
                auth_kind: request.auth_kind,
                fingerprint: &request.fingerprint,
                identity: &identity,
                priority: request.priority,
            },
            &secret_id,
            &sealed,
            false,
        )?;
        Ok(AddOutcome {
            account_id,
            created,
        })
    }

    /// Add a pasted API key. A key the user already holds (as an account or as
    /// the legacy default) is recognised and not stored twice.
    pub fn add_api_key(
        &self,
        user_id: &str,
        provider: &str,
        label: &str,
        key: &str,
    ) -> Result<AddOutcome> {
        let credential = ProviderCredential {
            api_key: key.to_owned(),
            ..Default::default()
        };
        credential.encode()?;
        if let Some(default) = self.credential(user_id, provider, DEFAULT_ACCOUNT)?
            && default.refresh_token.is_empty()
            && default.api_key == key
        {
            return Ok(AddOutcome {
                account_id: DEFAULT_ACCOUNT.into(),
                created: false,
            });
        }
        self.add(
            user_id,
            AddAccount {
                provider: provider.to_owned(),
                label: label.to_owned(),
                auth_kind: AuthKind::ApiKey,
                fingerprint: self.secrets.key_fingerprint(user_id, provider, key),
                identity: json!({}),
                credential,
                priority: 0,
            },
        )
    }

    /// Replace an existing account's credential (a rotated token, a re-mint).
    /// The account keeps its id, label and priority; a login that was in
    /// `needs_reauth` becomes usable again.
    pub fn store_credential(
        &self,
        user_id: &str,
        provider: &str,
        account_id: &str,
        credential: &ProviderCredential,
    ) -> Result<()> {
        let encoded = credential.encode()?;
        if account_id == DEFAULT_ACCOUNT {
            return self.secrets.set(user_id, provider, &encoded);
        }
        if self
            .store
            .provider_account(user_id, provider, account_id)?
            .is_none()
        {
            return Err(SeraError::NotFound("account".into()));
        }
        self.secrets
            .set_account(user_id, provider, account_id, &encoded)
    }

    /// Edit an account. The legacy `default` moves into the account scheme
    /// first; the returned account carries its new id.
    pub fn update(
        &self,
        user_id: &str,
        provider: &str,
        account_id: &str,
        patch: &AccountPatch,
    ) -> Result<ProviderAccount> {
        let patch = AccountPatch {
            label: patch.label.as_deref().map(clean_label).transpose()?,
            priority: patch.priority.map(clamp_priority),
            weight: patch.weight.map(|weight| weight.clamp(1, 100)),
            disabled: patch.disabled,
        };
        let account_id = if account_id == DEFAULT_ACCOUNT {
            self.materialize_default(user_id, provider)?.account_id
        } else {
            account_id.to_owned()
        };
        if !valid_account_id(&account_id)
            || !self
                .store
                .patch_provider_account(user_id, provider, &account_id, &patch)?
        {
            return Err(SeraError::NotFound("account".into()));
        }
        self.store
            .provider_account(user_id, provider, &account_id)?
            .ok_or_else(|| SeraError::NotFound("account".into()))
    }

    /// Remove an account and its sealed credential. False when the acting user
    /// has no such account.
    pub fn remove(&self, user_id: &str, provider: &str, account_id: &str) -> Result<bool> {
        if account_id == DEFAULT_ACCOUNT {
            return self.secrets.delete(user_id, provider);
        }
        if !valid_account_id(account_id) {
            return Ok(false);
        }
        let secret_id = account_provider_id(provider, account_id)?;
        self.store
            .delete_provider_account(user_id, provider, account_id, &secret_id)
    }

    /// Mark an account unusable (or usable again) with a short reason.
    pub fn set_status(
        &self,
        user_id: &str,
        provider: &str,
        account_id: &str,
        status: AccountStatus,
        last_error: Option<&str>,
        cooldown_until: Option<&str>,
    ) -> Result<()> {
        if !valid_account_id(account_id) {
            return Ok(());
        }
        self.store.set_provider_account_status(
            user_id,
            provider,
            account_id,
            status,
            last_error
                .map(|text| text.chars().take(200).collect::<String>())
                .as_deref(),
            cooldown_until,
        )?;
        Ok(())
    }

    /// The virtual `default` account, when a pre-accounts credential exists.
    fn default_account(&self, user_id: &str, provider: &str) -> Result<Option<ProviderAccount>> {
        if crate::user_secrets::validate_provider(provider).is_err() {
            return Ok(None);
        }
        let Some(row) = self.store.user_credential(user_id, provider)? else {
            return Ok(None);
        };
        // The kind is read from the stored shape. A blob that cannot be opened
        // still lists, so the user can see it and replace it.
        let (auth_kind, status, last_error) =
            match self.credential(user_id, provider, DEFAULT_ACCOUNT) {
                Ok(Some(credential)) if !credential.refresh_token.is_empty() => {
                    (AuthKind::Oauth, AccountStatus::Ok, None)
                }
                Ok(_) => (AuthKind::ApiKey, AccountStatus::Ok, None),
                Err(_) => (
                    AuthKind::ApiKey,
                    AccountStatus::NeedsReauth,
                    Some("the stored credential could not be read".to_owned()),
                ),
            };
        let stamp = crate::store::to_string(row.updated_at);
        Ok(Some(ProviderAccount {
            provider: provider.to_owned(),
            account_id: DEFAULT_ACCOUNT.into(),
            label: "Default".into(),
            auth_kind,
            status,
            priority: 0,
            weight: 1,
            identity: json!({}),
            quota: None,
            cooldown_until: None,
            last_error,
            last_used_at: None,
            session_generation: 0,
            created_at: stamp.clone(),
            updated_at: stamp,
        }))
    }

    /// Move the legacy credential into the account scheme in one transaction.
    fn materialize_default(&self, user_id: &str, provider: &str) -> Result<ProviderAccount> {
        let credential = self
            .credential(user_id, provider, DEFAULT_ACCOUNT)?
            .ok_or_else(|| SeraError::NotFound("account".into()))?;
        let (kind, basis) = if credential.refresh_token.is_empty() {
            (AuthKind::ApiKey, credential.api_key.as_str())
        } else {
            (AuthKind::Oauth, credential.refresh_token.as_str())
        };
        let fingerprint = self.secrets.key_fingerprint(user_id, provider, basis);
        let account_id = new_account_id();
        let secret_id = account_provider_id(provider, &account_id)?;
        let sealed =
            self.secrets
                .seal_account(user_id, provider, &account_id, &credential.encode()?)?;
        let identity = json!({});
        self.store.save_provider_account(
            user_id,
            &NewAccount {
                provider,
                account_id: &account_id,
                label: "Default",
                auth_kind: kind,
                fingerprint: &fingerprint,
                identity: &identity,
                priority: 0,
            },
            &secret_id,
            &sealed,
            true,
        )?;
        self.store
            .provider_account(user_id, provider, &account_id)?
            .ok_or_else(|| SeraError::Internal("the moved account is missing".into()))
    }
}

fn clamp_priority(priority: i64) -> i64 {
    priority.clamp(-1000, 1000)
}

fn clean_label(raw: &str) -> Result<String> {
    let label = raw.trim();
    let count = label.chars().count();
    if count == 0 || count > MAX_LABEL_CHARS || label.chars().any(char::is_control) {
        return Err(SeraError::Validation(format!(
            "the account label must be 1 to {MAX_LABEL_CHARS} characters"
        )));
    }
    Ok(label.to_owned())
}

fn clean_identity(identity: &Value) -> Result<Value> {
    if !identity.is_object() || identity.to_string().len() > MAX_IDENTITY_BYTES {
        return Err(SeraError::Validation(
            "the account identity must be a small object".into(),
        ));
    }
    Ok(identity.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    fn setup() -> (Store, Arc<UserSecrets>, Accounts) {
        let store = Store::open_in_memory(384).unwrap();
        let secrets = Arc::new(
            UserSecrets::with_master_key(store.clone(), [9u8; crate::user_secrets::KEY_BYTES])
                .unwrap(),
        );
        let accounts = Accounts::new(store.clone(), secrets.clone());
        (store, secrets, accounts)
    }

    fn member(store: &Store, subject: &str) -> String {
        store
            .create_user(subject, "member@example.example", "Synthetic Member")
            .unwrap()
            .id
    }

    fn oauth(provider: &str, identity: &str, token: &str) -> AddAccount {
        AddAccount {
            provider: provider.into(),
            label: identity.into(),
            auth_kind: AuthKind::Oauth,
            fingerprint: identity_fingerprint(provider, identity),
            identity: json!({"email": identity, "plan": "synthetic"}),
            credential: ProviderCredential {
                api_key: token.into(),
                refresh_token: format!("refresh-{token}"),
                ..Default::default()
            },
            priority: 0,
        }
    }

    #[test]
    fn accounts_are_sealed_under_their_reserved_id_and_listed_by_priority() {
        let (store, secrets, accounts) = setup();
        let alice = member(&store, "microsoft:acct-list");
        let one = accounts
            .add_api_key(&alice, "anthropic", "Work", "sk-synthetic-one")
            .unwrap();
        let two = accounts
            .add_api_key(&alice, "anthropic", "Home", "sk-synthetic-two")
            .unwrap();
        assert!(one.created && two.created);
        assert_ne!(one.account_id, two.account_id);
        accounts
            .update(
                &alice,
                "anthropic",
                &two.account_id,
                &AccountPatch {
                    priority: Some(5),
                    ..Default::default()
                },
            )
            .unwrap();
        let listed = accounts.list(&alice, "anthropic").unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|account| account.label.as_str())
                .collect::<Vec<_>>(),
            ["Home", "Work"]
        );
        // The credential opens, and sits under the reserved id, not the bare
        // provider id.
        let credential = accounts
            .credential(&alice, "anthropic", &one.account_id)
            .unwrap()
            .unwrap();
        assert_eq!(credential.api_key, "sk-synthetic-one");
        assert!(secrets.get(&alice, "anthropic").unwrap().is_none());
        let reserved = account_provider_id("anthropic", &one.account_id).unwrap();
        assert!(reserved.len() <= 64);
        assert!(store.user_credential(&alice, &reserved).unwrap().is_some());
        // Reserved ids never appear as providers the user can use.
        assert!(secrets.providers_for(&alice).unwrap().is_empty());
        // No secret in the serialized listing.
        let shown = serde_json::to_string(&listed).unwrap();
        assert!(!shown.contains("sk-synthetic"));
        assert!(!shown.contains("fingerprint"));
    }

    #[test]
    fn another_user_cannot_list_read_or_change_an_account() {
        let (store, _secrets, accounts) = setup();
        let alice = member(&store, "microsoft:acct-alice");
        let bob = member(&store, "microsoft:acct-bob");
        let added = accounts
            .add_api_key(&alice, "anthropic", "Alice key", "sk-alice-synthetic")
            .unwrap();
        assert!(accounts.list(&bob, "anthropic").unwrap().is_empty());
        assert!(
            accounts
                .get(&bob, "anthropic", &added.account_id)
                .unwrap()
                .is_none()
        );
        assert!(
            accounts
                .credential(&bob, "anthropic", &added.account_id)
                .unwrap()
                .is_none()
        );
        assert!(store.provider_accounts(&bob, None).unwrap().is_empty());
        assert!(matches!(
            accounts.update(
                &bob,
                "anthropic",
                &added.account_id,
                &AccountPatch {
                    label: Some("mine now".into()),
                    ..Default::default()
                },
            ),
            Err(SeraError::NotFound(_))
        ));
        assert!(
            !accounts
                .remove(&bob, "anthropic", &added.account_id)
                .unwrap()
        );
        // Alice's account is untouched.
        let still = accounts
            .get(&alice, "anthropic", &added.account_id)
            .unwrap()
            .unwrap();
        assert_eq!(still.label, "Alice key");
        assert!(
            accounts
                .credential(&alice, "anthropic", &added.account_id)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn a_sealed_blob_moved_between_accounts_fails_to_open() {
        let (store, secrets, accounts) = setup();
        let alice = member(&store, "microsoft:acct-aad");
        let first = accounts
            .add_api_key(&alice, "anthropic", "One", "sk-one-synthetic")
            .unwrap();
        let second = accounts
            .add_api_key(&alice, "anthropic", "Two", "sk-two-synthetic")
            .unwrap();
        let first_id = account_provider_id("anthropic", &first.account_id).unwrap();
        let second_id = account_provider_id("anthropic", &second.account_id).unwrap();
        let row = store.user_credential(&alice, &first_id).unwrap().unwrap();
        store
            .put_user_credential(&alice, &second_id, &row.ciphertext, &row.nonce)
            .unwrap();
        let error = secrets
            .get_account(&alice, "anthropic", &second.account_id)
            .unwrap_err();
        assert!(error.to_string().contains("could not be decrypted"));
        assert!(
            accounts
                .credential(&alice, "anthropic", &second.account_id)
                .is_err()
        );
        // The original still opens.
        assert!(
            secrets
                .get_account(&alice, "anthropic", &first.account_id)
                .unwrap()
                .unwrap()
                .expose_secret()
                .contains("sk-one-synthetic")
        );
        // A blob moved to another provider's account id fails the same way.
        let other = account_provider_id("openai", &first.account_id).unwrap();
        store
            .put_user_credential(&alice, &other, &row.ciphertext, &row.nonce)
            .unwrap();
        assert!(
            secrets
                .get_account(&alice, "openai", &first.account_id)
                .is_err()
        );
    }

    #[test]
    fn a_duplicate_fingerprint_updates_the_existing_account_in_place() {
        let (store, _secrets, accounts) = setup();
        let alice = member(&store, "microsoft:acct-dup");
        let first = accounts
            .add(
                &alice,
                oauth("google-antigravity", "a@example.example", "access-1"),
            )
            .unwrap();
        accounts
            .update(
                &alice,
                "google-antigravity",
                &first.account_id,
                &AccountPatch {
                    label: Some("Renamed".into()),
                    priority: Some(3),
                    ..Default::default()
                },
            )
            .unwrap();
        accounts
            .set_status(
                &alice,
                "google-antigravity",
                &first.account_id,
                AccountStatus::NeedsReauth,
                Some("refresh rejected"),
                None,
            )
            .unwrap();
        let again = accounts
            .add(
                &alice,
                oauth("google-antigravity", "a@example.example", "access-2"),
            )
            .unwrap();
        assert_eq!(again.account_id, first.account_id);
        assert!(!again.created);
        let listed = accounts.list(&alice, "google-antigravity").unwrap();
        assert_eq!(listed.len(), 1);
        // Label and priority survive; status is healthy again; the new token
        // replaced the old one.
        assert_eq!(listed[0].label, "Renamed");
        assert_eq!(listed[0].priority, 3);
        assert_eq!(listed[0].status, AccountStatus::Ok);
        assert!(listed[0].last_error.is_none());
        let credential = accounts
            .credential(&alice, "google-antigravity", &first.account_id)
            .unwrap()
            .unwrap();
        assert_eq!(credential.api_key, "access-2");
        // A different identity is a second account.
        let other = accounts
            .add(
                &alice,
                oauth("google-antigravity", "b@example.example", "access-3"),
            )
            .unwrap();
        assert!(other.created);
        assert_eq!(
            accounts.list(&alice, "google-antigravity").unwrap().len(),
            2
        );
        // The same key twice is one account.
        let key = accounts
            .add_api_key(&alice, "anthropic", "K", "sk-same-synthetic")
            .unwrap();
        let key_again = accounts
            .add_api_key(&alice, "anthropic", "K2", "sk-same-synthetic")
            .unwrap();
        assert_eq!(key.account_id, key_again.account_id);
        assert!(!key_again.created);
    }

    #[test]
    fn the_per_provider_cap_is_enforced() {
        let (store, _secrets, accounts) = setup();
        let alice = member(&store, "microsoft:acct-cap");
        for index in 0..MAX_ACCOUNTS_PER_PROVIDER {
            accounts
                .add_api_key(&alice, "anthropic", "Key", &format!("sk-synthetic-{index}"))
                .unwrap();
        }
        let error = accounts
            .add_api_key(&alice, "anthropic", "Key", "sk-synthetic-over")
            .unwrap_err();
        assert!(matches!(
            error,
            SeraError::Rejected {
                code: "account_limit",
                ..
            }
        ));
        // Another provider and another user are unaffected.
        accounts
            .add_api_key(&alice, "openai", "Key", "sk-synthetic-openai")
            .unwrap();
        let bob = member(&store, "microsoft:acct-cap-bob");
        accounts
            .add_api_key(&bob, "anthropic", "Key", "sk-synthetic-bob")
            .unwrap();
    }

    #[test]
    fn a_pre_accounts_credential_reads_as_default_and_moves_on_first_edit() {
        let (store, secrets, accounts) = setup();
        let alice = member(&store, "microsoft:acct-legacy");
        // As an old 0036 server wrote it: a bare provider row.
        let legacy = ProviderCredential {
            api_key: "sk-legacy-synthetic".into(),
            ..Default::default()
        };
        secrets
            .set(&alice, "mistral", &legacy.encode().unwrap())
            .unwrap();

        let listed = accounts.list(&alice, "mistral").unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].is_default());
        assert_eq!(listed[0].auth_kind, AuthKind::ApiKey);
        // Still usable through the old path and the new one.
        assert!(secrets.resolve(&alice, "mistral").unwrap().is_some());
        assert_eq!(
            accounts
                .credential(&alice, "mistral", DEFAULT_ACCOUNT)
                .unwrap()
                .unwrap()
                .api_key,
            "sk-legacy-synthetic"
        );
        // The same key pasted again is recognised as the default.
        let same = accounts
            .add_api_key(&alice, "mistral", "Again", "sk-legacy-synthetic")
            .unwrap();
        assert_eq!(same.account_id, DEFAULT_ACCOUNT);
        // A second key coexists with the virtual default.
        accounts
            .add_api_key(&alice, "mistral", "Second", "sk-second-synthetic")
            .unwrap();
        assert_eq!(accounts.list(&alice, "mistral").unwrap().len(), 2);

        // The first edit moves it: new id, legacy row gone, credential intact.
        let moved = accounts
            .update(
                &alice,
                "mistral",
                DEFAULT_ACCOUNT,
                &AccountPatch {
                    label: Some("Primary".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(!moved.is_default());
        assert_eq!(moved.label, "Primary");
        assert!(store.user_credential(&alice, "mistral").unwrap().is_none());
        assert_eq!(
            accounts
                .credential(&alice, "mistral", &moved.account_id)
                .unwrap()
                .unwrap()
                .api_key,
            "sk-legacy-synthetic"
        );
        let listed = accounts.list(&alice, "mistral").unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().all(|account| !account.is_default()));
    }

    #[test]
    fn a_failed_move_leaves_the_legacy_credential_in_place() {
        let (store, secrets, accounts) = setup();
        let alice = member(&store, "microsoft:acct-atomic");
        let legacy = ProviderCredential {
            api_key: "sk-atomic-synthetic".into(),
            ..Default::default()
        };
        secrets
            .set(&alice, "mistral", &legacy.encode().unwrap())
            .unwrap();
        // Make the account insert fail inside the transaction: the table is
        // gone, so the delete of the legacy row must roll back with it.
        store
            .connection()
            .unwrap()
            .execute_batch("ALTER TABLE provider_accounts RENAME TO provider_accounts_gone")
            .unwrap();
        let result = accounts.update(
            &alice,
            "mistral",
            DEFAULT_ACCOUNT,
            &AccountPatch {
                label: Some("X".into()),
                ..Default::default()
            },
        );
        assert!(result.is_err());
        store
            .connection()
            .unwrap()
            .execute_batch("ALTER TABLE provider_accounts_gone RENAME TO provider_accounts")
            .unwrap();
        assert!(secrets.get(&alice, "mistral").unwrap().is_some());
    }

    #[test]
    fn removing_and_disabling_work_per_user() {
        let (store, secrets, accounts) = setup();
        let alice = member(&store, "microsoft:acct-remove");
        let added = accounts
            .add_api_key(&alice, "anthropic", "Key", "sk-remove-synthetic")
            .unwrap();
        let disabled = accounts
            .update(
                &alice,
                "anthropic",
                &added.account_id,
                &AccountPatch {
                    disabled: Some(true),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(disabled.status, AccountStatus::Disabled);
        let enabled = accounts
            .update(
                &alice,
                "anthropic",
                &added.account_id,
                &AccountPatch {
                    disabled: Some(false),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(enabled.status, AccountStatus::Ok);
        assert!(
            accounts
                .remove(&alice, "anthropic", &added.account_id)
                .unwrap()
        );
        assert!(
            !accounts
                .remove(&alice, "anthropic", &added.account_id)
                .unwrap()
        );
        assert!(
            secrets
                .get_account(&alice, "anthropic", &added.account_id)
                .unwrap()
                .is_none()
        );
        // Removing the legacy default deletes the bare row.
        secrets
            .set(
                &alice,
                "groq",
                &ProviderCredential {
                    api_key: "gsk-synthetic".into(),
                    ..Default::default()
                }
                .encode()
                .unwrap(),
            )
            .unwrap();
        assert!(accounts.remove(&alice, "groq", DEFAULT_ACCOUNT).unwrap());
        assert!(accounts.list(&alice, "groq").unwrap().is_empty());
    }

    #[test]
    fn inputs_are_validated() {
        let (store, _secrets, accounts) = setup();
        let alice = member(&store, "microsoft:acct-validate");
        assert!(
            accounts
                .add_api_key(&alice, "anthropic", "", "sk-x")
                .is_err()
        );
        assert!(accounts.add_api_key(&alice, "anthropic", "K", "").is_err());
        assert!(
            accounts
                .add_api_key(&alice, "anthropic", "K", "line\nbreak")
                .is_err()
        );
        assert!(
            accounts
                .add_api_key(&alice, &"p".repeat(60), "K", "sk-x")
                .is_err(),
            "the reserved id must fit 64 characters"
        );
        assert!(
            accounts
                .add_api_key(&alice, "bad provider", "K", "sk-x")
                .is_err()
        );
        let mut request = oauth("anthropic", "a@example.example", "t");
        request.identity = json!("not an object");
        assert!(accounts.add(&alice, request).is_err());
        // Unknown and malformed account ids are simply absent.
        assert!(accounts.get(&alice, "anthropic", "nope").unwrap().is_none());
        assert!(!accounts.remove(&alice, "anthropic", "../../x").unwrap());
        assert!(matches!(
            accounts.update(&alice, "anthropic", "nope", &AccountPatch::default()),
            Err(SeraError::NotFound(_))
        ));
    }

    #[test]
    fn migration_0037_keeps_a_0036_database_working() {
        // Build a database as 0.48 left it (user_version 36), with a bare
        // provider credential, then open it with this build.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("serabot.db");
        let alice;
        {
            let store = Store::open(&path, 384, 5000).unwrap();
            alice = member(&store, "microsoft:acct-migrate");
            let secrets =
                UserSecrets::with_master_key(store.clone(), [9u8; crate::user_secrets::KEY_BYTES])
                    .unwrap();
            secrets
                .set(
                    &alice,
                    "mistral",
                    &ProviderCredential {
                        api_key: "sk-before-synthetic".into(),
                        ..Default::default()
                    }
                    .encode()
                    .unwrap(),
                )
                .unwrap();
            let connection = store.connection().unwrap();
            connection
                .execute_batch(
                    "DROP TABLE provider_holds; DROP TABLE route_events; DROP TABLE provider_accounts;
                     PRAGMA user_version = 36;",
                )
                .unwrap();
        }
        let store = Store::open(&path, 384, 5000).unwrap();
        let version: i64 = store
            .connection()
            .unwrap()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert!(version >= 37);
        let secrets = Arc::new(
            UserSecrets::with_master_key(store.clone(), [9u8; crate::user_secrets::KEY_BYTES])
                .unwrap(),
        );
        let accounts = Accounts::new(store.clone(), secrets.clone());
        // The old credential still resolves, lists as the default account, and
        // new accounts can be added beside it.
        assert!(
            secrets
                .resolve(&alice, "mistral")
                .unwrap()
                .unwrap()
                .expose_secret()
                .contains("sk-before-synthetic")
        );
        let listed = accounts.list(&alice, "mistral").unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].is_default());
        accounts
            .add_api_key(&alice, "mistral", "Second", "sk-after-synthetic")
            .unwrap();
        assert_eq!(accounts.list(&alice, "mistral").unwrap().len(), 2);
    }

    #[test]
    fn usable_account_ids_skip_disabled_and_other_users() {
        let (store, secrets, accounts) = setup();
        let alice = member(&store, "microsoft:acct-ids-a");
        let bob = member(&store, "microsoft:acct-ids-b");
        let first = accounts
            .add_api_key(&alice, "anthropic", "One", "sk-ids-one")
            .unwrap();
        let second = accounts
            .add_api_key(&alice, "anthropic", "Two", "sk-ids-two")
            .unwrap();
        accounts
            .add_api_key(&bob, "anthropic", "Bob", "sk-ids-bob")
            .unwrap();
        accounts
            .update(
                &alice,
                "anthropic",
                &second.account_id,
                &AccountPatch {
                    priority: Some(9),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            store.usable_account_ids(&alice, "anthropic").unwrap(),
            [second.account_id.clone(), first.account_id.clone()]
        );
        accounts
            .update(
                &alice,
                "anthropic",
                &second.account_id,
                &AccountPatch {
                    disabled: Some(true),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            store.usable_account_ids(&alice, "anthropic").unwrap(),
            [first.account_id]
        );
        // The legacy credential is listed as `default`, after real accounts.
        secrets
            .set(
                &alice,
                "mistral",
                &ProviderCredential {
                    api_key: "k".into(),
                    ..Default::default()
                }
                .encode()
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            store.usable_account_ids(&alice, "mistral").unwrap(),
            [DEFAULT_ACCOUNT]
        );
        assert!(
            store
                .usable_account_ids(&bob, "mistral")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn key_fingerprints_are_keyed_and_scoped() {
        let (store, secrets, _accounts) = setup();
        let alice = member(&store, "microsoft:acct-fp-a");
        let bob = member(&store, "microsoft:acct-fp-b");
        let a = secrets.key_fingerprint(&alice, "anthropic", "sk-same");
        assert_eq!(a, secrets.key_fingerprint(&alice, "anthropic", "sk-same"));
        assert_ne!(a, secrets.key_fingerprint(&bob, "anthropic", "sk-same"));
        assert_ne!(a, secrets.key_fingerprint(&alice, "openai", "sk-same"));
        assert!(!a.contains("sk-same"));
        // A different master key gives a different fingerprint.
        let other = UserSecrets::with_master_key(store.clone(), [1u8; 32]).unwrap();
        assert_ne!(a, other.key_fingerprint(&alice, "anthropic", "sk-same"));
        // An identity fingerprint is stable and provider-scoped.
        assert_eq!(
            identity_fingerprint("p", "id"),
            identity_fingerprint("p", "id")
        );
        assert_ne!(
            identity_fingerprint("p", "id"),
            identity_fingerprint("q", "id")
        );
    }
}
