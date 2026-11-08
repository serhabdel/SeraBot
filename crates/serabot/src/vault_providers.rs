//! Password manager references (0.23): a vault entry may point at an item in
//! the owner's 1Password or Proton Pass account instead of holding a sealed
//! value. The value is read through that account's own CLI when the browser
//! types it. It is never stored, logged or returned by the API.
//!
//! The CLI runs with an argument array, never through a shell. The owner's
//! access token is passed only in the child's environment. Stdout is the
//! value, stderr is discarded, and only the exit code is logged, so no
//! provider message can reach a log line or an error.
//!
//! The argument shapes are in [`read_args`], the one place to change when a
//! CLI release renames a flag. The 1Password form (`op read --no-newline`) is
//! the documented one. The Proton Pass `pass-cli item view` flags are not
//! verified against every release.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use secrecy::ExposeSecret;
use serde::Serialize;
use zeroize::{Zeroize, Zeroizing};

use crate::config::VaultConfig;
use crate::error::{Result, SeraError};
use crate::user_secrets::UserSecrets;
use crate::vault::{self, VaultField, VaultSource};

/// How long one CLI call may run before it is killed.
pub const CLI_TIMEOUT: Duration = Duration::from_secs(15);
/// Largest stdout accepted from a CLI. A password or key is far smaller.
const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_REFERENCE_CHARS: usize = 512;

/// A password manager an entry can read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    OnePassword,
    ProtonPass,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::OnePassword, Provider::ProtonPass];

    pub fn from_source(source: VaultSource) -> Option<Self> {
        match source {
            VaultSource::Local => None,
            VaultSource::OnePassword => Some(Self::OnePassword),
            VaultSource::ProtonPass => Some(Self::ProtonPass),
        }
    }

    /// The provider id used in routes and in the sealed token's id
    /// (`vaultprov:<id>`).
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|provider| provider.id() == id)
    }

    pub fn id(self) -> &'static str {
        self.source().as_str()
    }

    pub fn source(self) -> VaultSource {
        match self {
            Self::OnePassword => VaultSource::OnePassword,
            Self::ProtonPass => VaultSource::ProtonPass,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::OnePassword => "1Password",
            Self::ProtonPass => "Proton Pass",
        }
    }

    /// The executable looked up on `PATH` when no path is configured.
    pub fn cli_name(self) -> &'static str {
        match self {
            Self::OnePassword => "op",
            Self::ProtonPass => "pass-cli",
        }
    }

    /// The environment variable that carries the owner's access token.
    pub fn token_env(self) -> &'static str {
        match self {
            Self::OnePassword => "OP_SERVICE_ACCOUNT_TOKEN",
            Self::ProtonPass => "PROTON_PASS_PERSONAL_ACCESS_TOKEN",
        }
    }

    pub fn install_url(self) -> &'static str {
        match self {
            Self::OnePassword => "https://developer.1password.com/docs/cli/get-started/",
            Self::ProtonPass => "https://protonpass.github.io/pass-cli/",
        }
    }

    fn configured_path(self, config: &VaultConfig) -> &str {
        match self {
            Self::OnePassword => &config.op_path,
            Self::ProtonPass => &config.pass_cli_path,
        }
    }

    /// The reference scheme this provider uses.
    fn scheme(self) -> &'static str {
        match self {
            Self::OnePassword => "op://",
            Self::ProtonPass => "pass://",
        }
    }
}

/// Check the shape of a password manager reference. 1Password references are
/// `op://vault/item/field` (or with a section, four segments). Proton Pass
/// references use this server's own notation, `pass://vault/item/field`.
pub fn validate_reference(source: VaultSource, reference: &str) -> Result<()> {
    let provider = Provider::from_source(source).ok_or_else(|| {
        SeraError::Validation("only a password manager entry takes a reference".into())
    })?;
    if reference.chars().count() > MAX_REFERENCE_CHARS {
        return Err(SeraError::Validation("the reference is too long".into()));
    }
    // Proton Pass titles may hold spaces; the CLI gets them as one argument.
    // 1Password references never do.
    let spaces_ok = provider == Provider::ProtonPass;
    if reference
        .chars()
        .any(|c| c.is_control() || (!spaces_ok && c.is_whitespace()))
    {
        return Err(SeraError::Validation(
            "the reference must not contain control characters, or spaces for 1Password".into(),
        ));
    }
    let segments: Vec<&str> = reference
        .strip_prefix(provider.scheme())
        .unwrap_or_default()
        .split('/')
        .collect();
    let count_ok = match provider {
        Provider::OnePassword => (3..=4).contains(&segments.len()),
        Provider::ProtonPass => segments.len() == 3,
    };
    if !count_ok || segments.iter().any(|segment| segment.is_empty()) {
        return Err(SeraError::Validation(format!(
            "the reference must look like {}vault/item/field",
            provider.scheme()
        )));
    }
    Ok(())
}

