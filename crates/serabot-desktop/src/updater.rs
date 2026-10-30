//! Built-in updater for the Windows owner console.
//!
//! Updates are only possible when the binary was built with
//! `SERABOT_RELEASES_REPO=owner/repo`. Source builds have no repository and
//! make no network calls; the UI reports them as a developer build.
//!
//! Checks and downloads happen on a background thread. The executable is
//! written to `%LOCALAPPDATA%\SeraBot\updates`, verified against the release
//! `SHA256SUMS` asset, and only then offered for installation. Installation
//! never runs an unverified file: the verified executable is moved over the
//! current one by a detached `cmd` helper after this process exits.

use reqwest::blocking::{Client, Response};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use url::Url;

/// Stable asset name published by the release workflow.
pub const DESKTOP_ASSET: &str = "serabot-desktop-latest-windows-x64.exe";
/// Checksums asset published next to the stable aliases.
pub const CHECKSUMS_ASSET: &str = "SHA256SUMS";

const API_BASE: &str = "https://api.github.com";
const MAX_RELEASE_JSON: u64 = 512 * 1024;
const MAX_CHECKSUMS: u64 = 64 * 1024;
const MAX_DOWNLOAD: u64 = 160 * 1024 * 1024;
const API_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const CACHE_FILE: &str = "update-check.json";
const MAX_NOTES_CHARS: usize = 4000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateError {
    Network,
    Http(u16),
    InvalidRelease,
    TooLarge,
    HashMismatch,
    Storage,
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network => f.write_str("Update check failed: the network is unavailable."),
            Self::Http(403 | 429) => {
                f.write_str("Update check was rate-limited by GitHub. Try again later.")
            }
            Self::Http(code) => write!(f, "Update check failed with HTTP {code}."),
            Self::InvalidRelease => f.write_str("The latest release metadata is malformed."),
            Self::TooLarge => f.write_str("The update exceeds the download safety limit."),
            Self::HashMismatch => {
                f.write_str("SHA-256 verification failed. The update was discarded.")
            }
            Self::Storage => f.write_str("The update could not be stored locally."),
        }
    }
}

impl std::error::Error for UpdateError {}

/// A downloaded, hash-verified update ready for owner consent.
#[derive(Clone, Debug)]
pub struct UpdateInfo {
    pub version: Version,
    pub notes: String,
    pub size: u64,
    pub exe_path: PathBuf,
    /// SHA-256 of the bytes at `exe_path`, re-checked before installation.
    pub sha256: [u8; 32],
}

/// GitHub release metadata subset. Unknown fields are ignored.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Release {
    #[serde(default)]
    pub tag_name: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Asset {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub browser_download_url: String,
}

#[derive(Serialize, Deserialize)]
struct CachedUpdate {
    version: String,
    notes: String,
    size: u64,
    exe: String,
    sha256: String,
}

/// `owner/repo` from the build environment, if present and safe.
pub fn configured_repo() -> Option<&'static str> {
    option_env!("SERABOT_RELEASES_REPO").filter(|repo| valid_repo(repo))
}

/// Build commit recorded by release CI, if any.
pub fn build_sha() -> Option<&'static str> {
    option_env!("SERABOT_BUILD_SHA")
        .filter(|sha| (7..=64).contains(&sha.len()) && sha.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Compile-time application version. Never panics.
pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Version::new(0, 0, 0))
}

/// GitHub `owner/repo`: no spaces, no extra path segments, no `..`.
pub fn valid_repo(repo: &str) -> bool {
    let Some((owner, name)) = repo.split_once('/') else {
        return false;
    };
    !owner.is_empty()
        && owner.len() <= 39
        && owner
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !name.is_empty()
        && name.len() <= 100
        && !name.starts_with('.')
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Parse a release tag such as `v0.2.0` into a semver version.
pub fn parse_version(tag: &str) -> Option<Version> {
    let trimmed = tag.trim();
    let trimmed = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed);
    if trimmed.is_empty() {
        return None;
    }
    Version::parse(trimmed).ok()
}

