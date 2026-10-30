use axum::Json;
use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Value, json};

/// Strict policy for the embedded landing page: inline CSS only, no scripts,
/// no external origins, and no form or frame surface.
pub(crate) const CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

pub(crate) const APK_ASSET: &str = "serabot-mobile-latest.apk";
pub(crate) const SHA256SUMS_ASSET: &str = "SHA256SUMS";
const DESKTOP_ASSET: &str = "serabot-desktop-latest-windows-x64.exe";

const DOWNLOADS_PLACEHOLDER: &str = "{{DOWNLOADS}}";
const DOWNLOADS_UNCONFIGURED: &str =
    "<p class=\"meta\">Client downloads are not configured for this server build.</p>";
const APP_HTML: &str = include_str!("../assets/app.html");

pub(crate) fn build_sha() -> &'static str {
    option_env!("SERABOT_BUILD_SHA").unwrap_or("dev")
}

/// Build-time GitHub repository (`owner/repo`) used for release links.
/// Unset, empty, or malformed values disable downloads instead of guessing.
pub(crate) fn releases_repo() -> Option<String> {
    normalize_repo(option_env!("SERABOT_RELEASES_REPO").unwrap_or(""))
}

fn normalize_repo(value: &str) -> Option<String> {
    let value = value.trim();
    let (owner, repo) = value.split_once('/')?;
    if repo.contains('/') || !valid_owner(owner) || !valid_repo(repo) {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

fn valid_owner(owner: &str) -> bool {
    !owner.is_empty()
        && owner.len() <= 39
        && !owner.starts_with('-')
        && !owner.ends_with('-')
        && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn valid_repo(repo: &str) -> bool {
    !repo.is_empty()
        && repo.len() <= 100
        && !repo.starts_with('.')
        && repo
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn asset_url(repo: &str, asset: &str) -> Option<String> {
    let repo = normalize_repo(repo)?;
    if asset.is_empty()
        || !asset
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return None;
    }
    Some(format!(
        "https://github.com/{repo}/releases/latest/download/{asset}"
    ))
}

fn downloads_markup(repo: Option<&str>) -> String {
    let Some(repo) = repo.and_then(normalize_repo) else {
        return DOWNLOADS_UNCONFIGURED.to_string();
    };
    let (Some(apk), Some(desktop), Some(sums)) = (
        asset_url(&repo, APK_ASSET),
        asset_url(&repo, DESKTOP_ASSET),
        asset_url(&repo, SHA256SUMS_ASSET),
    ) else {
        return DOWNLOADS_UNCONFIGURED.to_string();
    };
    format!(
        "<div class=\"links\">\n      <a class=\"button\" href=\"{apk}\">Android APK</a>\n      <a class=\"button\" href=\"{desktop}\">Windows desktop</a>\n    </div>\n    <p class=\"meta\"><a href=\"{sums}\">SHA256SUMS</a></p>"
    )
}

fn render_landing(repo: Option<&str>) -> String {
    APP_HTML.replace(DOWNLOADS_PLACEHOLDER, &downloads_markup(repo))
}

/// Public, unauthenticated routes served next to the Owner API.
pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/", get(landing))
        .route("/app", get(landing))
        .route("/app/latest.apk", get(latest_apk))
        .route("/app/SHA256SUMS", get(sha256sums))
        .route("/healthz", get(healthz))
        .route(crate::elevenlabs::CIMD_PATH, get(cimd))
}

/// Public CIMD client metadata document, used as the ElevenLabs OAuth
/// `client_id` (client_id_metadata_document). Contains no secrets and is
/// served `no-store` so the authorization server always fetches a fresh copy.
async fn cimd() -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/json; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CONTENT_SECURITY_POLICY, CSP),
        ],
        Json(crate::elevenlabs::cimd_document()),
    )
        .into_response()
}

async fn landing() -> Response {
    let page = render_landing(releases_repo().as_deref());
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CONTENT_SECURITY_POLICY, CSP),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        page,
    )
        .into_response()
}

async fn latest_apk() -> Response {
    download_response(APK_ASSET)
}

async fn sha256sums() -> Response {
    download_response(SHA256SUMS_ASSET)
}

fn download_response(asset: &str) -> Response {
    let Some(url) = releases_repo().and_then(|repo| asset_url(&repo, asset)) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "client downloads are not configured on this server",
        )
            .into_response();
    };
    (
        StatusCode::FOUND,
        [
            (header::LOCATION, url.as_str()),
            (header::CACHE_CONTROL, "no-store"),
        ],
    )
        .into_response()
}

async fn healthz() -> Json<Value> {
    Json(json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "build": build_sha(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_release_urls_only_from_a_valid_owner_repo() {
        assert_eq!(
            asset_url("owner/repo", APK_ASSET).as_deref(),
            Some(
                "https://github.com/owner/repo/releases/latest/download/serabot-mobile-latest.apk"
            )
        );
        assert_eq!(
            asset_url(" owner/repo.git ", SHA256SUMS_ASSET).as_deref(),
            Some("https://github.com/owner/repo.git/releases/latest/download/SHA256SUMS")
        );
        assert!(asset_url("owner", APK_ASSET).is_none());
        assert!(asset_url("owner/repo/extra", APK_ASSET).is_none());
        assert!(asset_url("owner/..", APK_ASSET).is_none());
        assert!(asset_url("owner/.hidden", APK_ASSET).is_none());
        assert!(asset_url("-owner/repo", APK_ASSET).is_none());
        assert!(asset_url("own er/repo", APK_ASSET).is_none());
        assert!(asset_url("owner/re po", APK_ASSET).is_none());
        assert!(asset_url("owner/repo", "../evil").is_none());
        assert!(asset_url("owner/repo", "").is_none());
    }

    #[test]
    fn landing_page_is_script_free_and_hides_unconfigured_links() {
        let unconfigured = render_landing(None);
        assert!(!unconfigured.contains("<script"));
        assert!(!unconfigured.contains(DOWNLOADS_PLACEHOLDER));
        assert!(unconfigured.contains("SeraBot"));
        assert!(unconfigured.contains("private single-operator deployment"));
        assert!(!unconfigured.contains("releases/latest/download"));

        let configured = render_landing(Some("owner/repo"));
        assert!(!configured.contains("<script"));
        assert!(!configured.contains(DOWNLOADS_PLACEHOLDER));
        assert!(configured.contains(
            "https://github.com/owner/repo/releases/latest/download/serabot-mobile-latest.apk"
        ));
        assert!(configured.contains(
            "https://github.com/owner/repo/releases/latest/download/serabot-desktop-latest-windows-x64.exe"
        ));
        assert!(
            configured
                .contains("https://github.com/owner/repo/releases/latest/download/SHA256SUMS")
        );

        let invalid = render_landing(Some("owner/repo/extra"));
        assert!(invalid.contains(DOWNLOADS_UNCONFIGURED));
    }

    #[tokio::test]
    async fn cimd_route_serves_self_consistent_secret_free_json() {
        let response = cimd().await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json; charset=utf-8"
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let doc: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(doc["client_id"], json!(crate::elevenlabs::cimd_client_id()));
        assert_eq!(
            doc["redirect_uris"],
            json!([crate::elevenlabs::oauth_callback_uri()])
        );
        assert!(!bytes.windows(3).any(|w| w == b"sk_"));
    }
}
