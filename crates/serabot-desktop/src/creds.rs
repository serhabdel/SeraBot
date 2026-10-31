//! Non-secret settings and secure token storage for the owner console.
//!
//! The server origin lives in `%APPDATA%\SeraBot\settings.json`. The API token
//! never touches that file: on Windows it is stored in the Windows Credential
//! Manager through the `keyring` crate. Non-Windows builds (WSL tests) keep the
//! token in a process-local map only. No token is ever logged or written to
//! disk, and release builds do not read token environment variables.

use crate::{default_server_url, valid_id, valid_speaker, valid_token, validate_endpoint};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use zeroize::Zeroize;

/// Windows Credential Manager service name. Never holds the token itself.
pub const CREDENTIAL_SERVICE: &str = "SeraBot";
/// Windows Credential Manager account name for the owner API token.
pub const CREDENTIAL_ACCOUNT: &str = "owner-api-token";
/// Debug-only fallback. The value is read at run time, never compiled in.
pub const TOKEN_ENV_FALLBACK: &str = "SERABOT_OWNER_TOKEN";

const SETTINGS_FILE: &str = "settings.json";
const TEMP_SETTINGS_FILE: &str = "settings.json.tmp";

/// Maximum number of pinned conversations or bots in the sidebar/inspector.
pub const MAX_PINS: usize = 24;

/// Non-secret persisted settings. Only the server origin, UI theme and the
/// pin list are stored.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Settings {
    #[serde(default)]
    pub server: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pins: Option<Vec<String>>,
}

/// A pin is either a bot id slug (`analyst`) or a full conversation id
/// (`dir:analyst`, `room:review`, `windows-owner`, or a numeric chat id).
pub fn valid_pin_id(id: &str) -> bool {
    if !valid_id(id) {
        return false;
    }
    if valid_speaker(id) {
        return true;
    }
    id == "windows-owner"
        || id.chars().all(|c| c.is_ascii_digit())
        || ["sec:", "grp:", "dir:", "room:"]
            .iter()
            .any(|prefix| id.starts_with(prefix))
}

/// Keep only valid, unique pins in first-seen order, capped at [`MAX_PINS`].
pub fn sanitize_pins(pins: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for pin in pins {
        if out.len() >= MAX_PINS {
            break;
        }
        if valid_pin_id(&pin) && !out.iter().any(|kept| kept == &pin) {
            out.push(pin);
        }
    }
    out
}

/// Toggle one pin in place. Returns `true` when the list changed. Invalid
/// ids, duplicates already present and a full list are no-ops.
pub fn toggle_pin_in_list(pins: &mut Vec<String>, id: &str) -> bool {
    if !valid_pin_id(id) {
        return false;
    }
    if let Some(position) = pins.iter().position(|pin| pin == id) {
        pins.remove(position);
        return true;
    }
    if pins.len() >= MAX_PINS {
        return false;
    }
    pins.push(id.to_owned());
    true
}

/// `%APPDATA%\SeraBot` on Windows, `$HOME/.config/SeraBot` elsewhere.
/// `SERABOT_SETTINGS_DIR` overrides both for manual testing.
pub fn settings_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("SERABOT_SETTINGS_DIR") {
        let path = PathBuf::from(dir);
        if !path.as_os_str().is_empty() {
            return Some(path);
        }
    }
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(|base| PathBuf::from(base).join("SeraBot"))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config").join("SeraBot"))
    }
}

/// Load settings from the platform location, falling back to defaults.
pub fn load_settings() -> Settings {
    settings_dir()
        .map(|dir| load_settings_from(&dir))
        .unwrap_or_default()
}

/// Save settings to the platform location.
pub fn save_settings(settings: &Settings) -> Result<(), String> {
    let dir = settings_dir()
        .ok_or_else(|| "No application settings directory is available.".to_owned())?;
    save_settings_to(&dir, settings)
}