/// A stable installation ignores candidate pre-releases. A pre-release
/// installation may move to a newer pre-release or to stable.
pub fn is_newer(candidate: &Version, current: &Version) -> bool {
    if current.pre.is_empty() {
        candidate.pre.is_empty() && candidate > current
    } else {
        candidate > current
    }
}

pub fn parse_release(json: &[u8]) -> Result<Release, UpdateError> {
    let value: serde_json::Value =
        serde_json::from_slice(json).map_err(|_| UpdateError::InvalidRelease)?;
    if !value.is_object() {
        return Err(UpdateError::InvalidRelease);
    }
    serde_json::from_value(value).map_err(|_| UpdateError::InvalidRelease)
}

/// Pick the desktop executable and checksum assets by exact stable name.
pub fn select_assets(release: &Release) -> Option<(&Asset, &Asset)> {
    let executable = release.assets.iter().find(|asset| {
        asset.name == DESKTOP_ASSET
            && asset.size > 0
            && asset.size <= MAX_DOWNLOAD
            && valid_asset_url(&asset.browser_download_url)
    })?;
    let checksums = release.assets.iter().find(|asset| {
        asset.name == CHECKSUMS_ASSET
            && asset.size > 0
            && asset.size <= MAX_CHECKSUMS
            && valid_asset_url(&asset.browser_download_url)
    })?;
    Some((executable, checksums))
}

/// Asset URLs are only followed when they are GitHub HTTPS release downloads.
fn valid_asset_url(raw: &str) -> bool {
    let Ok(url) = Url::parse(raw) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str() == Some("github.com")
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && url.path().contains("/releases/download/")
        && !url.path().contains("..")
}

/// Parse `SHA256SUMS` lines of the form `<64 hex><whitespace><name>`.
pub fn parse_sha256sums(text: &str) -> Vec<(String, [u8; 32])> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (hash, rest) = line.split_once(char::is_whitespace)?;
            let rest = rest.trim_start();
            let name = rest.strip_prefix('*').unwrap_or(rest).trim();
            if name.is_empty() || name.contains('/') || name.contains('\\') {
                return None;
            }
            Some((name.to_owned(), parse_hex_32(hash)?))
        })
        .collect()
}

/// Expected digest for one asset name.
pub fn checksum_for(text: &str, name: &str) -> Option<[u8; 32]> {
    parse_sha256sums(text)
        .into_iter()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, digest)| digest)
}

fn parse_hex_32(raw: &str) -> Option<[u8; 32]> {
    if raw.len() != 64 {
        return None;
    }
    let mut digest = [0u8; 32];
    for (index, pair) in raw.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        digest[index] = (hex_digit(pair[0])? << 4) | hex_digit(pair[1])?;
    }
    Some(digest)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(DIGITS[(byte >> 4) as usize] as char);
        text.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    text
}

pub fn sha256_file(path: &Path) -> Result<[u8; 32], UpdateError> {
    let mut file = File::open(path).map_err(|_| UpdateError::Storage)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|_| UpdateError::Storage)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

pub fn verify_file(path: &Path, expected: &[u8; 32]) -> Result<(), UpdateError> {
    if sha256_file(path)? == *expected {
        Ok(())
    } else {
        Err(UpdateError::HashMismatch)
    }
}

/// Downloads and verifies releases from the configured repository.
pub struct Updater {
    repo: String,
    current: Version,
    dir: PathBuf,
    client: Client,
}

impl Updater {
    /// Build an updater from compile-time settings. `None` means updates are
    /// disabled (developer build) and no network calls are made.
    pub fn from_build_env() -> Option<Self> {
        let repo = configured_repo()?;
        let dir = downloads_dir()?;
        Self::new(repo, current_version(), &dir)
    }