/// The CLI arguments that print one reference's value to stdout. Pure, so
/// the shape is tested without a CLI. The access token is never an argument.
pub fn read_args(provider: Provider, reference: &str) -> Result<Vec<String>> {
    validate_reference(provider.source(), reference)?;
    let rest = reference
        .strip_prefix(provider.scheme())
        .unwrap_or_default();
    match provider {
        Provider::OnePassword => Ok(vec![
            "read".to_owned(),
            "--no-newline".to_owned(),
            reference.to_owned(),
        ]),
        Provider::ProtonPass => {
            let parts: Vec<&str> = rest.split('/').collect();
            let [vault, item, field] = parts.as_slice() else {
                return Err(SeraError::Validation(
                    "the reference must look like pass://vault/item/field".into(),
                ));
            };
            Ok(vec![
                "item".to_owned(),
                "view".to_owned(),
                "--vault-name".to_owned(),
                (*vault).to_owned(),
                "--item-title".to_owned(),
                (*item).to_owned(),
                "--field".to_owned(),
                (*field).to_owned(),
            ])
        }
    }
}

/// What a CLI call returned: its exit code, and its stdout as the value.
pub struct CommandOutput {
    pub status: Option<i32>,
    pub stdout: Zeroizing<String>,
}

/// Runs a password manager CLI. The production impl spawns the binary; tests
/// substitute a fake, so no test ever calls a live account.
pub trait SecretCommand: Send + Sync {
    fn run(&self, program: &Path, args: &[String], env: &[(&str, &str)]) -> Result<CommandOutput>;
}

/// Spawns the CLI directly (no shell), with a timeout that kills it.
pub struct ProcessCommand {
    pub timeout: Duration,
}

impl Default for ProcessCommand {
    fn default() -> Self {
        Self {
            timeout: CLI_TIMEOUT,
        }
    }
}

impl SecretCommand for ProcessCommand {
    fn run(&self, program: &Path, args: &[String], env: &[(&str, &str)]) -> Result<CommandOutput> {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().map_err(|_| {
            SeraError::External("the password manager command could not be started".into())
        })?;
        // Read stdout on its own thread so a full pipe cannot block the wait.
        let (sender, receiver) = std::sync::mpsc::channel();
        let stdout = child.stdout.take();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(stdout) = stdout {
                let _ = stdout
                    .take(MAX_OUTPUT_BYTES as u64 + 1)
                    .read_to_end(&mut bytes);
            }
            let _ = sender.send(bytes);
        });
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(SeraError::External(
                        "the password manager did not answer in time".into(),
                    ));
                }
                Err(_) => {
                    let _ = child.kill();
                    return Err(SeraError::External(
                        "the password manager command failed".into(),
                    ));
                }
            }
        };
        // The pipe closes with the child; the bound keeps a stray grandchild
        // that holds it open from stalling the call.
        let mut bytes = receiver
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| SeraError::External("the password manager output was lost".into()))?;
        if bytes.len() > MAX_OUTPUT_BYTES {
            bytes.zeroize();
            return Err(SeraError::External(
                "the password manager returned too much data".into(),
            ));
        }
        let stdout = match String::from_utf8(bytes) {
            Ok(text) => Zeroizing::new(text),
            Err(error) => {
                let mut bytes = error.into_bytes();
                bytes.zeroize();
                return Err(SeraError::External(
                    "the password manager returned data that is not text".into(),
                ));
            }
        };
        Ok(CommandOutput {
            status: status.code(),
            stdout,
        })
    }
}

