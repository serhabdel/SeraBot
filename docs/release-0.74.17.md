# Release `0.74.17`

## Summary

- **Social Accounts Hub — Persistent Browser Sessions for `marketer`**
  - **Social Browser Endpoints (`api/browser.rs`, `api/mod.rs`)**: Added `GET /v1/browser/social/status` and `POST /v1/browser/social/open` (UID-scoped) backed by a dedicated `"social"` Chromium profile (`<browser>/<user_id>/social`) that opens official login pages (`linkedin`, `instagram`, `x`, `facebook`, `youtube`) in user-controlled mode (`Controller::User`) so the owner signs in once and Chromium persists the real session.
  - **Checkpoint Refusal Guardrails (`browser/signin.rs`)**: Added LinkedIn, Instagram, X, and Facebook checkpoint/verification refusal rules so bots stop immediately instead of retrying or bypassing security challenges.
  - **`marketer` Browser & Social Workflow (`bots/marketer.toml`, `prompts/marketer.txt`)**: Equipped `marketer` with SeraBot's browser tools (`browser_navigate`, `browser_snapshot`, `browser_click`, `browser_type`, `browser_press`, `browser_screenshot`, `browser_tabs`, `browser_new_tab`, `browser_upload`, `browser_wait`, `browser_scroll`) and updated its prompt to inspect login state first, draft posts/media, and always confirm via `ask_user` before publishing, commenting, or sending DMs.
  - **Desktop Social Accounts Card (`api.rs`, `ui_connectors.rs`, `ui_integrations.rs`, `pages-connect.css`)**: Added **Social Accounts (Browser Sessions)** inside **Settings → Integrations** with 1-click **Sign in / Open** buttons for LinkedIn, Instagram, X, Facebook, and YouTube Studio plus a 1-click **Assign `marketer` to `social` profile** action.

- **Providers Bot Routing Polish & Dead ACP Cleanup**
  - **Startup Hydration & Badge Refresh (`session.rs`, `session/bot_settings.rs`, `ui/providers.rs`)**: Opening **Settings → Providers** now loads `listed_bots` immediately on startup, `bot_settings_saved` re-fetches `BotDetail` so thinking/fallback badges stay visible after inline model changes, and resetting a bot with fallbacks displays `"Default (reset route & fallbacks)"`.
  - **Dead ACP Removal (`ui.rs`, removed `ui_claude_code.rs`)**: Removed the unreferenced `ui_claude_code.rs` module and `claude-code.css` embed.

- **Integrations & Custom Email Refinements**
  - **Saved-Mailbox Test (`api/mail.rs`, `api/tests/mail_api.rs`, `desktop/api.rs`, `ui_connectors.rs`)**: Added `POST /v1/mail/test` and a **Test** button on every **Connected Mailboxes** row to verify SMTP/IMAP without editing or re-entering a password.
  - **Integrations Mail Summary (`ui_integrations.rs`)**: Shows the connected named mailbox or account count when the default slot is unused.

## Verification

- Verified with `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo clippy -p serabot-desktop --features ui-check --all-targets -- -D warnings`, `inherit` security & code review, server unit/integration tests (`browser::signin`, `mail_api`), and `serabot-desktop` unit tests (`--lib` + `--features ui-check`). Live social sign-in in the Browser tab and Hostinger mailbox test remain for owner verification.
