//! Per-user credential store: envelope encryption over SQLite.
//!
//! Phase 4 of the multi-user reconstruction splits secrets into two tiers.
//! The age store (`secrets.rs`) keeps **server-level** secrets — the Telegram
//! bot token, the auth/OneDrive app registration client id, the existing
//! global connectors. Everything a user owns — OneDrive refresh tokens, AI
//! provider API keys and OAuth tokens — lives here, sealed per user.
//!
//! # Key hierarchy
//!
//! ```text
//! private/master.key   32 random bytes, 0600 on unix, generated on first run
//!   └─ wraps  user_keys.wrapped_dek   32 random bytes per user, sealed with
//!        └─ seals  user_credentials.ciphertext   ChaCha20-Poly1305
//! ```
//!
//! The master key is **not** the age identity and never leaves the process:
//! no API returns it, no log line prints it, and the `Debug` impls redact it.
//! A per-user DEK is generated on that user's first write and wrapped with a
//! fresh 12-byte nonce; every credential write reuses the DEK but takes its
//! own fresh nonce.
//!
//! # Associated data
//!
//! Both layers bind their ciphertext with AEAD associated data:
//! `dek|<user_id>` for the wrapped DEK and `credential|<user_id>|<provider>`
//! for a credential. A blob copied to another user or another provider fails
//! to decrypt instead of silently opening — the module's tests prove it.
//!
//! # Permissions
//!
//! Every method takes the acting user's id. `get`/`set`/`delete` address only
//! that user's rows. `resolve` additionally follows an **active admin grant**
//! (`provider_grants`), reading the grantor's credential at call time; the
//! grantor's identity is never returned to the caller, and only the admin can
//! be a grantor. There is no method that lists or decrypts every user's
//! credentials, so the admin cannot read a member's secret.
//!
//! # Failure semantics
//!
//! Fail closed. A missing row is `None` (the caller decides what that means);
//! a corrupt blob, a wrong-length key, a non-UTF-8 plaintext, or a failed
//! AEAD open is an explicit error — never a silent empty value. A grant whose
//! grantor credential has disappeared is an error, never a fallback. Rotation
//! write-back goes to the credential's **owner** (the grantor for a granted
//! provider), so a rotated token is never written into the wrong account.

use std::path::Path;
use std::sync::Arc;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::RngCore;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::config::Config;
use crate::error::{Result, SeraError};
use crate::secrets::{CredentialWriter, restrict_permissions};
use crate::store::{CredentialStatus, ProviderGrant, Store, StoredUserKey};

/// Master key length in bytes (ChaCha20-Poly1305 key size).
pub const KEY_BYTES: usize = 32;
/// AEAD nonce length in bytes.
pub const NONCE_BYTES: usize = 12;
/// Longest provider id accepted by the store.
pub const MAX_PROVIDER_CHARS: usize = 64;
/// Hard cap on one credential value, matching the age store's limit.
pub const MAX_VALUE_BYTES: usize = 16 * 1024;

/// The per-user credential store. Cheap to clone behind an `Arc`; the master
/// key stays in memory for the process lifetime.
pub struct UserSecrets {
    store: Store,
    master_key: Zeroizing<[u8; KEY_BYTES]>,
    /// The bootstrap admin id: the only account allowed to grant a provider.
    admin_id: String,
    /// Serializes writes, so a read-merge-write (a token rotation) and a
    /// plain write (a new sign-in) cannot interleave and lose one another.
    writes: std::sync::Mutex<()>,
}

impl std::fmt::Debug for UserSecrets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UserSecrets")
            .field("store", &self.store)
            .field("admin_id", &self.admin_id)
            .field("master_key", &"<redacted>")
            .finish()
    }
}

impl UserSecrets {
    /// Open the store, generating `private/master.key` on first run.
    pub fn open(config: &Config, store: Store) -> Result<Self> {
        Self::open_path(store, config.path(&config.secrets.master_key_file))
    }

    /// Open the store with an explicit master-key path (tests, tooling).
    pub fn open_path(store: Store, path: impl AsRef<Path>) -> Result<Self> {
        let master_key = load_or_create_master_key(path.as_ref())?;
        Self::with_master_key(store, master_key)
    }

    /// Build from an in-memory master key. The key is copied into a
    /// zeroizing buffer; the caller's copy is its own responsibility.
    pub fn with_master_key(store: Store, master_key: [u8; KEY_BYTES]) -> Result<Self> {
        let admin_id = store.admin_user_id()?;
        Ok(Self {
            store,
            master_key: Zeroizing::new(master_key),
            admin_id,
            writes: std::sync::Mutex::new(()),
        })
    }

    /// Seal and store `value` as the acting user's credential for `provider`.
    ///
    /// Creates the user's DEK on first write. A fresh nonce is used for every
    /// write, so identical values produce different ciphertexts.
    pub fn set(&self, user_id: &str, provider: &str, value: &str) -> Result<()> {
        validate_provider(provider)?;
        validate_value(value)?;
        let _write = self.lock_writes();
        self.seal_and_store(user_id, provider, value)
    }

