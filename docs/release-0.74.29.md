# SeraBot 0.74.29

The Claude Agent SDK (Gateway) provider no longer forces an API key: it can
be saved keyless and used via "Sign in with browser".

## Changes

- **`admin.rs`**: a `claude-agent-sdk` provider may be saved without an API
  key (like OpenRouter), so the "secret claude_gateway_token is empty;
  provide an API key" refusal no longer blocks the card. Both ways stay
  available on the card: **Add a key** (the gateway bearer token, per user)
  and **Sign in with browser**.
- **`api/provider_accounts.rs` (`gateway_endpoint`)**: the browser sign-in
  relay now resolves the gateway bearer token from the provider's stored
  credential first, and falls back to the server-level `[claude_gateway]
  token_secret` in the age store — so the sign-in works even when the
  provider was saved without a key. If neither is set, the error says so
  plainly instead of leaking a secret name.
- **`serabot-desktop` (`provider_accounts.rs`)**: the legacy
  "secret … is empty; provide an API key" server error is mapped to a
  friendly "Sign in to Claude Agent SDK Gateway…" prompt (kind
  `claude-agent-sdk`) instead of showing the raw secret name.

## Verification

- `cargo fmt --all -- --check`, `cargo clippy -p serabot --locked --all-targets -- -D warnings`,
  `cargo clippy -p serabot-desktop --features ui-check --locked --all-targets -- -D warnings` — clean
- `cargo test -p serabot-desktop --features ui-check --locked --lib` — green
  (incl. extended `openrouter_saves_without_a_key…`-style admin test and the
  new plain-error mapping test; server lib tests run in CI)
