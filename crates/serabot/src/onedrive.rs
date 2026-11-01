//! Microsoft Graph OneDrive delivery for approved artifacts.
//!
//! This client is intentionally small and auditable. It uploads a single byte
//! buffer to a folder below a configured `base_folder` and never reads,
//! enumerates, or deletes other drive content.
//!
//! # Permissions
//!
//! The refresh-token flow requests `offline_access Files.ReadWrite.All User.Read`,
//! matching the existing OneDrive MCP registration on the operator host. This is a
//! broad delegated permission: it can write anywhere in the signed-in drive.
//! `Files.ReadWrite.AppFolder` would be narrower, but this agent must deliver
//! into the operator's existing dossier folder, which is not an app folder.
//! The effective allowlist is therefore enforced here: every destination is
//! `base_folder` plus validated relative segments, and `..`, absolute paths,
//! and separator tricks are rejected. Keep the application registration and
//! the signed-in account scoped accordingly.
//!
//! The registration is treated as a public client: the token request sends
//! only `client_id`, `grant_type`, `refresh_token`, and `scope`, with no client
//! secret. The registration must therefore allow public client flows, and the
//! initial refresh token is obtained out of band (for example with a
//! device-code or loopback flow).
//!
//! # Secrets
//!
//! The refresh token is supplied already decrypted by the caller. When the
//! token endpoint returns a different refresh token, that value is written
//! back through the configured [`CredentialWriter`]: the age store for the
//! legacy single-operator client, or the acting user's envelope-encrypted
//! credential row for the per-user registry ([`OneDriveRegistry`], phase 4).
//! If an access-token secret name was configured, the new access token is
//! written in the same update. A failed write fails the call. Tokens are
//! never logged. Access tokens otherwise stay in memory until shortly before
//! expiry. `Debug` output redacts tokens and the client id.
//!
//! # Per-user drives
//!
//! [`OneDriveRegistry`] builds one client per user from that user's own
//! sealed refresh token and the server app registration, caches it, and
//! shares one `reqwest::Client`. A user with no connected drive gets an
//! explicit "connect OneDrive first" error — never another account's drive.
//! [`OneDriveConsent`] runs the authorization-code + PKCE consent against the
//! shared Microsoft app registration; the callback seals the refresh token
//! under the user's DEK before rendering a success page.
//!
//! # Outcome semantics
//!
//! Uploads are external effects, so an ambiguous transport failure (timeout or
//! connection reset after the request was dispatched) is reported as
//! `SeraError::External` containing the words "outcome unknown". Callers must
//! not blindly retry such failures; the file may already exist. GET requests
//! used for folder discovery are the only operations retried.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use rand::RngCore;
use reqwest::StatusCode;
use reqwest::header::{CONTENT_RANGE, CONTENT_TYPE};
use secrecy::ExposeSecret;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tracing::debug;
use url::Url;

use crate::config::{Config, valid_secret_name};
use crate::error::{Result, SeraError};
use crate::secrets::{AgeCredentialWriter, CredentialWriter, SharedSecrets};
use crate::store::Store;
use crate::user_secrets::{UserCredentialWriter, UserSecrets};
use crate::util::sha256_hex;

/// Files smaller than this use a single `PUT .../content` request.
const SMALL_UPLOAD_LIMIT: usize = 4 * 1024 * 1024;

/// Chunk size for upload sessions. Microsoft requires a multiple of 320 KiB
/// for multi-chunk uploads; 5 MiB is a clean multiple.
const UPLOAD_CHUNK_SIZE: usize = 5 * 1024 * 1024;

/// Hard cap on in-memory uploads. Artifacts are rendered decks and documents;
/// 250 MiB leaves ample headroom while bounding memory on a small VPS.
const MAX_UPLOAD_BYTES: usize = 250 * 1024 * 1024;

/// Delegated permission set. `Files.ReadWrite.All` is broad; see the module docs.
const TOKEN_SCOPE: &str = "offline_access Files.ReadWrite.All User.Read";

const LIST_PAGE_SIZE: u32 = 200;
const LIST_MAX_PAGES_CAP: usize = 10;

/// Refresh this long before the reported expiry to avoid racing the clock.
const REFRESH_SKEW: Duration = Duration::from_secs(60);

const MAX_FOLDER_DEPTH: usize = 8;
const MAX_SUBFOLDER_DEPTH: usize = 4;
const MAX_SEGMENT_CHARS: usize = 200;

/// Maximum response body we will buffer from Graph or the token endpoint.
const MAX_RESPONSE_BYTES: usize = 256 * 1024;

const GET_MAX_ATTEMPTS: usize = 3;

/// Result of a successful upload.
///
/// With `conflict_behavior = "rename"` Graph may rename the artifact (for
/// example `report 1.pdf`); `name` and `path` always reflect what was actually
/// stored. `web_url` is empty only if Graph omitted it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadResult {
    pub id: String,
    pub name: String,
    pub web_url: String,
    /// Destination path, for example `/SeraBot/Deliverables/report.pdf`.
    pub path: String,
    pub size: u64,
    /// SHA-256 of the uploaded bytes, computed locally before the request.
    pub sha256: String,
}

/// One entry from a bounded folder listing below `base_folder`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveListItem {
    pub id: String,
    pub name: String,
    pub folder: bool,
    pub size: u64,
    pub path: String,
}

/// Result of listing a folder under the configured allowlist root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListFolderResult {
    pub items: Vec<DriveListItem>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConflictBehavior {
    Rename,
    Fail,
}