    /// Read, change and write back one JSON provider credential under the
    /// write lock. `change` returns an error to leave the row as it is.
    pub(crate) fn update_provider(
        &self,
        user_id: &str,
        provider: &str,
        change: impl FnOnce(&mut ProviderCredential) -> Result<()>,
    ) -> Result<()> {
        validate_provider(provider)?;
        let _write = self.lock_writes();
        let secret = self
            .get_unchecked(user_id, provider)?
            .ok_or_else(|| SeraError::Secrets("rotated credential was not persisted".into()))?;
        let mut credential = ProviderCredential::parse(&secret)?;
        change(&mut credential)?;
        self.seal_and_store(user_id, provider, &credential.encode()?)
    }

    fn lock_writes(&self) -> std::sync::MutexGuard<'_, ()> {
        self.writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The store behind this credential vault, for the vault's metadata rows.
    pub(crate) fn store(&self) -> &Store {
        &self.store
    }

    /// Seal a vault value under provider id `vault:<name>`. Vault values may
    /// hold tabs and newlines (free-text notes), so they use the vault's own
    /// value rules instead of [`validate_value`]; the provider-id check is
    /// the vault's too, which keeps `vault:` ids out of the public API's
    /// provider namespace.
    pub(crate) fn set_vault(&self, user_id: &str, name: &str, value: &str) -> Result<()> {
        let provider = vault_provider_id(name)?;
        crate::vault::validate_value(value)?;
        self.seal_and_store(user_id, &provider, value)
    }

    /// The acting user's vault value for `name`, if any.
    pub(crate) fn get_vault(&self, user_id: &str, name: &str) -> Result<Option<SecretString>> {
        let provider = vault_provider_id(name)?;
        self.get_unchecked(user_id, &provider)
    }

    /// Delete the acting user's vault value for `name`.
    pub(crate) fn delete_vault(&self, user_id: &str, name: &str) -> Result<bool> {
        let provider = vault_provider_id(name)?;
        self.store.delete_user_credential(user_id, &provider)
    }

    /// Seal a password manager's access token for the acting user. The id is
    /// one of the vault's own provider ids (`vaultprov:<id>`), so it is never
    /// reachable through the public credential API.
    pub(crate) fn set_vault_provider_token(
        &self,
        user_id: &str,
        provider_id: &str,
        token: &str,
    ) -> Result<()> {
        validate_value(token)?;
        self.seal_and_store(user_id, &vault_provider_token_id(provider_id), token)
    }

    /// The acting user's access token for a password manager, if one is set.
    pub(crate) fn vault_provider_token(
        &self,
        user_id: &str,
        provider_id: &str,
    ) -> Result<Option<SecretString>> {
        self.get_unchecked(user_id, &vault_provider_token_id(provider_id))
    }

    /// Delete the acting user's access token for a password manager.
    pub(crate) fn delete_vault_provider_token(
        &self,
        user_id: &str,
        provider_id: &str,
    ) -> Result<bool> {
        self.store
            .delete_user_credential(user_id, &vault_provider_token_id(provider_id))
    }

    fn seal_and_store(&self, user_id: &str, provider: &str, value: &str) -> Result<()> {
        if self.store.get_user(user_id)?.is_none() {
            return Err(SeraError::NotFound(format!("user {user_id}")));
        }
        let dek = self.dek(user_id)?;
        let (ciphertext, nonce) = seal(&dek, &credential_aad(user_id, provider), value.as_bytes())?;
        self.store
            .put_user_credential(user_id, provider, &ciphertext, &nonce)
    }

    /// The acting user's own credential for `provider`, if any. Never reads
    /// a grant: use [`Self::resolve`] for the routing path.
    pub fn get(&self, user_id: &str, provider: &str) -> Result<Option<SecretString>> {
        validate_provider(provider)?;
        self.get_unchecked(user_id, provider)
    }

    fn get_unchecked(&self, user_id: &str, provider: &str) -> Result<Option<SecretString>> {
        let Some(row) = self.store.user_credential(user_id, provider)? else {
            return Ok(None);
        };
        let dek = self.dek(user_id)?;
        let plaintext = open(
            &dek,
            &credential_aad(user_id, provider),
            &row.ciphertext,
            &row.nonce,
        )?;
        let value = String::from_utf8(plaintext.to_vec())
            .map_err(|_| SeraError::Secrets("stored credential is not valid UTF-8".into()))?;
        Ok(Some(SecretString::new(value)))
    }

    /// Delete the acting user's own credential for `provider`. Returns false
    /// when there is no such row; a grant is untouched.
    pub fn delete(&self, user_id: &str, provider: &str) -> Result<bool> {
        validate_provider(provider)?;
        self.store.delete_user_credential(user_id, provider)
    }

    /// Record an admin grant of `provider` to `grantee`.
    ///
    /// Only the bootstrap admin can grant, and only a provider the admin
    /// actually holds a credential for: a grant is permission to use an
    /// existing credential, never a way to conjure one.
    pub fn grant(&self, grantor: &str, grantee: &str, provider: &str) -> Result<ProviderGrant> {
        validate_provider(provider)?;
        if self.get(grantor, provider)?.is_none() {
            return Err(SeraError::Validation(
                "the admin has no credential for this provider to grant".into(),
            ));
        }
        self.store.upsert_provider_grant(grantor, grantee, provider)
    }