/// Load settings from an explicit directory. Absent or malformed files return
/// defaults; a stored origin is re-validated before it is trusted.
pub fn load_settings_from(dir: &Path) -> Settings {
    let Ok(text) = fs::read_to_string(dir.join(SETTINGS_FILE)) else {
        return Settings::default();
    };
    let Ok(settings) = serde_json::from_str::<Settings>(&text) else {
        return Settings::default();
    };
    let theme = settings.theme.and_then(|theme| match theme.as_str() {
        "dark" | "light" => Some(theme),
        _ => None,
    });
    let pins = settings
        .pins
        .map(sanitize_pins)
        .filter(|pins| !pins.is_empty());
    match validate_endpoint(settings.server.trim()) {
        Ok(url) => Settings {
            server: url.origin().ascii_serialization(),
            theme,
            pins,
        },
        Err(_) => Settings {
            server: String::new(),
            theme,
            pins,
        },
    }
}

/// Write settings atomically. Only a validated origin (or an empty value) is
/// accepted; this function never receives token material.
pub fn save_settings_to(dir: &Path, settings: &Settings) -> Result<(), String> {
    let server = if settings.server.trim().is_empty() {
        String::new()
    } else {
        validate_endpoint(settings.server.trim())
            .map_err(|error| error.to_string())?
            .origin()
            .ascii_serialization()
    };
    let text = serde_json::to_string_pretty(&Settings {
        server,
        theme: settings.theme.clone(),
        pins: settings
            .pins
            .as_ref()
            .map(|pins| sanitize_pins(pins.clone()))
            .filter(|pins| !pins.is_empty()),
    })
    .map_err(|_| "Could not encode settings.".to_owned())?;
    fs::create_dir_all(dir).map_err(|_| "Could not create the settings directory.".to_owned())?;
    let temp = dir.join(TEMP_SETTINGS_FILE);
    fs::write(&temp, text).map_err(|_| "Could not write settings.".to_owned())?;
    fs::rename(&temp, dir.join(SETTINGS_FILE)).map_err(|_| {
        let _ = fs::remove_file(&temp);
        "Could not replace settings.".to_owned()
    })
}

/// Load the stored owner token.
///
/// Order: the platform credential store, then (debug builds only) the
/// `SERABOT_OWNER_TOKEN` process environment. Release builds contain no code
/// that reads the environment variable.
pub fn load_token() -> Option<SecretString> {
    if let Some(token) = stored_token() {
        return Some(token);
    }
    #[cfg(debug_assertions)]
    if let Some(token) = environment_token() {
        return Some(token);
    }
    None
}

/// Persist the owner token in the platform credential store.
pub fn save_token(token: &str) -> Result<(), String> {
    if !valid_token(token) {
        return Err(
            "The token must contain 32–512 visible ASCII characters, without spaces.".to_owned(),
        );
    }
    store_token(token)
}

/// Remove the stored owner token. Missing entries are not an error.
pub fn forget_token() -> Result<(), String> {
    delete_token()
}

/// Server origin to prefill when no saved settings exist.
pub fn prefill_server(settings: &Settings) -> String {
    if validate_endpoint(&settings.server).is_ok() {
        settings.server.clone()
    } else {
        default_server_url()
    }
}

#[cfg(debug_assertions)]
fn environment_token() -> Option<SecretString> {
    let value = std::env::var(TOKEN_ENV_FALLBACK).ok()?;
    let trimmed = value.trim();
    if valid_token(trimmed) {
        Some(SecretString::new(trimmed.to_owned()))
    } else {
        None
    }
}

#[cfg(windows)]
fn stored_token() -> Option<SecretString> {
    let entry = keyring::Entry::new(CREDENTIAL_SERVICE, CREDENTIAL_ACCOUNT).ok()?;
    match entry.get_password() {
        Ok(mut value) => {
            if valid_token(&value) {
                Some(SecretString::new(value))
            } else {
                value.zeroize();
                None
            }
        }
        // NoEntry (and inaccessible stores) behave like an absent credential.
        Err(_) => None,
    }
}

#[cfg(windows)]
fn store_token(token: &str) -> Result<(), String> {
    keyring::Entry::new(CREDENTIAL_SERVICE, CREDENTIAL_ACCOUNT)
        .map_err(|_| credential_error())?
        .set_password(token)
        .map_err(|_| credential_error())
}

