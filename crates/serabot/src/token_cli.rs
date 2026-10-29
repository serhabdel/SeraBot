use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rand::RngCore;

use crate::domain::new_id;
use crate::error::{Result, SeraError};
use crate::store::{DeviceToken, Store};
use crate::util::sha256_hex;

pub const TOKEN_BYTES: usize = 32;
pub const MAX_LABEL_CHARS: usize = 64;

/// Mints a random device token, stores only its SHA-256 hash, and returns the
/// plaintext token once. Nothing else in the process keeps the plaintext.
pub fn mint(store: &Store, label: &str, days: Option<u32>) -> Result<String> {
    let label = label.trim();
    if label.is_empty()
        || label.chars().count() > MAX_LABEL_CHARS
        || label.chars().any(char::is_control)
    {
        return Err(SeraError::Validation(format!(
            "label must be 1-{MAX_LABEL_CHARS} characters without control characters"
        )));
    }
    let mut bytes = [0u8; TOKEN_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let token = hex::encode(bytes);
    let expires_at = days.map(|days| Utc::now() + Duration::days(i64::from(days)));
    store.insert_device_token(&new_id(), label, &sha256_hex(&token), expires_at)?;
    Ok(token)
}

pub fn list(store: &Store) -> Result<Vec<DeviceToken>> {
    store.list_device_tokens()
}

pub fn revoke(store: &Store, id: &str) -> Result<bool> {
    store.revoke_device_token(id)
}

/// One tab-separated line for `token list`. Hashes are not part of the struct.
pub fn format_line(token: &DeviceToken) -> String {
    fn stamp(value: Option<DateTime<Utc>>) -> String {
        value
            .map(|value| value.to_rfc3339_opts(SecondsFormat::Secs, true))
            .unwrap_or_else(|| "-".into())
    }
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}",
        token.id,
        token.label,
        stamp(Some(token.created_at)),
        stamp(token.last_used_at),
        stamp(token.expires_at),
        stamp(token.revoked_at)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory(384).unwrap()
    }

    fn is_hex_64(value: &str) -> bool {
        value.len() == TOKEN_BYTES * 2 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    }

    #[test]
    fn mint_stores_only_the_hash() {
        let store = store();
        let token = mint(&store, "desktop-win", None).unwrap();
        assert!(is_hex_64(&token));
        let other = mint(&store, "desktop-win", None).unwrap();
        assert_ne!(token, other);

        let hash = sha256_hex(&token);
        assert_ne!(hash, token);
        let found = store.device_token_by_hash(&hash).unwrap().unwrap();
        assert_eq!(found.label, "desktop-win");
        assert!(found.active_at(Utc::now()));
        assert!(
            store.list_device_tokens().unwrap()[0]
                .last_used_at
                .is_none()
        );
    }

    #[test]
    fn labels_are_bounded_and_printable() {
        let store = store();
        for label in ["", "   ", &"a".repeat(MAX_LABEL_CHARS + 1), "bad\nlabel"] {
            assert!(mint(&store, label, None).is_err(), "{label:?}");
        }
    }

    #[test]
    fn expiry_and_revocation_disable_the_token() {
        let store = store();
        mint(&store, "expired", Some(0)).unwrap();
        let expired = list(&store).unwrap().remove(0);
        assert!(!expired.active_at(Utc::now()));

        let token = mint(&store, "revoked", None).unwrap();
        let hash = sha256_hex(&token);
        let record = store.device_token_by_hash(&hash).unwrap().unwrap();
        assert!(revoke(&store, &record.id).unwrap());
        assert!(
            !store
                .device_token_by_hash(&hash)
                .unwrap()
                .unwrap()
                .active_at(Utc::now())
        );
        assert!(!revoke(&store, "missing").unwrap());
    }

    #[test]
    fn list_lines_never_expose_hashes() {
        let store = store();
        let token = mint(&store, "listed", None).unwrap();
        let line = format_line(&list(&store).unwrap()[0]);
        assert!(!line.contains(&sha256_hex(&token)));
        assert!(!line.contains(&token));
        assert!(line.contains("listed"));
    }
}
