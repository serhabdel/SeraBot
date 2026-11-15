//! Password manager references (0.23): a vault entry may point at an item in
//! the owner's 1Password or Proton Pass account instead of holding a sealed
//! value. The value is read through that account's own CLI when the browser
//! types it. It is never stored, logged or returned by the API.
//!
//! The CLI runs with an argument array, never through a shell, and with a
//! cleared environment: the owner's access token and a private, per-call home
//! directory are all it gets beyond `PATH`. Stdout is the value, stderr is
//! discarded, and only the exit code is logged, so no provider message can
//! reach a log line or an error. A value read is remembered in memory for the
//! model redactor (see [`ResolvedValues`]).
//!
//! The argument shapes are in [`read_args`], the one place to change when a
//! CLI release renames a flag. The 1Password form (`op read --no-newline`) is
//! the documented one. The Proton Pass `pass-cli item view` flags are not
//! verified against every release.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::config::VaultConfig;
use crate::error::{Result, SeraError};
use crate::user_secrets::UserSecrets;
use crate::vault::{self, VaultEntry, VaultField, VaultSource};

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

    /// Whether the server reads from this manager. Proton Pass is off unless
    /// `[vault] protonpass_enabled` is set.
    pub fn enabled(self, config: &VaultConfig) -> bool {
        match self {
            Self::OnePassword => true,
            Self::ProtonPass => config.protonpass_enabled,
        }
    }

    fn lock_index(self) -> usize {
        match self {
            Self::OnePassword => 0,
            Self::ProtonPass => 1,
        }
    }
}

/// Check the shape of a password manager reference. 1Password references are
/// `op://vault/item/field` (or with a section, four segments), optionally
/// ending in `?attribute=<name>`. Proton Pass references use this server's own
/// notation, `pass://vault/item/field`, with no query. No segment may start
/// with `-`, so a CLI can never read it as an option.
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
    let (path, query) = match reference.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (reference, None),
    };
    if let Some(query) = query {
        let attribute = query.strip_prefix("attribute=").unwrap_or_default();
        let word_ok = !attribute.is_empty()
            && attribute.len() <= 32
            && attribute
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
        if provider != Provider::OnePassword || !word_ok {
            return Err(SeraError::Validation(
                "only a 1Password reference may end with ?attribute=<name>".into(),
            ));
        }
    }
    let segments: Vec<&str> = path
        .strip_prefix(provider.scheme())
        .unwrap_or_default()
        .split('/')
        .collect();
    let count_ok = match provider {
        Provider::OnePassword => (3..=4).contains(&segments.len()),
        Provider::ProtonPass => segments.len() == 3,
    };
    if !count_ok
        || segments
            .iter()
            .any(|segment| segment.is_empty() || segment.starts_with('-'))
    {
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

/// Variables a CLI reads for its config, cache or data. Each one points at the
/// call's private home, so nothing the CLI writes lands in the server's own
/// profile.
const HOME_VARS: &[&str] = &[
    "HOME",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "APPDATA",
    "LOCALAPPDATA",
    "USERPROFILE",
    "TEMP",
    "TMP",
];

/// The environment a password manager CLI runs in. Nothing is inherited but
/// `PATH` (and `SYSTEMROOT` on Windows, which system calls need). Then every
/// home variable points at `home`, then the caller's variables (the token).
fn child_environment(home: &Path, extra: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    let mut inherited = vec!["PATH"];
    if cfg!(windows) {
        inherited.push("SYSTEMROOT");
    }
    let mut env: Vec<(OsString, OsString)> = inherited
        .into_iter()
        .filter_map(|key| std::env::var_os(key).map(|value| (key.into(), value)))
        .collect();
    env.extend(
        HOME_VARS
            .iter()
            .map(|key| ((*key).into(), home.as_os_str().to_owned())),
    );
    env.extend(
        extra
            .iter()
            .map(|(key, value)| ((*key).into(), (*value).into())),
    );
    env
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

fn run_in_home(
    home: &Path,
    program: &Path,
    args: &[String],
    env: &[(&str, &str)],
    timeout: Duration,
) -> Result<CommandOutput> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .envs(child_environment(home, env))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().map_err(|_| {
        SeraError::External("the password manager command could not be started".into())
    })?;
    // Read stdout on its own thread so a full pipe cannot block the wait.
    // The buffer is sized up front and never grows, so no stale copy of
    // the value is left behind by a reallocation.
    let (sender, receiver) = std::sync::mpsc::channel();
    let stdout = child.stdout.take();
    std::thread::spawn(move || {
        let mut bytes = Vec::with_capacity(MAX_OUTPUT_BYTES + 1);
        if let Some(mut stdout) = stdout {
            let mut chunk = [0u8; 4096];
            while bytes.len() <= MAX_OUTPUT_BYTES {
                let room = (MAX_OUTPUT_BYTES + 1 - bytes.len()).min(chunk.len());
                match stdout.read(&mut chunk[..room]) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => bytes.extend_from_slice(&chunk[..read]),
                }
            }
            chunk.zeroize();
        }
        // The receiver is gone when the caller timed out: wipe what was read.
        if let Err(error) = sender.send(bytes) {
            let mut bytes = error.0;
            bytes.zeroize();
        }
    });
    let deadline = Instant::now() + timeout;
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
                let _ = child.wait();
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

impl SecretCommand for ProcessCommand {
    fn run(&self, program: &Path, args: &[String], env: &[(&str, &str)]) -> Result<CommandOutput> {
        // Private to this call and removed when it returns. The child has
        // exited by then on every path below.
        let home = tempfile::TempDir::new().map_err(|_| {
            SeraError::External("the password manager command could not be started".into())
        })?;
        let output = run_in_home(home.path(), program, args, env, self.timeout)?;
        if output.status != Some(0)
            && env
                .iter()
                .any(|(k, _)| *k == "PROTON_PASS_PERSONAL_ACCESS_TOKEN")
            && args.first().map(String::as_str) != Some("login")
        {
            let _ = run_in_home(
                home.path(),
                program,
                &["login".to_owned()],
                env,
                self.timeout,
            );
            return run_in_home(home.path(), program, args, env, self.timeout);
        }
        Ok(output)
    }
}

/// Metadata fetch timeout for CLI auto-install.
const INSTALL_METADATA_TIMEOUT: Duration = Duration::from_secs(5);
/// Binary/archive download timeout for CLI auto-install.
const INSTALL_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum CLI archive or binary payload accepted (100 MiB).
const MAX_INSTALL_PAYLOAD_BYTES: usize = 100 * 1024 * 1024;
const PROTON_VERSIONS_URL: &str = "https://proton.me/download/pass-cli/versions.json";
const ONEPASSWORD_VERSION_URL: &str = "https://app-updates.agilebits.com/check/1/0/CLI2/en/2.0.0/N";

/// Executable filename for `provider` on the current OS.
pub fn executable_name(provider: Provider) -> String {
    if cfg!(windows) {
        format!("{}.exe", provider.cli_name())
    } else {
        provider.cli_name().to_owned()
    }
}

/// Persistent server-local bin directory for auto-installed password manager CLIs.
pub fn managed_bin_dir(config: &VaultConfig) -> PathBuf {
    let configured = config.bin_dir.trim();
    if !configured.is_empty() {
        return PathBuf::from(configured);
    }
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .map(|p| PathBuf::from(p).join("AppData").join("Local"))
            })
            .unwrap_or_else(std::env::temp_dir)
            .join("serabot")
            .join("bin")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local").join("share"))
            })
            .unwrap_or_else(std::env::temp_dir)
            .join("serabot")
            .join("bin")
    }
}