    /// Testable constructor with an injected version and directory.
    pub fn new(repo: &str, current: Version, dir: &Path) -> Option<Self> {
        if !valid_repo(repo) || dir.as_os_str().is_empty() {
            return None;
        }
        let client = Client::builder()
            .user_agent(format!("serabot-desktop/{}", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(API_TIMEOUT)
            .build()
            .ok()?;
        Some(Self {
            repo: repo.to_owned(),
            current,
            dir: dir.to_owned(),
            client,
        })
    }

    pub fn current_version(&self) -> &Version {
        &self.current
    }

    pub fn repo(&self) -> &str {
        &self.repo
    }

    pub fn downloads_dir(&self) -> &Path {
        &self.dir
    }

    /// Check GitHub for a newer release. On network, 403, or 429 failures the
    /// last verified cached update is used when it is still newer.
    pub fn check(&self) -> Result<Option<UpdateInfo>, UpdateError> {
        match self.check_online() {
            Ok(result) => Ok(result),
            Err(error) => match self.cached() {
                Some(cached) => Ok(Some(cached)),
                None => Err(error),
            },
        }
    }

    fn check_online(&self) -> Result<Option<UpdateInfo>, UpdateError> {
        let url = format!("{API_BASE}/repos/{}/releases/latest", self.repo);
        let response = self
            .client
            .get(url)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .send()
            .map_err(|_| UpdateError::Network)?;
        let status = response.status();
        if !status.is_success() {
            return Err(UpdateError::Http(status.as_u16()));
        }
        let release = parse_release(&read_capped(response, MAX_RELEASE_JSON)?)?;
        let Some(version) = parse_version(&release.tag_name) else {
            return Err(UpdateError::InvalidRelease);
        };
        if !is_newer(&version, &self.current) {
            return Ok(None);
        }
        let (executable, checksums) = select_assets(&release).ok_or(UpdateError::InvalidRelease)?;
        let sums = self.fetch_text(&checksums.browser_download_url, MAX_CHECKSUMS)?;
        let expected = checksum_for(&sums, DESKTOP_ASSET).ok_or(UpdateError::InvalidRelease)?;
        fs::create_dir_all(&self.dir).map_err(|_| UpdateError::Storage)?;
        let destination = self.dir.join(format!("serabot-desktop-{version}.exe"));
        let size = self.download(&executable.browser_download_url, &destination, &expected)?;
        let info = UpdateInfo {
            version,
            notes: sanitize_notes(&release.body),
            size,
            exe_path: destination,
            sha256: expected,
        };
        // A cache failure does not invalidate an otherwise verified update.
        let _ = self.write_cache(&info);
        Ok(Some(info))
    }

    fn fetch_text(&self, url: &str, limit: u64) -> Result<String, UpdateError> {
        if !valid_asset_url(url) {
            return Err(UpdateError::InvalidRelease);
        }
        let response = self
            .client
            .get(url)
            .timeout(API_TIMEOUT)
            .send()
            .map_err(|_| UpdateError::Network)?;
        let status = response.status();
        if !status.is_success() {
            return Err(UpdateError::Http(status.as_u16()));
        }
        let bytes = read_capped(response, limit)?;
        String::from_utf8(bytes).map_err(|_| UpdateError::InvalidRelease)
    }

    /// Stream to `<destination>.part`, hash while writing, then rename.
    /// Returns the number of bytes written.
    fn download(
        &self,
        url: &str,
        destination: &Path,
        expected: &[u8; 32],
    ) -> Result<u64, UpdateError> {
        if !valid_asset_url(url) {
            return Err(UpdateError::InvalidRelease);
        }
        let response = self
            .client
            .get(url)
            .timeout(DOWNLOAD_TIMEOUT)
            .send()
            .map_err(|_| UpdateError::Network)?;
        let status = response.status();
        if !status.is_success() {
            return Err(UpdateError::Http(status.as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_DOWNLOAD)
        {
            return Err(UpdateError::TooLarge);
        }
        let part = part_path(destination);
        let file = File::create(&part).map_err(|_| UpdateError::Storage)?;
        let mut writer = BufWriter::new(file);
        let mut hasher = Sha256::new();
        let mut total = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        let mut reader = response.take(MAX_DOWNLOAD + 1);
        loop {
            let read = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(_) => {
                    let _ = writer.flush();
                    let _ = fs::remove_file(&part);
                    return Err(UpdateError::Network);
                }
            };
            total += read as u64;
            if total > MAX_DOWNLOAD {
                let _ = writer.flush();
                let _ = fs::remove_file(&part);
                return Err(UpdateError::TooLarge);
            }
            hasher.update(&buffer[..read]);
            if writer.write_all(&buffer[..read]).is_err() {
                let _ = fs::remove_file(&part);
                return Err(UpdateError::Storage);
            }
        }
        if writer.flush().is_err() {
            let _ = fs::remove_file(&part);
            return Err(UpdateError::Storage);
        }
        drop(writer);
        let digest: [u8; 32] = hasher.finalize().into();
        if digest != *expected {
            let _ = fs::remove_file(&part);
            return Err(UpdateError::HashMismatch);
        }
        if destination.exists() {
            let _ = fs::remove_file(destination);
        }
        fs::rename(&part, destination).map_err(|_| {
            let _ = fs::remove_file(&part);
            UpdateError::Storage
        })?;
        Ok(total)
    }

    fn write_cache(&self, info: &UpdateInfo) -> Result<(), UpdateError> {
        let name = info
            .exe_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(UpdateError::Storage)?;
        let cached = CachedUpdate {
            version: info.version.to_string(),
            notes: info.notes.clone(),
            size: info.size,
            exe: name.to_owned(),
            sha256: to_hex(&info.sha256),
        };
        let text = serde_json::to_string_pretty(&cached).map_err(|_| UpdateError::Storage)?;
        fs::create_dir_all(&self.dir).map_err(|_| UpdateError::Storage)?;
        fs::write(self.dir.join(CACHE_FILE), text).map_err(|_| UpdateError::Storage)
    }

    /// Last verified update, if it is still newer and still matches its hash.
    fn cached(&self) -> Option<UpdateInfo> {
        let text = fs::read_to_string(self.dir.join(CACHE_FILE)).ok()?;
        let cached: CachedUpdate = serde_json::from_str(&text).ok()?;
        let version = Version::parse(&cached.version).ok()?;
        if !is_newer(&version, &self.current) {
            return None;
        }
        if cached.exe.is_empty()
            || !cached.exe.ends_with(".exe")
            || cached.exe.contains("..")
            || cached.exe.contains('/')
            || cached.exe.contains('\\')
        {
            return None;
        }
        let expected = parse_hex_32(&cached.sha256)?;
        let path = self.dir.join(&cached.exe);
        verify_file(&path, &expected).ok()?;
        Some(UpdateInfo {
            version,
            notes: sanitize_notes(&cached.notes),
            size: cached.size,
            exe_path: path,
            sha256: expected,
        })
    }
}

fn read_capped(response: Response, limit: u64) -> Result<Vec<u8>, UpdateError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err(UpdateError::TooLarge);
    }
    let mut bytes = Vec::new();
    response
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| UpdateError::Network)?;
    if bytes.len() as u64 > limit {
        return Err(UpdateError::TooLarge);
    }
    Ok(bytes)
}

