# Release `0.74.19`

## Summary

- **Antigravity Connector & Catalog Reliability (`cloud_code.rs`, `llm.rs`, `model_router.rs`)**
  - `complete_cloud_code` and `fetch_cloud_code_catalog` now try all fallback Cloud Code origins (`daily-cloudcode-pa.googleapis.com` → `cloudcode-pa.googleapis.com` → sandbox hosts) instead of aborting on the first `CloudPost::Failed`.
  - `antigravity_version()` caches failed live-version probes for 5 minutes (`CachedVersion::Failed`) so an unreachable updater endpoint does not stall every Cloud Code request for 3 seconds.
  - `model_router::refresh_provider` now distinguishes `401`/`403` (`KeyRejected`), `0`/`5xx` (`Unreachable`), and other `4xx` (`RequestFailed`, logged with provider/status/code) instead of collapsing non-auth failures into `"Provider did not answer."`.

- **Claude Plan & `setup-token` Transport Fixes (`providers.rs`, `llm/wire_calls.rs`, `auth_flows/claude.rs`, `provider_presets.rs`)**
  - `open_provider_client` now routes `sk-ant-oat*` tokens (including `setup-token` credentials without a `refresh_token`) through `Wire::AnthropicPlan` (`Authorization: Bearer` + `anthropic-beta: oauth-2025-04-20`), never `x-api-key`.
  - `send_wire` no longer appends a duplicate `User-Agent: SeraBot/...` header when `PLAN_USER_AGENT` is already set, and surfaces an explicit error when Anthropic's OAuth gate rejects a consumer token on `/v1/messages`.
  - `provider_presets.rs` reorders `anthropic` fallback models so valid Anthropic model IDs (`claude-sonnet-4-5`, `claude-opus-4-5`, `claude-haiku-4-5`, …) come first.

- **Tool Limit & Conversation Poisoning Fixes (`config.rs`, `store.rs`, `llm.rs`)**
  - Raised `MAX_SCAFFOLD_TOOLS` from `32` to `64` (server and desktop) to prevent `validation error: too many tools`.
  - Added `sanitize_llm_history` in `Store::history_for_llm` and `messages_to_json` / `message_to_json` guards in `llm.rs` so empty-id or orphaned `Role::Tool` results from a crashed turn are dropped instead of poisoning all subsequent routes with `400: Tool call id has to be defined.`.

- **Chat `"Retry on..."` & Settings / AI Providers UI/UX (`ui/wait_cards.rs`, `chat_failure.rs`, `runs.css`, `ui_settings_page.rs`, `ui/settings_view.rs`, `ui/providers.rs`, `pages-connect.css`, `settings.css`)**
  - Bounded scrollable `.failed-alt` popover (`max-height: 220px`), truncated primary fallback button labels, and diversified `retry_alternatives` across providers.
  - Overhauled the **Settings** cards and **AI Providers** page with a two-column **Bot routing & fallback chains** workspace (`bot-chain-shell`) for managing per-bot primary and fallback models.

## Verification

- Verified with `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo clippy -p serabot-desktop --features ui-check --all-targets -- -D warnings`, `inherit` security & code review, and `cargo test --workspace --locked`. Live Antigravity, Claude, and desktop UI flows remain for owner verification.