impl ConflictBehavior {
    fn parse(value: &str) -> Result<Self> {
        match value.trim() {
            "rename" => Ok(Self::Rename),
            "fail" => Ok(Self::Fail),
            _ => Err(SeraError::Config(
                "onedrive.conflict_behavior must be 'rename' or 'fail'; overwriting is not permitted"
                    .into(),
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Rename => "rename",
            Self::Fail => "fail",
        }
    }
}

/// Refresh token and cached access token. Deliberately has no `Debug` impl.
#[derive(Default)]
struct TokenState {
    access_token: Option<String>,
    refresh_token: String,
    expires_at: Option<Instant>,
    /// A new refresh token (and access token, when named) is not in the age store yet.
    refresh_unpersisted: bool,
}

struct RefreshPersistence {
    writer: Arc<dyn CredentialWriter>,
}

/// Microsoft Graph OneDrive client for approved artifacts.
///
/// Construct from already-decrypted values. Rotated credentials are written
/// back only after [`Self::with_refresh_persistence`]. A new refresh token
/// without that writer, or a failed store write, fails the call. Share the
/// client through an owner that serializes `&mut self` calls, or wrap it in a
/// mutex. Token refreshes are serialized internally so a shared client cannot
/// stampede the token endpoint.
pub struct OneDriveClient {
    http: reqwest::Client,
    graph_base: String,
    token_url: String,
    client_id: String,
    base_folder: String,
    conflict_behavior: ConflictBehavior,
    tokens: Mutex<TokenState>,
    persistence: Option<RefreshPersistence>,
}

impl fmt::Debug for OneDriveClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OneDriveClient")
            .field("graph_base", &self.graph_base)
            .field("base_folder", &self.base_folder)
            .field("conflict_behavior", &self.conflict_behavior)
            .field("client_id", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("access_token", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl OneDriveClient {
    /// Build a client for the given tenant and destination folder.
    ///
    /// All arguments are plain values: the caller is responsible for
    /// decrypting secrets and for choosing an approved destination.
    ///
    /// Validation:
    /// - `graph_base` must be HTTPS; HTTP is accepted only for loopback test
    ///   servers (`localhost`, `127.0.0.1`, `::1`).
    /// - `tenant` must be a simple id or domain (no path separators).
    /// - `base_folder` must be relative, non-empty, at most `MAX_FOLDER_DEPTH`
    ///   segments, with no `.`/`..`, empty, or separator-like segments.
    /// - `conflict_behavior` must be `rename` or `fail`.
    pub fn new(
        http: reqwest::Client,
        graph_base: String,
        tenant: String,
        client_id: String,
        refresh_token: String,
        base_folder: String,
        conflict_behavior: String,
    ) -> Result<Self> {
        let graph_base = validate_endpoint(&graph_base, "graph_base", false)?;
        let graph_base = graph_base.trim_end_matches('/').to_owned();
        let tenant = validate_tenant(&tenant)?;
        let client_id = validate_secret_like(&client_id, "client_id")?;
        let refresh_token = validate_secret_like(&refresh_token, "refresh_token")?;
        let base_folder = validate_segments(&base_folder, MAX_FOLDER_DEPTH, "base_folder")
            .map_err(SeraError::Config)?
            .join("/");
        let conflict_behavior = ConflictBehavior::parse(&conflict_behavior)?;

        Ok(Self {
            http,
            graph_base,
            token_url: format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token"),
            client_id,
            base_folder,
            conflict_behavior,
            tokens: Mutex::new(TokenState {
                refresh_token,
                ..TokenState::default()
            }),
            persistence: None,
        })
    }

    /// Write rotated refresh tokens back to the age store.
    ///
    /// `access_secret_name` is included in the same write when it is set.
    /// OneDrive has no access-token secret, so production passes `None` and
    /// only the refresh token is persisted.
    pub fn with_refresh_persistence(
        mut self,
        secrets: SharedSecrets,
        refresh_secret_name: impl Into<String>,
        access_secret_name: Option<&str>,
    ) -> Result<Self> {
        let refresh_secret_name = refresh_secret_name.into();
        let refresh_secret_name = refresh_secret_name.trim();
        if !valid_secret_name(refresh_secret_name) {
            return Err(SeraError::Config(
                "onedrive refresh secret name is missing or invalid".into(),
            ));
        }
        let access_secret_name = access_secret_name.unwrap_or("").trim();
        if !access_secret_name.is_empty() && !valid_secret_name(access_secret_name) {
            return Err(SeraError::Config(
                "onedrive access secret name is invalid".into(),
            ));
        }
        let writer =
            AgeCredentialWriter::new(secrets, refresh_secret_name, access_secret_name.to_owned())?;
        self.persistence = Some(RefreshPersistence {
            writer: Arc::new(writer),
        });
        Ok(self)
    }

    /// Write rotated refresh tokens back to a custom target. The per-user
    /// registry passes a writer that replaces the acting user's sealed
    /// OneDrive credential.
    pub fn with_credential_writer(mut self, writer: Arc<dyn CredentialWriter>) -> Result<Self> {
        self.persistence = Some(RefreshPersistence { writer });
        Ok(self)
    }

    /// Override the token endpoint. Intended for tests; HTTPS (or loopback
    /// HTTP) is enforced the same way as for `graph_base`.
    pub fn set_token_url(&mut self, token_url: impl Into<String>) -> Result<()> {
        let token_url = token_url.into();
        self.token_url = validate_endpoint(&token_url, "token_url", false)?;
        Ok(())
    }

    /// Builder form of [`Self::set_token_url`].
    pub fn with_token_url(mut self, token_url: impl Into<String>) -> Result<Self> {
        self.set_token_url(token_url)?;
        Ok(self)
    }

    /// List children of `base_folder` (or `base_folder/subfolder` when set).
    ///
    /// Pagination is capped at `max_pages` (clamped to `LIST_MAX_PAGES_CAP`); each
    /// page requests up to `LIST_PAGE_SIZE` items. Only GETs are retried.
    pub async fn list_folder(
        &self,
        subfolder: Option<&str>,
        max_pages: usize,
    ) -> Result<ListFolderResult> {
        let folders = self.destination_folders(subfolder)?;
        let max_pages = max_pages.clamp(1, LIST_MAX_PAGES_CAP);
        let encoded = encode_path(&folders);
        let mut url = format!(
            "{}/me/drive/root:/{}:/children?$top={LIST_PAGE_SIZE}",
            self.graph_base, encoded
        );
        let parent_path = display_folder_path(&folders);

        let mut items = Vec::new();
        let mut pages = 0usize;
        let mut truncated = false;

        while pages < max_pages {
            pages += 1;
            let token = self.access_token().await?;
            let response = self
                .graph_get_with_retries(&url, &token, "folder listing")
                .await?;
            let body = read_limited(response, MAX_RESPONSE_BYTES).await?;
            let page: ListPageResponse = serde_json::from_slice(&body).map_err(|error| {
                SeraError::External(format!("invalid OneDrive list response: {error}"))
            })?;

            for entry in page.value {
                if entry.id.is_empty() || entry.name.is_empty() {
                    continue;
                }
                items.push(DriveListItem {
                    id: entry.id,
                    name: entry.name.clone(),
                    folder: entry.folder.is_some(),
                    size: entry.size,
                    path: format!("{}/{}", parent_path.trim_end_matches('/'), entry.name),
                });
            }

            match page.next_link.filter(|link| !link.is_empty()) {
                Some(next) if pages < max_pages => {
                    url = validate_pagination_url(&next, &self.graph_base)?;
                }
                Some(_) => {
                    truncated = true;
                    break;
                }
                None => break,
            }
        }

        Ok(ListFolderResult { items, truncated })
    }

    /// Upload `bytes` as `filename` below `base_folder` (and `subfolder`).
    ///
    /// `filename` is sanitized: directory components, control characters, and
    /// characters OneDrive rejects are removed or replaced. `subfolder` is
    /// relative to `base_folder` and is validated as a folder path.
    ///
    /// The destination folder (including the base folder) is created when
    /// missing. Files below `SMALL_UPLOAD_LIMIT` use a single PUT; larger
    /// files use an upload session with 5 MiB chunks. `mime` is sent as
    /// `Content-Type` on the single-request path; upload sessions let Graph
    /// infer the type from the file extension.
    ///
    /// Errors after a mutating request was dispatched may carry "outcome
    /// unknown": the artifact may or may not have been stored.
    pub async fn upload(
        &mut self,
        filename: &str,
        bytes: Vec<u8>,
        mime: &str,
        subfolder: Option<&str>,
    ) -> Result<UploadResult> {
        let filename = safe_filename(filename)?;
        let mime = validate_mime(mime)?;
        if bytes.len() > MAX_UPLOAD_BYTES {
            return Err(SeraError::Validation(format!(
                "artifact exceeds the {MAX_UPLOAD_BYTES}-byte OneDrive upload limit"
            )));
        }

        let folders = self.destination_folders(subfolder)?;

        self.ensure_folder(&folders).await?;

        if bytes.len() < SMALL_UPLOAD_LIMIT {
            self.upload_single(&folders, &filename, mime, bytes).await
        } else {
            self.upload_chunked(&folders, &filename, &bytes).await
        }
    }

    /// Return a cached access token, refreshing it when it is missing or
    /// within `REFRESH_SKEW` of expiry.
    ///
    /// A refresh token that differs from the one already held is written to
    /// the age store before the access token is returned. The call fails if
    /// that write fails, and the rotated token stays in memory for a retry.
    async fn access_token(&self) -> Result<String> {
        let mut state = self.tokens.lock().await;
        if state.refresh_unpersisted {
            self.persist_rotated(&state)?;
            state.refresh_unpersisted = false;
        }

        if let (Some(token), Some(expires_at)) = (state.access_token.as_deref(), state.expires_at)
            && Instant::now() + REFRESH_SKEW < expires_at
        {
            return Ok(token.to_owned());
        }

        let current_refresh_token = state.refresh_token.clone();
        let response = self
            .http
            .post(&self.token_url)
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", current_refresh_token.as_str()),
                ("scope", TOKEN_SCOPE),
            ])
            .send()
            .await
            .map_err(|error| transport_error("token refresh", error, false))?;

        let status = response.status();
        if !status.is_success() {
            let body = read_limited(response, MAX_RESPONSE_BYTES)
                .await
                .unwrap_or_default();
            let code = serde_json::from_slice::<TokenResponse>(&body)
                .ok()
                .and_then(|token| token.error)
                .filter(|code| !code.is_empty())
                .map(|code| sanitize_code(&code))
                .unwrap_or_else(|| "unknown_error".into());
            return Err(SeraError::External(format!(
                "OneDrive token refresh failed (HTTP {}): {code}",
                status.as_u16()
            )));
        }

        let body = read_limited(response, MAX_RESPONSE_BYTES)
            .await
            .map_err(|_| {
                SeraError::External("OneDrive token refresh returned an unreadable response".into())
            })?;
        let token: TokenResponse = serde_json::from_slice(&body)
            .map_err(|_| SeraError::External("invalid OneDrive token response".into()))?;

        if let Some(error) = token.error.as_deref().filter(|value| !value.is_empty()) {
            return Err(SeraError::External(format!(
                "OneDrive token refresh failed: {}",
                sanitize_code(error)
            )));
        }

        let access_token = token
            .access_token
            .filter(|token| !token.is_empty())
            .ok_or_else(|| {
                SeraError::External("OneDrive token refresh returned no access token".into())
            })?;
        if !is_plausible_token(&access_token) {
            return Err(SeraError::External(
                "OneDrive token refresh returned an invalid access token".into(),
            ));
        }

        let rotated = match token.refresh_token {
            Some(refresh_token) if !refresh_token.is_empty() => {
                if !is_plausible_token(&refresh_token) {
                    return Err(SeraError::External(
                        "OneDrive token refresh returned an invalid refresh token".into(),
                    ));
                }
                Some(refresh_token)
            }
            _ => None,
        };

        let ttl = token.expires_in.unwrap_or(3600).clamp(1, 24 * 60 * 60);
        state.access_token = Some(access_token.clone());
        state.expires_at = Some(Instant::now() + Duration::from_secs(ttl));
        if let Some(refresh_token) = rotated
            && refresh_token != current_refresh_token
        {
            state.refresh_token = refresh_token;
            state.refresh_unpersisted = true;
        }
        if state.refresh_unpersisted {
            self.persist_rotated(&state)?;
            state.refresh_unpersisted = false;
        }
        debug!(expires_in = ttl, "refreshed OneDrive access token");

        Ok(access_token)
    }

    fn persist_rotated(&self, state: &TokenState) -> Result<()> {
        let Some(persistence) = &self.persistence else {
            return Err(SeraError::Secrets(
                "OneDrive rotated refresh token was not persisted".into(),
            ));
        };
        let refresh = state.refresh_token.as_str();
        if refresh.is_empty() {
            return Err(SeraError::Secrets(
                "OneDrive rotated refresh token was not persisted".into(),
            ));
        }
        let access = state.access_token.as_deref().unwrap_or("");
        persistence
            .writer
            .persist_rotation(refresh, Some(access).filter(|value| !value.is_empty()))
            .map_err(|error| redact_persist_error(error, &[refresh, access]))
    }

    /// Single-request upload for files below `SMALL_UPLOAD_LIMIT`.
    async fn upload_single(
        &self,
        folders: &[String],
        filename: &str,
        mime: &str,
        bytes: Vec<u8>,
    ) -> Result<UploadResult> {
        let size = bytes.len() as u64;
        let sha256 = sha256_hex(&bytes);
        let token = self.access_token().await?;
        let url = format!(
            "{}/me/drive/root:/{}:/content?@microsoft.graph.conflictBehavior={}",
            self.graph_base,
            encode_file_path(folders, filename),
            self.conflict_behavior.as_str()
        );
        debug!(
            path = %display_path(folders, filename),
            size,
            "uploading artifact to OneDrive with a single request"
        );

        let response = self
            .http
            .put(&url)
            .bearer_auth(&token)
            .header(CONTENT_TYPE, mime)
            .body(bytes)
            .send()
            .await
            .map_err(|error| transport_error("single-request upload", error, true))?;

        let status = response.status();
        if status == StatusCode::OK || status == StatusCode::CREATED {
            let item = read_item(response)
                .await
                .map_err(|_| outcome_unknown("the upload response could not be read or parsed"))?;
            let result = finish_upload(item, folders, filename, size, &sha256)?;
            debug!(
                id = %result.id,
                path = %result.path,
                size,
                "OneDrive upload completed"
            );
            return Ok(result);
        }

        Err(graph_error(response, "single-request upload").await)
    }

    /// Chunked upload using a Graph upload session.
    async fn upload_chunked(
        &self,
        folders: &[String],
        filename: &str,
        bytes: &[u8],
    ) -> Result<UploadResult> {
        let total = bytes.len() as u64;
        let sha256 = sha256_hex(bytes);
        let token = self.access_token().await?;

        let session_endpoint = format!(
            "{}/me/drive/root:/{}:/createUploadSession",
            self.graph_base,
            encode_file_path(folders, filename)
        );
        let session_body = json!({
            "item": {
                "@microsoft.graph.conflictBehavior": self.conflict_behavior.as_str(),
                "name": filename,
            }
        });
        debug!(
            path = %display_path(folders, filename),
            size = total,
            "creating OneDrive upload session"
        );

        let response = self
            .http
            .post(&session_endpoint)
            .bearer_auth(&token)
            .json(&session_body)
            .send()
            .await
            .map_err(|error| transport_error("upload session creation", error, false))?;

        let status = response.status();
        if status != StatusCode::OK && status != StatusCode::CREATED {
            return Err(graph_error(response, "upload session creation").await);
        }

        let body = read_limited(response, MAX_RESPONSE_BYTES)
            .await
            .map_err(|_| {
                SeraError::External(
                    "OneDrive upload session creation returned an unreadable response".into(),
                )
            })?;
        let session: UploadSession = serde_json::from_slice(&body).map_err(|error| {
            SeraError::External(format!("invalid OneDrive upload session response: {error}"))
        })?;

        // The upload URL is pre-authenticated and may contain a bearer-like
        // secret in its path. Validate its scheme, never log it, and never send
        // the Graph access token to it (Graph rejects mixed Authorization).
        let upload_url =
            validate_endpoint(&session.upload_url, "uploadUrl", true).map_err(|_| {
                SeraError::External("OneDrive returned an invalid upload session URL".into())
            })?;

        let mut offset = 0usize;
        while offset < bytes.len() {
            let end = (offset + UPLOAD_CHUNK_SIZE).min(bytes.len());
            let range = format!("bytes {}-{}/{}", offset, end - 1, total);

            let response = self
                .http
                .put(&upload_url)
                .header(CONTENT_RANGE, range.as_str())
                .body(bytes[offset..end].to_vec())
                .send()
                .await
                .map_err(|error| transport_error("upload chunk", error, true))?;

            let status = response.status();
            if status == StatusCode::ACCEPTED {
                // Best-effort progress check. A mismatch means the session is
                // out of sync; stop rather than send overlapping ranges.
                if let Ok(body) = read_limited(response, MAX_RESPONSE_BYTES).await
                    && let Ok(progress) = serde_json::from_slice::<UploadProgress>(&body)
                    && let Some(next) = progress.next_expected_ranges.first()
                    && let Some(start) = range_start(next)
                    && start != end as u64
                {
                    return Err(SeraError::External(format!(
                        "OneDrive upload session is out of sync: server expects byte {start}, client sent through {}",
                        end - 1
                    )));
                }
                offset = end;
            } else if status == StatusCode::OK || status == StatusCode::CREATED {
                if end != bytes.len() {
                    return Err(SeraError::External(
                        "OneDrive upload session completed before the final chunk".into(),
                    ));
                }
                let item = read_item(response).await.map_err(|_| {
                    outcome_unknown("the final chunk response could not be read or parsed")
                })?;
                let result = finish_upload(item, folders, filename, total, &sha256)?;
                debug!(
                    id = %result.id,
                    path = %result.path,
                    size = total,
                    "OneDrive chunked upload completed"
                );
                return Ok(result);
            } else {
                return Err(graph_error(response, "upload chunk").await);
            }
        }

        Err(SeraError::External(
            "OneDrive upload session ended without a completion response; the artifact may not have been stored"
                .into(),
        ))
    }

    /// Ensure the destination folder and all of its parents exist.
    ///
    /// One GET checks the final folder; only when it is missing do we create
    /// the levels top-down. Folder creation is not retried (it is not
    /// idempotent), but a 409 conflict is treated as "already exists".
    async fn ensure_folder(&self, folders: &[String]) -> Result<()> {
        if folders.is_empty() {
            return Ok(());
        }
        let encoded = encode_path(folders);
        if self.folder_exists(&encoded).await? {
            return Ok(());
        }

        let mut parent: Vec<String> = Vec::new();
        for segment in folders {
            self.create_folder(&parent, segment).await?;
            parent.push(segment.clone());
        }
        Ok(())
    }

    fn destination_folders(&self, subfolder: Option<&str>) -> Result<Vec<String>> {
        let mut folders: Vec<String> = self.base_folder.split('/').map(str::to_owned).collect();
        if let Some(subfolder) = subfolder {
            let subfolder = validate_segments(subfolder, MAX_SUBFOLDER_DEPTH, "subfolder")
                .map_err(SeraError::Validation)?;
            folders.extend(subfolder);
        }
        Ok(folders)
    }

    /// Idempotent folder lookup with bounded retries for 429/5xx and
    /// transport failures. Returns `false` on a definitive 404.
    async fn folder_exists(&self, encoded_path: &str) -> Result<bool> {
        let token = self.access_token().await?;
        let url = format!("{}/me/drive/root:/{}", self.graph_base, encoded_path);
        match self
            .graph_get_with_retries(&url, &token, "folder lookup")
            .await
        {
            Ok(response) if response.status().is_success() => Ok(true),
            Ok(response) if response.status() == StatusCode::NOT_FOUND => Ok(false),
            Ok(response) => Err(graph_error(response, "folder lookup").await),
            Err(error) => Err(error),
        }
    }

    async fn graph_get_with_retries(
        &self,
        url: &str,
        token: &str,
        context: &str,
    ) -> Result<reqwest::Response> {
        let mut last_error: Option<SeraError> = None;
        for attempt in 0..GET_MAX_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(250 * attempt as u64)).await;
            }

            match self.http.get(url).bearer_auth(token).send().await {
                Ok(response)
                    if response.status().is_success()
                        || response.status() == StatusCode::NOT_FOUND =>
                {
                    return Ok(response);
                }
                Ok(response)
                    if response.status() == StatusCode::TOO_MANY_REQUESTS
                        || response.status().is_server_error() =>
                {
                    last_error = Some(graph_error(response, context).await);
                }
                Ok(response) => return Err(graph_error(response, context).await),
                Err(error) if error.is_timeout() || error.is_connect() => {
                    last_error = Some(transport_error(context, error, false));
                }
                Err(error) => return Err(transport_error(context, error, false)),
            }
        }

