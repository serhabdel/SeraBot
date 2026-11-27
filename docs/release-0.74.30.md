# SeraBot 0.74.30

Claude Agent SDK Gateway overhaul (`nightly-labs/openbot` parity), Mail Signature Builder rich HTML paste/preview/save fix, and cleaner server diagnostics.

## Highlights

- **Claude Agent SDK (Gateway) — Official CLI Sign-In & Multi-Account Isolation (`services/claude_gateway/`, `auth_flows/gateway.rs`, `auth_flows/mod.rs`, `api/provider_accounts.rs`)**:
  - Removed the broken hand-rolled `claude.ai` OAuth client/token exchange in `services/claude_gateway/server.py`.
  - Replaced it with the official **`claude auth login --claudeai`** CLI flow (mirroring `nightly-labs/openbot`): PTY/pipe URL extraction, automatic completion polling (`GET /v1/auth/flow/{id}`), pasted authorization code submission (`POST /v1/auth/complete`) for remote/OVH servers, and 1-click import of an existing CLI login (`POST /v1/auth/import-cli`).
  - Added per-account **`CLAUDE_CONFIG_DIR`** profile isolation (`<state_dir>/profiles/<profile>`) and clean subprocess env sanitization (`_clean_claude_env` stripping nested `CLAUDECODE*` vars and `ANTHROPIC_API_KEY` on subscription profiles), so a single loopback gateway can serve multiple Claude accounts cleanly via `X-Claude-Profile`.
  - Added `GET /v1/providers/{id}/gateway-status`, `POST /v1/providers/{id}/gateway-token` (1-click token generation saved to the age store), and `POST /v1/providers/{id}/gateway-import-cli`.
  - Desktop (`ui/providers.rs`, `ui/account_views.rs`, `branding.rs`): fixed the modal title to **"Add account to Claude Agent SDK (Gateway)"**, added a live **Gateway Status** strip with 1-click **Generate token** + **Connect existing Claude login**, and added inline **Paste authorization code** support alongside automatic browser polling.

- **Mail Signature Builder & Rich HTML Paste / Live Preview (`ui_mail.rs`, `mail.rs`)**:
  - Replaced the hidden-`<textarea>` sync hack in `MailSignatureModal` and `MailComposeModal` with a direct `dioxus.send(...)` bridge (`rich_editor_bridge_js`) backed by `MutationObserver` and a live DOM flush right before saving.
  - Added lossless `CF_HTML` / `<table>` / `<style>` paste insertion via `Selection` / `Range` (and standalone pasted image support), so **Live preview** and the saved signature match what you paste.
  - Auto-uploads pasted embedded `data:image/*` logos to your UID-scoped Library on save (`cid:signature-logo@serabot` and `cid:sig-img-<hash>@serabot`, resolved in `mail.rs`), and fixed `Fields`-mode logo saving.
  - Upgraded `strip_html_tags` so `<style>`/`<head>` blocks don't leak into plain-text fallbacks.

- **Actionable Errors & Antigravity Transport Diagnostics (`api/mod.rs`, `llm.rs`, `model_router.rs`)**:
  - Replaced withheld `"invalid request"` validation responses for `gateway_token_missing`, `claude_oauth_retired`, and mail signature length/format checks with clear error messages.
  - Added safe `transport_reason` tags (`timeout`, `connect`, `dns`, `tls`, `transport`) to Antigravity `catalog refresh failed` warnings.

## Verification

- `python3 -m unittest discover -s services/claude_gateway/tests -v` — 14 passed
- `cargo fmt --all -- --check` — clean
- `cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo clippy -p serabot-desktop --features ui-check --all-targets -- -D warnings` — clean
- `cargo test -p serabot-desktop --features ui-check --locked --lib` — green
- `cargo test -p serabot --locked` (gateway, mail, api, model_router) — green (full workspace suite runs in CI)