/// The first file named `name` on `PATH` (`name.exe` on Windows).
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let extensions: &[&str] = if cfg!(windows) { &[".exe"] } else { &[""] };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .flat_map(|dir| {
            extensions
                .iter()
                .map(move |extension| dir.join(format!("{name}{extension}")))
        })
        .find(|candidate| candidate.is_file())
}

/// The CLI for `provider`: the configured path when one is set, else the
/// `PATH` lookup. `None` when it is not installed.
pub fn program_for(provider: Provider, config: &VaultConfig) -> Option<PathBuf> {
    let configured = provider.configured_path(config).trim();
    if configured.is_empty() {
        return find_on_path(provider.cli_name());
    }
    let path = PathBuf::from(configured);
    path.is_file().then_some(path)
}

/// Reads references through a CLI. Built once at startup from `[vault]`.
pub struct Providers {
    config: VaultConfig,
    runner: Arc<dyn SecretCommand>,
}

impl Providers {
    pub fn new(config: &VaultConfig) -> Self {
        Self::with_runner(config, Arc::new(ProcessCommand::default()))
    }

    pub fn with_runner(config: &VaultConfig, runner: Arc<dyn SecretCommand>) -> Self {
        Self {
            config: config.clone(),
            runner,
        }
    }

    pub fn cli_found(&self, provider: Provider) -> bool {
        program_for(provider, &self.config).is_some()
    }

    /// Read one reference's value with the owner's token. The value is
    /// returned to the caller and not kept anywhere.
    pub async fn read(
        &self,
        provider: Provider,
        reference: &str,
        token: &str,
    ) -> Result<Zeroizing<String>> {
        let args = read_args(provider, reference)?;
        let program = program_for(provider, &self.config).ok_or_else(|| {
            SeraError::External(format!(
                "the {} CLI is not installed on the server",
                provider.label()
            ))
        })?;
        let runner = self.runner.clone();
        let token = Zeroizing::new(token.to_owned());
        let env_name = provider.token_env();
        let output = tokio::task::spawn_blocking(move || {
            runner.run(&program, &args, &[(env_name, token.as_str())])
        })
        .await
        .map_err(|_| SeraError::Internal("the password manager task failed".into()))??;
        if output.status != Some(0) {
            tracing::warn!(
                provider = provider.id(),
                exit_code = ?output.status,
                "password manager read failed"
            );
            return Err(SeraError::External(format!(
                "{} could not return this item",
                provider.label()
            )));
        }
        let value = Zeroizing::new(output.stdout.trim_end_matches(['\r', '\n']).to_owned());
        if value.is_empty() {
            return Err(SeraError::External(format!(
                "the {} item has no value",
                provider.label()
            )));
        }
        Ok(value)
    }
}

/// Save the owner's access token for `provider`, sealed per user.
pub fn set_token(
    users: &UserSecrets,
    user_id: &str,
    provider: Provider,
    token: &str,
) -> Result<()> {
    users.set_vault_provider_token(user_id, provider.id(), token)
}

/// Remove the owner's access token for `provider`. False when none was set.
pub fn clear_token(users: &UserSecrets, user_id: &str, provider: Provider) -> Result<bool> {
    users.delete_vault_provider_token(user_id, provider.id())
}

/// One provider as the API and the desktop see it. Never a token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderStatus {
    pub id: &'static str,
    pub name: &'static str,
    /// The owner has a token saved.
    pub configured: bool,
    /// The CLI is installed on the server.
    pub cli_found: bool,
    pub install_url: &'static str,
}

pub fn status(
    users: &UserSecrets,
    providers: &Providers,
    user_id: &str,
    provider: Provider,
) -> Result<ProviderStatus> {
    Ok(ProviderStatus {
        id: provider.id(),
        name: provider.label(),
        configured: users
            .vault_provider_token(user_id, provider.id())?
            .is_some(),
        cli_found: providers.cli_found(provider),
        install_url: provider.install_url(),
    })
}