    /// Revoke one grant. Returns false when it was already revoked or absent.
    pub fn revoke_grant(&self, grantor: &str, grantee: &str, provider: &str) -> Result<bool> {
        validate_provider(provider)?;
        self.store.revoke_provider_grant(grantor, grantee, provider)
    }

    /// Resolve the credential the acting user may use for `provider`: their
    /// own first, then an active grant from the admin.
    ///
    /// The value is returned; the grantor's identity never is. A revoked
    /// grant, a disabled grantor row, or a missing grantor credential all
    /// fail closed — there is no fallback to another account.
    pub fn resolve(&self, user_id: &str, provider: &str) -> Result<Option<SecretString>> {
        Ok(self
            .resolve_with_owner(user_id, provider)?
            .map(|(_, secret)| secret))
    }

    /// Resolve and report which account owns the credential.
    ///
    /// Crate-internal only: the owner id exists so a rotated token is written
    /// back to the right row. It is never serialized into an API response.
    pub(crate) fn resolve_with_owner(
        &self,
        user_id: &str,
        provider: &str,
    ) -> Result<Option<(String, SecretString)>> {
        validate_provider(provider)?;
        if let Some(secret) = self.get(user_id, provider)? {
            return Ok(Some((user_id.to_owned(), secret)));
        }
        let Some(grantor) = self
            .store
            .active_grant_grantor(&self.admin_id, user_id, provider)?
        else {
            return Ok(None);
        };
        match self.get(&grantor, provider)? {
            Some(secret) => Ok(Some((grantor, secret))),
            None => Err(SeraError::InvalidState(format!(
                "the admin grant for provider '{provider}' has no credential behind it"
            ))),
        }
    }

    /// Provider names the acting user can use: their own rows plus active
    /// admin grants. Names only — never a value.
    pub fn providers_for(&self, user_id: &str) -> Result<Vec<String>> {
        let mut names = self.store.list_user_credential_providers(user_id)?;
        // Vault entries and password manager tokens are not AI providers.
        names.retain(|name| {
            !name.starts_with(VAULT_PROVIDER_PREFIX)
                && !name.starts_with(VAULT_PROVIDER_TOKEN_PREFIX)
        });
        for grant in self.store.list_active_grants_for(user_id)? {
            names.push(grant.provider);
        }
        names.sort();
        names.dedup();
        Ok(names)
    }

    /// Whether the acting user has an own credential and/or a grant for
    /// `provider`. Row existence only; nothing is decrypted.
    pub fn credential_status(&self, user_id: &str, provider: &str) -> Result<CredentialStatus> {
        validate_provider(provider)?;
        self.store.credential_status(user_id, provider)
    }

    /// The acting user's DEK, creating and wrapping it on first use.
    ///
    /// A concurrent first write is safe: the insert is `DO NOTHING` and the
    /// winner's row is re-read, so every writer ends up with the same DEK.
    fn dek(&self, user_id: &str) -> Result<Zeroizing<[u8; KEY_BYTES]>> {
        if let Some(row) = self.store.user_key(user_id)? {
            return self.open_dek(user_id, &row);
        }
        let mut dek = [0u8; KEY_BYTES];
        rand::rngs::OsRng.fill_bytes(&mut dek);
        let (wrapped, nonce) = seal(&self.master_key, &dek_aad(user_id), &dek)?;
        self.store.put_user_key(user_id, &wrapped, &nonce)?;
        let row = self
            .store
            .user_key(user_id)?
            .ok_or_else(|| SeraError::Internal("the per-user data key was not stored".into()))?;
        self.open_dek(user_id, &row)
    }

    fn open_dek(&self, user_id: &str, row: &StoredUserKey) -> Result<Zeroizing<[u8; KEY_BYTES]>> {
        let plaintext = open(
            &self.master_key,
            &dek_aad(user_id),
            &row.wrapped_dek,
            &row.nonce,
        )?;
        if plaintext.len() != KEY_BYTES {
            return Err(SeraError::Secrets(
                "the stored per-user data key has an invalid length".into(),
            ));
        }
        let mut dek = [0u8; KEY_BYTES];
        dek.copy_from_slice(&plaintext);
        Ok(Zeroizing::new(dek))
    }
}

/// The credential material for one AI provider, stored as a JSON object under
/// the provider's name.
///
/// A plain API-key provider stores `{"api_key": "..."}`. An OAuth provider
/// stores the access token as `api_key` (the bearer the client sends) and the
/// refresh token alongside it, so a rotation can update both without losing
/// the other field. `Debug` redacts both values.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ProviderCredential {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub refresh_token: String,
    /// OAuth only: when the access token stops being usable (Unix seconds).
    /// Unset means unknown, so the next use renews it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// OAuth only: identifies one sign-in. A new sign-in gets a new value, and
    /// a rotation written for an older sign-in is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
}

impl std::fmt::Debug for ProviderCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderCredential")
            .field("api_key", &redacted(&self.api_key))
            .field("refresh_token", &redacted(&self.refresh_token))
            .finish()
    }
}

