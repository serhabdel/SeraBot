# Release 0.49.0

Provider accounts, member sign-in, bot routes and failover. Every account,
secret, hold and route event is scoped to the acting user's id; secrets are
sealed per account and write-only (no API returns them, and the admin cannot
read them). Specification: `docs/specs/v0.49/spec.md`.

All tests use synthetic fixtures and wiremock. No live provider or account was
used, and nothing in this release is verified against a real provider.

## Highlights

- **Accounts** (`accounts.rs`, migration `0037_provider_accounts.sql`, `api/accounts.rs`): several accounts per provider per member (API key, PKCE or device sign-in), each with label, priority, weight, enable switch, status, identity and quota snapshot. The legacy single credential stays usable as the virtual `default` account. A duplicate fingerprint updates the existing account.
- **Sign-in framework** (`auth_flows/`): 10-minute flow map keyed by user and flow id, exact loopback redirects, pinned token hosts, no redirects, 64 KiB body cap, generic RFC 8628 device poller. Events: `provider.signin`.
- **Antigravity** (`auth_flows/google.rs`, `cloud_code.rs`): per-member Google sign-in with project discovery stored per account, thinking table, FNV session ids, thought-signature replay, schema sanitizer additions, catalog exclusions and quota reading. See `antigravity.md`.
- **Muse Code** (`auth_flows/muse.rs`, `wire/responses.rs`): server-side device sign-in and the Responses transport.
- **Anthropic** (`wire/anthropic.rs`): native Messages backend with multiple API-key accounts and extended thinking. Plan OAuth (`auth_flows/claude.rs`) is implemented and tested but off by default and admin-only (`plan_oauth = true`, owner decision O1); such accounts are refused in `provider_grants` and carry a risk banner.
- **OpenCode Go/Zen and Nous presets**.
- **Single-flight refresh** (`AccountRefresher`): one refresh per (user, provider, account); `invalid_grant` sets `needs_reauth`.
- **Errors and classifier** (`llm_failure.rs`): typed upstream errors and `classify_llm`; `gateway::is_ambiguous` uses an explicit flag.
- **Thinking catalog** (`thinking.rs`): per-provider levels; `/v1/models` exposes `thinking_levels` and `account_ids`.
- **Bot routes and failover** (`config.rs`, `route_chain.rs`, `model_router.rs`): a primary plus up to two fallbacks, account-first switching, holds in `provider_holds`, `route_events` and the `route_switched` event. Cross-provider failover is off unless the bot allows it, and re-runs egress and blocklist checks. Failover is skipped while an action outcome is unknown. See `model-routing.md`.
- **Desktop**: Providers page accounts list and Add account sheet, bot route editor, composer route chip, switch rows and "Retry on <fallback>".

## Upgrade notes

- Migration `0037` runs on first start and keeps existing credentials working.
- Bots with a legacy `model`/`thinking` load as a route with no fallbacks.
- `/v1/providers/{id}/oauth/*` is an alias of `signin/*`; `/v1/admin/providers/{id}/oauth/*` writes to the admin's own account and no longer returns `restart_required`.

## Not in this release

- Optional P19 (Mistral sign-in) was not implemented; Mistral stays API-key only.
- Live behaviour of Antigravity, Muse, Anthropic, OpenCode and Claude plan sign-in is unverified.
