//! Import password manager logins from this PC (0.23).
//!
//! The desktop runs the owner's own CLI (`op` for 1Password, `pass-cli` for
//! Proton Pass) to list their login items. Only metadata comes back: titles,
//! usernames and sites. Each picked item becomes a *reference* entry on the
//! server (`op://<vault id>/<item id>/password`, or
//! `pass://<vault>/<title>/password`). No password is read or stored here.
//!
//! The CLI runs with an argument array, never through a shell, with a 20 s
//! timeout. Its stderr is discarded, so no provider message reaches the UI.
//! The parsers are pure and take the CLI's JSON as bytes, so they are tested
//! with synthetic fixtures. The Proton Pass listing shape is an assumption
//! (see `parse_proton_items`); the 1Password shape follows its documented
//! `op item list --format json` output.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::api::{
    ApiError, VaultSource, normalize_vault_domain, valid_vault_name, valid_vault_reference,
};

/// How long a listing may run before it is killed.
pub const LIST_TIMEOUT: Duration = Duration::from_secs(20);
/// The largest listing accepted. A login list is far smaller.
const MAX_LIST_BYTES: usize = 8 * 1024 * 1024;
/// Most items one listing contributes.
const MAX_ITEMS: usize = 500;
const MAX_TITLE_CHARS: usize = 200;
const MAX_USERNAME_CHARS: usize = 256;
const MAX_VAULT_NAME_CHARS: usize = 200;

/// One login from a password manager, as the picker shows it. Metadata only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportItem {
    pub source: VaultSource,
    /// The reference the entry will hold. A pointer, never a value.
    pub reference: String,
    pub title: String,
    pub vault: String,
    pub username: Option<String>,
    /// The site the login is for, from its first URL.
    pub domain: Option<String>,
    /// A vault name derived from the title. Made unique at import time.
    pub suggested_name: String,
}

fn text_field<'a>(item: &'a Value, key: &str) -> Option<&'a str> {
    item.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn bounded(text: &str, max: usize) -> Option<String> {
    (text.chars().count() <= max && !text.chars().any(char::is_control)).then(|| text.to_owned())
}

/// The first URL's host, normalized the way the vault stores sites.
fn first_domain(urls: &[&str]) -> Option<String> {
    urls.iter().find_map(|raw| {
        let parsed = url::Url::parse(raw).ok()?;
        normalize_vault_domain(parsed.host_str()?)
    })
}

/// A vault name from a title: lower-case letters, digits and dashes, at most
/// 48 characters, never empty.
pub fn suggested_name(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let base: String = out.trim_end_matches('-').chars().take(48).collect();
    let base = base.trim_end_matches('-');
    if base.is_empty() {
        "login".to_owned()
    } else {
        base.to_owned()
    }
}

/// `base`, or `base-2`, `base-3`, ... the first one not in `taken`. Always a
/// valid vault name.
pub fn unique_name(base: &str, taken: &[String]) -> String {
    if !taken.iter().any(|name| name == base) {
        return base.to_owned();
    }
    (2u32..)
        .map(|n| {
            let suffix = format!("-{n}");
            let stem: String = base.chars().take(48 - suffix.len()).collect();
            format!("{}{suffix}", stem.trim_end_matches('-'))
        })
        .find(|candidate| !taken.contains(candidate) && valid_vault_name(candidate))
        .unwrap_or_else(|| base.to_owned())
}

fn items_of(value: &Value) -> Result<&Vec<Value>, ApiError> {
    value.as_array().ok_or(ApiError::InvalidResponse)
}