/// Full path to the managed CLI binary inside [`managed_bin_dir`].
pub fn managed_bin_path(provider: Provider, config: &VaultConfig) -> PathBuf {
    managed_bin_dir(config).join(executable_name(provider))
}

fn auto_install_disabled() -> bool {
    std::env::var("SERABOT_DISABLE_CLI_AUTO_INSTALL")
        .map(|val| {
            matches!(
                val.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        })
        .unwrap_or(false)
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

/// The CLI for `provider`: the configured path when one is set, else `PATH`
/// lookup, else the server-managed bin directory. `None` when not installed.
pub fn program_for(provider: Provider, config: &VaultConfig) -> Option<PathBuf> {
    let configured = provider.configured_path(config).trim();
    if !configured.is_empty() {
        let path = PathBuf::from(configured);
        return path.is_file().then_some(path);
    }
    if let Some(found) = find_on_path(provider.cli_name()) {
        return Some(found);
    }
    let managed = managed_bin_path(provider, config);
    managed.is_file().then_some(managed)
}

fn validate_download_url(raw: &str, allowed_domains: &[&str]) -> Result<url::Url> {
    let parsed = url::Url::parse(raw)
        .map_err(|err| SeraError::External(format!("invalid CLI download URL: {err}")))?;
    if parsed.scheme() != "https" {
        return Err(SeraError::External(
            "CLI download URL must use HTTPS".into(),
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| SeraError::External("CLI download URL has no host".into()))?
        .to_ascii_lowercase();
    let allowed = allowed_domains.iter().any(|domain| {
        let d = domain.to_ascii_lowercase();
        host == d || host.ends_with(&format!(".{d}"))
    });
    if !allowed {
        return Err(SeraError::External(format!(
            "CLI download host '{host}' is not on the allowlist"
        )));
    }
    Ok(parsed)
}

fn allowed_domains_for(provider: Provider) -> &'static [&'static str] {
    match provider {
        Provider::OnePassword => &["agilebits.com"],
        Provider::ProtonPass => &["proton.me"],
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProtonVersionsManifest {
    pass_cli_versions: ProtonPassCliVersions,
}

#[derive(Deserialize)]
struct ProtonPassCliVersions {
    version: String,
    urls: HashMap<String, HashMap<String, ProtonPlatformArtifact>>,
}

#[derive(Deserialize)]
struct ProtonPlatformArtifact {
    url: String,
    hash: String,
}

fn proton_platform_keys(os: &str, arch: &str) -> Result<(&'static str, &'static str)> {
    let os_key = match os {
        "linux" => "linux",
        "macos" => "macos",
        "windows" => "windows",
        other => {
            return Err(SeraError::External(format!(
                "Proton Pass CLI auto-install is unsupported on OS '{other}'"
            )));
        }
    };
    let arch_key = match arch {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => {
            return Err(SeraError::External(format!(
                "Proton Pass CLI auto-install is unsupported on arch '{other}'"
            )));
        }
    };
    Ok((os_key, arch_key))
}

fn parse_proton_manifest(bytes: &[u8], os: &str, arch: &str) -> Result<(String, String, String)> {
    let manifest: ProtonVersionsManifest = serde_json::from_slice(bytes).map_err(|err| {
        SeraError::External(format!("invalid Proton Pass CLI manifest JSON: {err}"))
    })?;
    let (os_key, arch_key) = proton_platform_keys(os, arch)?;
    let artifact = manifest
        .pass_cli_versions
        .urls
        .get(os_key)
        .and_then(|arches| arches.get(arch_key))
        .ok_or_else(|| {
            SeraError::External(format!(
                "no Proton Pass CLI build listed for {os_key}/{arch_key}"
            ))
        })?;
    Ok((
        manifest.pass_cli_versions.version,
        artifact.url.clone(),
        artifact.hash.clone(),
    ))
}

#[derive(Deserialize)]
struct OnePasswordVersionResponse {
    version: String,
}

fn parse_onepassword_version(bytes: &[u8]) -> Result<String> {
    let resp: OnePasswordVersionResponse = serde_json::from_slice(bytes)
        .map_err(|err| SeraError::External(format!("invalid 1Password CLI version JSON: {err}")))?;
    let version = resp.version.trim().to_owned();
    let valid = !version.is_empty()
        && version.len() <= 32
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    if !valid {
        return Err(SeraError::External(
            "1Password CLI version string is invalid".into(),
        ));
    }
    Ok(version)
}

fn onepassword_download_url(version: &str, os: &str, arch: &str) -> Result<String> {
    let target = match (os, arch) {
        ("linux", "x86_64") => "linux_amd64",
        ("linux", "aarch64") => "linux_arm64",
        ("macos", "x86_64") => "darwin_amd64",
        ("macos", "aarch64") => "darwin_arm64",
        ("windows", "x86_64") => "windows_amd64",
        _ => {
            return Err(SeraError::External(format!(
                "1Password CLI auto-install is unsupported on {os}/{arch}"
            )));
        }
    };
    Ok(format!(
        "https://cache.agilebits.com/dist/1P/op2/pkg/v{version}/op_{target}_v{version}.zip"
    ))
}

fn verify_sha256(bytes: &[u8], expected_hex: &str) -> Result<()> {
    let expected = expected_hex.trim();
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(SeraError::External(
            "invalid SHA-256 hash in CLI manifest".into(),
        ));
    }
    let actual = hex::encode(Sha256::digest(bytes));
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(SeraError::External(
            "downloaded CLI payload failed SHA-256 verification".into(),
        ));
    }
    Ok(())
}

fn extract_binary_from_zip(zip_bytes: &[u8], target_filename: &str) -> Result<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes))
        .map_err(|err| SeraError::External(format!("invalid CLI zip archive: {err}")))?;
    for idx in 0..archive.len() {
        let mut entry = archive
            .by_index(idx)
            .map_err(|err| SeraError::External(format!("failed to read zip entry: {err}")))?;
        if entry.is_dir() {
            continue;
        }
        let Some(safe_path) = entry.enclosed_name() else {
            return Err(SeraError::External(
                "CLI zip archive contains unsafe path traversal".into(),
            ));
        };
        let Some(file_name) = safe_path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let matches = if cfg!(windows) {
            file_name.eq_ignore_ascii_case(target_filename)
        } else {
            file_name == target_filename
        };
        if !matches {
            continue;
        }
        if entry.size() > MAX_INSTALL_PAYLOAD_BYTES as u64 {
            return Err(SeraError::External(
                "CLI binary inside zip archive exceeds size limit".into(),
            ));
        }
        let mut buf = Vec::new();
        entry
            .by_ref()
            .take((MAX_INSTALL_PAYLOAD_BYTES + 1) as u64)
            .read_to_end(&mut buf)
            .map_err(|err| SeraError::External(format!("failed to extract CLI binary: {err}")))?;
        if buf.is_empty() || buf.len() > MAX_INSTALL_PAYLOAD_BYTES {
            return Err(SeraError::External(
                "extracted CLI binary is empty or oversized".into(),
            ));
        }
        return Ok(buf);
    }
    Err(SeraError::External(format!(
        "CLI zip archive did not contain '{target_filename}'"
    )))
}