        Err(last_error.unwrap_or_else(|| {
            SeraError::External(format!("OneDrive {context} failed after retries"))
        }))
    }

    /// Create one folder below `parent`. Not retried.
    async fn create_folder(&self, parent: &[String], name: &str) -> Result<()> {
        let token = self.access_token().await?;
        let url = if parent.is_empty() {
            format!("{}/me/drive/root/children", self.graph_base)
        } else {
            format!(
                "{}/me/drive/root:/{}:/children",
                self.graph_base,
                encode_path(parent)
            )
        };
        let body = json!({
            "name": name,
            "folder": {},
            "@microsoft.graph.conflictBehavior": "fail",
        });

        let response = self
            .http
            .post(&url)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map_err(|error| transport_error("folder creation", error, false))?;

        match response.status() {
            status if status.is_success() => Ok(()),
            // A concurrent creator won the race; the folder exists.
            StatusCode::CONFLICT => Ok(()),
            _ => Err(graph_error(response, "folder creation").await),
        }
    }
}

// ---- Per-user drives (multi-user phase 4) ----

/// Provider name under which a user's OneDrive refresh token is stored in the
/// per-user credential store.
pub const ONEDRIVE_PROVIDER: &str = "onedrive";

/// Delegated scopes requested at consent. `offline_access` is what makes
/// Microsoft return a refresh token; `Files.ReadWrite` is the narrowest scope
/// that can deliver into an existing dossier folder (see the module docs).
pub const CONSENT_SCOPE: &str = "offline_access Files.ReadWrite User.Read";

/// Redirect path registered on the app registration for the consent flow.
pub const CONSENT_CALLBACK_PATH: &str = "/v1/connectors/onedrive/callback";

/// Pending consent TTL: a browser round trip, not a session.
pub const CONSENT_STATE_TTL: Duration = Duration::from_secs(10 * 60);
/// Bound on simultaneous in-flight consents; exceeded, new starts fail closed.
pub const MAX_PENDING_CONSENTS: usize = 64;
/// Authorization codes are bounded; providers use far shorter strings.
const MAX_CONSENT_CODE_CHARS: usize = 2048;

/// One per-user client, cached for the process lifetime (restart-to-apply).
type CachedClient = Arc<Mutex<OneDriveClient>>;

/// Per-user OneDrive client registry.
///
/// Builds a [`OneDriveClient`] lazily from the acting user's sealed refresh
/// token and the server app registration, and caches it. Every user gets a
/// client for **their own** credential: there is no fallback to another
/// account's drive, and a user with no connected drive gets an explicit
/// "connect OneDrive first" error. Rotated refresh tokens are written back
/// through the user credential store, never the age store.
///
/// One `reqwest::Client` is shared by every per-user client.
pub struct OneDriveRegistry {
    users: Arc<UserSecrets>,
    http: reqwest::Client,
    graph_base: String,
    tenant: String,
    client_id: String,
    folder: String,
    conflict_behavior: String,
    token_url: Option<String>,
    clients: std::sync::Mutex<std::collections::BTreeMap<String, CachedClient>>,
}

impl fmt::Debug for OneDriveRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OneDriveRegistry")
            .field("graph_base", &self.graph_base)
            .field("tenant", &self.tenant)
            .field("folder", &self.folder)
            .field("client_id", &"<redacted>")
            .field(
                "cached_users",
                &self
                    .clients
                    .lock()
                    .map(|clients| clients.len())
                    .unwrap_or(0),
            )
            .finish_non_exhaustive()
    }
}

impl OneDriveRegistry {
    /// Build the registry from validated configuration. The client id is
    /// required from the age store at startup; a missing value fails
    /// construction so the tools never run half-configured.
    pub fn new(config: &Config, secrets: &SharedSecrets, users: Arc<UserSecrets>) -> Result<Self> {
        if !config.onedrive.enabled {
            return Err(SeraError::Config("onedrive is not enabled".into()));
        }
        let client_id = secrets.require(&config.onedrive.client_id_secret)?;
        let client_id = validate_secret_like(&client_id, "client_id")?;
        let tenant = validate_tenant(&config.onedrive.tenant)?;
        // Validate the destination folder once at startup, before any call.
        validate_segments(&config.onedrive.folder, MAX_FOLDER_DEPTH, "base_folder")
            .map_err(SeraError::Config)?;
        ConflictBehavior::parse(&config.onedrive.conflict_behavior)?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| SeraError::Internal(format!("OneDrive HTTP client: {error}")))?;
        Ok(Self {
            users,
            http,
            graph_base: config.onedrive.graph_base.clone(),
            tenant,
            client_id,
            folder: config.onedrive.folder.clone(),
            conflict_behavior: config.onedrive.conflict_behavior.clone(),
            token_url: None,
            clients: std::sync::Mutex::new(std::collections::BTreeMap::new()),
        })
    }

    /// Override the token endpoint. Intended for tests; HTTPS (or loopback
    /// HTTP) is enforced exactly like the production endpoint.
    pub fn with_token_url(mut self, token_url: impl Into<String>) -> Result<Self> {
        self.token_url = Some(validate_endpoint(&token_url.into(), "token_url", false)?);
        Ok(self)
    }

    /// Whether the acting user has connected their own drive. Row existence
    /// only: nothing is decrypted and no token is exposed.
    pub fn connected(&self, user_id: &str) -> Result<bool> {
        Ok(self
            .users
            .credential_status(user_id, ONEDRIVE_PROVIDER)?
            .own)
    }

    /// Drop the cached client for one user (disconnect, credential change).
    pub fn invalidate(&self, user_id: &str) {
        self.clients
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(user_id);
    }

    /// The acting user's client, built on first use.
    ///
    /// Fails closed when the user has not connected a drive: the error says
    /// so explicitly and never falls back to the admin's (or any other)
    /// credential.
    pub fn client_for(&self, user_id: &str) -> Result<CachedClient> {
        if let Some(client) = self
            .clients
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(user_id)
            .cloned()
        {
            return Ok(client);
        }
        let refresh = self.users.get(user_id, ONEDRIVE_PROVIDER)?.ok_or_else(|| {
            SeraError::Forbidden(
                "OneDrive is not connected for this account; connect it from the console first"
                    .into(),
            )
        })?;
        let mut client = OneDriveClient::new(
            self.http.clone(),
            self.graph_base.clone(),
            self.tenant.clone(),
            self.client_id.clone(),
            refresh.expose_secret().to_owned(),
            self.folder.clone(),
            self.conflict_behavior.clone(),
        )?
        .with_credential_writer(Arc::new(UserCredentialWriter::plain(
            self.users.clone(),
            user_id,
            ONEDRIVE_PROVIDER,
        )))?;
        if let Some(token_url) = &self.token_url {
            client.set_token_url(token_url.clone())?;
        }
        let client = Arc::new(Mutex::new(client));
        // A concurrent caller may have built the same user's client first;
        // keep the first one so every caller shares one token state.
        Ok(self
            .clients
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(user_id.to_owned())
            .or_insert(client)
            .clone())
    }
}

/// Import the legacy global OneDrive refresh token into the admin's credential
/// row, once.
///
/// Idempotent and lossless: the value is copied verbatim, only when the admin
/// has no OneDrive credential yet, and a `kv` marker makes the import
/// once-only so a later disconnect is not undone by a restart. Nothing secret
/// is logged.
pub fn import_global_refresh_token(
    config: &Config,
    secrets: &SharedSecrets,
    users: &UserSecrets,
    store: &Store,
    admin_id: &str,
) -> Result<bool> {
    const MARKER: &str = "onedrive_global_imported";
    if !config.onedrive.enabled {
        return Ok(false);
    }
    if store.kv_get(MARKER)?.is_some() {
        return Ok(false);
    }
    if users.credential_status(admin_id, ONEDRIVE_PROVIDER)?.own {
        store.kv_set(MARKER, "1")?;
        return Ok(false);
    }
    let Some(token) = secrets.get(&config.onedrive.refresh_token_secret) else {
        // Nothing to import yet; stay unmarked so a later CLI-set token is
        // picked up on the next start.
        return Ok(false);
    };
    users.set(admin_id, ONEDRIVE_PROVIDER, &token)?;
    store.kv_set(MARKER, "1")?;
    Ok(true)
}

/// A pending browser consent: the state binds the callback to the user and
/// the PKCE verifier.
struct PendingConsent {
    user_id: String,
    verifier: String,
    created: Instant,
}

/// OneDrive consent service: authorization code + PKCE against the shared
/// Microsoft app registration, per user.
///
/// The callback is public (the browser is redirected by Microsoft); the
/// `state` value is the only thing that binds it to a user, exactly like the
/// sign-in callback. The refresh token is sealed under the user's DEK before
/// the success page is rendered; no token ever appears in a response.
pub struct OneDriveConsent {
    client_id: String,
    redirect_url: String,
    authorize_url: String,
    token_url: String,
    scope: String,
    http: reqwest::Client,
    users: Arc<UserSecrets>,
    pending: Mutex<HashMap<String, PendingConsent>>,
    ttl: Duration,
}

