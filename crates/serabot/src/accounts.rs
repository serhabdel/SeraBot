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

    /// Cache a quota reading for one account. False when the acting user has
    /// no such account row.
    pub fn set_provider_account_quota(
        &self,
        user_id: &str,
        provider: &str,
        account_id: &str,
        quota: &Value,
    ) -> Result<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE provider_accounts SET quota_json = ?4, updated_at = ?5
             WHERE user_id = ?1 AND provider = ?2 AND account_id = ?3",
            params![
                user_id,
                provider,
                account_id,
                serde_json::to_string(quota)?,
                now_string()
            ],
        )?;
        Ok(changed == 1)
    }

    /// Rotate an account's Cloud Code session, once. `seen` is the generation
    /// the caller's request used: when the row already moved past it (a sibling
    /// request rotated first) nothing changes. Returns the generation to use.
    pub fn bump_session_generation(
        &self,
        user_id: &str,
        provider: &str,
        account_id: &str,
        seen: i64,
    ) -> Result<i64> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE provider_accounts
             SET session_generation = session_generation + 1, updated_at = ?5
             WHERE user_id = ?1 AND provider = ?2 AND account_id = ?3
               AND session_generation = ?4",
            params![user_id, provider, account_id, seen, now_string()],
        )?;
        let current: Option<i64> = transaction
            .query_row(
                "SELECT session_generation FROM provider_accounts
                 WHERE user_id = ?1 AND provider = ?2 AND account_id = ?3",
                params![user_id, provider, account_id],
                |row| row.get(0),
            )
            .optional()?;
        transaction.commit()?;
        Ok(current.unwrap_or(seen + 1))
    }

    /// Merge `patch` (a small JSON object) into an account's non-secret
    /// identity. False when the acting user has no such account.
    pub fn merge_provider_account_identity(
        &self,
        user_id: &str,
        provider: &str,
        account_id: &str,
        patch: &Value,
    ) -> Result<bool> {
        let Some(patch) = patch.as_object() else {
            return Ok(false);
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<String> = transaction
            .query_row(
                "SELECT identity_json FROM provider_accounts
                 WHERE user_id = ?1 AND provider = ?2 AND account_id = ?3",
                params![user_id, provider, account_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(current) = current else {
            return Ok(false);
        };
        let mut identity: Value = serde_json::from_str(&current).unwrap_or_else(|_| json!({}));
        if let Some(map) = identity.as_object_mut() {
            for (key, value) in patch {
                map.insert(key.clone(), value.clone());
            }
        }
        let merged = clean_identity(&identity)?;
        transaction.execute(
            "UPDATE provider_accounts SET identity_json = ?4, updated_at = ?5
             WHERE user_id = ?1 AND provider = ?2 AND account_id = ?3",
            params![
                user_id,
                provider,
                account_id,
                serde_json::to_string(&merged)?,
                now_string()
            ],
        )?;
        transaction.commit()?;
        Ok(true)
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

    pub fn store(&self) -> &Store {
        &self.store
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

impl Accounts {
    /// The keyed fingerprint of a pasted API key (see
    /// [`UserSecrets::key_fingerprint`]).
    pub fn key_fingerprint(&self, user_id: &str, provider: &str, key: &str) -> String {
        self.secrets.key_fingerprint(user_id, provider, key)
    }

    /// Replace an account's credential only while it still matches `expected`.
    ///
    /// The read, compare and write run under the secret store's write lock, so
    /// a refresh that started from an older copy cannot overwrite a newer
    /// sign-in or an earlier refresh. Returns false (and writes nothing) when
    /// the stored credential has moved on or the account is gone.
    pub fn swap_credential(
        &self,
        user_id: &str,
        provider: &str,
        account_id: &str,
        expected: &CredentialStamp,
        next: &ProviderCredential,
    ) -> Result<bool> {
        next.encode()?;
        let change = |credential: &mut ProviderCredential| {
            if &CredentialStamp::of(credential) != expected {
                return Err(SeraError::InvalidState(STALE_CREDENTIAL.into()));
            }
            *credential = next.clone();
            Ok(())
        };
        let result = if account_id == DEFAULT_ACCOUNT {
            self.secrets.update_provider(user_id, provider, change)
        } else {
            if !valid_account_id(account_id) {
                return Ok(false);
            }
            self.secrets
                .update_account(user_id, provider, account_id, change)
        };
        match result {
            Ok(()) => Ok(true),
            Err(SeraError::InvalidState(message)) if message == STALE_CREDENTIAL => Ok(false),
            Err(SeraError::NotFound(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

const STALE_CREDENTIAL: &str = "the stored credential changed";

/// What a caller knew about an account's credential when its request failed.
/// Two stamps are equal only when the access token, the refresh token and the
/// generation all match, so a credential that someone else already refreshed
/// compares unequal. Holds secrets; `Debug` redacts them.
#[derive(Clone, PartialEq, Eq)]
pub struct CredentialStamp {
    generation: Option<String>,
    refresh_token: String,
    access_token: String,
}

impl CredentialStamp {
    pub fn of(credential: &ProviderCredential) -> Self {
        Self {
            generation: credential.generation.clone(),
            refresh_token: credential.refresh_token.clone(),
            access_token: credential.api_key.clone(),
        }
    }

    /// A stamp from loose parts, for a client that tracks them separately.
    pub fn new(generation: Option<String>, refresh_token: &str, access_token: &str) -> Self {
        Self {
            generation,
            refresh_token: refresh_token.to_owned(),
            access_token: access_token.to_owned(),
        }
    }
}

impl std::fmt::Debug for CredentialStamp {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CredentialStamp")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// Refresh an access token this many seconds before it expires.
pub const REFRESH_SKEW_SECS: i64 = 300;

/// What a successful token exchange returned.
pub struct RefreshGrant {
    pub access_token: String,
    /// Set when the provider rotated the refresh token.
    pub refresh_token: Option<String>,
    /// Unix seconds when `access_token` stops working, when the provider said.
    pub expires_at: Option<i64>,
}

impl std::fmt::Debug for RefreshGrant {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RefreshGrant")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// Why a token exchange failed.
#[derive(Debug)]
pub enum RefreshError {
    /// The provider refused the refresh token (`invalid_grant`, a revoked or
    /// expired sign-in). Retrying cannot help; the member must sign in again.
    InvalidGrant,
    /// Anything else (network, 5xx). The credential is untouched.
    Failed(SeraError),
}

impl RefreshError {
    pub fn into_error(self) -> SeraError {
        match self {
            Self::InvalidGrant => needs_reauth_error(),
            Self::Failed(error) => error,
        }
    }
}

/// The error a refresh returns once the account is parked in `needs_reauth`.
pub fn needs_reauth_error() -> SeraError {
    SeraError::Rejected {
        code: "needs_reauth",
        message: "This account's sign-in expired. Sign in again.".into(),
    }
}

/// The result of asking for a fresh credential.
#[derive(Debug)]
pub struct Refreshed {
    /// The credential to use now.
    pub credential: ProviderCredential,
    /// True when this call ran the exchange. False when another caller had
    /// already refreshed (or a newer sign-in replaced the credential) and this
    /// call adopted what was stored.
    pub performed: bool,
}

type LockKey = (String, String, String);

/// Single-flight token refresh, one lock per `(user, provider, account)`.
///
/// Rotating refresh tokens make a race destructive: two turns that refresh the
/// same account at once spend the old token twice, and the second spend gets
/// `invalid_grant`. Every refresh therefore takes the account's lock, re-reads
/// the stored credential and compares it with what the caller saw. If it
/// changed, someone else already refreshed and the caller adopts the stored
/// value. If not, the lock holder runs the exchange and writes the result back
/// (compare-and-swap) before releasing, so the next waiter reads it.
pub struct AccountRefresher {
    accounts: Arc<Accounts>,
    locks: std::sync::Mutex<std::collections::HashMap<LockKey, Arc<tokio::sync::Mutex<()>>>>,
}

impl AccountRefresher {
    pub fn new(accounts: Arc<Accounts>) -> Self {
        Self {
            accounts,
            locks: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    pub fn accounts(&self) -> &Arc<Accounts> {
        &self.accounts
    }

    fn lock_for(
        &self,
        user_id: &str,
        provider: &str,
        account_id: &str,
    ) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self
            .locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if locks.len() >= 256 {
            // Only entries nobody holds or waits on can go.
            locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        }
        locks
            .entry((
                user_id.to_owned(),
                provider.to_owned(),
                account_id.to_owned(),
            ))
            .or_default()
            .clone()
    }

    /// Whether `credential`'s access token expires within the refresh skew.
    /// Unknown expiry is not "near": the first rejected request renews it.
    pub fn is_near_expiry(credential: &ProviderCredential, now: i64) -> bool {
        !credential.refresh_token.is_empty()
            && credential
                .expires_at
                .is_some_and(|at| at - now <= REFRESH_SKEW_SECS)
    }

    /// Refresh one account's credential, or adopt a refresh someone else did.
    ///
    /// `seen` is what the caller held when its request was rejected.
    /// `exchange` gets the stored refresh token and runs the provider call; it
    /// runs at most once and only while the lock is held.
    pub async fn refresh<F, Fut>(
        &self,
        user_id: &str,
        provider: &str,
        account_id: &str,
        seen: &CredentialStamp,
        exchange: F,
    ) -> Result<Refreshed>
    where
        F: FnOnce(String) -> Fut,
        Fut: std::future::Future<Output = std::result::Result<RefreshGrant, RefreshError>>,
    {
        let lock = self.lock_for(user_id, provider, account_id);
        let _guard = lock.lock().await;
        let current = self
            .accounts
            .credential(user_id, provider, account_id)?
            .ok_or_else(|| SeraError::NotFound("account".into()))?;
        if &CredentialStamp::of(&current) != seen {
            return Ok(Refreshed {
                credential: current,
                performed: false,
            });
        }
        if current.refresh_token.is_empty() {
            return Err(self.park(user_id, provider, account_id, "no refresh token"));
        }
        let grant = match exchange(current.refresh_token.clone()).await {
            Ok(grant) => grant,
            Err(RefreshError::InvalidGrant) => {
                return Err(self.park(user_id, provider, account_id, "refresh token rejected"));
            }
            Err(RefreshError::Failed(error)) => return Err(error),
        };
        let mut next = current.clone();
        next.api_key = grant.access_token;
        if let Some(rotated) = grant.refresh_token.filter(|token| !token.is_empty()) {
            next.refresh_token = rotated;
        }
        next.expires_at = grant.expires_at;
        next.generation = Some(new_generation());
        if !self.accounts.swap_credential(
            user_id,
            provider,
            account_id,
            &CredentialStamp::of(&current),
            &next,
        )? {
            // A new sign-in landed while the exchange ran. It wins; what the
            // exchange returned belonged to the old chain.
            let latest = self
                .accounts
                .credential(user_id, provider, account_id)?
                .ok_or_else(|| SeraError::NotFound("account".into()))?;
            return Ok(Refreshed {
                credential: latest,
                performed: false,
            });
        }
        // A refresh that worked proves the sign-in is good again.
        if let Some(account) = self.accounts.get(user_id, provider, account_id)?
            && account.status == AccountStatus::NeedsReauth
        {
            self.accounts.set_status(
                user_id,
                provider,
                account_id,
                AccountStatus::Ok,
                None,
                None,
            )?;
        }
        Ok(Refreshed {
            credential: next,
            performed: true,
        })
    }

    /// Refresh ahead of expiry: when the stored access token expires within
    /// [`REFRESH_SKEW_SECS`], run [`Self::refresh`] against what is stored now.
    /// Returns the credential to use.
    pub async fn ensure_fresh<F, Fut>(
        &self,
        user_id: &str,
        provider: &str,
        account_id: &str,
        now: i64,
        exchange: F,
    ) -> Result<ProviderCredential>
    where
        F: FnOnce(String) -> Fut,
        Fut: std::future::Future<Output = std::result::Result<RefreshGrant, RefreshError>>,
    {
        let stored = self
            .accounts
            .credential(user_id, provider, account_id)?
            .ok_or_else(|| SeraError::NotFound("account".into()))?;
        if !Self::is_near_expiry(&stored, now) {
            return Ok(stored);
        }
        Ok(self
            .refresh(
                user_id,
                provider,
                account_id,
                &CredentialStamp::of(&stored),
                exchange,
            )
            .await?
            .credential)
    }

    /// Park an account whose sign-in cannot be renewed. The row and its
    /// sealed credential stay, so the audit trail and the label survive and
    /// the member can sign in again over it.
    fn park(&self, user_id: &str, provider: &str, account_id: &str, reason: &str) -> SeraError {
        if let Err(error) = self.accounts.set_status(
            user_id,
            provider,
            account_id,
            AccountStatus::NeedsReauth,
            Some(reason),
            None,
        ) {
            tracing::warn!(%error, "could not mark an account as needing sign-in");
        }
        if let Err(error) = self.accounts.store.record_audit(
            user_id,
            "provider.account_needs_reauth",
            None,
            None,
            None,
            None,
            None,
            Some(&json!({"provider": provider, "account": account_id})),
        ) {
            tracing::warn!(%error, "could not audit an account needing sign-in");
        }
        needs_reauth_error()
    }
}

fn new_generation() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 8];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
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

    // ---- single-flight refresh (P11) ----

    use crate::domain::LlmMessage;
    use crate::llm::{AccountRefreshTarget, LlmClient};
    use crate::providers::{OAuthMaterial, OAuthStyle};
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn refresher() -> (Store, Arc<Accounts>, Arc<AccountRefresher>) {
        let (store, secrets, _) = setup();
        let accounts = Arc::new(Accounts::new(store.clone(), secrets));
        let refresher = Arc::new(AccountRefresher::new(accounts.clone()));
        (store, accounts, refresher)
    }

    fn sign_in(accounts: &Accounts, user: &str, expires_at: Option<i64>) -> String {
        let mut request = oauth("google-antigravity", "p@example.example", "old-access");
        request.credential.refresh_token = "refresh-old".into();
        request.credential.expires_at = expires_at;
        accounts.add(user, request).unwrap().account_id
    }

    fn chat_ok() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        }))
    }

    /// A mock identity provider: the old refresh token yields a rotated pair
    /// (slowly, so concurrent turns really overlap); the chat endpoint rejects
    /// the old access token and accepts the new one.
    async fn idp() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("refresh_token=refresh-old"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(std::time::Duration::from_millis(150))
                    .set_body_json(json!({
                        "access_token": "new-access",
                        "refresh_token": "refresh-new",
                        "expires_in": 3600
                    })),
            )
            .mount(&server)
            .await;
        // A second spend of the old token is the failure single-flight prevents.
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("refresh_token=refresh-new"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer old-access"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer new-access"))
            .respond_with(chat_ok())
            .mount(&server)
            .await;
        server
    }

    fn account_client(
        server: &MockServer,
        refresher: &Arc<AccountRefresher>,
        user: &str,
        account: &str,
        expires_at: Option<i64>,
    ) -> LlmClient {
        LlmClient::new(server.uri(), "old-access", 10, 64)
            .unwrap()
            .with_model("synthetic-model")
            .with_oauth_unpinned(OAuthMaterial {
                style: OAuthStyle::RefreshToken,
                refresh_token: "refresh-old".into(),
                token_url: format!("{}/token", server.uri()),
                client_id: "client".into(),
                client_secret: "secret".into(),
            })
            .unwrap()
            .with_account_refresh(
                AccountRefreshTarget {
                    refresher: refresher.clone(),
                    user_id: user.into(),
                    provider: "google-antigravity".into(),
                    account_id: account.into(),
                },
                None,
                expires_at,
            )
            .unwrap()
    }

    async fn token_calls(server: &MockServer) -> usize {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.path() == "/token")
            .count()
    }

    #[tokio::test]
    async fn eight_concurrent_turns_on_one_client_refresh_exactly_once() {
        let (store, accounts, refresher) = refresher();
        let alice = member(&store, "microsoft:refresh-one");
        let account = sign_in(&accounts, &alice, None);
        let server = idp().await;
        let client = account_client(&server, &refresher, &alice, &account, None);
        let mut turns = Vec::new();
        for _ in 0..8 {
            let client = client.clone();
            turns.push(tokio::spawn(async move {
                client
                    .complete(&[LlmMessage::user("hello")], &[], None, None)
                    .await
            }));
        }
        for turn in turns {
            assert_eq!(
                turn.await.unwrap().unwrap().message.content.as_deref(),
                Some("hi")
            );
        }
        assert_eq!(token_calls(&server).await, 1);
        // The rotated pair and a new generation were written back.
        let stored = accounts
            .credential(&alice, "google-antigravity", &account)
            .unwrap()
            .unwrap();
        assert_eq!(stored.api_key, "new-access");
        assert_eq!(stored.refresh_token, "refresh-new");
        assert!(stored.generation.is_some());
        assert!(stored.expires_at.is_some());
    }

    #[tokio::test]
    async fn eight_separate_clients_for_one_account_refresh_exactly_once() {
        // Each turn can hold its own client built from the same stale copy.
        let (store, accounts, refresher) = refresher();
        let alice = member(&store, "microsoft:refresh-eight");
        let account = sign_in(&accounts, &alice, None);
        let server = idp().await;
        let mut turns = Vec::new();
        for _ in 0..8 {
            let client = account_client(&server, &refresher, &alice, &account, None);
            turns.push(tokio::spawn(async move {
                client
                    .complete(&[LlmMessage::user("hello")], &[], None, None)
                    .await
            }));
        }
        for turn in turns {
            assert!(turn.await.unwrap().is_ok());
        }
        assert_eq!(token_calls(&server).await, 1);
    }

    #[tokio::test]
    async fn a_stale_write_is_refused_and_changes_nothing() {
        let (store, accounts, _refresher) = refresher();
        let alice = member(&store, "microsoft:refresh-stale");
        let account = sign_in(&accounts, &alice, None);
        let before = accounts
            .credential(&alice, "google-antigravity", &account)
            .unwrap()
            .unwrap();
        let old = CredentialStamp::of(&before);
        let mut newer = before.clone();
        newer.api_key = "newer-access".into();
        newer.generation = Some("g1".into());
        assert!(
            accounts
                .swap_credential(&alice, "google-antigravity", &account, &old, &newer)
                .unwrap()
        );
        // A writer still holding the first copy is refused.
        let mut late = before.clone();
        late.api_key = "late-access".into();
        late.generation = Some("g2".into());
        assert!(
            !accounts
                .swap_credential(&alice, "google-antigravity", &account, &old, &late)
                .unwrap()
        );
        let stored = accounts
            .credential(&alice, "google-antigravity", &account)
            .unwrap()
            .unwrap();
        assert_eq!(stored.api_key, "newer-access");
        assert_eq!(stored.generation.as_deref(), Some("g1"));
        // Another user's id cannot be swapped either.
        let bob = member(&store, "microsoft:refresh-stale-bob");
        assert!(
            !accounts
                .swap_credential(
                    &bob,
                    "google-antigravity",
                    &account,
                    &CredentialStamp::of(&newer),
                    &late
                )
                .unwrap()
        );
    }

    #[tokio::test]
    async fn a_caller_that_lost_the_race_adopts_the_stored_credential() {
        let (store, accounts, refresher) = refresher();
        let alice = member(&store, "microsoft:refresh-adopt");
        let account = sign_in(&accounts, &alice, None);
        let seen = CredentialStamp::of(
            &accounts
                .credential(&alice, "google-antigravity", &account)
                .unwrap()
                .unwrap(),
        );
        let first = refresher
            .refresh(
                &alice,
                "google-antigravity",
                &account,
                &seen,
                |refresh| async move {
                    assert_eq!(refresh, "refresh-old");
                    Ok(RefreshGrant {
                        access_token: "a2".into(),
                        refresh_token: Some("r2".into()),
                        expires_at: Some(4_000_000_000),
                    })
                },
            )
            .await
            .unwrap();
        assert!(first.performed);
        // The second caller still holds the old stamp: no exchange runs.
        let second = refresher
            .refresh(&alice, "google-antigravity", &account, &seen, |_| async {
                panic!("the exchange must not run twice");
            })
            .await
            .unwrap();
        assert!(!second.performed);
        assert_eq!(second.credential.api_key, "a2");
        assert_eq!(second.credential.refresh_token, "r2");
    }

    #[tokio::test]
    async fn a_new_sign_in_during_the_exchange_wins() {
        let (store, accounts, refresher) = refresher();
        let alice = member(&store, "microsoft:refresh-signin");
        let account = sign_in(&accounts, &alice, None);
        let seen = CredentialStamp::of(
            &accounts
                .credential(&alice, "google-antigravity", &account)
                .unwrap()
                .unwrap(),
        );
        let during = accounts.clone();
        let user = alice.clone();
        let outcome = refresher
            .refresh(
                &alice,
                "google-antigravity",
                &account,
                &seen,
                move |_| async move {
                    // The member signs in again while the exchange is in flight.
                    let mut again =
                        oauth("google-antigravity", "p@example.example", "fresh-signin");
                    again.credential.refresh_token = "fresh-refresh".into();
                    during.add(&user, again).unwrap();
                    Ok(RefreshGrant {
                        access_token: "from-old-chain".into(),
                        refresh_token: Some("old-chain-rotated".into()),
                        expires_at: None,
                    })
                },
            )
            .await
            .unwrap();
        assert!(!outcome.performed);
        assert_eq!(outcome.credential.api_key, "fresh-signin");
        let stored = accounts
            .credential(&alice, "google-antigravity", &account)
            .unwrap()
            .unwrap();
        assert_eq!(stored.api_key, "fresh-signin");
        assert_eq!(stored.refresh_token, "fresh-refresh");
    }

    #[tokio::test]
    async fn invalid_grant_parks_the_account_and_keeps_the_row() {
        let (store, accounts, refresher) = refresher();
        let alice = member(&store, "microsoft:refresh-grant");
        let account = sign_in(&accounts, &alice, None);
        let seen = CredentialStamp::of(
            &accounts
                .credential(&alice, "google-antigravity", &account)
                .unwrap()
                .unwrap(),
        );
        let error = refresher
            .refresh(&alice, "google-antigravity", &account, &seen, |_| async {
                Err(RefreshError::InvalidGrant)
            })
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            SeraError::Rejected {
                code: "needs_reauth",
                ..
            }
        ));
        let row = accounts
            .get(&alice, "google-antigravity", &account)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, AccountStatus::NeedsReauth);
        assert!(row.last_error.is_some());
        // The sealed credential is still there for the audit trail, and signing
        // in again over it makes the account usable.
        assert!(
            accounts
                .credential(&alice, "google-antigravity", &account)
                .unwrap()
                .is_some()
        );
        let again = accounts
            .add(
                &alice,
                oauth("google-antigravity", "p@example.example", "back"),
            )
            .unwrap();
        assert_eq!(again.account_id, account);
        assert_eq!(
            accounts
                .get(&alice, "google-antigravity", &account)
                .unwrap()
                .unwrap()
                .status,
            AccountStatus::Ok
        );
        // A transient failure changes nothing.
        let seen = CredentialStamp::of(
            &accounts
                .credential(&alice, "google-antigravity", &account)
                .unwrap()
                .unwrap(),
        );
        let error = refresher
            .refresh(&alice, "google-antigravity", &account, &seen, |_| async {
                Err(RefreshError::Failed(SeraError::External("down".into())))
            })
            .await
            .unwrap_err();
        assert!(matches!(error, SeraError::External(_)));
        assert_eq!(
            accounts
                .get(&alice, "google-antigravity", &account)
                .unwrap()
                .unwrap()
                .status,
            AccountStatus::Ok
        );
    }

    #[tokio::test]
    async fn invalid_grant_from_the_token_endpoint_marks_needs_reauth() {
        let (store, accounts, refresher) = refresher();
        let alice = member(&store, "microsoft:refresh-endpoint");
        let account = sign_in(&accounts, &alice, None);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let client = account_client(&server, &refresher, &alice, &account, None);
        let error = client
            .complete(&[LlmMessage::user("hello")], &[], None, None)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            SeraError::Rejected {
                code: "needs_reauth",
                ..
            }
        ));
        assert_eq!(
            accounts
                .get(&alice, "google-antigravity", &account)
                .unwrap()
                .unwrap()
                .status,
            AccountStatus::NeedsReauth
        );
        assert!(!error.to_string().contains("refresh-old"));
    }

    #[tokio::test]
    async fn a_token_near_expiry_is_refreshed_before_the_request() {
        let (store, accounts, refresher) = refresher();
        let alice = member(&store, "microsoft:refresh-early");
        let soon = chrono::Utc::now().timestamp() + 120;
        let account = sign_in(&accounts, &alice, Some(soon));
        let server = idp().await;
        let client = account_client(&server, &refresher, &alice, &account, Some(soon));
        client
            .complete(&[LlmMessage::user("hello")], &[], None, None)
            .await
            .unwrap();
        assert_eq!(token_calls(&server).await, 1);
        // The old token was never sent: the chat request used the new one.
        let chats = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|request| request.url.path() == "/chat/completions")
            .count();
        assert_eq!(chats, 1);

        // A token with time left is left alone.
        let (store, accounts, refresher) = self::refresher();
        let bob = member(&store, "microsoft:refresh-late");
        let later = chrono::Utc::now().timestamp() + 3600;
        let account = sign_in(&accounts, &bob, Some(later));
        let calm = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer old-access"))
            .respond_with(chat_ok())
            .mount(&calm)
            .await;
        let client = account_client(&calm, &refresher, &bob, &account, Some(later));
        client
            .complete(&[LlmMessage::user("hello")], &[], None, None)
            .await
            .unwrap();
        assert_eq!(token_calls(&calm).await, 0);
        assert!(AccountRefresher::is_near_expiry(
            &ProviderCredential {
                api_key: "a".into(),
                refresh_token: "r".into(),
                expires_at: Some(100),
                ..Default::default()
            },
            100 - REFRESH_SKEW_SECS
        ));
        // No refresh token, or an unknown expiry, is never "near".
        assert!(!AccountRefresher::is_near_expiry(
            &ProviderCredential {
                api_key: "a".into(),
                ..Default::default()
            },
            0
        ));
    }

    #[tokio::test]
    async fn refreshing_another_users_account_finds_nothing() {
        let (store, accounts, refresher) = refresher();
        let alice = member(&store, "microsoft:refresh-iso-a");
        let bob = member(&store, "microsoft:refresh-iso-b");
        let account = sign_in(&accounts, &alice, None);
        let seen = CredentialStamp::new(None, "refresh-old", "old-access");
        let error = refresher
            .refresh(&bob, "google-antigravity", &account, &seen, |_| async {
                panic!("must not run for another user's account");
            })
            .await
            .unwrap_err();
        assert!(matches!(error, SeraError::NotFound(_)));
    }
}
