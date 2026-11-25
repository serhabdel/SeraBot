# Release `0.74.18`

## Summary

- **Antigravity Connector Reliability Fixes (`cloud_code.rs`, `llm.rs`)**
  - **6-hour TTL Version Cache**: Replaced the sticky `OnceCell<Option<String>>` in `cloud_code.rs` with a 6-hour TTL `RwLock` cache that never caches `None`/failures, updated fallback `IDE_VERSION` to `2.31.4`, and added `invalidate_live_version()`.
  - **Automatic Version & Project Recovery**: `complete_cloud_code` and `fetch_cloud_code_catalog` now invalidate `LIVE_VERSION` and clear negative project discovery (`cloud_project()`) on an initial `400`/`403` Cloud Code rejection and retry once.

- **Claude Sign-In Upgrade (`auth_flows/claude.rs`, `api/connectors.rs`, `provider_presets.rs`, `provider_accounts.rs`, `ui/providers.rs`, `ui/account_views.rs`)**
  - **Clearer OAuth + Manual Fallback**: Updated `anthropic` preset badge to `"OAuth / API Key"`, exposed OAuth flow/secret metadata cleanly in `/v1/providers`, and added inline **"Paste callback URL or code#state"** support in the desktop sign-in flow (for when loopback port `54545` is blocked or remote).
  - **`setup-token` Support**: `ClaudeSignIn` now also accepts a pasted `sk-ant-oat01-...` `setup-token` directly with a deterministic `sha256` account fingerprint.

- **Inception Labs Native Provider + Logo (`providers.rs`, `relay_net.rs`, `costs.rs`, `provider_presets.rs`, `branding.rs`)**
  - Registered `inception` (`https://api.inceptionlabs.ai/v1`, OpenAI-compatible) across server builtins, desktop network relay allowlist (`api.inceptionlabs.ai`), default Mercury pricing (`mercury-3`, `mercury-2.5`, `mercury-2`, `mercury-edit-2`, `mercury-voice`), desktop provider presets (noting Inception's published Dec 28, 2026 API wind-down), and a custom **Inception Labs** SVG logo in `branding.rs`.

- **Mistral Vibe Browser Sign-In (`auth_flows/mistral.rs`, `auth_flows/mod.rs`, `provider_presets.rs`, `provider_flow.rs`, `ui/account_views.rs`, `provider_accounts.rs`, `ui/providers.rs`)**
  - Integrated Mistral Vibe's PKCE-bound browser sign-in (`POST https://console.mistral.ai/api/vibe/sign-in` → browser approval → poll → `POST .../exchange` for a minted `MISTRAL_API_KEY`) under `SignInRegistry` (`"mistral"`) as a `FlowKind::Device` flow without a `user_code`.
  - Updated the desktop `mistral` preset to `"OAuth / API Key"` (`sign_in: "browser"`) and updated `FlowPanel` / `ProviderFlow` to show browser sign-in copy when `user_code` is absent.

## Verification

- Verified with `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo clippy -p serabot-desktop --features ui-check --all-targets -- -D warnings`, `inherit` security & code review, server unit tests (`cloud_code`, `providers::tests`, `auth_flows::claude`, `auth_flows::mistral`, `costs::tests`), and `serabot-desktop` unit tests (`branding`, `provider_presets`, `provider_accounts`, `provider_flow`, `provider_view`). Live Claude sign-in, Mistral browser sign-in, Antigravity chat, and Inception `mercury-3` calls remain for owner verification.