fn sanitize_notes(body: &str) -> String {
    let mut text = String::new();
    let mut count = 0usize;
    for character in body.chars() {
        if count >= MAX_NOTES_CHARS {
            text.push('…');
            break;
        }
        if character == '\n' || character == '\t' || !character.is_control() {
            text.push(character);
            count += 1;
        }
    }
    text.trim().to_owned()
}

fn part_path(destination: &Path) -> PathBuf {
    let mut raw = destination.as_os_str().to_owned();
    raw.push(".part");
    PathBuf::from(raw)
}

/// `%LOCALAPPDATA%\SeraBot\updates` (or the XDG data fallback off Windows).
fn downloads_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(|base| PathBuf::from(base).join("SeraBot").join("updates"))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(".local/share/SeraBot/updates"))
    }
}

/// Batch helper run after this process exits: wait for the PID, replace the
/// executable, restart it, and append failures to `update.log`. `{}` in the
/// PowerShell block is escaped for `format!`; paths are quoted and `%`-escaped.
pub fn helper_script(pid: u32, new_exe: &Path, current_exe: &Path, log: &Path) -> String {
    let new_exe = batch_quote(new_exe);
    let current_exe = batch_quote(current_exe);
    let log = batch_quote(log);
    format!(
        "@echo off\r\n\
         setlocal\r\n\
         :wait\r\n\
         powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command \"while (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ Start-Sleep -Milliseconds 500 }}\" >> {log} 2>&1\r\n\
         move /Y {new_exe} {current_exe} >> {log} 2>&1\r\n\
         if errorlevel 1 goto failed\r\n\
         start \"\" {current_exe}\r\n\
         del \"%~f0\"\r\n\
         exit /b 0\r\n\
         :failed\r\n\
         echo Update failed. The previous executable was left in place. >> {log}\r\n\
         del \"%~f0\"\r\n\
         exit /b 1\r\n"
    )
}