impl fmt::Debug for OneDriveConsent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OneDriveConsent")
            .field("redirect_url", &self.redirect_url)
            .field("authorize_url", &self.authorize_url)
            .field("client_id", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl OneDriveConsent {
    /// Build the service when OneDrive is enabled; `None` otherwise so every
    /// consent endpoint fails closed.
    pub fn new(
        config: &Config,
        secrets: &SharedSecrets,
        users: Arc<UserSecrets>,
    ) -> Result<Option<Self>> {
        if !config.onedrive.enabled {
            return Ok(None);
        }
        let client_id = secrets.require(&config.onedrive.client_id_secret)?;
        let client_id = validate_secret_like(&client_id, "client_id")?;
        let tenant = validate_tenant(&config.onedrive.tenant)?;
        let base = config
            .onedrive
            .effective_redirect_base(&config.auth)
            .trim()
            .trim_end_matches('/')
            .to_owned();
        let redirect_url = if base.is_empty() {
            String::new()
        } else {
            validate_endpoint(
                &format!("{base}{CONSENT_CALLBACK_PATH}"),
                "onedrive redirect_url",
                false,
            )?
        };
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                SeraError::Internal(format!("OneDrive consent HTTP client: {error}"))
            })?;
        Ok(Some(Self {
            client_id,
            redirect_url,
            authorize_url: format!(
                "https://login.microsoftonline.com/{tenant}/oauth2/v2.0/authorize"
            ),
            token_url: format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token"),
            scope: CONSENT_SCOPE.to_owned(),
            http,
            users,
            pending: Mutex::new(HashMap::new()),
            ttl: CONSENT_STATE_TTL,
        }))
    }

    /// Override the provider endpoints. Intended for tests; HTTPS (or
    /// loopback HTTP) is enforced exactly like the production endpoints.
    pub fn set_endpoints(
        &mut self,
        authorize_url: impl Into<String>,
        token_url: impl Into<String>,
    ) -> Result<()> {
        self.authorize_url = validate_endpoint(&authorize_url.into(), "authorize_url", false)?;
        self.token_url = validate_endpoint(&token_url.into(), "token_url", false)?;
        Ok(())
    }

    /// Override the pending-consent TTL. Intended for tests; bounded to at
    /// most a day.
    pub fn set_ttl(&mut self, ttl: Duration) -> Result<()> {
        if ttl > Duration::from_secs(24 * 60 * 60) {
            return Err(SeraError::Validation(
                "consent ttl must be at most one day".into(),
            ));
        }
        self.ttl = ttl;
        Ok(())
    }

    /// Start a consent: bind a fresh state + PKCE verifier to the acting user
    /// and return the provider's authorize URL.
    pub async fn start(&self, user_id: &str) -> Result<String> {
        if self.redirect_url.is_empty() {
            return Err(SeraError::Config(
                "OneDrive consent needs a public base URL: set onedrive.redirect_base_url or auth.base_url"
                    .into(),
            ));
        }
        let (challenge, verifier) = pkce_pair();
        let mut state_bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut state_bytes);
        let state = hex::encode(state_bytes);
        let mut url = Url::parse(&self.authorize_url)
            .map_err(|_| SeraError::Config("OneDrive authorize URL is invalid".into()))?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("client_id", &self.client_id);
            query.append_pair("response_type", "code");
            query.append_pair("redirect_uri", &self.redirect_url);
            query.append_pair("response_mode", "query");
            query.append_pair("scope", &self.scope);
            query.append_pair("state", &state);
            query.append_pair("code_challenge", &challenge);
            query.append_pair("code_challenge_method", "S256");
        }

        let mut pending = self.pending.lock().await;
        let now = Instant::now();
        pending.retain(|_, consent| now.duration_since(consent.created) < self.ttl);
        if pending.len() >= MAX_PENDING_CONSENTS {
            return Err(SeraError::InvalidState(
                "too many OneDrive consents are in flight; try again shortly".into(),
            ));
        }
        pending.insert(
            state,
            PendingConsent {
                user_id: user_id.to_owned(),
                verifier,
                created: now,
            },
        );
        Ok(url.to_string())
    }

    /// Complete a consent: exchange the code with the PKCE verifier and seal
    /// the refresh token under the user's DEK. Returns the user id.
    pub async fn complete(&self, state: &str, code: &str) -> Result<String> {
        let state = state.trim();
        if state.is_empty() || state.len() > 256 {
            return Err(SeraError::InvalidState(
                "unknown or expired OneDrive consent state".into(),
            ));
        }
        let code = code.trim();
        if code.is_empty()
            || code.len() > MAX_CONSENT_CODE_CHARS
            || code.chars().any(char::is_control)
        {
            return Err(SeraError::Validation(
                "the OneDrive authorization code is invalid".into(),
            ));
        }
        let pending = {
            let mut pending = self.pending.lock().await;
            let consent = pending.remove(state).ok_or_else(|| {
                SeraError::InvalidState("unknown or expired OneDrive consent state".into())
            })?;
            if Instant::now().duration_since(consent.created) > self.ttl {
                return Err(SeraError::InvalidState(
                    "unknown or expired OneDrive consent state".into(),
                ));
            }
            consent
        };

        let response = self
            .http
            .post(&self.token_url)
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", self.redirect_url.as_str()),
                ("code_verifier", pending.verifier.as_str()),
                ("scope", self.scope.as_str()),
            ])
            .send()
            .await
            .map_err(|error| transport_error("consent token exchange", error, false))?;
        let status = response.status();
        let body = read_limited(response, MAX_RESPONSE_BYTES)
            .await
            .map_err(|_| {
                SeraError::External("OneDrive consent returned an unreadable response".into())
            })?;
        if !status.is_success() {
            let code = serde_json::from_slice::<TokenResponse>(&body)
                .ok()
                .and_then(|token| token.error)
                .filter(|code| !code.is_empty())
                .map(|code| sanitize_code(&code))
                .unwrap_or_else(|| "unknown_error".into());
            return Err(SeraError::External(format!(
                "OneDrive consent was rejected (HTTP {}): {code}",
                status.as_u16()
            )));
        }
        let token: TokenResponse = serde_json::from_slice(&body)
            .map_err(|_| SeraError::External("invalid OneDrive consent response".into()))?;
        if let Some(error) = token.error.as_deref().filter(|value| !value.is_empty()) {
            return Err(SeraError::External(format!(
                "OneDrive consent was rejected: {}",
                sanitize_code(error)
            )));
        }
        let refresh = token
            .refresh_token
            .filter(|token| !token.is_empty())
            .ok_or_else(|| {
                SeraError::External(
                    "Microsoft did not return a refresh token; consent may have been declined"
                        .into(),
                )
            })?;
        if !is_plausible_token(&refresh) {
            return Err(SeraError::External(
                "Microsoft returned an invalid refresh token".into(),
            ));
        }
        self.users
            .set(&pending.user_id, ONEDRIVE_PROVIDER, &refresh)?;
        Ok(pending.user_id)
    }
}

/// A PKCE verifier/challenge pair (S256), URL-safe and unpadded.
fn pkce_pair() -> (String, String) {
    let mut verifier_bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut verifier_bytes);
    let verifier = base64_url(&verifier_bytes);
    let challenge = base64_url(&Sha256::digest(verifier.as_bytes()));
    (challenge, verifier)
}

