# SeraBot 0.74.31

Claude Agent SDK zero-friction setup & loopback supervisor, remaining quota bars (Claude, Antigravity, Kiro), Proton Pass sync 502 fix, fast Hostinger/SMTP sending, and Desktop UI/UX fixes.

## Highlights

- **Claude Agent SDK (Gateway) — Fixed End-to-End (`claude_gateway_proc.rs`, `api/connectors.rs`, `api/provider_accounts.rs`, `ui/providers.rs`)**:
  - `GET /v1/providers` now always includes `serabot.toml`-configured providers even with 0 accounts, so `Claude Agent SDK (Gateway)` appears directly under **Your providers** instead of duplicating under **Add a provider** and failing with `provider 'claude-agent-sdk' is configured in serabot.toml`.
  - `ensure_gateway_token` automatically generates and stores `claude_gateway_token` in the age store when unset (eliminating `gateway_token_missing`).
  - Added [`ClaudeGatewaySupervisor`](file:///home/antigravity/serhabdel/SeraBot/crates/serabot/src/claude_gateway_proc.rs) (`[claude_gateway] auto_start = true` by default): probes `/health` on loopback and automatically starts/supervises `services/claude_gateway/server.py` (or surfaces exact install instructions when Python/`claude-agent-sdk` is missing).

- **Live Remaining Quota for Claude, Antigravity & Kiro (`services/claude_gateway/server.py`, `provider_quota.rs`, `ui/account_views.rs`, `provider_accounts.rs`)**:
  - **Claude**: `GET /v1/auth/usage` + `ClaudeGatewayQuota` (5h / 7d plan utilization + SDK `rate_limit_info` fallback).
  - **Google Antigravity**: `AntigravityQuota` now routes through the member's Desktop Network Relay when enabled; `cloud_code::catalog_urls` skips enterprise-only `businessaicode.*` probing for personal accounts and lowers per-attempt transport warnings to `debug!`.
  - **AWS Kiro**: `KiroQuota` reads monthly credits and free-trial remaining allowance via `getUsageLimits`.
  - **Desktop**: expanded `QUOTA_KINDS` to all 7 quota-capable providers with explicit `XX% remaining` bars and reset times.

- **Proton Pass Sync 502 Fix (`vault_providers.rs`, `panel.rs`)**:
  - Raised list stdout cap to 16 MiB (`MAX_LIST_OUTPUT_BYTES`), ran `vault list` + `item list` inside a single `pass-cli login` session with a 45s timeout, stripped non-JSON CLI preamble, and returned structured HTTP 422 `Rejected` errors instead of opaque HTTP 502.

- **Fast & Reliable Email Sending (`mail.rs`, `scheduler.rs`, `api/mail.rs`, `ui_mail.rs`, `ui_mail_list.rs`)**:
  - Added `scheduled_mail_wake` + 2s imminent-send polling in `scheduler.rs`, `POST /v1/mail/scheduled/{id}/send-now`, a **Send now** button on `UndoBar`, and an **Undo delay** selector (`Off` / `5s` / `10s`) in Compose.
  - Upgraded `smtp_send` & `smtp_test` with multi-line `220-` greeting support, `EHLO serabot.local`, single-RTT `AUTH PLAIN` when advertised, single-pass dot-stuffing, 60s DATA write timeout, and per-phase timing logs.

- **Desktop UI/UX Fixes (`system.css`, `app.css`, `settings.css`, `ux.css`, `pages-data.css`, `shell.rs`, `ui_settings_page.rs`)**:
  - Fixed `.header-model-pop` right-edge alignment (`left: auto; right: 0;`) so the chat model picker is never clipped off-screen.
  - Floated `.menu.rail-menu` at `z-index: 120` with a click-outside scrim so the bottom-left profile popover never hides behind adjacent views.
  - Added crisp vector icons to all 16 Settings sections and separated `.settings-search` from `.settings-nav-scroll` so scrolled items never overlap the search box.

## Verification

- `python3 -m unittest discover -s services/claude_gateway/tests -v` — 16 passed
- `cargo fmt --all -- --check` — clean
- `cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo clippy -p serabot-desktop --features ui-check --all-targets -- -D warnings` — clean
- `cargo test -p serabot-desktop --features ui-check --locked --lib` — green
- `cargo test -p serabot --locked --lib` (gateway, quota, vault_providers, mail, connectors, model_router) — green (full workspace suite runs in CI)
