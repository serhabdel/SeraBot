# Release 0.43.0

Wave 6 UX, connector, marketplace, and preview polish across `serabot` and `serabot-desktop`.

## Highlights

- **Long conversation scroll & details auto-scroll** (`crates/serabot-desktop/src/chat_scroll.rs`, `crates/serabot-desktop/src/ui_files.rs`, `crates/serabot-desktop/assets/app.css`):
  - Added bottom padding on `.messages` so the last message clears floating controls cleanly.
  - Updated the `scroll` handler to call `setFollow(true)` when scrolled back to the bottom so `Jump to latest` hides automatically.
  - Added a capturing `toggle` listener that scrolls opened `<details class="activity done">` and `<details class="step-details">` elements into view.

- **ElevenLabs OAuth 2.1-only & media handling** (`crates/serabot/src/elevenlabs_mcp.rs`, `crates/serabot/src/elevenlabs_oauth.rs`, `crates/serabot/src/api/connectors.rs`, `crates/serabot/src/api/admin.rs`, `crates/serabot-desktop/src/integrations.rs`, `crates/serabot-desktop/src/ui_connectors.rs`, `crates/serabot-desktop/src/branding.rs`):
  - Removed `elevenlabs_api_key` in favor of per-user ElevenLabs OAuth 2.1 + PKCE, and enabled non-admin OAuth routes in `api/admin.rs` and `ui_connectors.rs`.
  - Updated `collect_links` to skip non-media navigation keys and `/app/`, `/docs/`, `/pricing`, `/studio`, etc. URLs, and added inline `data:` / `*_base64` extraction.
  - Expanded `MEDIA_HOSTS`, extended `sniff_media` to support `image/avif`, `audio/mp4` (`m4a`), and `image/svg+xml`, and updated `save_link` to ignore `text/html`, `text/plain`, and `application/json` non-media responses.

- **Integrations overhaul & Marketplace base enabled** (`crates/serabot-desktop/src/marketplace.rs`, `crates/serabot-desktop/src/ui_marketplace.rs`, `crates/serabot-desktop/src/ui_integrations.rs`, `crates/serabot-desktop/src/integrations.rs`, `crates/serabot-desktop/src/branding.rs`, `crates/serabot-desktop/src/session.rs`, `crates/serabot-desktop/assets/app.css`, `crates/serabot-desktop/assets/foundation.css`, `crates/serabot/src/marketplace.rs`, `crates/serabot/src/api/marketplace.rs`, `crates/serabot/src/main.rs`, `AGENTS.md`):
  - Set `RUNTIME_CALLS_PLUGINS = true` in `crates/serabot-desktop/src/marketplace.rs` for live `http` MCP plugins while keeping `stdio` and `*.example.invalid` placeholder hosts blocked.
  - Embedded all 10 shipped `plugins/*/plugin.json` manifests in `crates/serabot/src/marketplace.rs` (`builtin_catalog()` / `load_catalog_with_builtin`).
  - Added SVG brand marks in `branding.rs` for the 10 shipped plugin slugs plus connectors (`todoist`, `slack`, `github`, `exa`), and updated `IntegrationCard` and `PluginSheet` with `suite_label` and `BrandLogo` card layouts.

- **Artifact preview fixes** (`crates/serabot/src/api/preview.rs`, `crates/serabot-desktop/src/preview.rs`):
  - Fixed server and desktop artifact preview envelope handling and PDF fallback behavior.