fn base64_url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[derive(Deserialize)]
struct ListPageResponse {
    #[serde(default)]
    value: Vec<ListDriveItem>,
    #[serde(default, rename = "@odata.nextLink")]
    next_link: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ListDriveItem {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    folder: Option<FolderFacet>,
}

#[derive(Deserialize)]
struct FolderFacet {}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadSession {
    upload_url: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct UploadProgress {
    #[serde(default)]
    next_expected_ranges: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveItem {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    web_url: Option<String>,
    #[serde(default)]
    parent_reference: Option<ParentReference>,
    #[serde(default)]
    file: Option<FileFacet>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParentReference {
    #[serde(default)]
    path: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileFacet {
    #[serde(default)]
    hashes: Option<FileHashes>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileHashes {
    #[serde(default)]
    sha256_hash: Option<String>,
}

#[derive(Deserialize)]
struct GraphErrorEnvelope {
    error: GraphErrorBody,
}

#[derive(Deserialize)]
struct GraphErrorBody {
    #[serde(default)]
    code: String,
}

/// Validate and normalize a Graph drive item into an [`UploadResult`].
///
/// Size is always verified. `file.hashes.sha256Hash` is compared when the
/// service returns it; Microsoft documents that field as unsupported, so its
/// absence is not an error. The locally computed hash is always returned.
fn finish_upload(
    item: DriveItem,
    folders: &[String],
    requested_name: &str,
    expected_size: u64,
    sha256: &str,
) -> Result<UploadResult> {
    let DriveItem {
        id,
        name,
        size,
        web_url,
        parent_reference,
        file,
    } = item;

    if id.is_empty() {
        return Err(SeraError::External(
            "OneDrive upload response omitted the item id; the artifact may exist".into(),
        ));
    }
    if size != expected_size {
        return Err(SeraError::External(format!(
            "OneDrive upload verification failed: expected {expected_size} bytes, service reported {size}"
        )));
    }
    if let Some(server_sha256) = file
        .and_then(|facet| facet.hashes)
        .and_then(|hashes| hashes.sha256_hash)
        && !server_sha256.eq_ignore_ascii_case(sha256)
    {
        return Err(SeraError::External(format!(
            "OneDrive upload verification failed: sha256 mismatch at {}",
            display_path(folders, requested_name)
        )));
    }

    let name = if name.is_empty() {
        requested_name.to_owned()
    } else {
        name
    };
    let parent_path = parent_reference.and_then(|reference| reference.path);
    let parent_path = parent_path
        .as_deref()
        .map(|path| path.strip_prefix("/drive/root:").unwrap_or(path));
    let path = match parent_path {
        Some(path) if !path.is_empty() => format!("{}/{}", path.trim_end_matches('/'), name),
        _ => display_path(folders, &name),
    };

    Ok(UploadResult {
        id,
        name,
        web_url: web_url.unwrap_or_default(),
        path,
        size,
        sha256: sha256.to_owned(),
    })
}

/// Read a success body as a drive item.
async fn read_item(response: reqwest::Response) -> Result<DriveItem> {
    let body = read_limited(response, MAX_RESPONSE_BYTES).await?;
    serde_json::from_slice(&body)
        .map_err(|error| SeraError::External(format!("invalid OneDrive response: {error}")))
}

/// Read at most `limit` bytes from a response body.
async fn read_limited(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| transport_error("response read", error, false))?
    {
        if body.len() + chunk.len() > limit {
            return Err(SeraError::External(format!(
                "OneDrive response exceeded the {limit}-byte limit"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Map a non-success Graph response to a safe, explicit error.
///
/// Only the HTTP status and the Graph error code are exposed. The service's
/// `message` field is intentionally dropped: it is not needed for diagnosis
/// and may echo request details.
async fn graph_error(response: reqwest::Response, context: &str) -> SeraError {
    let status = response.status();
    let body = read_limited(response, MAX_RESPONSE_BYTES)
        .await
        .unwrap_or_default();
    let code = serde_json::from_slice::<GraphErrorEnvelope>(&body)
        .ok()
        .map(|envelope| envelope.error.code)
        .filter(|code| !code.is_empty())
        .map(|code| sanitize_code(&code));

    match code {
        Some(code) => SeraError::External(format!(
            "OneDrive {context} failed (HTTP {}): {code}",
            status.as_u16()
        )),
        None => SeraError::External(format!(
            "OneDrive {context} failed (HTTP {})",
            status.as_u16()
        )),
    }
}

/// Map a reqwest transport failure. The URL is deliberately omitted because
/// upload session URLs are pre-authenticated and must not leak into logs.
fn transport_error(context: &str, error: reqwest::Error, mutation: bool) -> SeraError {
    let detail = if error.is_timeout() {
        "request timed out"
    } else if error.is_connect() {
        "connection failed"
    } else if error.is_body() {
        "request body failed"
    } else if error.is_decode() {
        "response could not be decoded"
    } else {
        "transport error"
    };

    if mutation {
        SeraError::External(format!(
            "OneDrive {context} outcome unknown: {detail}; the artifact may or may not have been stored"
        ))
    } else {
        SeraError::External(format!("OneDrive {context} failed: {detail}"))
    }
}

fn outcome_unknown(detail: &str) -> SeraError {
    SeraError::External(format!(
        "OneDrive upload outcome unknown: {detail}; the artifact may or may not have been stored"
    ))
}

/// Keep only short, printable error codes from the service.
fn sanitize_code(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .filter(|character| character.is_ascii_graphic())
        .take(64)
        .collect();
    if cleaned.is_empty() {
        "unknown_error".into()
    } else {
        cleaned
    }
}

fn redact_persist_error(error: SeraError, secrets: &[&str]) -> SeraError {
    let message = error.to_string();
    if secrets
        .iter()
        .any(|secret| secret.len() >= 8 && message.contains(secret))
    {
        SeraError::Secrets("OneDrive rotated refresh token was not persisted".into())
    } else {
        error
    }
}

fn is_plausible_token(token: &str) -> bool {
    token.len() <= 16 * 1024 && !token.chars().any(|c| c.is_control() || c.is_whitespace())
}

fn validate_tenant(tenant: &str) -> Result<String> {
    let tenant = tenant.trim();
    let valid = !tenant.is_empty()
        && tenant.len() <= 128
        && tenant != "."
        && tenant != ".."
        && tenant
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'));
    if !valid {
        return Err(SeraError::Config(
            "onedrive tenant must be a tenant id or domain name".into(),
        ));
    }
    Ok(tenant.to_owned())
}

fn validate_secret_like(value: &str, label: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 16 * 1024 || value.chars().any(|c| c.is_control()) {
        return Err(SeraError::Config(format!(
            "onedrive {label} is missing or invalid"
        )));
    }
    Ok(value.to_owned())
}

/// Require HTTPS, except for loopback hosts used by tests.
///
/// Pre-authenticated upload session URLs legitimately carry a query string, so
/// `allow_query` controls that check; credentials and fragments are always
/// rejected.
fn validate_endpoint(value: &str, label: &str, allow_query: bool) -> Result<String> {
    let value = value.trim();
    let url =
        Url::parse(value).map_err(|_| SeraError::Config(format!("{label} is not a valid URL")))?;

    if url.host_str().is_none_or(str::is_empty) {
        return Err(SeraError::Config(format!("{label} must include a host")));
    }
    let secure =
        url.scheme() == "https" || (url.scheme() == "http" && is_loopback_host(url.host_str()));
    if !secure {
        return Err(SeraError::Config(format!(
            "{label} must use https (http is allowed only for loopback test servers)"
        )));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(SeraError::Config(format!(
            "{label} must not contain credentials"
        )));
    }
    if (!allow_query && url.query().is_some()) || url.fragment().is_some() {
        return Err(SeraError::Config(format!(
            "{label} must not contain a query or fragment"
        )));
    }

    Ok(value.to_owned())
}

fn is_loopback_host(host: Option<&str>) -> bool {
    host.map(|host| {
        let host = host.trim_start_matches('[').trim_end_matches(']');
        host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
    })
    .unwrap_or(false)
}

/// Validate a relative folder path, returning its segments.
fn validate_segments(
    value: &str,
    max_depth: usize,
    label: &str,
) -> std::result::Result<Vec<String>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{label} must not be empty"));
    }
    if value.starts_with('/') || value.ends_with('/') {
        return Err(format!("{label} must not start or end with '/'"));
    }

    let parts: Vec<&str> = value.split('/').collect();
    if parts.len() > max_depth {
        return Err(format!(
            "{label} exceeds the maximum depth of {max_depth} segments"
        ));
    }

    for part in &parts {
        if part.is_empty() {
            return Err(format!("{label} must not contain empty path segments"));
        }
        if *part == "." || *part == ".." {
            return Err(format!("{label} must not contain '.' or '..' segments"));
        }
        if part.chars().count() > MAX_SEGMENT_CHARS {
            return Err(format!(
                "{label} segments must be at most {MAX_SEGMENT_CHARS} characters"
            ));
        }
        if part.trim() != *part {
            return Err(format!(
                "{label} segments must not have leading or trailing whitespace"
            ));
        }
        if part
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | ':' | '*' | '<' | '>' | '?' | '"' | '|'))
        {
            return Err(format!("{label} contains an unsupported character"));
        }
    }

    Ok(parts.into_iter().map(str::to_owned).collect())
}

/// Turn arbitrary caller text into a single safe OneDrive file name.
///
/// Directory separators are stripped (only the last component is kept),
/// control characters are removed, and characters OneDrive rejects are
/// replaced with `_`. The result is never empty, `.`, or `..`.
fn safe_filename(raw: &str) -> Result<String> {
    let last_component = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let mut cleaned = String::with_capacity(last_component.len());
    for character in last_component.chars() {
        if character.is_control() {
            continue;
        }
        if matches!(character, '*' | ':' | '<' | '>' | '?' | '"' | '|') {
            cleaned.push('_');
        } else {
            cleaned.push(character);
        }
    }

    let cleaned = cleaned
        .trim_start()
        .trim_end_matches(|character: char| character.is_whitespace() || character == '.');
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return Err(SeraError::Validation(
            "filename is empty or consists only of dots after sanitization".into(),
        ));
    }
    if cleaned.chars().count() > MAX_SEGMENT_CHARS {
        return Err(SeraError::Validation(format!(
            "filename must be at most {MAX_SEGMENT_CHARS} characters"
        )));
    }

    Ok(cleaned.to_owned())
}

fn validate_mime(mime: &str) -> Result<&str> {
    let mime = mime.trim();
    let valid = !mime.is_empty()
        && mime.len() <= 255
        && mime.is_ascii()
        && mime.contains('/')
        && !mime.bytes().any(|byte| byte < 0x20 || byte == 0x7f);
    if !valid {
        return Err(SeraError::Validation(
            "mime type is empty or invalid".into(),
        ));
    }
    Ok(mime)
}

/// Percent-encode one path segment. Unreserved characters are kept; every
/// other byte (including spaces, `#`, `%`, and non-ASCII UTF-8) is escaped.
fn encode_segment(segment: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(segment.len());
    for &byte in segment.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            _ => {
                encoded.push('%');
                encoded.push(HEX[(byte >> 4) as usize] as char);
                encoded.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
    encoded
}

fn encode_path(segments: &[String]) -> String {
    segments
        .iter()
        .map(|segment| encode_segment(segment))
        .collect::<Vec<_>>()
        .join("/")
}

fn encode_file_path(segments: &[String], filename: &str) -> String {
    format!("{}/{}", encode_path(segments), encode_segment(filename))
}

/// Human-readable destination path for logs and results (not URL-encoded).
fn display_path(folders: &[String], filename: &str) -> String {
    format!("/{}/{}", folders.join("/"), filename)
}

fn display_folder_path(folders: &[String]) -> String {
    format!("/{}", folders.join("/"))
}

/// Graph pagination links must stay on the configured Graph host.
fn validate_pagination_url(url: &str, graph_base: &str) -> Result<String> {
    let url = validate_endpoint(url, "pagination", true)?;
    let parsed = Url::parse(&url)
        .map_err(|_| SeraError::External("invalid OneDrive pagination URL".into()))?;
    let base = Url::parse(graph_base)
        .map_err(|_| SeraError::Config("graph_base is not a valid URL".into()))?;
    if parsed.host() != base.host() {
        return Err(SeraError::External(
            "OneDrive pagination URL host mismatch".into(),
        ));
    }
    Ok(url)
}

/// Parse the start of a Graph `nextExpectedRanges` entry such as `"5242880-"`.
fn range_start(value: &str) -> Option<u64> {
    value
        .split_once('-')
        .and_then(|(start, _)| start.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::secrets::{SecretStore, SharedSecrets, ensure_identity};
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

    const FOLDER_PATH: &str = "/v1.0/me/drive/root:/SeraBot";
    const CONTENT_PATH: &str = "/v1.0/me/drive/root:/SeraBot/report.pdf:/content";

    fn make_client(
        graph_base: &str,
        base_folder: &str,
        conflict_behavior: &str,
    ) -> Result<OneDriveClient> {
        OneDriveClient::new(
            reqwest::Client::new(),
            graph_base.to_owned(),
            "contoso.example".to_owned(),
            "client-1".to_owned(),
            "refresh-1".to_owned(),
            base_folder.to_owned(),
            conflict_behavior.to_owned(),
        )
    }

    fn test_client(server: &MockServer) -> OneDriveClient {
        let mut client =
            make_client(&format!("{}/v1.0", server.uri()), "SeraBot", "rename").unwrap();
        client
            .set_token_url(format!("{}/token", server.uri()))
            .unwrap();
        client
    }

    fn test_client_with_http(server: &MockServer, http: reqwest::Client) -> OneDriveClient {
        let mut client = OneDriveClient::new(
            http,
            format!("{}/v1.0", server.uri()),
            "contoso.example".into(),
            "client-1".into(),
            "refresh-1".into(),
            "SeraBot".into(),
            "rename".into(),
        )
        .unwrap();
        client
            .set_token_url(format!("{}/token", server.uri()))
            .unwrap();
        client
    }

    fn age_store(
        pairs: &[(&str, &str)],
    ) -> (
        tempfile::TempDir,
        std::path::PathBuf,
        std::path::PathBuf,
        SharedSecrets,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let identity = dir.path().join("identity.txt");
        let path = dir.path().join("secrets.age");
        ensure_identity(&identity).unwrap();
        let mut store = SecretStore::open_paths(&path, &identity).unwrap();
        for (name, value) in pairs {
            store.set(name, value).unwrap();
        }
        store.save().unwrap();
        let secrets = SharedSecrets::new(SecretStore::open_paths(&path, &identity).unwrap());
        (dir, identity, path, secrets)
    }

    fn client_with_refresh(
        server: &MockServer,
        refresh: &str,
        secrets: SharedSecrets,
        access_secret_name: Option<&str>,
    ) -> OneDriveClient {
        let mut client = OneDriveClient::new(
            reqwest::Client::new(),
            format!("{}/v1.0", server.uri()),
            "contoso.example".into(),
            "client-id-value".into(),
            refresh.into(),
            "SeraBot".into(),
            "rename".into(),
        )
        .unwrap();
        client
            .set_token_url(format!("{}/token", server.uri()))
            .unwrap();
        client
            .with_refresh_persistence(secrets, "onedrive_refresh_token", access_secret_name)
            .unwrap()
    }

    async fn mount_token(
        server: &MockServer,
        access: &str,
        refresh: Option<&str>,
        expires_in: u64,
    ) {
        let mut body = json!({
            "access_token": access,
            "expires_in": expires_in,
            "token_type": "Bearer",
        });
        if let Some(refresh) = refresh {
            body["refresh_token"] = json!(refresh);
        }
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(BodyContains("grant_type=refresh_token"))
            .and(BodyContains("client_id=client-1"))
            .and(BodyContains("refresh_token=refresh-1"))
            .and(BodyContains("scope=offline_access"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(server)
            .await;
    }

    async fn mount_folder_exists(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path(FOLDER_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "folder-1",
                "name": "SeraBot",
                "folder": {},
            })))
            .mount(server)
            .await;
    }

    fn drive_item(name: &str, size: usize, sha256: Option<&str>) -> serde_json::Value {
        let mut item = json!({
            "id": "item-1",
            "name": name,
            "size": size,
            "webUrl": "https://contoso.example/drive/report.pdf",
            "parentReference": {"path": "/drive/root:/SeraBot"},
        });
        if let Some(sha256) = sha256 {
            item["file"] = json!({"hashes": {"sha256Hash": sha256}});
        }
        item
    }

    #[tokio::test]
    async fn list_folder_returns_items_under_base_path() {
        let server = MockServer::start().await;
        mount_token(&server, "access-1", None, 3600).await;

        Mock::given(method("GET"))
            .and(path("/v1.0/me/drive/root:/SeraBot/Deliverables:/children"))
            .and(query_param("$top", "200"))
            .and(header("authorization", "Bearer access-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "value": [
                    {
                        "id": "item-1",
                        "name": "report.pdf",
                        "size": 42,
                        "file": {},
                    },
                    {
                        "id": "folder-2",
                        "name": "archive",
                        "folder": {},
                    }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = test_client(&server);
        let result = client.list_folder(Some("Deliverables"), 5).await.unwrap();

        assert_eq!(result.items.len(), 2);
        assert!(!result.truncated);
        assert_eq!(result.items[0].name, "report.pdf");
        assert_eq!(result.items[0].path, "/SeraBot/Deliverables/report.pdf");
        assert!(!result.items[0].folder);
        assert!(result.items[1].folder);
        server.verify().await;
    }

    #[tokio::test]
    async fn small_upload_sends_expected_request() {
        let server = MockServer::start().await;
        mount_token(&server, "access-1", Some("refresh-2"), 3600).await;
        mount_folder_exists(&server).await;

        let bytes = b"%PDF-1.7 fake artifact".to_vec();
        let sha256 = sha256_hex(&bytes);
        Mock::given(method("PUT"))
            .and(path(CONTENT_PATH))
            .and(query_param("@microsoft.graph.conflictBehavior", "rename"))
            .and(header("authorization", "Bearer access-1"))
            .and(header("content-type", "application/pdf"))
            .and(BodyIs(bytes.clone()))
            .respond_with(ResponseTemplate::new(201).set_body_json(drive_item(
                "report.pdf",
                bytes.len(),
                Some(sha256.as_str()),
            )))
            .expect(1)
            .mount(&server)
            .await;

        let (_dir, _identity, _path, secrets) = age_store(&[
            ("onedrive_refresh_token", "refresh-1"),
            ("other_secret", "other-value"),
        ]);
        let mut client = test_client(&server)
            .with_refresh_persistence(secrets.clone(), "onedrive_refresh_token", None)
            .unwrap();
        let result = client
            .upload("report.pdf", bytes.clone(), "application/pdf", None)
            .await
            .unwrap();

        assert_eq!(result.id, "item-1");
        assert_eq!(result.name, "report.pdf");
        assert_eq!(result.web_url, "https://contoso.example/drive/report.pdf");
        assert_eq!(result.path, "/SeraBot/report.pdf");
        assert_eq!(result.size, bytes.len() as u64);
        assert_eq!(result.sha256, sha256);
        assert_eq!(
            secrets.get("onedrive_refresh_token").as_deref(),
            Some("refresh-2")
        );
        assert_eq!(secrets.get("other_secret").as_deref(), Some("other-value"));
        server.verify().await;
    }

    #[tokio::test]
    async fn large_upload_uses_upload_session_and_content_ranges() {
        let server = MockServer::start().await;
        mount_token(&server, "access-1", None, 3600).await;
        mount_folder_exists(&server).await;

        let upload_url = format!("{}/upload/session-1", server.uri());
        Mock::given(method("POST"))
            .and(path(
                "/v1.0/me/drive/root:/SeraBot/big.bin:/createUploadSession",
            ))
            .and(header("authorization", "Bearer access-1"))
            .and(body_json(json!({
                "item": {
                    "@microsoft.graph.conflictBehavior": "rename",
                    "name": "big.bin",
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "uploadUrl": upload_url,
                "expirationDateTime": "2026-10-28T12:00:00Z",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let bytes = vec![0xAB_u8; UPLOAD_CHUNK_SIZE + 1024 * 1024 + 17];
        let sha256 = sha256_hex(&bytes);
        let total = bytes.len();
        let first_len = UPLOAD_CHUNK_SIZE.min(total);
        let first_range = format!("bytes 0-{}/{}", first_len - 1, total);
        let final_range = format!("bytes {}-{}/{}", first_len, total - 1, total);

        Mock::given(method("PUT"))
            .and(path("/upload/session-1"))
            .and(header("content-range", first_range.as_str()))
            .and(NoAuthorization)
            .and(BodyLen(first_len))
            .respond_with(ResponseTemplate::new(202).set_body_json(json!({
                "nextExpectedRanges": [format!("{}-", first_len)],
            })))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("PUT"))
            .and(path("/upload/session-1"))
            .and(header("content-range", final_range.as_str()))
            .and(NoAuthorization)
            .and(BodyLen(total - first_len))
            .respond_with(ResponseTemplate::new(201).set_body_json(drive_item(
                "big.bin",
                total,
                Some(sha256.as_str()),
            )))
            .expect(1)
            .mount(&server)
            .await;

        let mut client = test_client(&server);
        let result = client
            .upload("big.bin", bytes.clone(), "application/octet-stream", None)
            .await
            .unwrap();

        assert_eq!(result.name, "big.bin");
        assert_eq!(result.size, total as u64);
        assert_eq!(result.sha256, sha256);
        assert_eq!(result.path, "/SeraBot/big.bin");
        server.verify().await;
    }

    #[tokio::test]
    async fn creates_missing_destination_folder() {
        let server = MockServer::start().await;
        mount_token(&server, "access-1", None, 3600).await;

        Mock::given(method("GET"))
            .and(path(FOLDER_PATH))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "error": {"code": "itemNotFound", "message": "not found"},
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1.0/me/drive/root/children"))
            .and(body_json(json!({
                "name": "SeraBot",
                "folder": {},
                "@microsoft.graph.conflictBehavior": "fail",
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id": "folder-1",
                "name": "SeraBot",
                "folder": {},
            })))
            .expect(1)
            .mount(&server)
            .await;

        let bytes = b"artifact".to_vec();
        let sha256 = sha256_hex(&bytes);
        Mock::given(method("PUT"))
            .and(path(CONTENT_PATH))
            .respond_with(ResponseTemplate::new(201).set_body_json(drive_item(
                "report.pdf",
                bytes.len(),
                Some(sha256.as_str()),
            )))
            .expect(1)
            .mount(&server)
            .await;

        let mut client = test_client(&server);
        let result = client
            .upload("report.pdf", bytes.clone(), "application/pdf", None)
            .await
            .unwrap();
        assert_eq!(result.path, "/SeraBot/report.pdf");
        server.verify().await;
    }

    #[tokio::test]
    async fn percent_encodes_each_path_segment() {
        let server = MockServer::start().await;
        mount_token(&server, "access-1", None, 3600).await;

        Mock::given(method("GET"))
            .and(path("/v1.0/me/drive/root:/SeraBot/sub%20folder"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "folder-2",
                "name": "sub folder",
                "folder": {},
            })))
            .expect(1)
            .mount(&server)
            .await;

        let name = "report 2026 #1.pdf";
        let mut item = drive_item(name, 3, None);
        item["parentReference"] = json!({"path": "/drive/root:/SeraBot/sub folder"});

        Mock::given(method("PUT"))
            .and(path(
                "/v1.0/me/drive/root:/SeraBot/sub%20folder/report%202026%20%231.pdf:/content",
            ))
            .and(query_param("@microsoft.graph.conflictBehavior", "rename"))
            .respond_with(ResponseTemplate::new(201).set_body_json(item))
            .expect(1)
            .mount(&server)
            .await;

        let mut client = test_client(&server);
        let result = client
            .upload(name, b"abc".to_vec(), "application/pdf", Some("sub folder"))
            .await
            .unwrap();

        assert_eq!(result.name, name);
        assert_eq!(result.path, "/SeraBot/sub folder/report 2026 #1.pdf");
        server.verify().await;
    }

    #[test]
    fn sanitizes_filenames() {
        assert_eq!(safe_filename("../report.pdf").unwrap(), "report.pdf");
        assert_eq!(
            safe_filename("C:\\Users\\owner\\report.pdf").unwrap(),
            "report.pdf"
        );
        assert_eq!(safe_filename("bad\u{0}name.pdf").unwrap(), "badname.pdf");
        assert_eq!(
            safe_filename("report:final?.pdf").unwrap(),
            "report_final_.pdf"
        );
        assert_eq!(safe_filename("  spaced  ").unwrap(), "spaced");
        assert!(safe_filename("").is_err());
        assert!(safe_filename("..").is_err());
        assert!(safe_filename("folder/").is_err());
        assert!(safe_filename(&"a".repeat(MAX_SEGMENT_CHARS + 1)).is_err());
    }

    #[test]
    fn rejects_unsafe_folder_paths() {
        let graph = "https://graph.microsoft.com/v1.0";
        for folder in [
            "../SeraBot",
            "/SeraBot",
            "SeraBot/",
            "SeraBot/../Other",
            "SeraBot//Sub",
            "SeraBot\\Sub",
            "SeraBot:Sub",
            "..",
            ".",
            "",
        ] {
            assert!(
                make_client(graph, folder, "rename").is_err(),
                "folder {folder:?} should be rejected"
            );
        }

        assert!(make_client(graph, "SeraBot/Deliverables", "rename").is_ok());
        let too_deep = (0..MAX_FOLDER_DEPTH + 1)
            .map(|index| format!("level{index}"))
            .collect::<Vec<_>>()
            .join("/");
        assert!(make_client(graph, &too_deep, "rename").is_err());
    }

    #[test]
    fn rejects_unknown_conflict_behavior() {
        let graph = "https://graph.microsoft.com/v1.0";
        for behavior in ["replace", "overwrite", "RENAME", ""] {
            assert!(
                make_client(graph, "SeraBot", behavior).is_err(),
                "conflict behavior {behavior:?} should be rejected"
            );
        }
        assert!(make_client(graph, "SeraBot", "rename").is_ok());
        assert!(make_client(graph, "SeraBot", "fail").is_ok());
    }

    #[test]
    fn rejects_non_https_endpoints() {
        assert!(make_client("http://graph.microsoft.com/v1.0", "SeraBot", "rename").is_err());
        assert!(make_client("ftp://graph.microsoft.com/v1.0", "SeraBot", "rename").is_err());
        assert!(make_client("http://localhost:8080/v1.0", "SeraBot", "rename").is_ok());
        assert!(make_client("http://127.0.0.1:8080/v1.0", "SeraBot", "rename").is_ok());

        let mut client =
            make_client("https://graph.microsoft.com/v1.0", "SeraBot", "rename").unwrap();
        assert!(
            client
                .set_token_url("http://login.microsoftonline.com/token")
                .is_err()
        );
        assert!(client.set_token_url("http://127.0.0.1:9/token").is_ok());

        // Upload session URLs are pre-authenticated and may carry a query.
        assert!(validate_endpoint("https://host.example/up/x?guid=1", "uploadUrl", false).is_err());
        assert!(validate_endpoint("https://host.example/up/x?guid=1", "uploadUrl", true).is_ok());
        assert!(validate_endpoint("https://host.example/up/x#frag", "uploadUrl", true).is_err());
    }

    #[tokio::test]
    async fn rejects_upload_path_traversal_before_network() {
        // Nothing listens on port 9; validation must fail before any request.
        let mut client = make_client("http://127.0.0.1:9/v1.0", "SeraBot", "rename").unwrap();

        for subfolder in [
            "../Outside",
            "Sub/../../Outside",
            "/Outside",
            "Sub/",
            "Sub//Nested",
            "Sub\\Nested",
        ] {
            let error = client
                .upload(
                    "report.pdf",
                    b"x".to_vec(),
                    "application/pdf",
                    Some(subfolder),
                )
                .await
                .unwrap_err();
            assert!(
                matches!(&error, SeraError::Validation(_)),
                "subfolder {subfolder:?} should be rejected, got {error}"
            );
        }

        let error = client
            .upload("..", b"x".to_vec(), "application/pdf", None)
            .await
            .unwrap_err();
        assert!(matches!(error, SeraError::Validation(_)));
    }

    #[tokio::test]
    async fn maps_refresh_failure_without_leaking_details() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": "invalid_grant",
                "error_description": "AADSTS70008: refresh token expired; trace super-secret-trace",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let mut client = test_client(&server);
        let error = client
            .upload("report.pdf", b"payload".to_vec(), "application/pdf", None)
            .await
            .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("invalid_grant"));
        assert!(!message.contains("AADSTS"));
        assert!(!message.contains("super-secret-trace"));
        assert!(!message.contains("refresh-1"));
        assert!(matches!(error, SeraError::External(_)));
        server.verify().await;
    }

    #[tokio::test]
    async fn marks_ambiguous_upload_failure_as_outcome_unknown() {
        let server = MockServer::start().await;
        mount_token(&server, "access-1", None, 3600).await;
        mount_folder_exists(&server).await;

        Mock::given(method("PUT"))
            .and(path(CONTENT_PATH))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_delay(Duration::from_secs(3))
                    .set_body_json(drive_item("report.pdf", 7, None)),
            )
            .mount(&server)
            .await;

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();
        let mut client = test_client_with_http(&server, http);
        let error = client
            .upload("report.pdf", b"payload".to_vec(), "application/pdf", None)
            .await
            .unwrap_err();

        let message = error.to_string();
        assert!(
            message.contains("outcome unknown"),
            "unexpected message: {message}"
        );
        assert!(!message.contains(server.uri().as_str()));
        assert!(matches!(error, SeraError::External(_)));
    }

    #[tokio::test]
    async fn reuses_access_token_until_expiry() {
        let server = MockServer::start().await;
        // `.expect(1)` on the token mock proves the cached token was reused.
        mount_token(&server, "access-1", Some("refresh-2"), 3600).await;
        mount_folder_exists(&server).await;

        Mock::given(method("PUT"))
            .and(path(CONTENT_PATH))
            .and(header("authorization", "Bearer access-1"))
            .respond_with(ResponseTemplate::new(201).set_body_json(drive_item(
                "report.pdf",
                7,
                None,
            )))
            .expect(2)
            .mount(&server)
            .await;

        let (_dir, _identity, _path, secrets) =
            age_store(&[("onedrive_refresh_token", "refresh-1")]);
        let mut client = test_client(&server)
            .with_refresh_persistence(secrets.clone(), "onedrive_refresh_token", None)
            .unwrap();
        client
            .upload("report.pdf", b"payload".to_vec(), "application/pdf", None)
            .await
            .unwrap();
        client
            .upload("report.pdf", b"payload".to_vec(), "application/pdf", None)
            .await
            .unwrap();
        assert_eq!(
            secrets.get("onedrive_refresh_token").as_deref(),
            Some("refresh-2")
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn refreshes_early_and_keeps_rotated_refresh_token() {
        let server = MockServer::start().await;

        // expires_in below the 60 s skew makes the token stale immediately, so
        // the folder lookup and the content upload each refresh once.
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(BodyContains("refresh_token=refresh-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-1",
                "refresh_token": "refresh-2",
                "expires_in": 30,
            })))
            .expect(1)
            .mount(&server)
            .await;
        // The second refresh must present the rotated token.
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(BodyContains("refresh_token=refresh-2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-2",
                "refresh_token": "refresh-3",
                "expires_in": 30,
            })))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path(FOLDER_PATH))
            .and(header("authorization", "Bearer access-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "folder-1",
                "name": "SeraBot",
                "folder": {},
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path(CONTENT_PATH))
            .and(header("authorization", "Bearer access-2"))
            .respond_with(ResponseTemplate::new(201).set_body_json(drive_item(
                "report.pdf",
                7,
                None,
            )))
            .expect(1)
            .mount(&server)
            .await;

        let (_dir, _identity, path, secrets) = age_store(&[
            ("onedrive_refresh_token", "refresh-1"),
            ("other_secret", "other-value"),
        ]);
        let mut client = test_client(&server)
            .with_refresh_persistence(secrets.clone(), "onedrive_refresh_token", None)
            .unwrap();
        client
            .upload("report.pdf", b"payload".to_vec(), "application/pdf", None)
            .await
            .unwrap();
        assert_eq!(
            secrets.get("onedrive_refresh_token").as_deref(),
            Some("refresh-3")
        );
        assert_eq!(secrets.get("other_secret").as_deref(), Some("other-value"));
        let raw = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
        for secret in ["refresh-1", "refresh-2", "refresh-3", "other-value"] {
            assert!(!raw.contains(secret), "{secret}");
        }
        server.verify().await;
    }

    #[tokio::test]
    async fn keeps_refresh_token_when_response_omits_it() {
        let server = MockServer::start().await;

        // Both refreshes present the original token; the service never rotates
        // it, so the client must retain it.
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(BodyContains("refresh_token=refresh-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-1",
                "expires_in": 30,
            })))
            .expect(2)
            .mount(&server)
            .await;
        mount_folder_exists(&server).await;
        Mock::given(method("PUT"))
            .and(path(CONTENT_PATH))
            .and(header("authorization", "Bearer access-1"))
            .respond_with(ResponseTemplate::new(201).set_body_json(drive_item(
                "report.pdf",
                7,
                None,
            )))
            .expect(1)
            .mount(&server)
            .await;

        let secrets = SharedSecrets::new(SecretStore::from_map(BTreeMap::from([(
            "onedrive_refresh_token".into(),
            "refresh-1".into(),
        )])));
        let mut client = test_client(&server)
            .with_refresh_persistence(secrets.clone(), "onedrive_refresh_token", None)
            .unwrap();
        client
            .upload("report.pdf", b"payload".to_vec(), "application/pdf", None)
            .await
            .unwrap();

        let state = client.tokens.lock().await;
        assert_eq!(state.refresh_token, "refresh-1");
        assert_eq!(state.access_token.as_deref(), Some("access-1"));
        assert!(!state.refresh_unpersisted);
        drop(state);
        assert_eq!(
            secrets.get("onedrive_refresh_token").as_deref(),
            Some("refresh-1")
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn debug_output_redacts_credentials() {
        let server = MockServer::start().await;
        mount_token(&server, "access-secret", None, 3600).await;
        mount_folder_exists(&server).await;
        Mock::given(method("PUT"))
            .and(path(CONTENT_PATH))
            .respond_with(ResponseTemplate::new(201).set_body_json(drive_item(
                "report.pdf",
                7,
                None,
            )))
            .expect(1)
            .mount(&server)
            .await;

        let mut client = test_client(&server);
        client
            .upload("report.pdf", b"payload".to_vec(), "application/pdf", None)
            .await
            .unwrap();

        let debug = format!("{client:?}");
        assert!(!debug.contains("refresh-1"));
        assert!(!debug.contains("access-secret"));
        assert!(!debug.contains("client-1"));
        assert!(debug.contains("SeraBot"));
        server.verify().await;
    }

    #[tokio::test]
    async fn rotated_refresh_and_access_tokens_are_written_to_the_age_store() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(BodyContains("refresh_token=refresh-old-value"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-new-value",
                "refresh_token": "refresh-new-value",
                "expires_in": 3600,
                "token_type": "Bearer",
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (_dir, _identity, path, secrets) = age_store(&[
            ("onedrive_refresh_token", "refresh-old-value"),
            ("onedrive_access_token", "access-old-value"),
            ("other_secret", "other-value"),
        ]);
        let client = client_with_refresh(
            &server,
            "refresh-old-value",
            secrets.clone(),
            Some("onedrive_access_token"),
        );
        assert_eq!(client.access_token().await.unwrap(), "access-new-value");
        assert_eq!(client.access_token().await.unwrap(), "access-new-value");
        assert_eq!(
            secrets.get("onedrive_refresh_token").as_deref(),
            Some("refresh-new-value")
        );
        assert_eq!(
            secrets.get("onedrive_access_token").as_deref(),
            Some("access-new-value")
        );
        assert_eq!(secrets.get("other_secret").as_deref(), Some("other-value"));
        let raw = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
        for secret in [
            "refresh-new-value",
            "refresh-old-value",
            "access-new-value",
            "access-old-value",
            "other-value",
            "client-id-value",
        ] {
            assert!(!raw.contains(secret), "{secret}");
        }
        let rendered = format!("{client:?}");
        assert!(!rendered.contains("refresh-new-value"));
        assert!(!rendered.contains("access-new-value"));
        assert!(!rendered.contains("client-id-value"));
        server.verify().await;
    }

    #[tokio::test]
    async fn refresh_only_persistence_does_not_invent_an_access_secret() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(BodyContains("refresh_token=refresh-old-value"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-new-value",
                "refresh_token": "refresh-new-value",
                "expires_in": 3600,
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (_dir, _identity, path, secrets) = age_store(&[
            ("onedrive_refresh_token", "refresh-old-value"),
            ("other_secret", "other-value"),
        ]);
        let client = client_with_refresh(&server, "refresh-old-value", secrets.clone(), None);
        assert_eq!(client.access_token().await.unwrap(), "access-new-value");
        assert_eq!(
            secrets.get("onedrive_refresh_token").as_deref(),
            Some("refresh-new-value")
        );
        assert_eq!(secrets.get("onedrive_access_token"), None);
        assert_eq!(secrets.get("other_secret").as_deref(), Some("other-value"));
        let raw = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
        assert!(!raw.contains("refresh-new-value"));
        assert!(!raw.contains("access-new-value"));
        assert!(!raw.contains("other-value"));
        server.verify().await;
    }

    #[tokio::test]
    async fn store_write_failure_fails_closed_and_keeps_the_rotated_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(BodyContains("refresh_token=refresh-old-value"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-new-value",
                "refresh_token": "refresh-new-value",
                "expires_in": 3600,
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (_dir, identity, path, secrets) = age_store(&[
            ("onedrive_refresh_token", "refresh-old-value"),
            ("onedrive_access_token", "access-old-value"),
            ("other_secret", "other-value"),
        ]);
        let backup = std::fs::read(&identity).unwrap();
        let before = std::fs::read(&path).unwrap();
        std::fs::write(&identity, "not-an-identity").unwrap();
        let client = client_with_refresh(
            &server,
            "refresh-old-value",
            secrets.clone(),
            Some("onedrive_access_token"),
        );
        let error = client.access_token().await.unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("identity") || message.contains("not persisted"),
            "{message}"
        );
        for secret in [
            "refresh-old-value",
            "refresh-new-value",
            "access-old-value",
            "access-new-value",
            "client-id-value",
            "other-value",
        ] {
            assert!(!message.contains(secret), "{message}");
        }
        assert_eq!(std::fs::read(&path).unwrap(), before);
        {
            let state = client.tokens.lock().await;
            assert_eq!(state.refresh_token, "refresh-new-value");
            assert_eq!(state.access_token.as_deref(), Some("access-new-value"));
            assert!(state.refresh_unpersisted);
        }
        assert_eq!(
            secrets.get("onedrive_refresh_token").as_deref(),
            Some("refresh-old-value")
        );
        assert_eq!(
            secrets.get("onedrive_access_token").as_deref(),
            Some("access-old-value")
        );

        std::fs::write(&identity, backup).unwrap();
        assert_eq!(client.access_token().await.unwrap(), "access-new-value");
        assert_eq!(
            secrets.get("onedrive_refresh_token").as_deref(),
            Some("refresh-new-value")
        );
        assert_eq!(
            secrets.get("onedrive_access_token").as_deref(),
            Some("access-new-value")
        );
        assert_eq!(secrets.get("other_secret").as_deref(), Some("other-value"));
        let raw = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
        assert!(!raw.contains("refresh-new-value"));
        assert!(!raw.contains("access-new-value"));
        assert!(!raw.contains("other-value"));
        {
            let state = client.tokens.lock().await;
            assert!(!state.refresh_unpersisted);
        }
        server.verify().await;
    }

    #[tokio::test]
    async fn rotation_without_a_secret_store_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(BodyContains("refresh_token=refresh-old-value"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-new-value",
                "refresh_token": "refresh-new-value",
                "expires_in": 3600,
            })))
            .expect(1)
            .mount(&server)
            .await;
        let mut client = OneDriveClient::new(
            reqwest::Client::new(),
            format!("{}/v1.0", server.uri()),
            "contoso.example".into(),
            "client-id-value".into(),
            "refresh-old-value".into(),
            "SeraBot".into(),
            "rename".into(),
        )
        .unwrap();
        client
            .set_token_url(format!("{}/token", server.uri()))
            .unwrap();
        let error = client.access_token().await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("not persisted"), "{message}");
        for secret in [
            "refresh-old-value",
            "refresh-new-value",
            "access-new-value",
            "client-id-value",
        ] {
            assert!(!message.contains(secret), "{message}");
        }
        {
            let state = client.tokens.lock().await;
            assert_eq!(state.refresh_token, "refresh-new-value");
            assert!(state.refresh_unpersisted);
        }
        let again = client.access_token().await.unwrap_err();
        assert!(!again.to_string().contains("refresh-new-value"));
        assert!(!again.to_string().contains("access-new-value"));
        server.verify().await;
    }

    #[tokio::test]
    async fn repeated_refresh_token_does_not_rewrite_the_store() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(BodyContains("refresh_token=refresh-old-value"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-new-value",
                "refresh_token": "refresh-old-value",
                "expires_in": 3600,
            })))
            .expect(1)
            .mount(&server)
            .await;
        let secrets = SharedSecrets::new(SecretStore::from_map(BTreeMap::from([
            ("onedrive_refresh_token".into(), "refresh-old-value".into()),
            ("onedrive_access_token".into(), "access-old-value".into()),
        ])));
        let client = client_with_refresh(
            &server,
            "refresh-old-value",
            secrets.clone(),
            Some("onedrive_access_token"),
        );
        assert_eq!(client.access_token().await.unwrap(), "access-new-value");
        assert_eq!(
            secrets.get("onedrive_refresh_token").as_deref(),
            Some("refresh-old-value")
        );
        assert_eq!(
            secrets.get("onedrive_access_token").as_deref(),
            Some("access-old-value")
        );
        server.verify().await;
    }