impl ProviderCredential {
    /// Parse a stored credential value. A value that is not our JSON shape is
    /// an error, never treated as a bare key.
    pub fn parse(secret: &SecretString) -> Result<Self> {
        let credential: Self = serde_json::from_str(secret.expose_secret()).map_err(|_| {
            SeraError::Secrets("the stored provider credential is not valid".into())
        })?;
        credential.validate()?;
        Ok(credential)
    }

    /// Serialize for storage. Validates first so an empty or oversized
    /// credential never reaches the database.
    pub fn encode(&self) -> Result<String> {
        self.validate()?;
        Ok(serde_json::to_string(self)?)
    }

    /// Apply a rotation: the provider issued a new refresh token and,
    /// usually, a new access token. Fields not mentioned are preserved.
    pub fn merge_rotation(&mut self, refresh: &str, access: Option<&str>) -> Result<()> {
        if refresh.trim().is_empty() {
            return Err(SeraError::Secrets(
                "a rotated refresh token must not be empty".into(),
            ));
        }
        self.refresh_token = refresh.to_owned();
        if let Some(access) = access.filter(|value| !value.trim().is_empty()) {
            self.api_key = access.to_owned();
        }
        self.validate()
    }

    fn validate(&self) -> Result<()> {
        if self.api_key.is_empty() && self.refresh_token.is_empty() {
            return Err(SeraError::Validation(
                "a provider credential needs an API key or a refresh token".into(),
            ));
        }
        validate_value(&self.api_key).or_else(|_| {
            if self.api_key.is_empty() {
                Ok(())
            } else {
                Err(SeraError::Validation(
                    "the provider API key is invalid".into(),
                ))
            }
        })?;
        if !self.refresh_token.is_empty() {
            validate_value(&self.refresh_token).map_err(|_| {
                SeraError::Validation("the provider refresh token is invalid".into())
            })?;
        }
        Ok(())
    }
}

/// How a [`UserCredentialWriter`] treats the stored value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CredentialFormat {
    /// A JSON [`ProviderCredential`]: merge the rotated fields.
    Provider,
    /// A plain token (OneDrive): replace the whole value.
    Plain,
}

/// Write-back target for a rotated credential owned by a user.
///
/// Built by the per-user OneDrive registry and the per-user provider client
/// factory. The target is the credential's **owner** — the acting user for
/// their own row, the grantor for a granted provider — so a rotation never
/// writes into the wrong account.
pub struct UserCredentialWriter {
    users: Arc<UserSecrets>,
    user_id: String,
    provider: String,
    format: CredentialFormat,
    /// The sign-in generation this writer was issued for. When set, a write
    /// is refused unless the stored credential still carries it.
    generation: Option<String>,
}

impl UserCredentialWriter {
    /// Merge a rotation into a JSON provider credential.
    pub fn provider(
        users: Arc<UserSecrets>,
        user_id: impl Into<String>,
        provider: impl Into<String>,
    ) -> Self {
        Self {
            users,
            user_id: user_id.into(),
            provider: provider.into(),
            format: CredentialFormat::Provider,
            generation: None,
        }
    }

    /// Only write while the stored sign-in still has `generation`, so an older
    /// token chain cannot overwrite a newer sign-in.
    pub fn expecting(mut self, generation: Option<String>) -> Self {
        self.generation = generation;
        self
    }

    /// Replace a plain token value (the OneDrive refresh token).
    pub fn plain(
        users: Arc<UserSecrets>,
        user_id: impl Into<String>,
        provider: impl Into<String>,
    ) -> Self {
        Self {
            users,
            user_id: user_id.into(),
            provider: provider.into(),
            format: CredentialFormat::Plain,
            generation: None,
        }
    }
}

impl std::fmt::Debug for UserCredentialWriter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UserCredentialWriter")
            .field("user_id", &self.user_id)
            .field("provider", &self.provider)
            .field("format", &self.format)
            .finish()
    }
}

impl UserCredentialWriter {
    /// Apply `change` to the stored JSON credential, unless the sign-in it was
    /// issued for has been replaced.
    fn update(&self, change: impl FnOnce(&mut ProviderCredential) -> Result<()>) -> Result<()> {
        self.users
            .update_provider(&self.user_id, &self.provider, |credential| {
                if let Some(expected) = &self.generation
                    && credential.generation.as_ref() != Some(expected)
                {
                    return Err(SeraError::Secrets(
                        "the ElevenLabs sign-in changed; this refresh was not saved".into(),
                    ));
                }
                change(credential)
            })
    }
}

impl CredentialWriter for UserCredentialWriter {
    fn persist_rotation(&self, refresh: &str, access: Option<&str>) -> Result<()> {
        match self.format {
            CredentialFormat::Plain => self.users.set(&self.user_id, &self.provider, refresh),
            CredentialFormat::Provider => {
                self.update(|credential| credential.merge_rotation(refresh, access))
            }
        }
    }

