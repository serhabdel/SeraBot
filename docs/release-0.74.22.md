# Release `0.74.22`

## Summary

- **AWS Kiro OAuth & EventStream Wire (`auth_flows/kiro.rs`, `wire/kiro.rs`, `providers.rs`, `llm.rs`, `llm/wire_calls.rs`, `relay_net.rs`, `provider_presets.rs`, `branding.rs`)**
  - Added `KiroSignIn` (AWS SSO OIDC Device Authorization Grant with dynamic `/client/register`, `/device_authorization`, RFC 8628 `/token` polling, and `refresh_kiro_token`) with a strict `KiroRegion` allowlist (`us-east-1`, `us-west-2`, `eu-central-1`, `eu-west-1`, `ap-southeast-1`, `ap-northeast-1`) to prevent region-injection SSRF.
  - Added `wire/kiro.rs` (zero-dependency AWS EventStream binary frame decoder with 4 MiB frame cap, `generateAssistantResponse` request builder, and `ListAvailableModels` catalog parser).
  - Embedded real 64×64 PNG (`assets/brands/kiro.png`), desktop preset (`oauth_or_key`, `PLAN_RISK_KIRO`), and relay allowlist entries.

- **Devin CLI OAuth & Connect-RPC Wire (`auth_flows/devin.rs`, `wire/devin.rs`, `providers.rs`, `llm.rs`, `llm/wire_calls.rs`, `relay_net.rs`, `oauth.rs`, `provider_presets.rs`, `provider_flow.rs`, `branding.rs`)**
  - Added `DevinSignIn` (`https://app.devin.ai/auth/cli/continue` PKCE → `127.0.0.1:59653/callback` → `POST https://api.devin.ai/auth/cli/token` with JWT `exp` parsing and `devin-session-token$...` paste fallback).
  - Added `wire/devin.rs` (Connect-RPC JSON transport for `server.codeium.com` `GetCliModelConfigs` and `GetChatMessage`).
  - Embedded real 64×64 PNG (`assets/brands/devin.png`), desktop preset (`oauth_or_key`, `PLAN_RISK_DEVIN`), fixed port `59653` redirect plan, and relay allowlist entries.

- **Per-Model Thinking Level Accuracy Across All Providers (`thinking.rs`, `cloud_code.rs`)**
  - Refined `google-antigravity` rules (always-on Claude vs optional Claude vs non-thinking Claude, suffixed Gemini `-low`/`-medium`/`-high`/`-minimal`/`-xhigh`/`-max`, unsuffixed Gemini Pro/Argon vs Flash/tiered, `gpt-oss`).
  - Added accurate per-model `levels_for()` and `wire_fields()` for `claude-agent-sdk`, `kiro`, `devin`, `moonshot`, `inception`, `ollama`, `cerebras`, `together`, `fireworks`, `minimax`, `minimax-cn`, `nebius`, and `sambanova`, and tightened `anthropic`, `openai`, `gemini`, `xai`, and `github-copilot`.

- **Usage Page & Card Redesign (`ui_usage.rs`, `ui_usage_insights.rs`, `pages-data.css`)**
  - Added a 4-card KPI Hero Strip (**Tokens**, **Estimated Cost**, **Turns & Tools**, **Health & Quota**).
  - Replaced the 8-section vertical scroll wall with 5 segmented sub-view tabs (**Overview**, **Models & Bots**, **Insights & Relay**, **Costs & Alerts**, **Health**), `.usage-card` containers, and inline `.usage-share-bar` proportion bars.

## Verification

- Verified with `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo clippy -p serabot-desktop --features ui-check --all-targets -- -D warnings`, `inherit` security & code review, `cargo test -p serabot-desktop --lib --locked`, and targeted `serabot` server unit tests (`auth_flows::kiro`, `auth_flows::devin`, `wire::kiro`, `wire::devin`, `providers::tests`, `thinking`, `relay_net`, `cloud_code::parity_tests`). Live Kiro and Devin sign-in/chat runs remain for owner verification.