/// Parse `op item list --categories Login --format json`. The reference is
/// `op://<vault id>/<item id>/password`, so it survives a renamed item.
pub fn parse_onepassword_items(bytes: &[u8]) -> Result<Vec<ImportItem>, ApiError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| ApiError::InvalidResponse)?;
    let mut out = Vec::new();
    for item in items_of(&value)?.iter().take(MAX_ITEMS) {
        let vault_id = item.pointer("/vault/id").and_then(Value::as_str);
        let item_id = text_field(item, "id");
        let title = text_field(item, "title");
        let (Some(vault_id), Some(item_id), Some(title)) = (vault_id, item_id, title) else {
            continue;
        };
        let reference = format!("op://{vault_id}/{item_id}/password");
        if !valid_vault_reference(VaultSource::OnePassword, &reference) {
            continue;
        }
        let urls = urls_of(item);
        out.push(ImportItem {
            source: VaultSource::OnePassword,
            reference,
            title: bounded(title, MAX_TITLE_CHARS).unwrap_or_default(),
            vault: item
                .pointer("/vault/name")
                .and_then(Value::as_str)
                .and_then(|name| bounded(name, MAX_VAULT_NAME_CHARS))
                .unwrap_or_default(),
            username: text_field(item, "additional_information")
                .and_then(|name| bounded(name, MAX_USERNAME_CHARS)),
            domain: first_domain(&urls),
            suggested_name: suggested_name(title),
        });
    }
    sort_by_title(&mut out);
    Ok(out)
}

/// Parse the Proton Pass listing. Accepts a bare array or `{"items": [...]}`.
/// Each item needs a `title` and a vault name (`vault_name`, or `vault.name`).
/// The reference is `pass://<vault>/<title>/password`, the notation
/// `pass-cli item view --vault-name --item-title` reads. The field names are
/// an assumption to confirm against the installed CLI.
pub fn parse_proton_items(bytes: &[u8]) -> Result<Vec<ImportItem>, ApiError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| ApiError::InvalidResponse)?;
    let list = match value.get("items") {
        Some(items) => items_of(items)?,
        None => items_of(&value)?,
    };
    let mut out = Vec::new();
    for item in list.iter().take(MAX_ITEMS) {
        let title = text_field(item, "title");
        let vault = text_field(item, "vault_name").or_else(|| {
            item.pointer("/vault/name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
        });
        let (Some(title), Some(vault)) = (title, vault) else {
            continue;
        };
        let reference = format!("pass://{vault}/{title}/password");
        if !valid_vault_reference(VaultSource::ProtonPass, &reference) {
            continue;
        }
        let urls = urls_of(item);
        out.push(ImportItem {
            source: VaultSource::ProtonPass,
            reference,
            title: bounded(title, MAX_TITLE_CHARS).unwrap_or_default(),
            vault: bounded(vault, MAX_VAULT_NAME_CHARS).unwrap_or_default(),
            username: text_field(item, "username")
                .and_then(|name| bounded(name, MAX_USERNAME_CHARS)),
            domain: first_domain(&urls),
            suggested_name: suggested_name(title),
        });
    }
    sort_by_title(&mut out);
    Ok(out)
}

/// URLs as strings. Accepts `"urls": ["https://…"]` and `[{"href": "…"}]`.
fn urls_of(item: &Value) -> Vec<&str> {
    item.get("urls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|url| {
            url.as_str()
                .or_else(|| url.get("href").and_then(Value::as_str))
        })
        .collect()
}

fn sort_by_title(items: &mut [ImportItem]) {
    items.sort_by(|a, b| {
        a.title
            .to_lowercase()
            .cmp(&b.title.to_lowercase())
            .then_with(|| a.reference.cmp(&b.reference))
    });
}

/// The first file named `name` on `PATH` (`name.exe` on Windows).
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(&file))
        .find(|candidate| candidate.is_file())
}