#[cfg(windows)]
fn delete_token() -> Result<(), String> {
    let entry = keyring::Entry::new(CREDENTIAL_SERVICE, CREDENTIAL_ACCOUNT)
        .map_err(|_| credential_error())?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err(credential_error()),
    }
}

#[cfg(windows)]
fn credential_error() -> String {
    "Windows Credential Manager is unavailable or refused the request.".to_owned()
}

#[cfg(not(windows))]
fn memory_token() -> &'static std::sync::Mutex<Option<String>> {
    static TOKEN: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    &TOKEN
}

#[cfg(not(windows))]
fn lock_memory_token() -> std::sync::MutexGuard<'static, Option<String>> {
    memory_token()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Test/WSL builds: process-local memory only. Never a file.
#[cfg(not(windows))]
fn stored_token() -> Option<SecretString> {
    let guard = lock_memory_token();
    guard
        .as_ref()
        .filter(|token| valid_token(token))
        .map(|token| SecretString::new(token.clone()))
}

#[cfg(not(windows))]
fn store_token(token: &str) -> Result<(), String> {
    let mut guard = lock_memory_token();
    if let Some(mut previous) = guard.replace(token.to_owned()) {
        previous.zeroize();
    }
    Ok(())
}

#[cfg(not(windows))]
fn delete_token() -> Result<(), String> {
    let mut guard = lock_memory_token();
    if let Some(mut previous) = guard.take() {
        previous.zeroize();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(windows))]
    use secrecy::ExposeSecret;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "serabot-desktop-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn settings_round_trip_keeps_only_the_server_origin() {
        let dir = temp_dir("settings-round-trip");
        assert_eq!(load_settings_from(&dir), Settings::default());

        save_settings_to(
            &dir,
            &Settings {
                server: "https://bot.example/".into(),
                theme: Some("light".into()),
                pins: Some(vec!["dir:analyst".into(), "room:review".into()]),
            },
        )
        .unwrap();
        assert_eq!(
            load_settings_from(&dir),
            Settings {
                server: "https://bot.example".into(),
                theme: Some("light".into()),
                pins: Some(vec!["dir:analyst".into(), "room:review".into()])
            }
        );
        let text = fs::read_to_string(dir.join(SETTINGS_FILE)).unwrap();
        assert!(text.contains("https://bot.example"));
        assert!(text.contains("dir:analyst"));
        assert!(!text.contains("token"));

        save_settings_to(
            &dir,
            &Settings {
                server: "http://127.0.0.1:8787".into(),
                theme: None,
                pins: None,
            },
        )
        .unwrap();
        assert_eq!(load_settings_from(&dir).server, "http://127.0.0.1:8787");
        assert_eq!(load_settings_from(&dir).pins, None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pins_round_trip_sanitizes_duplicates_invalid_and_overflow() {
        let dir = temp_dir("settings-pins");
        save_settings_to(
            &dir,
            &Settings {
                server: String::new(),
                theme: None,
                pins: Some(vec![
                    "analyst".into(),
                    "dir:analyst".into(),
                    "analyst".into(),
                    "../escape".into(),
                    "bad id".into(),
                    "windows-owner".into(),
                    "room:review".into(),
                    "12345".into(),
                ]),
            },
        )
        .unwrap();
        assert_eq!(
            load_settings_from(&dir).pins,
            Some(vec![
                "analyst".to_string(),
                "dir:analyst".to_string(),
                "windows-owner".to_string(),
                "room:review".to_string(),
                "12345".to_string(),
            ])
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pin_ids_are_bot_slugs_or_conversation_ids() {
        assert!(valid_pin_id("analyst"));
        assert!(valid_pin_id("bot-2"));
        assert!(valid_pin_id("dir:analyst"));
        assert!(valid_pin_id("sec:finance"));
        assert!(valid_pin_id("grp:ops"));
        assert!(valid_pin_id("room:review"));
        assert!(valid_pin_id("windows-owner"));
        assert!(valid_pin_id("1234567890"));
        assert!(!valid_pin_id(""));
        assert!(!valid_pin_id("../etc"));
        assert!(!valid_pin_id("bad id"));
        assert!(!valid_pin_id("Main"));
        assert!(!valid_pin_id("unknown:thing"));
        assert!(!valid_pin_id(&"a".repeat(129)));
    }

    #[test]
    fn toggling_pins_dedups_caps_and_rejects_invalid_ids() {
        let mut pins = Vec::new();
        assert!(toggle_pin_in_list(&mut pins, "analyst"));
        assert_eq!(pins, vec!["analyst".to_string()]);
        // Toggling an already pinned id unpins it, so duplicates never appear.
        assert!(toggle_pin_in_list(&mut pins, "analyst"));
        assert!(pins.is_empty());
        assert!(toggle_pin_in_list(&mut pins, "analyst"));
        assert!(toggle_pin_in_list(&mut pins, "room:review"));
        assert_eq!(pins.len(), 2);
        assert!(!toggle_pin_in_list(&mut pins, "not a pin"));
        assert_eq!(pins.len(), 2, "invalid ids are a no-op");
        for index in 0..MAX_PINS - 2 {
            assert!(toggle_pin_in_list(&mut pins, &format!("bot-{index}")));
        }
        assert_eq!(pins.len(), MAX_PINS);
        assert!(!toggle_pin_in_list(&mut pins, "one-too-many"), "cap holds");
        assert!(
            toggle_pin_in_list(&mut pins, "analyst"),
            "removal still works at the cap"
        );
        assert_eq!(pins.len(), MAX_PINS - 1);
    }

    #[test]
    fn absent_and_malformed_settings_return_defaults() {
        let dir = temp_dir("settings-malformed");
        assert_eq!(load_settings_from(&dir), Settings::default());

        fs::write(dir.join(SETTINGS_FILE), "{not json").unwrap();
        assert_eq!(load_settings_from(&dir), Settings::default());

        fs::write(
            dir.join(SETTINGS_FILE),
            r#"{"server":"http://bot.example","token":"synthetic"}"#,
        )
        .unwrap();
        assert_eq!(load_settings_from(&dir), Settings::default());

        fs::write(dir.join(SETTINGS_FILE), r#"{"server":42}"#).unwrap();
        assert_eq!(load_settings_from(&dir), Settings::default());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn invalid_origins_are_rejected_and_not_persisted() {
        let dir = temp_dir("settings-invalid");
        assert!(
            save_settings_to(
                &dir,
                &Settings {
                    server: "http://bot.example".into(),
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert!(
            save_settings_to(
                &dir,
                &Settings {
                    server: "https://user:secret@bot.example".into(),
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert_eq!(load_settings_from(&dir), Settings::default());
        save_settings_to(&dir, &Settings::default()).unwrap();
        assert_eq!(load_settings_from(&dir), Settings::default());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn prefill_prefers_a_saved_origin_over_the_built_in_hint() {
        assert_eq!(
            prefill_server(&Settings {
                server: "https://owner.example".into(),
                ..Default::default()
            }),
            "https://owner.example"
        );
        let settings = Settings {
            server: "http://owner.example".into(),
            theme: None,
            pins: None,
        };
        // Invalid saved origins fall back to the optional compiled hint.
        assert_eq!(prefill_server(&settings), default_server_url());
    }

    #[cfg(not(windows))]
    #[test]
    fn memory_token_store_round_trips_and_clears() {
        let token = "synthetic-desktop-token-000000000000";
        assert!(save_token("too-short").is_err());
        forget_token().unwrap();
        assert!(stored_token().is_none());
        save_token(token).unwrap();
        assert_eq!(stored_token().unwrap().expose_secret(), token);
        forget_token().unwrap();
        // Idempotent: clearing an absent credential succeeds.
        forget_token().unwrap();
        assert!(stored_token().is_none());
    }

    #[test]
    fn credential_names_are_stable_and_not_tokens() {
        assert_eq!(CREDENTIAL_SERVICE, "SeraBot");
        assert_eq!(CREDENTIAL_ACCOUNT, "owner-api-token");
        assert_eq!(TOKEN_ENV_FALLBACK, "SERABOT_OWNER_TOKEN");
        assert!(!valid_token(CREDENTIAL_SERVICE));
    }
}
