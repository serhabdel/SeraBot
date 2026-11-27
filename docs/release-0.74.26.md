# SeraBot 0.74.26

Provider sign-in and Antigravity routing fixes found from owner logs
(`unknown_provider`, `forbidden: AWS Kiro sign-in is turned off on this
server`, `validation error: too many tools`, and a "Turn on Claude sign-in"
button that only refreshed).

## Highlights

- **Retired Claude plan sign-in shows a terminal notice, not a dead button
  (`serabot-desktop/src/provider_accounts.rs`)**: the server reports
  `plan_signin = "off"` for every `anthropic` provider since the 2026
  third-party `claude.ai` OAuth shutdown, and `plan_signin_gate` /
  `ClaudeSignIn` reject any attempt with the shutdown message, so re-saving
  the provider could never change anything. The console now maps that state
  to a new `SigninBlocker::Retired` notice ("Anthropic shut down third-party
  claude.ai sign-in in 2026…") with no fix button, pointing to an Anthropic
  Console API key, the Claude Agent SDK Gateway, or Desktop ACP. Codex and
  Copilot keep the working "Turn on … sign-in" fix.
- **Kiro / Devin OAuth accounts are gated on the raw `plan_oauth` flag
  (`providers.rs`)**: the client-build gate used
  `ProviderConfig::plan_oauth_enabled()`, which is kind-restricted to
  `anthropic` / `openai-codex` / `github-copilot` and therefore always false
  for `kiro` and `devin` — every Kiro/Devin sign-in account was refused at
  runtime with `forbidden: … sign-in is turned off on this server` even with
  the flag on. The gate now uses `profile.plan_oauth` (on by default;
  `plan_oauth = false` still disables it), matching the sign-in start path.
- **Antigravity wire tool cap truncates instead of failing the task
  (`cloud_code.rs`)**: `build_cloud_code_envelope_with` returned
  `validation error: too many tools` for more than 128 tools, killing the
  whole task. It now keeps the first `MAX_WIRE_TOOLS` (128) declarations and
  logs a `warn` with the dropped count.

## Notes for the owner

- `model route rejected provider=kiro reason=unknown_provider` means a chat or
  bot is still pinned to provider id `kiro` while the running server has no
  provider with that id (renamed or removed). Re-pin the route to the
  provider's current id (e.g. `antigravity`) or re-add the provider.
- Provider cards show "N models in the saved list" whenever the live catalog
  was not fetched (provider not connected, or the last fetch failed); the
  saved list is the configured allowlist seeded from the preset. Connect the
  provider (and approve data egress) and press Test/refresh to get the live
  list (`model_source: "live"`).

## Verification

- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --locked` (new tests:
  `envelope_truncates_beyond_the_wire_tool_cap_instead_of_failing`,
  `kiro_and_devin_oauth_accounts_are_gated_on_the_raw_plan_oauth_flag`,
  updated `a_plan_sign_in_that_is_off_or_admin_only_says_so`).