/// List the owner's logins from a password manager on this PC. Blocking: call
/// it from a worker thread, never the UI thread.
pub fn list_items(source: VaultSource) -> Result<Vec<ImportItem>, ApiError> {
    match source {
        VaultSource::OnePassword => {
            let program = find_on_path("op").ok_or(ApiError::Validation(
                "The 1Password CLI (op) is not on this PC. Install it from developer.1password.com/docs/cli/get-started, then sign in.",
            ))?;
            let bytes = run_listing(
                &program,
                &["item", "list", "--categories", "Login", "--format", "json"],
                "1Password did not list the logins. Sign in to the 1Password CLI on this PC and try again.",
            )?;
            parse_onepassword_items(&bytes)
        }
        VaultSource::ProtonPass => {
            let program = find_on_path("pass-cli").ok_or(ApiError::Validation(
                "The Proton Pass CLI (pass-cli) is not on this PC. Install it from protonpass.github.io/pass-cli, then sign in.",
            ))?;
            let bytes = run_listing(
                &program,
                &["item", "list", "--output", "json"],
                "Proton Pass did not list the logins. Sign in to pass-cli on this PC and try again.",
            )?;
            parse_proton_items(&bytes)
        }
        VaultSource::Local => Err(ApiError::Validation(
            "Only a password manager can be imported.",
        )),
    }
}

