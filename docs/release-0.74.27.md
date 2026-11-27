# SeraBot 0.74.27

Proton Pass sync errors are no longer a bare "HTTP 502", and the Claude
Agent SDK gateway is documented for multi-account use.

## Highlights

- **Proton Pass sync shows the real reason instead of "Server returned
  HTTP 502"** (`vault_providers.rs`, `serabot-desktop/src/api.rs`,
  `apps/serabot_mobile/lib/api_client.dart`):
  - `classify_cli_failure` now recognizes upstream outages in pass-cli
    stderr (502/503/504, "bad gateway", "service unavailable", "upstream")
    → "Proton Pass servers answered with an outage (HTTP 5xx). Try again
    shortly."
  - CLI *exit* failures (read, list, sync) are now `SeraError::Rejected`
    (HTTP 422) with the classified, server-authored sentence instead of
    `External` (HTTP 502), which the server genericized and the clients
    reduced to "Check server availability".
  - The desktop API client now parses the server's safe `{"error", "code"}`
    body for every non-success status except 401/403/409/429 (which keep
    their fixed wording), returning `ApiError::Coded` — a 502 from a
    connected service no longer reads as "SeraBot server unavailable".
  - Mobile `serverErrorText` also parses 500/502 bodies.
- **Claude Agent SDK: auth model + multi-account + native Rust documented**
  (`docs/specs/claude-agent-sdk-gateway.md`, `AGENTS.md`,
  `provider_presets.rs`):
  - There is no third-party claude.ai OAuth (Anthropic shut it down in
    2026; retired here in 0.74.25). The Agent SDK authenticates inside the
    gateway process with `claude auth login` or `ANTHROPIC_API_KEY`; SeraBot
    only holds the gateway bearer token.
  - Multi-account = one loopback gateway per Claude login on its own port,
    each registered as its own `claude-agent-sdk` provider (config accepts
    any number on distinct loopback ports — locked in by
    `multiple_claude_agent_sdk_gateways_each_on_their_own_loopback_port`).
  - The Rust server already speaks the gateway natively over loopback HTTP
    (`Wire::ClaudeGateway`: `GET /v1/models`, `POST /v1/turn`, Bearer) with
    tool execution routed back through the Rust `gateway`. A Rust-native
    reimplementation of the SDK would be a new local runtime and stays out
    of scope per `AGENTS.md`.

## Verification

- `cargo fmt --all -- --check`, `cargo clippy -p serabot --locked --all-targets -- -D warnings`,
  `cargo clippy -p serabot-desktop --features ui-check --locked --all-targets -- -D warnings`,
  `cargo test -p serabot-desktop --features ui-check --locked --lib` — all green locally.
- CI (`cargo test --workspace --locked`) runs the server lib tests on
  Windows runners.