fn batch_quote(path: &Path) -> String {
    let mut text = path.to_string_lossy().replace('%', "%%");
    text.retain(|character| character != '"' && character != '\r' && character != '\n');
    format!("\"{text}\"")
}

/// Verify once more, then hand off to a detached helper. The helper waits for
/// this process to exit before moving the verified executable into place.
pub fn spawn_apply(info: &UpdateInfo) -> Result<(), String> {
    verify_file(&info.exe_path, &info.sha256)
        .map_err(|_| "The downloaded update no longer matches its SHA-256 checksum.".to_owned())?;
    spawn_detached(info)
}

#[cfg(windows)]
fn spawn_detached(info: &UpdateInfo) -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    // CREATE_NO_WINDOW: no console flashes while the helper waits.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let current = std::env::current_exe()
        .map_err(|_| "Could not locate the running executable.".to_owned())?;
    let dir = info
        .exe_path
        .parent()
        .ok_or_else(|| "The update path is invalid.".to_owned())?;
    let log = dir.join("update.log");
    let script = helper_script(std::process::id(), &info.exe_path, &current, &log);
    let script_path = dir.join("apply-update.cmd");
    fs::write(&script_path, script)
        .map_err(|_| "Could not write the update helper script.".to_owned())?;
    std::process::Command::new("cmd")
        .arg("/C")
        .arg(&script_path)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|_| "Could not start the update helper.".to_owned())?;
    Ok(())
}