    fn persist_refresh(
        &self,
        refresh: Option<&str>,
        access: &str,
        expires_at: Option<i64>,
    ) -> Result<()> {
        match (self.format, refresh) {
            (CredentialFormat::Plain, Some(refresh)) => {
                self.users.set(&self.user_id, &self.provider, refresh)
            }
            (CredentialFormat::Plain, None) => Ok(()),
            (CredentialFormat::Provider, refresh) => self.update(|credential| {
                match refresh {
                    Some(refresh) => credential.merge_rotation(refresh, Some(access))?,
                    None => credential.api_key = access.to_owned(),
                }
                credential.expires_at = expires_at;
                Ok(())
            }),
        }
    }
}

/// Provider-id prefix under which vault values are sealed.
pub(crate) const VAULT_PROVIDER_PREFIX: &str = "vault:";

/// The sealed-credential provider id for vault entry `name`.
pub(crate) fn vault_provider_id(name: &str) -> Result<String> {
    crate::vault::validate_name(name)?;
    Ok(format!("{VAULT_PROVIDER_PREFIX}{name}"))
}

/// Provider-id prefix under which a password manager's access token is sealed.
pub(crate) const VAULT_PROVIDER_TOKEN_PREFIX: &str = "vaultprov:";

/// The sealed-credential provider id for a password manager's token.
fn vault_provider_token_id(provider_id: &str) -> String {
    format!("{VAULT_PROVIDER_TOKEN_PREFIX}{provider_id}")
}

/// Provider ids are config keys: letters, digits, `-`, `_`, and `.`.
pub fn validate_provider(provider: &str) -> Result<()> {
    let valid = !provider.is_empty()
        && provider.len() <= MAX_PROVIDER_CHARS
        && provider
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if !valid {
        return Err(SeraError::Validation(
            "provider id must be 1-64 letters, digits, '-', '_', or '.'".into(),
        ));
    }
    Ok(())
}