fn atomic_install_binary(bin_dir: &Path, target_name: &str, bytes: &[u8]) -> Result<PathBuf> {
    if bytes.is_empty() {
        return Err(SeraError::External("downloaded CLI binary is empty".into()));
    }
    std::fs::create_dir_all(bin_dir)
        .map_err(|err| SeraError::External(format!("failed to create CLI bin directory: {err}")))?;
    let mut temp = tempfile::NamedTempFile::new_in(bin_dir).map_err(|err| {
        SeraError::External(format!("failed to create temporary CLI file: {err}"))
    })?;
    temp.write_all(bytes)
        .map_err(|err| SeraError::External(format!("failed to write temporary CLI file: {err}")))?;
    temp.flush()
        .map_err(|err| SeraError::External(format!("failed to flush temporary CLI file: {err}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o755)).map_err(
            |err| SeraError::External(format!("failed to set CLI executable permissions: {err}")),
        )?;
    }
    let dest = bin_dir.join(target_name);
    temp.persist(&dest).map_err(|err| {
        SeraError::External(format!("failed to install CLI binary: {}", err.error))
    })?;
    Ok(dest)
}

async fn fetch_bounded_https(
    url_str: &str,
    allowed_domains: &[&str],
    timeout: Duration,
) -> Result<Vec<u8>> {
    let url = validate_download_url(url_str, allowed_domains)?;
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .https_only(true)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|err| SeraError::External(format!("HTTP client build failed: {err}")))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|err| SeraError::External(format!("CLI download request failed: {err}")))?;
    validate_download_url(resp.url().as_str(), allowed_domains)?;
    if !resp.status().is_success() {
        return Err(SeraError::External(format!(
            "CLI download returned HTTP {}",
            resp.status()
        )));
    }
    if let Some(len) = resp.content_length()
        && len > MAX_INSTALL_PAYLOAD_BYTES as u64
    {
        return Err(SeraError::External(
            "CLI download exceeds maximum allowed size".into(),
        ));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|err| SeraError::External(format!("failed to read CLI download body: {err}")))?;
    if bytes.is_empty() || bytes.len() > MAX_INSTALL_PAYLOAD_BYTES {
        return Err(SeraError::External(
            "CLI download payload is empty or oversized".into(),
        ));
    }
    Ok(bytes.to_vec())
}