#[cfg(not(windows))]
fn spawn_detached(_info: &UpdateInfo) -> Result<(), String> {
    Err("Updates can only be installed on Windows.".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "serabot-updater-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn release_json() -> String {
        r#"{
            "tag_name": "v0.2.0",
            "body": "Synthetic release notes.",
            "assets": [
                {
                    "name": "serabot-desktop-latest-windows-x64.exe",
                    "size": 2048,
                    "browser_download_url": "https://github.com/owner/repo/releases/download/v0.2.0/serabot-desktop-latest-windows-x64.exe"
                },
                {
                    "name": "SHA256SUMS",
                    "size": 128,
                    "browser_download_url": "https://github.com/owner/repo/releases/download/v0.2.0/SHA256SUMS"
                }
            ]
        }"#
        .to_owned()
    }

    #[test]
    fn versions_strip_v_and_ignore_prereleases_for_stable_installs() {
        assert_eq!(parse_version("v1.2.3").unwrap(), Version::new(1, 2, 3));
        assert_eq!(parse_version("1.2.3").unwrap(), Version::new(1, 2, 3));
        assert_eq!(parse_version("V1.2.3").unwrap(), Version::new(1, 2, 3));
        assert!(parse_version("v").is_none());
        assert!(parse_version("release-1.2.3").is_none());
        assert!(parse_version("1.2").is_none());

        let current = Version::new(1, 2, 3);
        assert!(is_newer(&Version::new(1, 2, 4), &current));
        assert!(is_newer(&Version::new(2, 0, 0), &current));
        assert!(!is_newer(&Version::new(1, 2, 3), &current));
        assert!(!is_newer(
            &Version::parse("1.2.4-beta.1").unwrap(),
            &current
        ));

        let prerelease = Version::parse("1.2.3-beta.2").unwrap();
        assert!(is_newer(
            &Version::parse("1.2.3-beta.3").unwrap(),
            &prerelease
        ));
        assert!(is_newer(&Version::new(1, 2, 3), &prerelease));
        assert!(!is_newer(
            &Version::parse("1.2.3-beta.1").unwrap(),
            &prerelease
        ));
    }

    #[test]
    fn repository_names_stay_on_the_owner_slash_repo_shape() {
        assert!(valid_repo("owner/repo"));
        assert!(valid_repo("Owner-1/repo.name_2"));
        for repo in [
            "",
            "owner",
            "owner/",
            "/repo",
            "owner/repo/extra",
            "owner/../repo",
            "owner/repo..name",
            "own er/repo",
            "owner/re po",
            "owner/repo\n",
        ] {
            assert!(!valid_repo(repo), "{repo}");
        }
    }

    #[test]
    fn release_json_parses_and_selects_stable_assets() {
        let release = parse_release(release_json().as_bytes()).unwrap();
        assert_eq!(release.tag_name, "v0.2.0");
        assert_eq!(release.assets.len(), 2);
        let (executable, checksums) = select_assets(&release).unwrap();
        assert_eq!(executable.name, DESKTOP_ASSET);
        assert_eq!(checksums.name, CHECKSUMS_ASSET);

        let mut insecure: Release = serde_json::from_str(&release_json()).unwrap();
        insecure.assets[0].browser_download_url =
            format!("http://github.com/owner/repo/releases/download/v0.2.0/{DESKTOP_ASSET}");
        assert!(select_assets(&insecure).is_none());

        let mut missing = release;
        missing.assets.retain(|asset| asset.name != CHECKSUMS_ASSET);
        assert!(select_assets(&missing).is_none());
    }

    #[test]
    fn malformed_release_json_is_rejected_without_panicking() {
        for body in [
            br#"{"assets": "nope"}"#.as_slice(),
            br#"{"tag_name": 5}"#.as_slice(),
            br#"[]"#.as_slice(),
            br#"{"tag_name": "v0.2.0"#.as_slice(),
        ] {
            assert_eq!(
                parse_release(body).unwrap_err(),
                UpdateError::InvalidRelease
            );
        }
        let empty = parse_release(br#"{}"#).unwrap();
        assert!(parse_version(&empty.tag_name).is_none());
        assert!(select_assets(&empty).is_none());
    }

    #[test]
    fn sha256sums_are_parsed_and_looked_up_by_exact_name() {
        let hex = "ab".repeat(32);
        let text = format!(
            "{hex}  {DESKTOP_ASSET}\n{} *other.exe\nnot-a-hash  ignored.exe\n{hex}  nested/file.exe\n",
            "00".repeat(32)
        );
        assert_eq!(checksum_for(&text, DESKTOP_ASSET), Some([0xab; 32]));
        assert_eq!(checksum_for(&text, "other.exe"), Some([0x00; 32]));
        assert!(checksum_for(&text, "missing.exe").is_none());
        assert!(checksum_for(&text, "nested/file.exe").is_none());
        assert!(parse_sha256sums(&hex).is_empty());
    }

    #[test]
    fn sha256_verification_rejects_a_mismatch() {
        let dir = temp_dir("hash");
        let path = dir.join("payload.bin");
        fs::write(&path, b"synthetic update payload").unwrap();
        assert_eq!(
            verify_file(&path, &[0u8; 32]).unwrap_err(),
            UpdateError::HashMismatch
        );
        let mut hasher = Sha256::new();
        hasher.update(b"synthetic update payload");
        let digest: [u8; 32] = hasher.finalize().into();
        verify_file(&path, &digest).unwrap();
        assert_eq!(sha256_file(&path).unwrap(), digest);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cached_update_is_used_only_when_still_newer_and_still_verified() {
        let dir = temp_dir("cache");
        let exe = dir.join("serabot-desktop-9.9.9.exe");
        fs::write(&exe, b"synthetic cached update").unwrap();
        let mut hasher = Sha256::new();
        hasher.update(b"synthetic cached update");
        let digest: [u8; 32] = hasher.finalize().into();
        let cached = CachedUpdate {
            version: "9.9.9".into(),
            notes: "Cached notes".into(),
            size: 23,
            exe: "serabot-desktop-9.9.9.exe".into(),
            sha256: to_hex(&digest),
        };
        fs::write(
            dir.join(CACHE_FILE),
            serde_json::to_string(&cached).unwrap(),
        )
        .unwrap();

        let updater = Updater::new("owner/repo", Version::new(0, 1, 0), &dir).unwrap();
        let info = updater.cached().unwrap();
        assert_eq!(info.version, Version::new(9, 9, 9));
        assert_eq!(info.exe_path, exe);
        assert_eq!(info.size, 23);

        // A cache entry for an already-installed version is not offered.
        let same = Updater::new("owner/repo", Version::new(9, 9, 9), &dir).unwrap();
        assert!(same.cached().is_none());

        // Tampering with the cached executable invalidates it.
        fs::write(&exe, b"tampered").unwrap();
        assert!(updater.cached().is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn helper_script_waits_moves_restarts_and_logs() {
        let new_exe =
            Path::new(r"C:\Users\owner\AppData\Local\SeraBot\updates\serabot-desktop-9.9.9.exe");
        let current = Path::new(r"C:\Program Files\SeraBot\serabot-desktop.exe");
        let log = Path::new(r"C:\Users\owner\AppData\Local\SeraBot\updates\update.log");
        let script = helper_script(4242, new_exe, current, log);
        assert!(script.contains("Get-Process -Id 4242"));
        assert!(script.contains("Start-Sleep -Milliseconds 500"));
        assert!(script.contains(
            "move /Y \"C:\\Users\\owner\\AppData\\Local\\SeraBot\\updates\\serabot-desktop-9.9.9.exe\" \"C:\\Program Files\\SeraBot\\serabot-desktop.exe\""
        ));
        assert!(script.contains("start \"\" \"C:\\Program Files\\SeraBot\\serabot-desktop.exe\""));
        assert!(script.contains("update.log"));
        assert!(script.contains("del \"%~f0\""));
        assert!(script.contains("exit /b 1"));
        assert!(script.contains("Update failed. The previous executable was left in place."));

        let escaped = helper_script(
            1,
            Path::new("C:/100%/update.exe"),
            Path::new("C:/app/current.exe"),
            Path::new("C:/app/update.log"),
        );
        assert!(escaped.contains("C:/100%%/update.exe"));
        assert!(!escaped.contains("C:/100%/update.exe"));
    }

    #[cfg(not(windows))]
    #[test]
    fn installation_is_rejected_off_windows() {
        let info = UpdateInfo {
            version: Version::new(9, 9, 9),
            notes: String::new(),
            size: 0,
            exe_path: PathBuf::from("missing-synthetic-update.exe"),
            sha256: [0u8; 32],
        };
        let error = spawn_apply(&info).unwrap_err();
        assert!(error.contains("SHA-256") || error.contains("Windows"));
    }
}