/// Run a listing command with no shell and a timeout. Stdout is the result;
/// stderr is discarded. A failed run shows `failure` and nothing the CLI said.
fn run_listing(program: &Path, args: &[&str], failure: &'static str) -> Result<Vec<u8>, ApiError> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ApiError::Validation("The password manager command could not be started."))?;
    let stdout = child.stdout.take();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(stdout) = stdout {
            let _ = stdout
                .take(MAX_LIST_BYTES as u64 + 1)
                .read_to_end(&mut bytes);
        }
        let _ = sender.send(bytes);
    });
    let deadline = Instant::now() + LIST_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ApiError::Validation(
                    "The password manager did not answer in time.",
                ));
            }
        }
    };
    let bytes = receiver
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| ApiError::InvalidResponse)?;
    if !status.success() || bytes.len() > MAX_LIST_BYTES {
        return Err(ApiError::Validation(failure));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONEPASSWORD: &str = r#"[
        {"id":"kx7qwe2rtyuiopasdfgh1234","title":"Bank Login","vault":{"id":"vlt0001","name":"Personal"},
         "category":"LOGIN","additional_information":"ada@example.example",
         "urls":[{"label":"website","primary":true,"href":"https://Bank.Example/login"}]},
        {"id":"abc123","title":"Notes only","vault":{"id":"vlt0001","name":"Personal"},"category":"LOGIN"},
        {"id":"","title":"No id","vault":{"id":"vlt0001"}},
        {"id":"zz9","title":"No vault id","vault":{}}
    ]"#;

    const PROTON: &str = r#"{"items":[
        {"id":"i1","title":"GitHub account","vault_name":"Work","username":"ada-dev",
         "urls":["https://github.com/login"]},
        {"id":"i2","title":"Forum","vault":{"name":"Personal"},"urls":[]},
        {"id":"i3","title":"Has/slash","vault_name":"Work"}
    ]}"#;

    #[test]
    fn onepassword_items_become_id_references_with_metadata_only() {
        let items = parse_onepassword_items(ONEPASSWORD.as_bytes()).unwrap();
        assert_eq!(items.len(), 2);
        let bank = items
            .iter()
            .find(|item| item.title == "Bank Login")
            .unwrap();
        assert_eq!(
            bank.reference,
            "op://vlt0001/kx7qwe2rtyuiopasdfgh1234/password"
        );
        assert_eq!(bank.username.as_deref(), Some("ada@example.example"));
        assert_eq!(bank.domain.as_deref(), Some("bank.example"));
        assert_eq!(bank.vault, "Personal");
        assert_eq!(bank.suggested_name, "bank-login");
        let notes = items
            .iter()
            .find(|item| item.title == "Notes only")
            .unwrap();
        assert!(notes.username.is_none() && notes.domain.is_none());
    }

    #[test]
    fn proton_items_accept_both_shapes_and_skip_unusable_titles() {
        let items = parse_proton_items(PROTON.as_bytes()).unwrap();
        let titles: Vec<&str> = items.iter().map(|item| item.title.as_str()).collect();
        assert_eq!(titles, ["Forum", "GitHub account"]);
        let github = items
            .iter()
            .find(|item| item.title == "GitHub account")
            .unwrap();
        assert_eq!(github.reference, "pass://Work/GitHub account/password");
        assert_eq!(github.username.as_deref(), Some("ada-dev"));
        assert_eq!(github.domain.as_deref(), Some("github.com"));
        let bare = parse_proton_items(br#"[{"title":"Solo","vault":{"name":"Work"}}]"#).unwrap();
        assert_eq!(bare[0].reference, "pass://Work/Solo/password");
    }

    #[test]
    fn malformed_listings_are_refused() {
        assert!(parse_onepassword_items(b"not json").is_err());
        assert!(parse_onepassword_items(br#"{"items":[]}"#).is_err());
        assert!(parse_proton_items(br#"{"items":"nope"}"#).is_err());
        assert!(parse_proton_items(b"[]").unwrap().is_empty());
    }

    #[test]
    fn suggested_names_are_valid_vault_names() {
        assert_eq!(suggested_name("GitHub account!"), "github-account");
        assert_eq!(suggested_name("  --  "), "login");
        assert_eq!(suggested_name("Café Bank"), "caf-bank");
        let long = suggested_name(&"Ab".repeat(60));
        assert!(valid_vault_name(&long) && long.len() <= 48);
    }

    #[test]
    fn unique_names_never_collide_and_stay_valid() {
        let taken = vec!["bank".to_owned(), "bank-2".to_owned()];
        assert_eq!(unique_name("forum", &taken), "forum");
        assert_eq!(unique_name("bank", &taken), "bank-3");
        let long = "x".repeat(48);
        let taken = vec![long.clone()];
        let next = unique_name(&long, &taken);
        assert!(valid_vault_name(&next) && next != long && next.len() <= 48);
    }

    #[test]
    fn references_follow_the_server_shape() {
        assert!(valid_vault_reference(
            VaultSource::OnePassword,
            "op://Vault/Item/password"
        ));
        assert!(valid_vault_reference(
            VaultSource::OnePassword,
            "op://Vault/Section/Item/password"
        ));
        assert!(!valid_vault_reference(
            VaultSource::OnePassword,
            "op://Vault/It em/password"
        ));
        assert!(!valid_vault_reference(
            VaultSource::OnePassword,
            "pass://Vault/Item/password"
        ));
        assert!(valid_vault_reference(
            VaultSource::ProtonPass,
            "pass://Work/GitHub account/password"
        ));
        assert!(!valid_vault_reference(
            VaultSource::ProtonPass,
            "op://Work/Item/password"
        ));
        assert!(!valid_vault_reference(
            VaultSource::ProtonPass,
            "pass://Work/Item"
        ));
        assert!(!valid_vault_reference(VaultSource::Local, "pass://a/b/c"));
    }

    #[test]
    fn a_reference_form_builds_a_write_without_a_value() {
        use crate::api::VaultKind;
        let mut form = crate::panel::VaultForm::new();
        form.name = "bank".into();
        form.source = VaultSource::OnePassword;
        form.reference = "op://vlt/item/password".into();
        form.domains = vec!["bank.example".into()];
        let (name, input) = form.build().unwrap();
        assert_eq!(name, "bank");
        assert!(input.value.is_empty());
        assert_eq!(input.reference.as_deref(), Some("op://vlt/item/password"));
        assert_eq!(input.source, VaultSource::OnePassword);
        // A bad pointer and a note are refused before anything is sent.
        form.reference = "op://vlt/item".into();
        assert!(form.build().is_err());
        form.reference = "op://vlt/item/password".into();
        form.kind = VaultKind::Note;
        assert!(form.build().is_err());
    }

    #[test]
    fn the_cli_is_looked_up_only_on_path() {
        assert!(find_on_path("serabot-no-such-cli-0123").is_none());
        assert!(list_items(VaultSource::Local).is_err());
    }
}