/// One credential value: non-empty, bounded, no control characters.
pub fn validate_value(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_VALUE_BYTES {
        return Err(SeraError::Validation(format!(
            "a credential must be 1-{MAX_VALUE_BYTES} bytes"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(SeraError::Validation(
            "a credential must not contain control characters".into(),
        ));
    }
    Ok(())
}

/// Read the master key, or generate it on first run.
///
/// The file is exactly [`KEY_BYTES`] raw bytes. On unix it is created with
/// mode 0600 (and re-restricted after the write); a wrong-length file is an
/// error, never truncated or replaced. The bytes never appear in a log line
/// or an error message.
pub fn load_or_create_master_key(path: &Path) -> Result<[u8; KEY_BYTES]> {
    if path.exists() {
        return read_master_key(path);
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut key = [0u8; KEY_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut key);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(&key)?;
            file.sync_all()?;
            restrict_permissions(path)?;
            Ok(key)
        }
        // Another process created it first: use that key, never overwrite.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => read_master_key(path),
        Err(error) => Err(error.into()),
    }
}

fn read_master_key(path: &Path) -> Result<[u8; KEY_BYTES]> {
    let bytes = std::fs::read(path)?;
    if bytes.len() != KEY_BYTES {
        return Err(SeraError::Secrets(format!(
            "the master key at {} must be exactly {KEY_BYTES} bytes",
            path.display()
        )));
    }
    let mut key = [0u8; KEY_BYTES];
    key.copy_from_slice(&bytes);
    Ok(key)
}

fn dek_aad(user_id: &str) -> Vec<u8> {
    format!("dek|{user_id}").into_bytes()
}

fn credential_aad(user_id: &str, provider: &str) -> Vec<u8> {
    format!("credential|{user_id}|{provider}").into_bytes()
}

/// Seal `plaintext` with a fresh random nonce.
fn seal(
    key: &[u8; KEY_BYTES],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<(Vec<u8>, [u8; NONCE_BYTES])> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let mut nonce = [0u8; NONCE_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| SeraError::Secrets("credential encryption failed".into()))?;
    Ok((ciphertext, nonce))
}

/// Open a sealed blob. A wrong nonce length, an empty blob, or any AEAD
/// failure is an explicit error.
fn open(
    key: &[u8; KEY_BYTES],
    aad: &[u8],
    ciphertext: &[u8],
    nonce: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    if nonce.len() != NONCE_BYTES || ciphertext.is_empty() {
        return Err(SeraError::Secrets(
            "the stored credential is corrupt".into(),
        ));
    }
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| SeraError::Secrets("the stored credential could not be decrypted".into()))?;
    Ok(Zeroizing::new(plaintext))
}

fn redacted(value: &str) -> &'static str {
    if value.is_empty() { "" } else { "<redacted>" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{ADMIN_USER_ID, UserStatus};

    fn store() -> Store {
        Store::open_in_memory(384).unwrap()
    }

    fn users(store: &Store) -> UserSecrets {
        UserSecrets::with_master_key(store.clone(), [7u8; KEY_BYTES]).unwrap()
    }

    /// A member account for isolation tests.
    fn member(store: &Store, subject: &str) -> String {
        store
            .create_user(subject, "member@example.example", "Synthetic Member")
            .unwrap()
            .id
    }

    #[test]
    fn master_key_is_created_once_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private/master.key");
        let first = load_or_create_master_key(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), KEY_BYTES as u64);
        let second = load_or_create_master_key(&path).unwrap();
        assert_eq!(first, second, "an existing key is never regenerated");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // A wrong-length file fails closed instead of being replaced.
        std::fs::write(&path, b"short").unwrap();
        assert!(load_or_create_master_key(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"short");
    }

    #[test]
    fn set_and_get_round_trip_with_a_stable_dek() {
        let store = store();
        let secrets = users(&store);
        let admin = store.admin_user_id().unwrap();
        secrets.set(&admin, "openai", "sk-synthetic-one").unwrap();
        secrets.set(&admin, "openai", "sk-synthetic-two").unwrap();
        assert_eq!(
            secrets
                .get(&admin, "openai")
                .unwrap()
                .unwrap()
                .expose_secret(),
            "sk-synthetic-two"
        );
        // The DEK row is stable; only the credential ciphertext changes.
        let key = store.user_key(&admin).unwrap().unwrap();
        secrets.set(&admin, "openai", "sk-synthetic-three").unwrap();
        assert_eq!(store.user_key(&admin).unwrap().unwrap(), key);
        // A fresh nonce per write: identical values never share a ciphertext.
        secrets.set(&admin, "groq", "same-value").unwrap();
        let first = store.user_credential(&admin, "groq").unwrap().unwrap();
        secrets.set(&admin, "groq", "same-value").unwrap();
        let second = store.user_credential(&admin, "groq").unwrap().unwrap();
        assert_ne!(first.ciphertext, second.ciphertext);
        assert_ne!(first.nonce, second.nonce);
    }

    #[test]
    fn associated_data_binds_a_blob_to_its_user_and_provider() {
        let store = store();
        let secrets = users(&store);
        let admin = store.admin_user_id().unwrap();
        let other = member(&store, "microsoft:ad-binding");
        secrets.set(&admin, "openai", "sk-admin-only").unwrap();
        let row = store.user_credential(&admin, "openai").unwrap().unwrap();

        // Copy the exact ciphertext+nonce to another user: the AEAD open must
        // fail, not return the admin's key.
        store
            .put_user_credential(&other, "openai", &row.ciphertext, &row.nonce)
            .unwrap();
        let error = secrets.get(&other, "openai").unwrap_err();
        assert!(error.to_string().contains("could not be decrypted"));

        // Copy it under a different provider for the same user: also fails.
        store
            .put_user_credential(&admin, "groq", &row.ciphertext, &row.nonce)
            .unwrap();
        assert!(secrets.get(&admin, "groq").is_err());

        // The original still opens.
        assert_eq!(
            secrets
                .get(&admin, "openai")
                .unwrap()
                .unwrap()
                .expose_secret(),
            "sk-admin-only"
        );
    }

    #[test]
    fn a_wrapped_dek_cannot_be_moved_between_users() {
        let store = store();
        let secrets = users(&store);
        let admin = store.admin_user_id().unwrap();
        let other = member(&store, "microsoft:dek-binding");
        secrets.set(&admin, "openai", "sk-admin").unwrap();
        let key = store.user_key(&admin).unwrap().unwrap();
        let row = store.user_credential(&admin, "openai").unwrap().unwrap();
        // Give the other user the admin's sealed DEK *and* ciphertext: the DEK
        // is bound to `dek|admin`, so opening it as `other` must fail.
        store
            .put_user_key(&other, &key.wrapped_dek, &key.nonce)
            .unwrap();
        store
            .put_user_credential(&other, "openai", &row.ciphertext, &row.nonce)
            .unwrap();
        let error = secrets.get(&other, "openai").unwrap_err();
        assert!(error.to_string().contains("could not be decrypted"));
    }

    #[test]
    fn a_corrupt_blob_is_an_error_never_an_empty_value() {
        let store = store();
        let secrets = users(&store);
        let admin = store.admin_user_id().unwrap();
        secrets.set(&admin, "openai", "sk-corrupt-me").unwrap();
        let mut row = store.user_credential(&admin, "openai").unwrap().unwrap();
        row.ciphertext[0] ^= 0xff;
        store
            .put_user_credential(&admin, "openai", &row.ciphertext, &row.nonce)
            .unwrap();
        let error = secrets.get(&admin, "openai").unwrap_err();
        assert!(error.to_string().contains("could not be decrypted"));

        // A truncated nonce is rejected before any AEAD call.
        store
            .put_user_credential(&admin, "openai", &row.ciphertext, &[0u8; 4])
            .unwrap();
        assert!(secrets.get(&admin, "openai").is_err());
    }

    #[test]
    fn grant_resolution_prefers_own_then_the_admin_grant() {
        let store = store();
        let secrets = users(&store);
        let admin = store.admin_user_id().unwrap();
        let member = member(&store, "microsoft:grant-resolution");
        secrets.set(&admin, "openai", "sk-admin").unwrap();

        // No own row, no grant: nothing.
        assert!(secrets.resolve(&member, "openai").unwrap().is_none());

        secrets.grant(&admin, &member, "openai").unwrap();
        assert_eq!(
            secrets
                .resolve(&member, "openai")
                .unwrap()
                .unwrap()
                .expose_secret(),
            "sk-admin"
        );

        // Own credential wins over the grant.
        secrets.set(&member, "openai", "sk-member").unwrap();
        assert_eq!(
            secrets
                .resolve(&member, "openai")
                .unwrap()
                .unwrap()
                .expose_secret(),
            "sk-member"
        );
        secrets.delete(&member, "openai").unwrap();
        assert_eq!(
            secrets
                .resolve(&member, "openai")
                .unwrap()
                .unwrap()
                .expose_secret(),
            "sk-admin"
        );

        // Revocation fails closed immediately.
        assert!(secrets.revoke_grant(&admin, &member, "openai").unwrap());
        assert!(secrets.resolve(&member, "openai").unwrap().is_none());
        // Revoking twice is idempotent and reports false.
        assert!(!secrets.revoke_grant(&admin, &member, "openai").unwrap());

        // Re-granting restores access.
        secrets.grant(&admin, &member, "openai").unwrap();
        assert!(secrets.resolve(&member, "openai").unwrap().is_some());
    }

    #[test]
    fn a_grant_never_reaches_another_members_credential() {
        let store = store();
        let secrets = users(&store);
        let admin = store.admin_user_id().unwrap();
        let alice = member(&store, "microsoft:alice");
        let bob = member(&store, "microsoft:bob");
        secrets.set(&alice, "openai", "sk-alice").unwrap();
        // Only the admin can grant; a member cannot share their own key.
        let error = secrets.grant(&alice, &bob, "openai").unwrap_err();
        assert!(error.to_string().contains("only the admin"));
        assert!(secrets.resolve(&bob, "openai").unwrap().is_none());
        // The admin cannot grant a provider they do not hold.
        assert!(secrets.grant(&admin, &bob, "openai").is_err());
        // Self-grants are rejected.
        secrets.set(&admin, "openai", "sk-admin").unwrap();
        assert!(secrets.grant(&admin, &admin, "openai").is_err());
        assert!(secrets.resolve(&bob, "openai").unwrap().is_none());
    }

    #[test]
    fn a_grant_without_a_credential_behind_it_fails_closed() {
        let store = store();
        let secrets = users(&store);
        let admin = store.admin_user_id().unwrap();
        let member = member(&store, "microsoft:empty-grant");
        secrets.set(&admin, "openai", "sk-admin").unwrap();
        secrets.grant(&admin, &member, "openai").unwrap();
        // The admin deletes their own key after granting it.
        secrets.delete(&admin, "openai").unwrap();
        let error = secrets.resolve(&member, "openai").unwrap_err();
        assert!(error.to_string().contains("no credential"));
    }

    #[test]
    fn providers_for_lists_names_only_own_and_granted() {
        let store = store();
        let secrets = users(&store);
        let admin = store.admin_user_id().unwrap();
        let member_id = member(&store, "microsoft:providers-for");
        secrets.set(&admin, "openai", "sk-admin").unwrap();
        secrets.set(&admin, "groq", "gsk-admin").unwrap();
        secrets.set(&member_id, "mistral", "ms-member").unwrap();
        secrets.grant(&admin, &member_id, "openai").unwrap();
        assert_eq!(
            secrets.providers_for(&member_id).unwrap(),
            vec!["mistral".to_owned(), "openai".to_owned()]
        );
        assert_eq!(
            secrets.providers_for(&admin).unwrap(),
            vec!["groq".to_owned(), "openai".to_owned()]
        );
        // Another user's names never leak into the list.
        let third = member(&store, "microsoft:third");
        assert!(secrets.providers_for(&third).unwrap().is_empty());
    }

    #[test]
    fn deleting_a_credential_is_scoped_to_the_acting_user() {
        let store = store();
        let secrets = users(&store);
        let admin = store.admin_user_id().unwrap();
        let member = member(&store, "microsoft:delete-scope");
        secrets.set(&admin, "openai", "sk-admin").unwrap();
        // The member cannot delete the admin's row.
        assert!(!secrets.delete(&member, "openai").unwrap());
        assert!(secrets.get(&admin, "openai").unwrap().is_some());
        assert!(secrets.delete(&admin, "openai").unwrap());
        assert!(secrets.get(&admin, "openai").unwrap().is_none());
    }

    #[test]
    fn no_plaintext_credential_reaches_the_database_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("serabot.db");
        let store = Store::open(&path, 384, 5000).unwrap();
        let secrets = users(&store);
        let admin = store.admin_user_id().unwrap();
        secrets
            .set(&admin, "openai", "sk-plaintext-marker")
            .unwrap();
        secrets.set(&admin, "groq", "gsk-plaintext-marker").unwrap();
        // WAL mode keeps recent pages out of the main file until a
        // checkpoint; scan both so the assertion covers everything on disk.
        for candidate in [path.clone(), path.with_extension("db-wal")] {
            if !candidate.exists() {
                continue;
            }
            let raw = std::fs::read(&candidate).unwrap();
            let text = String::from_utf8_lossy(&raw);
            assert!(
                !text.contains("sk-plaintext-marker"),
                "plaintext leaked into {}",
                candidate.display()
            );
            assert!(
                !text.contains("gsk-plaintext-marker"),
                "plaintext leaked into {}",
                candidate.display()
            );
        }
    }

    #[test]
    fn debug_output_redacts_keys_and_values() {
        let store = store();
        let secrets = users(&store);
        let admin = store.admin_user_id().unwrap();
        secrets.set(&admin, "openai", "sk-debug-marker").unwrap();
        let debug = format!("{secrets:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("sk-debug-marker"));
        let credential = ProviderCredential {
            api_key: "sk-debug-marker".into(),
            refresh_token: "refresh-debug-marker".into(),
            ..Default::default()
        };
        let debug = format!("{credential:?}");
        assert!(!debug.contains("sk-debug-marker"));
        assert!(!debug.contains("refresh-debug-marker"));
        let row = store.user_credential(&admin, "openai").unwrap().unwrap();
        let debug = format!("{row:?}");
        assert!(!debug.contains("sk-debug-marker"));
    }

    #[test]
    fn provider_credentials_round_trip_and_rotate_losslessly() {
        let credential = ProviderCredential {
            api_key: "access-one".into(),
            refresh_token: "refresh-one".into(),
            ..Default::default()
        };
        let encoded = credential.encode().unwrap();
        let parsed = ProviderCredential::parse(&SecretString::new(encoded)).unwrap();
        assert_eq!(parsed.api_key, "access-one");
        assert_eq!(parsed.refresh_token, "refresh-one");

        let mut rotated = parsed.clone();
        rotated
            .merge_rotation("refresh-two", Some("access-two"))
            .unwrap();
        assert_eq!(rotated.api_key, "access-two");
        assert_eq!(rotated.refresh_token, "refresh-two");
        // An access-less rotation keeps the current access token.
        rotated.merge_rotation("refresh-three", None).unwrap();
        assert_eq!(rotated.api_key, "access-two");
        assert_eq!(rotated.refresh_token, "refresh-three");

        // A non-JSON value is an error, never treated as a bare key.
        let error = ProviderCredential::parse(&SecretString::new("sk-bare".into())).unwrap_err();
        assert!(error.to_string().contains("not valid"));
        // An empty credential is rejected.
        assert!(ProviderCredential::default().encode().is_err());
    }

    #[test]
    fn rotation_writers_target_the_right_shape() {
        let store = store();
        let secrets = Arc::new(users(&store));
        let admin = store.admin_user_id().unwrap();
        let credential = ProviderCredential {
            api_key: "access-one".into(),
            refresh_token: "refresh-one".into(),
            ..Default::default()
        };
        secrets
            .set(&admin, "antigravity", &credential.encode().unwrap())
            .unwrap();
        UserCredentialWriter::provider(secrets.clone(), admin.clone(), "antigravity")
            .persist_rotation("refresh-two", Some("access-two"))
            .unwrap();
        let stored =
            ProviderCredential::parse(&secrets.get(&admin, "antigravity").unwrap().unwrap())
                .unwrap();
        assert_eq!(stored.refresh_token, "refresh-two");
        assert_eq!(stored.api_key, "access-two");

        // The plain writer replaces the whole value (OneDrive).
        secrets.set(&admin, "onedrive", "refresh-old").unwrap();
        UserCredentialWriter::plain(secrets.clone(), admin.clone(), "onedrive")
            .persist_rotation("refresh-new", None)
            .unwrap();
        assert_eq!(
            secrets
                .get(&admin, "onedrive")
                .unwrap()
                .unwrap()
                .expose_secret(),
            "refresh-new"
        );

        // A rotation for a missing row fails closed.
        let error = UserCredentialWriter::provider(secrets.clone(), admin, "groq")
            .persist_rotation("refresh", None)
            .unwrap_err();
        assert!(error.to_string().contains("not persisted"));
    }

    #[test]
    fn invalid_provider_names_and_values_are_rejected() {
        let store = store();
        let secrets = users(&store);
        let admin = store.admin_user_id().unwrap();
        for provider in ["", "with space", "with|pipe", "with/slash", &"x".repeat(65)] {
            assert!(
                secrets.set(&admin, provider, "value").is_err(),
                "{provider}"
            );
            assert!(secrets.get(&admin, provider).is_err(), "{provider}");
        }
        assert!(secrets.set(&admin, "openai", "").is_err());
        assert!(
            secrets
                .set(&admin, "openai", &"x".repeat(MAX_VALUE_BYTES + 1))
                .is_err()
        );
        assert!(secrets.set(&admin, "openai", "line\nbreak").is_err());
        // Unknown users are an explicit not-found, not a database error.
        let error = secrets.set("no-such-user", "openai", "value").unwrap_err();
        assert!(error.to_string().contains("not found"));
        // A disabled grantee cannot receive a grant.
        let member = member(&store, "microsoft:disabled-grantee");
        secrets.set(&admin, "openai", "sk-admin").unwrap();
        store
            .set_user_status(&member, UserStatus::Disabled)
            .unwrap();
        assert!(secrets.grant(&admin, &member, "openai").is_err());
        // The documented admin id is the bootstrap account.
        assert_eq!(admin, ADMIN_USER_ID);
    }
}
