//! Per-user secret vault (0.14): named secrets any of the user's bots may use
//! through the server, never shown to the model.
//!
//! STUB: this file fixes the cross-module signature so the browser work can
//! compile against it. The vault work replaces the body; the signature of
//! `resolve_for_browser` must stay as written (see
//! `docs/specs/vault-browser-contract.md`).

use zeroize::Zeroizing;

use crate::error::{Result, SeraError};
use crate::user_secrets::UserSecrets;

/// Resolve vault secret `name` of `user_id` for typing into a page on `host`.
/// Fails closed: unknown name, a host outside the secret's allowed domains,
/// or a disabled vault are all errors. The value never leaves the server.
pub fn resolve_for_browser(
    users: &UserSecrets,
    user_id: &str,
    name: &str,
    host: &str,
) -> Result<Zeroizing<String>> {
    let _ = (users, user_id, name, host);
    Err(SeraError::Validation(
        "the vault is not available yet".into(),
    ))
}
