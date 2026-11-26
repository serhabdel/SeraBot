# Release `0.74.20`

## Summary

- **Python `claude-agent-sdk` Loopback Gateway (`services/claude_gateway/`, `config.rs`, `providers.rs`, `llm.rs`, `llm/wire_calls.rs`, `wire/mod.rs`, `provider_presets.rs`, `AGENTS.md`)**
  - Added the opt-in loopback-only Python `claude-agent-sdk` gateway (`services/claude_gateway/server.py`, `127.0.0.1:8791`) with constant-time Bearer token auth (`SERABOT_CLAUDE_GATEWAY_TOKEN`), `tools=[]` and `setting_sources=[]` on `ClaudeAgentOptions`, and `create_sdk_mcp_server("serabot", ...)` capturing `mcp__serabot__*` tool calls so every tool executes through SeraBot's Rust `gateway` (preserving memory, approvals, policy, and audit).
  - Added `[claude_gateway]` config + loopback validation in `config.rs`, `Wire::ClaudeGateway` + `"claude-agent-sdk"` builtin provider in `providers.rs` / `llm.rs`, and the `"Claude Agent SDK (Gateway)"` preset in `provider_presets.rs`.

- **Settings Independent Sidebar & Content Scrolling (`assets/settings.css`, `assets/pages-data.css`, `ui/settings_view.rs`)**
  - Turned `.pane-body.settings-layout` into a non-scrolling split flex container where `.settings-nav` and `.settings-content` scroll independently (`overscroll-behavior: contain`), flattened nested `.pane` sub-pages to eliminate double scrollbars, and reset `#settings-content-scroll` to top on section change.

- **Fixed Stuck Lazy-Loading Skeletons (`session.rs`, `ui/providers.rs`, `ui_integrations.rs`, `ui_logs.rs`, `ui/memory.rs`)**
  - Cleared `poll_paused` and `section_load_error` when navigating via `open_section()` / `open_settings_section()` while connected, and added `section_load_error` + inline **Retry** buttons (`retry_section_load()`) when `Providers`, `Connectors`, `MarketplaceList`, `ListMemories`, or `Logs` fail to load.

- **Real Vendor PNG Icons (`assets/brands/`, `branding.rs`, `SOURCES.md`)**
  - Downloaded, resized to 64×64 RGBA PNGs, and embedded real vendor icons for **OpenCode** (`opencode.png`), **Muse** (`muse.png`), **Nous** (`nous.png`), and **Inception Labs** (`inception.png`), replacing generic SVG placeholders, and mapped `"claude-agent-sdk"` to the Claude mark.

## Verification

- Verified with `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo clippy -p serabot-desktop --features ui-check --all-targets -- -D warnings`, `python3 -m unittest discover -s services/claude_gateway/tests -v` (5/5 OK), `inherit` security & code review, `cargo test -p serabot-desktop --lib --locked`, and targeted `serabot` server unit tests (`claude_gateway`, `wire_clients_tests`, `config::tests`). Live end-to-end runs against `claude-agent-sdk` and desktop WebView2 remain for owner verification.