async fn install_provider_cli(provider: Provider, bin_dir: &Path) -> Result<PathBuf> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let allowed = allowed_domains_for(provider);
    let target_name = executable_name(provider);

    let binary_bytes = match provider {
        Provider::ProtonPass => {
            let meta =
                fetch_bounded_https(PROTON_VERSIONS_URL, allowed, INSTALL_METADATA_TIMEOUT).await?;
            let (version, url, hash) = parse_proton_manifest(&meta, os, arch)?;
            tracing::info!(
                provider = provider.id(),
                version = %version,
                "downloading Proton Pass CLI"
            );
            let payload = fetch_bounded_https(&url, allowed, INSTALL_DOWNLOAD_TIMEOUT).await?;
            verify_sha256(&payload, &hash)?;
            if url.ends_with(".zip") || payload.starts_with(b"PK\x03\x04") {
                extract_binary_from_zip(&payload, &target_name)?
            } else {
                if payload.starts_with(b"<") {
                    return Err(SeraError::External(
                        "downloaded Proton Pass CLI payload is not a binary".into(),
                    ));
                }
                payload
            }
        }
        Provider::OnePassword => {
            let meta =
                fetch_bounded_https(ONEPASSWORD_VERSION_URL, allowed, INSTALL_METADATA_TIMEOUT)
                    .await?;
            let version = parse_onepassword_version(&meta)?;
            let url = onepassword_download_url(&version, os, arch)?;
            tracing::info!(
                provider = provider.id(),
                version = %version,
                "downloading 1Password CLI"
            );
            let payload = fetch_bounded_https(&url, allowed, INSTALL_DOWNLOAD_TIMEOUT).await?;
            extract_binary_from_zip(&payload, &target_name)?
        }
    };

    let dest = bin_dir.to_path_buf();
    tokio::task::spawn_blocking(move || atomic_install_binary(&dest, &target_name, &binary_bytes))
        .await
        .map_err(|_| SeraError::Internal("CLI install task failed".into()))?
}

/// How long a value read from a password manager stays in the model redactor.
/// A fill is typed right after the read, so this only covers page text and
/// tool output that may echo it back.
pub const RESOLVED_TTL: Duration = Duration::from_secs(600);
/// Most resolved values kept per user; the oldest goes first.
pub const RESOLVED_PER_USER: usize = 32;

struct Resolved {
    name: String,
    value: Zeroizing<String>,
    at: Instant,
}

/// Values recently read from a password manager, per user, so the model
/// redactor can scrub them. The values exist only here, in memory, for
/// [`RESOLVED_TTL`]; they are never written anywhere. Keyed by entry name:
/// a second read of the same entry replaces the first.
#[derive(Default)]
pub struct ResolvedValues {
    by_user: Mutex<HashMap<String, VecDeque<Resolved>>>,
}

impl ResolvedValues {
    /// Remember a value just read for the acting user's entry `name`.
    pub fn remember(&self, user_id: &str, name: &str, value: &Zeroizing<String>) {
        self.remember_at(user_id, name, value, Instant::now());
    }

    /// The acting user's values that are still within their TTL, as
    /// `(entry name, value)`.
    pub fn live(&self, user_id: &str) -> Vec<(String, Zeroizing<String>)> {
        self.live_at(user_id, Instant::now())
    }