/// The browser fill for one entry: authorize the host, then read the value
/// from the sealed store (local) or from the owner's password manager
/// (reference). The fill is recorded as a `vault.resolve` audit row with the
/// name and host only.
pub async fn resolve_for_browser(
    users: &UserSecrets,
    providers: &Providers,
    user_id: &str,
    name: &str,
    host: &str,
    field: VaultField,
) -> Result<Zeroizing<String>> {
    let entry = vault::authorize_fill(users, user_id, name, host, field)?;
    let value = match (field, Provider::from_source(entry.source)) {
        (VaultField::Username, _) => vault::username_of(&entry)?,
        (VaultField::Value, None) => vault::local_value(users, user_id, name)?,
        (VaultField::Value, Some(provider)) => {
            let token = users
                .vault_provider_token(user_id, provider.id())?
                .ok_or_else(|| {
                    SeraError::Validation(format!(
                        "save your {} access token in the Vault first",
                        provider.label()
                    ))
                })?;
            let reference = entry.reference.as_deref().ok_or_else(|| {
                SeraError::InvalidState("a password manager entry has no reference".into())
            })?;
            providers
                .read(provider, reference, token.expose_secret())
                .await?
        }
    };
    vault::record_fill(users, user_id, name, host)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::user_secrets::KEY_BYTES;
    use crate::vault::{VaultInput, VaultKind};
    use std::sync::Mutex;

    /// Program, arguments and child environment of one recorded call.
    type RecordedCall = (String, Vec<String>, Vec<(String, String)>);

    /// A fake CLI: records each call and answers with a canned result.
    #[derive(Default)]
    struct FakeCommand {
        calls: Mutex<Vec<RecordedCall>>,
        status: Option<i32>,
        stdout: String,
    }

    impl SecretCommand for FakeCommand {
        fn run(
            &self,
            program: &Path,
            args: &[String],
            env: &[(&str, &str)],
        ) -> Result<CommandOutput> {
            self.calls.lock().unwrap().push((
                program.display().to_string(),
                args.to_vec(),
                env.iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            ));
            Ok(CommandOutput {
                status: self.status,
                stdout: Zeroizing::new(self.stdout.clone()),
            })
        }
    }

    fn fake(status: Option<i32>, stdout: &str) -> Arc<FakeCommand> {
        Arc::new(FakeCommand {
            calls: Mutex::default(),
            status,
            stdout: stdout.to_owned(),
        })
    }

    /// A config that points both CLIs at paths that do not exist, so nothing
    /// depends on what is installed on the machine running the tests.
    fn config() -> VaultConfig {
        VaultConfig {
            op_path: "Z:/synthetic/op-missing.exe".into(),
            pass_cli_path: "Z:/synthetic/pass-cli-missing.exe".into(),
        }
    }

    fn setup() -> (Store, UserSecrets, String) {
        let store = Store::open_in_memory(384).unwrap();
        let users = UserSecrets::with_master_key(store.clone(), [5u8; KEY_BYTES]).unwrap();
        let user = store
            .create_user("subject-vp", "vp@example.example", "Synthetic VP")
            .unwrap()
            .id;
        (store, users, user)
    }

    fn reference_entry(source: VaultSource, reference: &str, domains: &[&str]) -> VaultInput {
        VaultInput {
            kind: VaultKind::Password,
            username: Some("synthetic-user".into()),
            value: Zeroizing::new(String::new()),
            domains: domains.iter().map(|d| (*d).to_owned()).collect(),
            note: None,
            source,
            reference: Some(reference.into()),
        }
    }

    #[test]
    fn onepassword_references_need_three_or_four_segments() {
        let ok = [
            "op://Vault/Item/password",
            "op://Vault/Section/Item/password",
        ];
        for reference in ok {
            validate_reference(VaultSource::OnePassword, reference).unwrap();
        }
        let bad = [
            "op://Vault/Item",
            "op://Vault/Item/",
            "op://a/b/c/d/e",
            "pass://Vault/Item/password",
            "op://Vault/It em/password",
            "op://Vault/Item/pass\nword",
            "",
        ];
        for reference in bad {
            assert!(
                validate_reference(VaultSource::OnePassword, reference).is_err(),
                "{reference}"
            );
        }
    }

    #[test]
    fn proton_pass_references_need_exactly_three_segments() {
        validate_reference(VaultSource::ProtonPass, "pass://Personal/github/password").unwrap();
        validate_reference(
            VaultSource::ProtonPass,
            "pass://Work/GitHub account/password",
        )
        .unwrap();
        assert!(validate_reference(VaultSource::OnePassword, "op://Vault/It em/password").is_err());
        for reference in [
            "op://Personal/github/password",
            "pass://Personal/github",
            "pass://Personal/github/password/extra",
            "pass://Personal//password",
        ] {
            assert!(
                validate_reference(VaultSource::ProtonPass, reference).is_err(),
                "{reference}"
            );
        }
        assert!(validate_reference(VaultSource::Local, "pass://a/b/c").is_err());
    }

    #[test]
    fn read_arguments_are_fixed_shapes_without_the_token() {
        assert_eq!(
            read_args(Provider::OnePassword, "op://Vault/Item/password").unwrap(),
            ["read", "--no-newline", "op://Vault/Item/password"]
        );
        assert_eq!(
            read_args(Provider::ProtonPass, "pass://Personal/github/password").unwrap(),
            [
                "item",
                "view",
                "--vault-name",
                "Personal",
                "--item-title",
                "github",
                "--field",
                "password"
            ]
        );
        assert!(read_args(Provider::ProtonPass, "pass://a/b").is_err());
    }

    /// A config whose 1Password path exists (this test binary), so `read`
    /// finds a program. The fake runner never executes it.
    fn config_with_op() -> VaultConfig {
        let mut config = config();
        config.op_path = std::env::current_exe().unwrap().display().to_string();
        config
    }

    #[tokio::test]
    async fn read_passes_the_token_only_in_the_child_environment() {
        let runner = fake(Some(0), "synthetic-value\n");
        let providers = Providers::with_runner(&config_with_op(), runner.clone());
        let value = providers
            .read(
                Provider::OnePassword,
                "op://Vault/Item/password",
                "synthetic-token",
            )
            .await
            .unwrap();
        assert_eq!(value.as_str(), "synthetic-value");
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].1,
            ["read", "--no-newline", "op://Vault/Item/password"]
        );
        assert_eq!(
            calls[0].2,
            [(
                "OP_SERVICE_ACCOUNT_TOKEN".to_owned(),
                "synthetic-token".to_owned()
            )]
        );
        assert!(!calls[0].1.iter().any(|arg| arg.contains("synthetic-token")));
    }

    #[tokio::test]
    async fn a_failed_read_reports_only_the_exit_code() {
        let providers =
            Providers::with_runner(&config_with_op(), fake(Some(1), "synthetic-leak-value"));
        let error = providers
            .read(Provider::OnePassword, "op://Vault/Item/password", "t")
            .await
            .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("could not return this item"));
        assert!(!text.contains("synthetic-leak-value"));
    }

    #[tokio::test]
    async fn an_empty_value_is_refused_and_a_missing_cli_is_named() {
        let providers = Providers::with_runner(&config_with_op(), fake(Some(0), "\n"));
        assert!(
            providers
                .read(Provider::OnePassword, "op://Vault/Item/password", "t")
                .await
                .is_err()
        );
        let missing = Providers::with_runner(&config(), fake(Some(0), "x"));
        let error = missing
            .read(Provider::ProtonPass, "pass://a/b/c", "t")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Proton Pass"));
    }

    #[test]
    fn a_configured_path_must_exist_and_a_missing_one_is_not_found() {
        assert!(program_for(Provider::OnePassword, &config()).is_none());
        let mut found = config();
        found.op_path = std::env::current_exe().unwrap().display().to_string();
        assert!(program_for(Provider::OnePassword, &found).is_some());
        assert!(find_on_path("serabot-no-such-cli-0123").is_none());
    }

    #[test]
    fn provider_ids_and_sources_round_trip() {
        for provider in Provider::ALL {
            assert_eq!(Provider::from_id(provider.id()), Some(provider));
            assert_eq!(Provider::from_source(provider.source()), Some(provider));
        }
        assert_eq!(Provider::from_id("vault"), None);
        assert_eq!(Provider::from_source(VaultSource::Local), None);
    }

    #[tokio::test]
    async fn resolve_reads_the_reference_for_an_allowed_host_only() {
        let (_, users, user) = setup();
        let runner = fake(Some(0), "synthetic-op-value");
        let providers = Providers::with_runner(&config_with_op(), runner.clone());
        vault::put(
            &users,
            &user,
            "bank",
            &reference_entry(
                VaultSource::OnePassword,
                "op://Vault/Bank/password",
                &["bank.example"],
            ),
        )
        .unwrap();
        // No token yet: refused, and the CLI is not called.
        assert!(
            resolve_for_browser(
                &users,
                &providers,
                &user,
                "bank",
                "bank.example",
                VaultField::Value
            )
            .await
            .is_err()
        );
        assert!(runner.calls.lock().unwrap().is_empty());
        users
            .set_vault_provider_token(&user, "onepassword", "synthetic-token")
            .unwrap();
        let value = resolve_for_browser(
            &users,
            &providers,
            &user,
            "bank",
            "login.bank.example",
            VaultField::Value,
        )
        .await
        .unwrap();
        assert_eq!(value.as_str(), "synthetic-op-value");
        // A reference is never stored as a value.
        assert!(users.get_vault(&user, "bank").unwrap().is_none());
        // A host outside the entry's sites is refused before the CLI runs.
        assert!(matches!(
            resolve_for_browser(
                &users,
                &providers,
                &user,
                "bank",
                "evil.example",
                VaultField::Value
            )
            .await,
            Err(SeraError::Forbidden(_))
        ));
        assert_eq!(runner.calls.lock().unwrap().len(), 1);
        let entry = vault::get(&users, &user, "bank").unwrap().unwrap();
        assert!(entry.last_used_at.is_some());
        assert_eq!(entry.source, VaultSource::OnePassword);
    }

    #[tokio::test]
    async fn username_fill_needs_no_cli_and_no_token() {
        let (_, users, user) = setup();
        let runner = fake(Some(0), "unused");
        let providers = Providers::with_runner(&config(), runner.clone());
        vault::put(
            &users,
            &user,
            "bank",
            &reference_entry(
                VaultSource::ProtonPass,
                "pass://Personal/bank/password",
                &["bank.example"],
            ),
        )
        .unwrap();
        let name = resolve_for_browser(
            &users,
            &providers,
            &user,
            "bank",
            "bank.example",
            VaultField::Username,
        )
        .await
        .unwrap();
        assert_eq!(name.as_str(), "synthetic-user");
        assert!(runner.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_local_entry_is_read_from_the_sealed_store() {
        let (_, users, user) = setup();
        let providers = Providers::with_runner(&config(), fake(Some(0), "unused"));
        vault::put(
            &users,
            &user,
            "site",
            &VaultInput {
                kind: VaultKind::Password,
                username: None,
                value: Zeroizing::new("synthetic-local-value".into()),
                domains: vec!["example.com".into()],
                note: None,
                source: VaultSource::Local,
                reference: None,
            },
        )
        .unwrap();
        let value = resolve_for_browser(
            &users,
            &providers,
            &user,
            "site",
            "example.com",
            VaultField::Value,
        )
        .await
        .unwrap();
        assert_eq!(value.as_str(), "synthetic-local-value");
    }

    #[test]
    fn tokens_are_per_user_and_never_listed_as_providers() {
        let (store, users, user) = setup();
        let other = store
            .create_user("subject-vp-b", "vpb@example.example", "Synthetic VPB")
            .unwrap()
            .id;
        set_token(&users, &user, Provider::OnePassword, "synthetic-token").unwrap();
        assert!(
            users
                .vault_provider_token(&other, "onepassword")
                .unwrap()
                .is_none()
        );
        assert!(users.providers_for(&user).unwrap().is_empty());
        assert!(users.get(&user, "vaultprov:onepassword").is_err());
        let status = status(
            &users,
            &Providers::new(&config()),
            &user,
            Provider::OnePassword,
        )
        .unwrap();
        assert!(status.configured && !status.cli_found);
        assert!(clear_token(&users, &user, Provider::OnePassword).unwrap());
        assert!(!clear_token(&users, &user, Provider::OnePassword).unwrap());
    }
}