    struct BodyIs(Vec<u8>);

    impl Match for BodyIs {
        fn matches(&self, request: &Request) -> bool {
            request.body == self.0
        }
    }

    struct BodyContains(&'static str);

    impl Match for BodyContains {
        fn matches(&self, request: &Request) -> bool {
            String::from_utf8_lossy(&request.body).contains(self.0)
        }
    }

    struct BodyLen(usize);

    impl Match for BodyLen {
        fn matches(&self, request: &Request) -> bool {
            request.body.len() == self.0
        }
    }

    struct NoAuthorization;

    impl Match for NoAuthorization {
        fn matches(&self, request: &Request) -> bool {
            request.headers.get("authorization").is_none()
        }
    }

    // ---- Per-user drives (multi-user phase 4) ----

    fn per_user_store() -> (Store, Arc<UserSecrets>, String, String) {
        let store = Store::open_in_memory(384).unwrap();
        let users = Arc::new(UserSecrets::with_master_key(store.clone(), [5u8; 32]).unwrap());
        let alice = store
            .create_user("microsoft:alice", "alice@example.example", "Alice")
            .unwrap()
            .id;
        let bob = store
            .create_user("microsoft:bob", "bob@example.example", "Bob")
            .unwrap()
            .id;
        (store, users, alice, bob)
    }

    fn registry_config(server: &MockServer) -> Config {
        let mut config = Config::default();
        config.onedrive.enabled = true;
        config.onedrive.graph_base = format!("{}/v1.0", server.uri());
        config.onedrive.folder = "SeraBot".into();
        config
    }

    fn registry_secrets() -> SharedSecrets {
        SharedSecrets::new(SecretStore::from_map(BTreeMap::from([(
            "onedrive_client_id".into(),
            "client-1".into(),
        )])))
    }

    async fn mount_folder_exists_once(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path(FOLDER_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "folder"})))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn per_user_clients_use_their_own_refresh_token() {
        let server = MockServer::start().await;
        let (_store, users, alice, bob) = per_user_store();
        users
            .set(&alice, ONEDRIVE_PROVIDER, "refresh-alice")
            .unwrap();
        users.set(&bob, ONEDRIVE_PROVIDER, "refresh-bob").unwrap();
        for (refresh, access) in [
            ("refresh-alice", "access-alice"),
            ("refresh-bob", "access-bob"),
        ] {
            Mock::given(method("POST"))
                .and(path("/token"))
                .and(BodyContains(refresh))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "access_token": access,
                    "expires_in": 3600,
                })))
                .expect(1)
                .mount(&server)
                .await;
        }
        mount_folder_exists_once(&server).await;
        for access in ["access-alice", "access-bob"] {
            Mock::given(method("PUT"))
                .and(path(CONTENT_PATH))
                .and(header("authorization", format!("Bearer {access}")))
                .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                    "id": format!("item-{access}"),
                    "name": "report.pdf",
                    "size": 3,
                    "webUrl": "https://example.example/report.pdf",
                })))
                .expect(1)
                .mount(&server)
                .await;
        }

        let registry = Arc::new(
            OneDriveRegistry::new(
                &registry_config(&server),
                &registry_secrets(),
                users.clone(),
            )
            .unwrap()
            .with_token_url(format!("{}/token", server.uri()))
            .unwrap(),
        );
        for user in [&alice, &bob] {
            let client = registry.client_for(user).unwrap();
            let result = client
                .lock()
                .await
                .upload("report.pdf", b"abc".to_vec(), "application/pdf", None)
                .await
                .unwrap();
            assert!(result.id.starts_with("item-access-"));
        }
        server.verify().await;
        // The cache returns the same client for the same user.
        assert!(Arc::ptr_eq(
            &registry.client_for(&alice).unwrap(),
            &registry.client_for(&alice).unwrap()
        ));
    }

    #[tokio::test]
    async fn a_user_without_a_connected_drive_fails_closed() {
        let server = MockServer::start().await;
        let (_store, users, alice, _bob) = per_user_store();
        let registry = OneDriveRegistry::new(
            &registry_config(&server),
            &registry_secrets(),
            users.clone(),
        )
        .unwrap();
        let error = registry.client_for(&alice).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("connect it from the console first")
        );
        assert!(!registry.connected(&alice).unwrap());
        // The error never names another account or a token.
        assert!(!error.to_string().contains("refresh"));
    }

    #[tokio::test]
    async fn a_rotated_refresh_token_is_written_back_to_the_user_store() {
        let server = MockServer::start().await;
        let (_store, users, alice, _bob) = per_user_store();
        users.set(&alice, ONEDRIVE_PROVIDER, "refresh-old").unwrap();
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(BodyContains("refresh_token=refresh-old"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-new",
                "refresh_token": "refresh-new",
                "expires_in": 3600,
            })))
            .expect(1)
            .mount(&server)
            .await;
        mount_folder_exists_once(&server).await;
        Mock::given(method("PUT"))
            .and(path(CONTENT_PATH))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id": "item-1",
                "name": "report.pdf",
                "size": 3,
            })))
            .mount(&server)
            .await;
        let registry = OneDriveRegistry::new(
            &registry_config(&server),
            &registry_secrets(),
            users.clone(),
        )
        .unwrap()
        .with_token_url(format!("{}/token", server.uri()))
        .unwrap();
        registry
            .client_for(&alice)
            .unwrap()
            .lock()
            .await
            .upload("report.pdf", b"abc".to_vec(), "application/pdf", None)
            .await
            .unwrap();
        assert_eq!(
            users
                .get(&alice, ONEDRIVE_PROVIDER)
                .unwrap()
                .unwrap()
                .expose_secret(),
            "refresh-new"
        );
    }

    #[tokio::test]
    async fn consent_start_and_complete_store_the_refresh_token() {
        let server = MockServer::start().await;
        let (_store, users, alice, bob) = per_user_store();
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(BodyContains("grant_type=authorization_code"))
            .and(BodyContains("code_verifier="))
            .and(BodyContains("code=synthetic-code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "refresh_token": "refresh-consent",
                "access_token": "access-consent",
                "expires_in": 3600,
            })))
            .expect(1)
            .mount(&server)
            .await;
        let mut config = Config::default();
        config.onedrive.enabled = true;
        config.onedrive.redirect_base_url = "https://bot.example.example".into();
        let mut consent = OneDriveConsent::new(&config, &registry_secrets(), users.clone())
            .unwrap()
            .unwrap();
        consent
            .set_endpoints(
                format!("{}/authorize", server.uri()),
                format!("{}/token", server.uri()),
            )
            .unwrap();

        let url = consent.start(&alice).await.unwrap();
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("scope=offline_access"));
        assert!(url.contains("redirect_uri=https%3A%2F%2Fbot.example.example"));
        let parsed = Url::parse(&url).unwrap();
        let state = parsed
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .unwrap();

        assert_eq!(
            consent.complete(&state, "synthetic-code").await.unwrap(),
            alice
        );
        assert_eq!(
            users
                .get(&alice, ONEDRIVE_PROVIDER)
                .unwrap()
                .unwrap()
                .expose_secret(),
            "refresh-consent"
        );
        // The state is single use and Bob never sees Alice's token.
        assert!(consent.complete(&state, "synthetic-code").await.is_err());
        assert!(users.get(&bob, ONEDRIVE_PROVIDER).unwrap().is_none());
        server.verify().await;
    }

    #[tokio::test]
    async fn consent_without_a_public_base_url_fails_closed() {
        let (_store, users, alice, _bob) = per_user_store();
        let mut config = Config::default();
        config.onedrive.enabled = true;
        let consent = OneDriveConsent::new(&config, &registry_secrets(), users)
            .unwrap()
            .unwrap();
        let error = consent.start(&alice).await.unwrap_err();
        assert!(error.to_string().contains("public base URL"));
    }

    #[tokio::test]
    async fn consent_rejects_an_unknown_state_and_a_missing_refresh_token() {
        let server = MockServer::start().await;
        let (_store, users, alice, _bob) = per_user_store();
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-only",
            })))
            .mount(&server)
            .await;
        let mut config = Config::default();
        config.onedrive.enabled = true;
        config.onedrive.redirect_base_url = "https://bot.example.example".into();
        let mut consent = OneDriveConsent::new(&config, &registry_secrets(), users.clone())
            .unwrap()
            .unwrap();
        consent
            .set_endpoints(
                format!("{}/authorize", server.uri()),
                format!("{}/token", server.uri()),
            )
            .unwrap();
        assert!(
            consent
                .complete("no-such-state", "synthetic-code")
                .await
                .is_err()
        );
        let url = consent.start(&alice).await.unwrap();
        let state = Url::parse(&url)
            .unwrap()
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .unwrap();
        let error = consent
            .complete(&state, "synthetic-code")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("refresh token"));
        assert!(users.get(&alice, ONEDRIVE_PROVIDER).unwrap().is_none());
    }

    #[test]
    fn the_global_refresh_token_imports_once_and_respects_a_disconnect() {
        let store = Store::open_in_memory(384).unwrap();
        let users = UserSecrets::with_master_key(store.clone(), [6u8; 32]).unwrap();
        let admin = store.admin_user_id().unwrap();
        let mut config = Config::default();
        config.onedrive.enabled = true;
        let secrets = SharedSecrets::new(SecretStore::from_map(BTreeMap::from([(
            "onedrive_refresh_token".into(),
            "global-refresh".into(),
        )])));

        assert!(import_global_refresh_token(&config, &secrets, &users, &store, &admin).unwrap());
        assert_eq!(
            users
                .get(&admin, ONEDRIVE_PROVIDER)
                .unwrap()
                .unwrap()
                .expose_secret(),
            "global-refresh"
        );
        // Idempotent: a second startup imports nothing.
        assert!(!import_global_refresh_token(&config, &secrets, &users, &store, &admin).unwrap());
        // A disconnect is not undone by the next restart.
        users.delete(&admin, ONEDRIVE_PROVIDER).unwrap();
        assert!(!import_global_refresh_token(&config, &secrets, &users, &store, &admin).unwrap());
        assert!(users.get(&admin, ONEDRIVE_PROVIDER).unwrap().is_none());
    }

    #[test]
    fn an_existing_admin_credential_is_never_overwritten_by_the_import() {
        let store = Store::open_in_memory(384).unwrap();
        let users = UserSecrets::with_master_key(store.clone(), [6u8; 32]).unwrap();
        let admin = store.admin_user_id().unwrap();
        let mut config = Config::default();
        config.onedrive.enabled = true;
        let secrets = SharedSecrets::new(SecretStore::from_map(BTreeMap::from([(
            "onedrive_refresh_token".into(),
            "global-refresh".into(),
        )])));
        users
            .set(&admin, ONEDRIVE_PROVIDER, "consent-refresh")
            .unwrap();
        assert!(!import_global_refresh_token(&config, &secrets, &users, &store, &admin).unwrap());
        assert_eq!(
            users
                .get(&admin, ONEDRIVE_PROVIDER)
                .unwrap()
                .unwrap()
                .expose_secret(),
            "consent-refresh"
        );
    }
}