    fn remember_at(&self, user_id: &str, name: &str, value: &Zeroizing<String>, now: Instant) {
        let mut map = self.map();
        let entries = map.entry(user_id.to_owned()).or_default();
        entries.retain(|entry| entry.name != name && now < entry.at + RESOLVED_TTL);
        entries.push_back(Resolved {
            name: name.to_owned(),
            value: Zeroizing::new(value.to_string()),
            at: now,
        });
        // Dropping an evicted entry zeroizes its value.
        while entries.len() > RESOLVED_PER_USER {
            entries.pop_front();
        }
    }

    fn live_at(&self, user_id: &str, now: Instant) -> Vec<(String, Zeroizing<String>)> {
        let mut map = self.map();
        let Some(entries) = map.get_mut(user_id) else {
            return Vec::new();
        };
        entries.retain(|entry| now < entry.at + RESOLVED_TTL);
        entries
            .iter()
            .map(|entry| (entry.name.clone(), Zeroizing::new(entry.value.to_string())))
            .collect()
    }

    /// A poisoned lock still holds the values, so it is used as is: a panic
    /// elsewhere must not make the redactor forget them.
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, VecDeque<Resolved>>> {
        self.by_user
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Reads references through a CLI. Built once at startup from `[vault]`.
pub struct Providers {
    config: VaultConfig,
    runner: Arc<dyn SecretCommand>,
    resolved: Arc<ResolvedValues>,
    install_locks: [tokio::sync::Mutex<()>; 2],
}

impl Providers {
    pub fn new(config: &VaultConfig) -> Self {
        Self::with_runner(config, Arc::new(ProcessCommand::default()))
    }

    pub fn with_runner(config: &VaultConfig, runner: Arc<dyn SecretCommand>) -> Self {
        Self {
            config: config.clone(),
            runner,
            resolved: Arc::default(),
            install_locks: [tokio::sync::Mutex::new(()), tokio::sync::Mutex::new(())],
        }
    }

    /// Ensure the CLI for `provider` is available, auto-installing it into the
    /// managed bin directory when no custom path is configured.
    pub async fn ensure_cli(&self, provider: Provider) -> Result<Option<PathBuf>> {
        if let Some(path) = program_for(provider, &self.config) {
            return Ok(Some(path));
        }
        if !provider.configured_path(&self.config).trim().is_empty() {
            return Ok(None);
        }
        if !self.enabled(provider) || auto_install_disabled() {
            return Ok(None);
        }
        let _guard = self.install_locks[provider.lock_index()].lock().await;
        if let Some(path) = program_for(provider, &self.config) {
            return Ok(Some(path));
        }
        let bin_dir = managed_bin_dir(&self.config);
        let installed = install_provider_cli(provider, &bin_dir).await?;
        Ok(Some(installed))
    }

    /// The registry of values read through these providers. Share it with the
    /// gateway so the model redactor scrubs them.
    pub fn resolved_values(&self) -> Arc<ResolvedValues> {
        self.resolved.clone()
    }

    pub fn cli_found(&self, provider: Provider) -> bool {
        program_for(provider, &self.config).is_some()
    }

    pub fn enabled(&self, provider: Provider) -> bool {
        provider.enabled(&self.config)
    }

    /// Refuses a provider the server has not enabled, before anything runs.
    fn require_enabled(&self, provider: Provider) -> Result<()> {
        if self.enabled(provider) {
            return Ok(());
        }
        Err(SeraError::Validation(format!(
            "{} is disabled on this server: set [vault] protonpass_enabled = true to use it",
            provider.label()
        )))
    }

    /// Read one reference's value with the owner's token. The value is
    /// returned to the caller; the caller may remember it for redaction.
    pub async fn read(
        &self,
        provider: Provider,
        reference: &str,
        token: &str,
    ) -> Result<Zeroizing<String>> {
        self.require_enabled(provider)?;
        let args = read_args(provider, reference)?;
        let program = match program_for(provider, &self.config) {
            Some(path) => path,
            None => match self.ensure_cli(provider).await {
                Ok(Some(path)) => path,
                Ok(None) => {
                    return Err(SeraError::External(format!(
                        "the {} CLI is not installed on the server",
                        provider.label()
                    )));
                }
                Err(error) => {
                    tracing::warn!(provider = provider.id(), %error, "password manager CLI auto-install failed");
                    return Err(SeraError::External(format!(
                        "the {} CLI is not installed on the server",
                        provider.label()
                    )));
                }
            },
        };
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
    /// The server reads from this manager (Proton Pass is opt-in).
    pub enabled: bool,
    pub install_url: &'static str,
}

/// Arguments for a token probe. Stdout is discarded; only the exit code matters.
pub fn probe_args(provider: Provider) -> Vec<String> {
    match provider {
        Provider::OnePassword => vec!["account".to_owned(), "list".to_owned()],
        Provider::ProtonPass => vec!["vault".to_owned(), "list".to_owned()],
    }
}

/// Result of probing a saved password-manager token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderProbe {
    pub ok: bool,
    pub cli_found: bool,
    pub configured: bool,
    pub message: String,
}

/// Check whether the acting user's token works against the provider CLI.
/// Never returns token text or CLI stdout.
pub async fn probe(
    users: &UserSecrets,
    providers: &Providers,
    user_id: &str,
    provider: Provider,
) -> Result<ProviderProbe> {
    let mut cli_found = providers.cli_found(provider);
    let configured = users
        .vault_provider_token(user_id, provider.id())?
        .is_some();
    if !providers.enabled(provider) {
        return Ok(ProviderProbe {
            ok: false,
            cli_found,
            configured,
            message: format!("{} is disabled on this server", provider.label()),
        });
    }
    if !configured {
        return Ok(ProviderProbe {
            ok: false,
            cli_found,
            configured,
            message: "save an access token first".into(),
        });
    }
    if !cli_found {
        if let Err(error) = providers.ensure_cli(provider).await {
            tracing::warn!(provider = provider.id(), %error, "password manager CLI auto-install failed during probe");
        }
        cli_found = providers.cli_found(provider);
    }
    if !cli_found {
        return Ok(ProviderProbe {
            ok: false,
            cli_found,
            configured,
            message: format!(
                "the {} CLI is not installed on the server",
                provider.label()
            ),
        });
    }
    let token = users
        .vault_provider_token(user_id, provider.id())?
        .ok_or_else(|| SeraError::InvalidState("token missing after check".into()))?;
    let args = probe_args(provider);
    let program = program_for(provider, &providers.config).ok_or_else(|| {
        SeraError::External(format!(
            "the {} CLI is not installed on the server",
            provider.label()
        ))
    })?;
    let runner = providers.runner.clone();
    let env_name = provider.token_env();
    let token = Zeroizing::new(token.expose_secret().to_owned());
    let output = tokio::task::spawn_blocking(move || {
        runner.run(&program, &args, &[(env_name, token.as_str())])
    })
    .await
    .map_err(|_| SeraError::Internal("the password manager probe failed".into()))??;
    let ok = output.status == Some(0);
    drop(output.stdout);
    if ok {
        Ok(ProviderProbe {
            ok: true,
            cli_found,
            configured,
            message: "the token answered".into(),
        })
    } else {
        tracing::warn!(
            provider = provider.id(),
            exit_code = ?output.status,
            "password manager probe failed"
        );
        Ok(ProviderProbe {
            ok: false,
            cli_found,
            configured,
            message: format!("{} rejected the token", provider.label()),
        })
    }
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
        enabled: providers.enabled(provider),
        install_url: provider.install_url(),
    })
}

/// The browser fill's approval-time check: the entry must allow `host` (and
/// hold `field`), and a password manager value needs its provider enabled.
/// Nothing is read or decrypted.
pub fn authorize_for_browser(
    users: &UserSecrets,
    providers: &Providers,
    user_id: &str,
    name: &str,
    host: &str,
    field: VaultField,
) -> Result<VaultEntry> {
    let entry = vault::authorize_fill(users, user_id, name, host, field)?;
    if let (VaultField::Value, Some(provider)) = (field, Provider::from_source(entry.source)) {
        providers.require_enabled(provider)?;
    }
    Ok(entry)
}

/// The browser fill for one entry: authorize the host, then read the value
/// from the sealed store (local) or from the owner's password manager
/// (reference). A value read from a password manager is remembered for the
/// model redactor. The fill is recorded as a `vault.resolve` audit row with
/// the name and host only.
pub async fn resolve_for_browser(
    users: &UserSecrets,
    providers: &Providers,
    user_id: &str,
    name: &str,
    host: &str,
    field: VaultField,
) -> Result<Zeroizing<String>> {
    let entry = authorize_for_browser(users, providers, user_id, name, host, field)?;
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
            let value = providers
                .read(provider, reference, token.expose_secret())
                .await?;
            providers.resolved.remember(user_id, name, &value);
            value
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
    use crate::vault::{Redactor, VaultInput, VaultKind};
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
            bin_dir: String::new(),
            protonpass_enabled: false,
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
            // A segment that a CLI could read as an option.
            "op://-Vault/Item/password",
            "op://Vault/-Item/password",
            "op://Vault/Item/-password",
            // A query other than one attribute name.
            "op://Vault/Item/password?vault=x",
            "op://Vault/Item/password?attribute=",
            "op://Vault/Item/password?attribute=otp&x=1",
            "op://Vault/Item/password?attribute=a/b",
            "op://Vault?attribute=otp/Item/password",
        ];
        for reference in bad {
            assert!(
                validate_reference(VaultSource::OnePassword, reference).is_err(),
                "{reference}"
            );
        }
        validate_reference(
            VaultSource::OnePassword,
            "op://Vault/Item/password?attribute=otp",
        )
        .unwrap();
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
            "pass://-Personal/github/password",
            "pass://Personal/-github/password",
            "pass://Personal/github/password?attribute=otp",
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

    #[tokio::test]
    async fn probe_uses_exit_code_only_and_never_returns_stdout() {
        let (_, users, user) = setup();
        set_token(&users, &user, Provider::OnePassword, "synthetic-token").unwrap();
        let runner = fake(Some(0), "synthetic-account-email@example.example");
        let providers = Providers::with_runner(&config_with_op(), runner);
        let report = probe(&users, &providers, &user, Provider::OnePassword)
            .await
            .unwrap();
        assert!(report.ok);
        assert!(!report.message.contains("synthetic-account"));
    }

    #[test]
    fn the_child_environment_inherits_only_path_and_its_own_home() {
        let home = tempfile::TempDir::new().unwrap();
        let env = child_environment(
            home.path(),
            &[("OP_SERVICE_ACCOUNT_TOKEN", "synthetic-token")],
        );
        let allowed = [
            "PATH",
            "SYSTEMROOT",
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "XDG_DATA_HOME",
            "APPDATA",
            "LOCALAPPDATA",
            "USERPROFILE",
            "TEMP",
            "TMP",
            "OP_SERVICE_ACCOUNT_TOKEN",
        ];
        for (key, _) in &env {
            assert!(allowed.contains(&key.to_str().unwrap()), "{key:?}");
        }
        let value = |name: &str| {
            env.iter()
                .find(|(key, _)| key.to_str() == Some(name))
                .map(|(_, value)| value.to_str().unwrap().to_owned())
        };
        let home_text = home.path().to_str().unwrap().to_owned();
        for name in HOME_VARS {
            assert_eq!(value(name).as_deref(), Some(home_text.as_str()), "{name}");
        }
        assert_eq!(
            value("OP_SERVICE_ACCOUNT_TOKEN").unwrap(),
            "synthetic-token"
        );
    }

    #[tokio::test]
    async fn proton_pass_is_refused_until_the_server_enables_it() {
        let (_, users, user) = setup();
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
        users
            .set_vault_provider_token(&user, "protonpass", "synthetic-token")
            .unwrap();
        let runner = fake(Some(0), "synthetic-pass-value");
        assert!(VaultConfig::default().protonpass_enabled);
        let mut disabled_config = config();
        disabled_config.protonpass_enabled = false;
        let disabled = Providers::with_runner(&disabled_config, runner.clone());
        let error = resolve_for_browser(
            &users,
            &disabled,
            &user,
            "bank",
            "bank.example",
            VaultField::Value,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("disabled on this server"));
        assert!(runner.calls.lock().unwrap().is_empty());
        assert!(
            !status(&users, &disabled, &user, Provider::ProtonPass)
                .unwrap()
                .enabled
        );
        // The approval is refused too, before anything is asked of the owner.
        assert!(
            authorize_for_browser(
                &users,
                &disabled,
                &user,
                "bank",
                "bank.example",
                VaultField::Value
            )
            .is_err()
        );

        let mut enabled_config = config();
        enabled_config.protonpass_enabled = true;
        enabled_config.pass_cli_path = std::env::current_exe().unwrap().display().to_string();
        let enabled = Providers::with_runner(&enabled_config, runner.clone());
        assert!(
            status(&users, &enabled, &user, Provider::ProtonPass)
                .unwrap()
                .enabled
        );
        let value = resolve_for_browser(
            &users,
            &enabled,
            &user,
            "bank",
            "bank.example",
            VaultField::Value,
        )
        .await
        .unwrap();
        assert_eq!(value.as_str(), "synthetic-pass-value");
        assert_eq!(runner.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_provider_value_is_redacted_after_it_is_typed_and_only_for_its_owner() {
        let (store, users, user) = setup();
        let other = store
            .create_user("subject-vp-r", "vpr@example.example", "Synthetic VPR")
            .unwrap()
            .id;
        let providers =
            Providers::with_runner(&config_with_op(), fake(Some(0), "synthetic-op-value"));
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
        users
            .set_vault_provider_token(&user, "onepassword", "synthetic-token")
            .unwrap();
        resolve_for_browser(
            &users,
            &providers,
            &user,
            "bank",
            "bank.example",
            VaultField::Value,
        )
        .await
        .unwrap();
        let resolved = providers.resolved_values();
        let redactor = Redactor::for_user(&users, &user, &resolved).unwrap();
        assert_eq!(
            redactor.redact("the page shows synthetic-op-value"),
            "the page shows [vault:bank]"
        );
        let others = Redactor::for_user(&users, &other, &resolved).unwrap();
        assert_eq!(others.redact("synthetic-op-value"), "synthetic-op-value");
    }

    #[test]
    fn resolved_values_expire_after_their_ttl_and_stay_bounded() {
        let registry = ResolvedValues::default();
        let start = Instant::now();
        let value = Zeroizing::new("synthetic-resolved".to_owned());
        registry.remember_at("u", "bank", &value, start);
        let just_before = start + RESOLVED_TTL - Duration::from_secs(1);
        assert_eq!(registry.live_at("u", just_before).len(), 1);
        assert!(registry.live_at("u", start + RESOLVED_TTL).is_empty());
        assert!(registry.live_at("v", start).is_empty());

        // At most RESOLVED_PER_USER per user: the oldest go first.
        for index in 0..RESOLVED_PER_USER + 8 {
            registry.remember_at("u", &format!("entry-{index}"), &value, start);
        }
        let live = registry.live_at("u", start);
        assert_eq!(live.len(), RESOLVED_PER_USER);
        assert!(live.iter().all(|(name, _)| name != "entry-0"));
        assert!(live.iter().any(|(name, _)| name == "entry-39"));

        // Re-reading an entry replaces its value rather than adding a second one.
        registry.remember_at("u", "entry-39", &value, start);
        let again = registry.live_at("u", start);
        assert_eq!(
            again.iter().filter(|(name, _)| name == "entry-39").count(),
            1
        );
    }

    #[test]
    fn proton_and_onepassword_manifests_parse_cleanly() {
        let proton_json = br#"{
            "formatVersion": 1,
            "passCliVersions": {
                "version": "2.7.0",
                "urls": {
                    "windows": {
                        "x86_64": {
                            "url": "https://proton.me/download/pass-cli/2.7.0/pass-cli-windows-x86_64.zip",
                            "hash": "c752af109dbda98ad154dfcdb40e8e43f1a38bc37de224b6182647b625290bb7"
                        }
                    },
                    "linux": {
                        "x86_64": {
                            "url": "https://proton.me/download/pass-cli/2.7.0/pass-cli-linux-x86_64",
                            "hash": "fd933e76179a5a33b295853e9c6696e49e584fe2141b1aafc63a3d78c0791cdb"
                        }
                    }
                }
            }
        }"#;
        let (ver, url, hash) = parse_proton_manifest(proton_json, "windows", "x86_64").unwrap();
        assert_eq!(ver, "2.7.0");
        assert!(url.ends_with("pass-cli-windows-x86_64.zip"));
        assert_eq!(hash.len(), 64);
        assert!(parse_proton_manifest(proton_json, "macos", "aarch64").is_err());

        let op_json = br#"{"available":"1","version":"2.42.0"}"#;
        let op_ver = parse_onepassword_version(op_json).unwrap();
        assert_eq!(op_ver, "2.42.0");
        let op_url = onepassword_download_url(&op_ver, "linux", "x86_64").unwrap();
        assert_eq!(
            op_url,
            "https://cache.agilebits.com/dist/1P/op2/pkg/v2.42.0/op_linux_amd64_v2.42.0.zip"
        );
        assert!(parse_onepassword_version(br#"{"version":"../bad"}"#).is_err());
    }

    #[test]
    fn sha256_verification_accepts_match_and_rejects_mismatch() {
        let payload = b"synthetic-pass-cli-binary";
        let expected = hex::encode(Sha256::digest(payload));
        assert!(verify_sha256(payload, &expected).is_ok());
        assert!(verify_sha256(payload, &expected.to_ascii_uppercase()).is_ok());
        assert!(verify_sha256(b"tampered", &expected).is_err());
        assert!(verify_sha256(payload, "not-a-hex-hash").is_err());
    }

    #[test]
    fn zip_extraction_extracts_target_and_blocks_zip_slip() {
        use std::io::Write as _;
        let target = executable_name(Provider::OnePassword);

        // Valid archive
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default();
            zw.start_file(&target, opts).unwrap();
            zw.write_all(b"synthetic-op-bytes").unwrap();
            zw.finish().unwrap();
        }
        let extracted = extract_binary_from_zip(&buf, &target).unwrap();
        assert_eq!(extracted, b"synthetic-op-bytes");

        // Zip-slip archive
        let mut bad_buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(Cursor::new(&mut bad_buf));
            let opts = zip::write::SimpleFileOptions::default();
            zw.start_file("../../evil.exe", opts).unwrap();
            zw.write_all(b"evil").unwrap();
            zw.finish().unwrap();
        }
        assert!(extract_binary_from_zip(&bad_buf, &target).is_err());
    }

    #[test]
    fn download_url_allowlist_enforces_https_and_provider_domains() {
        assert!(
            validate_download_url("https://proton.me/download/pass-cli/a", &["proton.me"]).is_ok()
        );
        assert!(
            validate_download_url(
                "https://cache.agilebits.com/dist/op.zip",
                &["agilebits.com"]
            )
            .is_ok()
        );
        assert!(validate_download_url("http://proton.me/download/a", &["proton.me"]).is_err());
        assert!(validate_download_url("https://evil.example/op.zip", &["agilebits.com"]).is_err());
    }

    #[tokio::test]
    async fn custom_path_override_bypasses_auto_install_and_managed_bin_is_resolved() {
        let custom = VaultConfig {
            op_path: "Z:/synthetic/op-missing.exe".into(),
            ..Default::default()
        };
        let providers = Providers::new(&custom);
        assert!(
            providers
                .ensure_cli(Provider::OnePassword)
                .await
                .unwrap()
                .is_none()
        );
        assert!(program_for(Provider::OnePassword, &custom).is_none());

        let dir = tempfile::TempDir::new().unwrap();
        let managed_cfg = VaultConfig {
            bin_dir: dir.path().display().to_string(),
            ..Default::default()
        };
        let installed = atomic_install_binary(
            dir.path(),
            &executable_name(Provider::ProtonPass),
            b"synthetic-pass-cli",
        )
        .unwrap();
        assert!(installed.is_file());
        assert_eq!(
            program_for(Provider::ProtonPass, &managed_cfg),
            Some(installed)
        );
    }
}
