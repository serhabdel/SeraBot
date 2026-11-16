# SeraBot v0.49 spec, with a roadmap for v0.50 to v0.52

Status: approved plan input for agents. It is built on the research notes in this folder: `serabot-providers.md`, `hielcowork-providers.md`, `openbot-providers.md`, `openbot-channels-marketplace.md` and `serabot-agents-marketplace.md`. Rules come from `AGENTS.md` and `docs/specs/multi-user-reconstruction.md`.
Baseline: `main` at 0.48.0. The newest migration is `0036_console_pages.sql`. Line numbers are from the tree as read and may drift.

## 0. Goal and "done"

**Goal.** Make AI providers multi-account and member-scoped with working sign-in. Give every bot a provider/model/thinking route with up to two fallbacks and automatic failover. Let the main bot manage and resume its subagents. Rebuild the chat for long, parallel work. Add real marketplace integrations and inbound channels. Every product boundary in AGENTS.md still holds.

**Done for v0.49** means all of the following:
- A member can add several accounts per provider: API keys for every kind; OAuth for Antigravity; the device flow for Muse. They do this from the desktop, with no restart and no admin involvement.
- `anthropic` works natively, with several API-key accounts. OpenCode Go and Zen work.
- A bot has a primary route and optional fallback 1 and 2, each `{provider, account?, model, thinking}`. A provider failure that classifies as switchable moves the turn to the next ready candidate, with no lost state.
- The composer shows the route and thinking level and lets you change them. Route switches appear in the chat.
- The fmt, clippy and test gate passes on Windows and WSL. Only synthetic fixtures are used, and nothing is claimed as live-verified.

## 1. Product boundary and non-goals (all releases)

- Do not add open signup, Docker, Postgres, a Node runtime, or a local chat LLM. Do not add a local-process (stdio) plugin runner, and do not wrap vendor CLIs. openbot's CLI-per-account model is **not** ported. SeraBot speaks the vendors' HTTP APIs natively.
- Do not import credential files from the server's home directories (HielCowork `CliSessions`). Do not port the OpenCode Free proxy. Do not spoof a User-Agent beyond what the existing Antigravity envelope already sends. Do not imitate the Copilot/VS Code headers, so Copilot sign-in is out.
- Secrets are write-only: no API returns one, and the admin never reads member credentials. Each account's secret is sealed under the acting user's DEK. AAD is `credential|<user>|<reserved id>`.
- All tool and outbound effects go through `gateway`. Failover never bypasses `scan_model_messages`, `scan_egress`, the blocklist or `data_egress_approved`.
- Never blindly replay a tool call whose outcome is unknown.
- Placeholder plugin hosts (`*.example.invalid`) and stdio plugins stay off. Plan-OAuth for Claude and Codex ships behind an admin flag that defaults to **off** (decision O1).
- New dependencies need a recorded justification in the PR. Prefer what is already locked: `sha2`, `jsonwebtoken`, `wiremock`, `chacha20poly1305`, and `hmac` (already in `Cargo.lock` as a transitive dependency).

## 2. Owner decisions (defaults the build uses until the owner overrides)

| Id | Question | Default in this spec |
|---|---|---|
| O1 | Claude.ai plan OAuth in a multi-user server. openbot's notes and HielCowork's audit both say Anthropic forbids it. | Build it behind `plan_oauth = false` (admin-only). The account can never be shared through `provider_grants`. Show a risk banner. Ship it OFF. |
| O2 | ChatGPT/Codex plan OAuth | Same as O1. Scheduled for v0.52. |
| O3 | Cross-provider automatic failover | Per bot `allow_cross_provider = false`. Each fallback provider must already be egress-approved, and the user must tick the consent box per fallback. |
| O4 | Concurrency above 2 | Raise the ceiling to 8 with a default of 4, but only after the load fixture in B7 passes. |
| O5 | Real marketplace hosts (Canva, Linear, Notion, ...) | Use a host only after a researcher has confirmed its live OAuth metadata and terms. Each host flip is its own small PR. |
| O6 | GitHub App registration (client id) | The client id goes in config (`[connectors.github] app_client_id`). Without it the connector shows "not configured". |
| O7 | WhatsApp | Use the Cloud API only, behind `[channels.whatsapp]` (default off). No unofficial web-client libraries. |
| O8 | Hermes | Treat it as Nous Portal, OpenAI-compatible, with API key. A researcher confirms the base URL first. No OAuth claim. |

## 3. v0.49: accounts, sign-in, routes, failover

### 3.1 Data model: migration `0037_provider_accounts.sql`

```sql
CREATE TABLE IF NOT EXISTS provider_accounts (
  user_id TEXT NOT NULL REFERENCES users(id), provider TEXT NOT NULL, account_id TEXT NOT NULL,
  label TEXT NOT NULL,
  auth_kind TEXT NOT NULL CHECK (auth_kind IN ('api_key','oauth','device','minted')),
  fingerprint TEXT NOT NULL,            -- sha256("<provider>:<stable identity>"), never of a token
  identity_json TEXT NOT NULL DEFAULT '{}', -- non-secret: email, plan, org, project_id, tier
  status TEXT NOT NULL DEFAULT 'ok'
    CHECK (status IN ('ok','needs_reauth','rate_limited','quota_exhausted','disabled')),
  priority INTEGER NOT NULL DEFAULT 0, weight INTEGER NOT NULL DEFAULT 1,
  cooldown_until TEXT, quota_json TEXT, last_error TEXT, last_used_at TEXT,
  session_generation INTEGER NOT NULL DEFAULT 0,  -- Antigravity 1M-token session wall
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
  PRIMARY KEY (user_id, provider, account_id));
CREATE UNIQUE INDEX IF NOT EXISTS idx_provider_accounts_fp ON provider_accounts(user_id, provider, fingerprint);
CREATE TABLE IF NOT EXISTS route_events (
  id TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id),
  conversation_id TEXT, task_id TEXT, bot TEXT,
  from_provider TEXT, from_model TEXT, from_account TEXT,
  to_provider TEXT, to_model TEXT, to_account TEXT,
  reason TEXT NOT NULL, created_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_route_events_conv ON route_events(user_id, conversation_id, created_at);
CREATE TABLE IF NOT EXISTS provider_holds (
  user_id TEXT NOT NULL REFERENCES users(id), provider TEXT NOT NULL, account_id TEXT NOT NULL,
  model TEXT NOT NULL, kind TEXT NOT NULL, until TEXT NOT NULL, reason TEXT NOT NULL,
  PRIMARY KEY (user_id, provider, account_id, model, kind));
```

How these tables fit with the rest of the store:
- **Account secrets.** They live in `user_credentials` under the reserved id `acct:<provider>:<account_id12>`. This is at most 64 characters and follows the `vault:` pattern. Also add the prefix to the exclusion list in `providers_for` (`user_secrets.rs:316`).
- **Accounts per provider.** A user can have at most 10 per provider. Cap 10, per openbot.
- **Legacy rows.** An existing `(user, provider)` row reads as a virtual account `default`. The first edit moves it into the new scheme, inside one transaction.
- **Muse.** The stored `ProviderCredential` carries the minted key as `api_key` and the Meta OIDC token as `refresh_token`.

`BotConfig` (`config.rs:1892`) gets these serde-default fields. The rows need no SQL change.

```rust
pub struct BotRoute { provider: String, model: String, thinking: Option<String>, account: Option<String> }
pub struct FailoverPolicy { enabled: bool /*true*/, switch_account_first: bool /*true*/,
                            max_switches: u8 /*3*/, allow_cross_provider: bool /*false*/ }
// BotConfig: route: Option<BotRoute>, fallbacks: Vec<BotRoute> (<=2), failover: FailoverPolicy
```

- `ModelSelection` (`model_router.rs:47`) gains `account: Option<String>`.
- The pin fingerprint covers the account and the whole chain.
- Resolution order: conversation selection, then bot `route`, then the user's default route, then the legacy bot `model`, then the server default.

### 3.2 API (member-scoped; existing routes keep working on the `default` account)

| Method | Path | Notes |
|---|---|---|
| GET | `/v1/providers` | Adds `accounts[]` with `{id,label,auth_kind,status,priority,identity(email/plan masked),quota,cooldown_until}`. Never returns secrets. |
| GET/POST | `/v1/providers/{id}/accounts` | POST `{label, api_key}` validates the key with one catalog call and dedupes by fingerprint. |
| PATCH/DELETE | `/v1/providers/{id}/accounts/{aid}` | Changes label, priority, weight or `disabled`. DELETE refuses while a bot pins the account, unless `?force=true` (the pin then becomes the pool). |
| POST | `/v1/providers/{id}/accounts/{aid}/test` | Returns a stable status code. |
| POST | `/v1/providers/{id}/signin/start` | Takes `{flow:"pkce"\|"device", redirect_uri?, challenge?, state?}` and returns `{flow_id,url,user_code?,interval?,expires_in}`. |
| POST | `/v1/providers/{id}/signin/complete` | Takes `{flow_id,code,redirect_uri}` and returns `{account_id,created}`. |
| GET | `/v1/providers/{id}/signin/{flow_id}` | Returns `pending\|ok\|denied\|expired`. |
| GET | `/v1/providers/{id}/accounts/{aid}/quota` | Returns the cached snapshot. `?refresh=1` reads it again, at most once per 4 minutes. |
| GET/PUT | `/v1/bots/{id}/route` | Takes `{route,fallbacks,failover}`. Every field is validated against the live catalog, the thinking levels, and the egress approval of each fallback provider. |
| GET | `/v1/route-events?conversation=` | Feeds the switch rows in the chat. |

Old `/v1/providers/{id}/oauth/*` becomes an alias of `signin/*`. `/v1/admin/providers/{id}/oauth/*` now writes to the admin's own account and drops `restart_required`.
Audit events: `provider.account_added|signed_in|reauthed|removed|disabled`, `model.failover`, `model.account_cooldown`.

### 3.3 Work items (ordered)

Roles: **C** coder, **R** researcher, **D** debugger, **V** reviewer. "Dep" lists the items that must land first. "Par" lists the items it can run alongside.

**P1. Typed provider errors and classifier** (C). Dep: none.
- Files: new `crates/serabot/src/llm_failure.rs`. Change `llm.rs` (error paths around :268, :351, :404, :560), `cloud_code.rs` (`classify_cloud`), `error.rs`, and `gateway.rs` (`is_ambiguous`).
- Return `SeraError::Upstream{status, code, message}` from model calls and keep `retry_after`.
- Add `classify_llm(&SeraError) -> Result<FailKind, NoSwitch>`:
  - `FailKind` covers RateLimited, Overloaded, ModelUnavailable, QuotaExhausted{resets_at}, CredentialsRejected, AccountForbidden, ProviderDown, Transport, ContextTooLong.
  - `NoSwitch` covers PolicyBlocked, EgressDenied, Cancelled, InvalidRequest, Content.
- Replace the string matching in `gateway::is_ambiguous` with an `ambiguous` flag.
- Tests: a table of (kind, status, body code) to class. It must cover OpenAI `insufficient_quota`, Google `RESOURCE_EXHAUSTED`, Anthropic `overloaded_error`/529, and Cloud Code 403/404/429. Also test the ambiguity cases.

**P2. Account store** (C). Dep: none. Par: P1.
- Files: migration `0037_provider_accounts.sql`; new `accounts.rs`; `user_secrets.rs` (`set_account`, `get_account`, `delete_account`, reserved prefix); `store.rs` (migration list).
- Every method takes `user_id` first. Include the legacy `default` virtual account.
- Tests:
  - user B cannot list, read or test A's accounts;
  - an AAD blob moved between accounts fails to open;
  - a duplicate fingerprint updates the existing account in place;
  - migrating from 0036 keeps old credentials usable.

**P3. Accounts API and provider listing** (C). Dep: P2.
- Files: `api/connectors.rs` (`list_providers` at :17, `set_provider_key` at :503), new `api/accounts.rs`, `api.rs` (routes), `providers.rs` (`UserProviderCredentials::client` at :1021 becomes account-aware, with `open_provider_client` at :1061).
- Tests in the style of `api/tests.rs`, one per route: validation errors, 404 for another user's account, and no secret in any response body.

**P4. Sign-in flow framework** (C). Dep: P2.
- Files: new `auth_flows/{mod,pkce,device}.rs`, moving PKCE helpers out of `cloud_code.rs`. Add `api/accounts.rs` `signin_*`.
- Keep a flow map with a 10-minute TTL, keyed by `(user_id, flow_id)` and bound to the provider and redirect URI. The redirect must be an exact loopback with a fixed port per provider.
- Pin token hosts by host and path, disable redirects, and cap bodies at 64 KiB.
- The device poller is generic RFC 8628: `slow_down` adds 5 s up to 30 s; `authorization_pending` and a 400 without a code continue; deny and expiry are terminal. Bounded at 900 s.
- Publish a `provider.signin` event.
- Tests: wiremock IdP for PKCE; every device state; completion from a different user is refused; an expired flow is refused.

**P5. Antigravity member sign-in** (C). Dep: P4.
- Files: new `auth_flows/google.rs`, `cloud_code.rs` (`discover_project` around :1355), `api/admin.rs:205-370`, `providers.rs:1140` (`import_age_provider_credentials`).
- Follow HielCowork:
  - Scopes include no `openid`. Use `prompt=consent` and `access_type=offline`.
  - The client secret comes from the age store.
  - Look up the email via userinfo.
  - Run project discovery: `loadCodeAssist`, then `onboardUser` up to 3 times, 3 s apart, against daily then prod. Store `project_id` and `tier` in `identity_json` per account. Rediscover lazily when missing. Never use the profile project for a member account.
- Tests: wiremock covers token, userinfo and loadCodeAssist/onboard. Two accounts get two project ids. A missing project means the envelope omits `project`.

**P6a. Antigravity wire parity, part 1** (C). Dep: P5, P8.
- File: `cloud_code.rs` (`generation_config` at :326 and the envelope builder).
- Add a pure function `antigravity_thinking(model, level, max_out)` following the table in `hielcowork-providers.md` §8:
  - Gemini 3/4 level models: off becomes `thinkingBudget:0`, max maps to `high`. An id that already ends in a level suffix gets no thinking config.
  - Claude: budgets of 4000/10000/24000/32000, clamped to [1024, maxOut-1024].
- `maxOutputTokens` caps are 64000 for Claude and 65536 otherwise.
- `sessionId` = FNV-1a 64 over `"{project}:{conversation}[:genN]"`, folded with `% 9e18` and prefixed with `-`. Store the generation in `provider_accounts.session_generation`. On the 400 "exceeds the maximum number of tokens", bump it once and retry.
- Strip a trailing `model` turn for every `claude*` id. Set `is_error` from the tool's real outcome.
- Keep `anthropic-beta` interleaved-thinking only if a fixture shows it is required (HielCowork's code does not send it).
- Tests: known FNV vectors and the thinking table.

**P6b. Antigravity wire parity, part 2** (C). Dep: P6a.
- Files: `cloud_code.rs`; a new column `reasoning_signature` on stored assistant tool calls, kept in turn state as bounded per-(user, conversation) LRU JSON. No process-global map.
- Capture and replay `thoughtSignature`, falling back to the sentinel. Parse `thought:true` parts as reasoning.
- On `MALFORMED_FUNCTION_CALL`, resample up to 3 times.
- Map tool names `web_search` and `search_web` to `agy_*` and back.
- Sanitizer additions: a union `type` array becomes `nullable`; `const` becomes `enum`; `required` is filtered to existing properties.
- Tests: a signature survives a round trip; property test that the sanitizer output contains only allow-listed keys.

**P6c. Antigravity catalog and quota** (C). Dep: P6a.
- Apply the exclusion lists (tab, image, audio, deprecated, legacy `*-4-6`). Carry `maxTokens`/`maxOutputTokens` into capabilities.
- Quota: `retrieveUserQuotaSummary`, with `fetchAvailableModels.quotaInfo` as the fallback, written to `quota_json`. Any failure means "no reading", not an error.
- Make the UA version config-overridable (`[llm.providers.<id>] client_version`). No updater fetch.

**P7a. Muse member device sign-in** (C). Dep: P4.
- File: `providers.rs` (`login_muse_code` at :767, mint).
- Run the sign-in server-side on the account store.
- `onboard:true` is sent only on the interactive login mint. Retry the mint on 429/5xx after 5 s and then 10 s. A failed mint stores nothing.
- Send `x-api-version: 1.0.0` on every Muse call.
- On a 401, re-mint without `onboard` only when a Meta token exists; otherwise set `needs_reauth`.
- Quota: `POST /muse-code/key {}`, parsing `subs_usage` resets given as seconds, milliseconds or ISO. `is_subs_active=false` and `require_payment` are terminal states.
- Tests: mirror `MuseProviderTests.cs` with synthetic fixtures.

**P7b. Responses transport** (C). Dep: P1. Par: P7a.
- New `wire/responses.rs`: `POST {base}/responses` over SSE with `store:false` and `instructions`.
- Translate tools to function_call / function_call_output items and drop orphaned outputs.
- Thinking: send `reasoning:{effort,summary:"auto"}` and clamp max to high for Muse. On a 400 that mentions the reasoning, `max_output_tokens`, a recursive schema or `web_search`, strip the offending part and retry once.
- Add a shared `schema.rs` flattener (cycles become string; arrays always get `items`).
- Route `muse-code` through this transport. Codex reuses it in v0.52.
- Tests: SSE fixture parsing, orphan pruning, each 400-strip case.

**P8. Thinking catalog** (C). Dep: none. Par: P1, P2.
- File: `thinking.rs` (`levels_for`, `wire_fields`).
- Add arms for `anthropic` (budget), `google-antigravity`, `muse-code` (off/low/medium/high) and `opencode-*` (pass `reasoning_effort` through only when the catalog tags say reasoning).
- `/v1/models` entries gain `thinking_levels` and `account_ids`.
- On a model switch, keep the level when the new model supports it; otherwise use the model's default.

**P9a. Anthropic Messages backend: core** (C). Dep: P1, P3.
- New `wire/anthropic.rs` and kind `anthropic` in `BUILTINS` (`providers.rs:108`).
- `POST api.anthropic.com/v1/messages` with `x-api-key` and `anthropic-version: 2023-06-01`, streaming over SSE.
- Map blocks for text, tool_use and tool_result. `max_tokens` is required.
- Error map: authentication, permission, not_found, 413, rate_limit with `retry-after`, 529 overloaded.
- The catalog is `GET /v1/models` with cursor paging, pinned in `OFFICIAL_CATALOGS` (`providers.rs:45`).
- Tests: wiremock stream, tool round trip, 429 and 529 classification.

**P9b. Anthropic thinking** (C). Dep: P9a, P8.
- Send `thinking:{type:enabled,budget_tokens}`, with defaults low 4k, medium 10k, high 24k, xhigh 32k, clamped below `max_tokens`. Omit temperature while thinking is on.
- Replay signed `thinking` and `redacted_thinking` blocks byte-identical. Strip them when a route crosses providers.
- An R step first checks current Anthropic docs for an `effort`-style parameter and model capability fields.
- Add the desktop preset and the `docs/providers.md` rows.

**P10. OpenCode Go/Zen and the Nous preset** (C, after an R check of the Nous docs). Dep: P3.
- Kinds `opencode-go` (`https://opencode.ai/zen/go/v1`) and `opencode-zen` (`https://opencode.ai/zen/v1`).
- For Go, attach `x-opencode-session: ses_<12hex><14 base62>` to every request, derived from the conversation id so it is stable across retries.
- Go usage: `GET .../go/v1/usage`.
- Nous: preset `openai-compatible` with the documented base URL.
- Tests: assert the header on every request; Zen requests carry no header.

**P11. Single-flight refresh** (C). Dep: P2.
- New `accounts.rs::AccountRefresher`: a per-`(user, provider, account)` `tokio::sync::Mutex`. Inside the lock, re-read the row, compare `generation`, refresh, write back, then release.
- Refresh proactively within 5 min of expiry. `invalid_grant` sets `needs_reauth` and keeps the row.
- Change `llm.rs::refresh_oauth` (:404) to call it.
- Test: 8 concurrent turns perform exactly one refresh, and a stale-generation write is refused.

**P12. Bot routes** (C). Dep: P3, P8.
- Files: `config.rs` (BotRoute/FailoverPolicy, `validate_route`), `fleet.rs` (`rows_to_bots` maps legacy `model`/`thinking` into `route`), `api/bots.rs` (`update_bot` at :520 plus `/route`; GET view at :416-457 shows the real fallbacks), `runtime/models.rs` (`bot_route_chain`), `delegate.rs` (the specialist's chain head wins over `model_class`).
- Templates under `bots/*.toml` may name a `provider`. It resolves per user, or falls back to the user's default route.
- Tests:
  - more than 2 fallbacks, or duplicates, are rejected;
  - an unsupported thinking level is rejected;
  - a fallback provider without egress approval is rejected;
  - legacy rows still load.

**P13. Failover engine** (C, with D if flaky). Dep: P1, P11, P12.
- New `route_chain.rs`. Change `model_router.rs` (`PinnedChain`, `complete_with_failover`) and `runtime/turn_loop.rs` (:635 and :940).
- Candidate order:
  1. Accounts of the primary provider that are not disabled, not `needs_reauth`, not cooling and not quota-held. Quota-held means a snapshot at most 10 min old shows 0 remaining with a future reset.
  2. Within those: priority descending, then weighted, with conversation affinity first.
  3. Then fallback 1, then fallback 2.
- `switch_account_first`; `max_switches` 3.
- Cross-provider rules:
  - only when `allow_cross_provider` is set;
  - strip provider-bound reasoning;
  - re-validate the thinking level and drop it if unsupported;
  - re-run the egress and blocklist scans;
  - require egress approval.
- Record holds in `provider_holds`: RateLimited until `retry_after`; QuotaExhausted until `resets_at`; a model failure for 10 min. Write a `route_events` row and emit a `route_switched` event.
- Skip failover entirely when the turn has a pending action that is `dispatching` or `outcome_unknown`.
- When every candidate fails, return `Rejected{code:"all_routes_failed", reasons}`.
- Update `docs/model-routing.md`, which still says "no cross-provider fallback".
- Tests with the fake LLM in `runtime/tests.rs`:
  - a 429 moves to the next account;
  - an exhausted quota creates a hold and the next fallback runs;
  - a 401 refreshes once, then fails over;
  - a policy block never switches;
  - cross-provider failover is off by default;
  - holds are per user;
  - editing the chain invalidates queued pins.

**P14. Desktop Providers page** (C). Dep: P3, P4.
- Files: `ui/providers.rs` (fix :183 and :560, which only handle `pkce`/openrouter), `provider_presets.rs`, `provider_view.rs`, `api.rs` (:4969-5000), `oauth.rs`.
- The page shows an accounts list: label, email/plan chip, status chip, quota meter, priority order, enable switch, Test, Remove.
- An "Add account" sheet offers a key, a PKCE sign-in (loopback 51121 for Antigravity) or the device dialog (code, Open page, countdown, polling).
- New presets: anthropic, opencode-go, opencode-zen, nous. Add a `risk` field for plan sign-ins.
- Tests: preset table, device dialog state machine.

**P15. Desktop bot route editor** (C). Dep: P12, P14.
- File: `ui_bots.rs`.
- A "Model route" block with Primary and Fallback 1 and 2. Each has pickers for provider, account (or "any"), model and thinking (limited to `thinking_levels`).
- An auto-switch toggle, plus a cross-provider consent line per fallback.
- Inline validation errors, and the effective route with its last switch.

**P16. Composer route chip and switch rows** (C). Dep: P13, P14.
- Files: `chat_compose.rs`, `chat_feed.rs`, `chat_failure.rs`.
- The chip reads `provider/account · model · thinking`. Its combined picker (provider, then account, then model, with search) uses `picker_rows`. Thinking is a segmented control.
- A row "Switched to X (reason)" from `route_events`. A failed-turn card offers "Retry on <fallback>".

**P17. Claude plan OAuth, flag-gated** (C, then V). Dep: P9a, P4, P11.
- New `auth_flows/claude.rs`. Only active when `[llm.providers.<id>] plan_oauth = true` and the actor is admin.
- PKCE through desktop loopback 54545, or paste-code. The client id comes from config and is never committed.
- Fingerprint is `sha256("claude:account:<uuid>")`. Refuse a login that has no stable id.
- The bearer plus `anthropic-beta: oauth-2025-04-20` goes over the P9 transport. Refresh tokens rotate, so P11 is mandatory.
- The account is refused in `provider_grants` and gets a risk banner. Docs state it is off by default (O1).
- Tests: wiremock covers exchange, refresh rotation and duplicate login.

**P18. Release 0.49.0** (C, then V). Dep: all of the above.
- Bump `Cargo.toml` to 0.49.0.
- Update `docs/providers.md` (rewrite "Anthropic: Not added"; sign-in is per member), `docs/antigravity.md` and `docs/model-routing.md`, plus the release notes and `tasks/todo.md`.

**Optional P19. Mistral sign-in** (C). Dep: P4.
- The flow is vibe-style and returns a plain API key (HielCowork §5). It is clean and low risk.

### 3.4 Acceptance for v0.49

- `cargo fmt --check`, `clippy -D warnings` and `cargo test --workspace --locked` pass. So does `scripts\cargo-win.bat test --workspace --locked`.
- Isolation suite: every new store method and route is proven one-directional between two synthetic users.
- Manual check (desktop against a local server with wiremock-backed providers):
  1. Add two API keys to one provider.
  2. Set a bot to primary A plus fallback B.
  3. Force a 429 on A. The chat shows a switch row and the answer arrives.
- No live account is used and none is claimed.

### 3.5 Risks for v0.49

- Antigravity and Muse are vendor-internal endpoints and can break or ban without notice. Show `needs_reauth` and degrade gracefully; never call them "official".
- Rotating refresh tokens make a race destructive. P11 must land before P17 or any rotating provider.
- `cloud_code.rs` (1884 lines) is touched by P5, P6a, P6b and P6c. Keep them sequential to avoid merge conflicts.
- If P13 is wrong, a turn could loop or reach a provider the user never approved. Mitigations: `max_switches`, egress checks per hop, and the test table.

## 4. v0.50: orchestration, subagents, chat overhaul

### 4.1 Data model: migration `0038_handoffs.sql`

```sql
CREATE TABLE IF NOT EXISTS handoffs (
  id TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id),
  parent_task_id TEXT NOT NULL, parent_turn_id TEXT, child_task_id TEXT NOT NULL,
  to_bot TEXT NOT NULL, key_hash TEXT NOT NULL,
  status TEXT NOT NULL,          -- queued|running|completed|failed_resumable|failed|resumed
  outcome_code TEXT, effects_unknown INTEGER NOT NULL DEFAULT 0,
  resume_count INTEGER NOT NULL DEFAULT 0, route_json TEXT,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
  UNIQUE (user_id, parent_task_id, key_hash));
```

`tasks.status` has no CHECK constraint (`0009_user_scoping.sql:70`). New values `waiting_model` and `needs_review` therefore need no table rebuild.

### 4.2 API and tools

API:
- `GET /v1/handoffs?conversation_id=` returns the subagent tree: status, route, outcome code, resume count.
- `POST /v1/handoffs/{id}/retry {route_override?}` is the owner action.
- `GET /v1/bots` gains the route, the current fallback in use, and the hold state.

Gateway tools for the orchestrator only (`role == Orchestrator`), filtered like `DELEGATE_TOOL` in `turn_loop.rs::advance`:

| Tool | Approval |
|---|---|
| `bot_list`, `bot_read`, `model_list` (accounts as `{id,label,state}` only), `handoff_status` | none (read, audited) |
| `bot_set_route` (route, fallbacks, failover, thinking) | required; no standing grant; never YOLO |
| `bot_set_enabled`, `bot_set_delegates` | required |
| `subagent_retry {handoff_id, route_override?}` | none when the route stays within the bot's approved chain; required when it names a new provider |

Guards, adapted from openbot `widensAgentSettings`:
- A bot never widens `tools`, `skills`, `require_approval`, YOLO or egress consent.
- A bot never touches `main`'s role.
- Targets are limited to `main.delegates` plus main itself (profile only).
- A route change needs an idle target: no running task and no awaiting approval.
- Refuse the call when the turn source is a handoff, team or routine. This is the prompt-injection guard.
- Accept no user id from the model.

### 4.3 Work items

**B1. Shared fleet mutation path** (C). Dep: P12.
- New `fleet_mutation.rs`, used by REST `update_bot` and by the tools. It runs validation, `scan_owner_output`, catalog checks and the audit `bot.profile_changed` with before and after values.
- Test: the REST and tool paths produce identical rows and audit.

**B2. Bot management tools** (C). Dep: B1.
- New `tools_bots.rs`. Register in `tools.rs` and add to `known_tool_names()`. Update `bots/main.toml`, `prompts/main.txt` and `docs/approvals.md`.
- Tests:
  - A's main cannot read or change B's bots;
  - widening is refused;
  - a call from a handoff turn is refused;
  - the approval binds the canonical args.

**B3. Handoff ledger** (C). Dep: none (needs migration 0038). Par: B1.
- `delegate.rs`: `key_hash = sha256(parent_turn_id, to_bot, objective)`, so a duplicate call returns the existing child. Write the row in the same transaction as the enqueue.
- Add the `handoff_status` tool and the `GET /v1/handoffs` route.
- Count handoff and resume tasks against `child_daily_limit` (default 500). Today they bypass the quota.

**B4. Make `tasks.uncertain` real** (C, then D). Dep: P1.
- Add `store.mark_task_uncertain(user, id)`. Call it when an `external_effect` tool starts dispatch and the process fails or loses its lease.
- `recover_expired_tasks` and `fail_task` do not replay uncertain tasks; they mark them `needs_review`.
- Tests: kill the process mid-dispatch and check there is no replay.

**B5. Parking in `waiting_model`** (C). Dep: P13.
- Files: `runtime/worker.rs`, `store.rs`.
- When no candidate is ready, `TurnOutcome::Waiting{until}` releases the lease and permit and sets `run_after` to the earliest hold expiry, or 60 s. Re-entry resumes from the stored `TurnState`.
- The chat shows one line: "Waiting for a model".

**B6. Subagent auto-resume** (C). Dep: B3, B4, B5.
- When a child exhausts its chain, the handoff becomes `failed_resumable {reason_codes, effects_unknown}`.
- Main gets one synthetic follow-up turn: "subagent X failed: codes; options".
- Main calls `subagent_retry` with the next unused account or fallback, or with another approved provider. The cap is `resume_count <= 2`.
- Refuse when `effects_unknown`; main must verify with a read tool or ask the owner.
- `route_events` records each resume.
- Tests: a retry cap with no loop, refusal on unknown effects, and the resumed child's answer posted once.

**B7. Concurrency and fairness** (C, then a D load test). Dep: none.
- Files: `config.rs:257` (lift the `<= 2` rule to a ceiling of 8), `store.rs::claim_task` (`max_turns_per_user`, default 2, plus a per-user round-robin tiebreak), and the `max_active_children` default of 4 with child priority below owner turns.
- Add a semaphore per `(provider, account)` (`max_inflight_per_account`, default 3) in `ModelRouter`.
- Add a load fixture script under `scripts/`.
- Raise the default to 4 only when the fixture passes (O4).
- Test: user A's 16 children do not block user B's turn.

**B8. Long sessions** (C). Dep: P1.
- `ContextTooLong` triggers compaction and a retry on the same route.
- Add a rolling conversation summary at 75% of the model window; stop after 2 compactions that do not help.
- "Continue after limit" reuses `resume_turn` with a payload extension.
- Tests: a compaction trigger, and a summary row stored.

**B9. Events for the UI** (C). Dep: B3, P13.
- Add `handoff.started|completed|failed`, `model.failover`, `provider.hold_set|cleared` and `task.waiting_model` to `runtime/activity.rs` and the SSE stream.
- Payloads are small and carry ids, so the desktop can coalesce them.

Chat overhaul (desktop). Design principles from apple-design:
- every animation can be interrupted, using CSS transitions rather than keyframes for interactive state;
- springs use the existing `--spring` and `--ease` tokens (`assets/app.css:24-26`);
- input gets an immediate response under 100 ms with optimistic UI;
- `prefers-reduced-motion` turns every movement into an opacity change;
- one primary action per surface;
- clear focus rings, keyboard parity, and no layout shift while streaming.

**C1. Virtualized feed** (C). Dep: none. Par: B-items.
- Files: `chat_feed.rs`, `chat_scroll.rs`, `ui/chat.rs` (2369 lines; extract `ui/chat_feed_view.rs`), `session.rs` (`load_older_messages` at :1100).
- Render only the viewport plus overscan. Keep the scroll anchor while older pages load. Cache parsed markdown per message hash in `ui/markdown.rs`. Collapse completed tool blocks.
- Test: 5,000 synthetic messages keep the rendered node count bounded, and the anchor stays stable.

**C2. Event coalescing** (C). Dep: B9.
- Files: `events.rs`, `session.rs`.
- Batch SSE updates into 16 ms frames, append tokens per frame, and reduce per-subagent progress to counters.
- Test: 50 parallel children produce at most 60 store writes per second.

**C3. Runs sidebar** (C). Dep: B3, B9.
- New `ui/runs.rs`, mounted from `ui/shell.rs` and `ui/conversations.rs`.
- Shows the subagent tree per conversation with status chips: running, waiting approval, waiting model, failed resumable, resumed on X.
- Parallel runs collapse with counts. Clicking a run opens its thread. Includes a hold badge with the reset time, and per-run Stop and Retry.

**C4. Composer for heavy work** (C). Dep: P16.
- File: `chat_compose.rs`.
- A queue panel for queued messages (edit, steer, delete, reorder, with an edit hold) and `@bot` mentions that route to a delegate.
- A Stop/interrupt button that is visible while any run is active. Keyboard shortcuts with a help sheet. Drafts kept per conversation.

**C5. Failure and wait UX** (C). Dep: B5, B6, P16.
- Files: `chat_failure.rs`, `chat_cards.rs`.
- A failed card with reason chips and "Retry on fallback", "Retry on…" and "Ask main" actions. A waiting-model banner with a countdown. An inline compaction summary row.

**C6. Motion and accessibility pass** (C, then V). Dep: C1 to C5.
- File: `assets/app.css` (the existing reduced-motion blocks at :2447 and others).
- Audit every new transition for interruptibility and reduced motion. Add a focus-visible audit and contrast checks.
- Do one screenshot review of the chat, runs sidebar and composer in light and dark.

**C7. Release 0.50.0** (C, then V).
- Docs: `docs/orchestration.md` (stale: says main only has `delegate`) and the release notes.

### 4.4 Acceptance and risks for v0.50

Acceptance:
- Fixture: main fans out to 6 specialists, 2 children hit 429s and resume on their fallbacks, and the tree shows all states.
- A second synthetic user's turn starts within one claim cycle.
- No replay happens when `effects_unknown`.

Risks:
- SQLite write contention at 4 or more workers. B7 is gated on the load fixture.
- `ui/chat.rs` and `session.rs` are large files. C1 and C2 must not run in parallel with C4 on the same file; pull out the module boundaries first.

## 5. v0.51: marketplace (native GitHub, Canva, OAuth for MCP)

### 5.1 Data model: migration `0039_marketplace_accounts.sql`

- `marketplace_accounts(user_id, slug, account_id, label, state CHECK IN ('needs_setup','needs_sign_in','ready','unreachable'), last_error, last_ok_at, created_at, PRIMARY KEY(user_id, slug, account_id))`.
- Tokens are sealed under `marketplace.<slug>.<account>.oauth`. The record holds access, refresh, expiry, scope and the client registration.
- `github_connector(user_id PK, login, gh_user_id, state, access_expires_at, refresh_expires_at, updated_at)`. Tokens are sealed under `connector:github`.

### 5.2 Work items

**M1. Manifest schema v2** (C). Dep: none.
- File: `marketplace.rs`.
- Fields:
  - `auth.kind`: oauth, api-key or none;
  - `oauth{resourceMetadataUrl?, scopes, clientId?, usesCimd?}`;
  - `header` with a deny list (host, content-length, cookie) and `scheme` (bearer or raw);
  - multi-field auth;
  - `tools{allow,deny,readOnly}`;
  - `accounts.multiple`.
- Keep the v1 loader.
- Tests: v1 still loads; a bad header is rejected.

**M2. MCP OAuth** (C). Dep: M1.
- New `mcp_oauth.rs`, generalising `elevenlabs_oauth.rs` and the `onedrive.rs` PKCE flow.
- Discovery via RFC 9728 and then 8414/OIDC.
- Client identity: CIMD first, using the existing `/.well-known/serabot-oauth-client.json` pattern; then DCR (RFC 7591); then a configured client id.
- PKCE S256 plus an RFC 8707 `resource`. Bounds: 64 KiB and 20 s, and every endpoint must resolve to a public IP (`plugin_runtime::resolve_checked`).
- Outcome table:
  - `invalid_grant`: drop the tokens and set needs_sign_in;
  - `invalid_client`: also drop the registration;
  - network or 5xx: keep everything.
- Best-effort RFC 7009 revoke on uninstall.
- Tests: a wiremock authorization server covering each outcome, and an SSRF refusal.

**M3. API and health** (C). Dep: M2.
- File: `api/marketplace.rs`.
- Routes: `POST .../plugins/{slug}/oauth/start|complete`, `GET/DELETE .../plugins/{slug}/accounts`. `GET .../plugins/{slug}` returns the state.
- `plugin_runtime.rs::specs_for` records failures instead of swallowing them. Main gets a one-line note such as "Canva needs sign-in".

**M4. Runtime auth and annotations** (C). Dep: M1, M3.
- File: `plugin_runtime.rs`.
- Choose `McpAuth` from the manifest; inject the OAuth bearer at call time (never into the prompt); on a 401, refresh once and retry.
- Use every `http` app, not only the first.
- `readOnlyHint=true` tools skip approval but are still audited and scanned. The owner can override per tool. Destructive hints never get a standing grant.

**M5. GitHub connector, native** (C, then V). Dep: P4 (device poller), M4.
- New `github_connector.rs` and `api/connectors.rs` routes.
- GitHub App device flow (`github.com/login/device/code`, `/login/oauth/access_token`). The client id comes from config (O6) and no secret is needed.
- Refresh 10 min early, single-flight. A rejected refresh sets `expired` but keeps the login.
- Tools come from the hosted MCP at `https://api.githubcopilot.com/mcp/` through the M4 bearer-injection path. Write actions need approval bound to repo and args.
- Keep `token_connectors/github.rs` as the PAT fallback and align the tool naming.
- Disconnect opens the app's connections page.

**M6. Real hosts, one PR each** (R verifies, then C). Dep: M2 to M4. Par: all the manifests.
- Order: Canva (`mcp.canva.com/mcp`), Linear, Notion, Sentry, Vercel, Stripe (read tools only), PostHog, Context7, Atlassian.
- Each is a manifest under `plugins/` plus an egress host and a fixture test.
- A manifest still on `*.example.invalid` stays blocked. Nothing is called "working" until the owner runs it.

**M7. Desktop marketplace** (C). Dep: M3.
- Files: `ui_marketplace.rs`, `ui_integrations.rs`, `marketplace.rs`, `oauth.rs`.
- A Sign-in button, an accounts list, state badges, per-tool toggles and a GitHub card.

**M8. `suggest_marketplace_app` tool** (C). Dep: M3.
- Renders an install card in the chat. It is read-only.

**M9. Docs and release 0.51.0** (C, then V).
- `docs/marketplace.md` is stale (says the runtime is absent). Also update `docs/connectors.md`.

Acceptance for v0.51:
- A wiremock MCP server with OAuth goes through install, sign-in, list tools and call a tool.
- A read tool runs without approval and a write tool needs approval.
- Sign-out triggers revoke.
- The GitHub device flow fixture covers expiry and refresh.

Risks for v0.51:
- OAuth varies by host (DCR, CIMD or fixed client), so each host needs a live owner test.
- The hosted GitHub MCP terms need checking by R.

## 6. v0.52: channels and Codex

### 6.1 Data model: migration `0040_channels.sql`

- `channel_links(user_id, channel, external_id_hash, external_label_sealed, linked_at, revoked_at, PRIMARY KEY(channel, external_id_hash))`.
- `channel_threads(user_id, channel, thread_key, conversation_id, created_at)`.
- `channel_inbound(channel, event_id PK, received_at)` for dedupe, pruned after 7 days.

### 6.2 Work items

**H1. Channel framework** (C). Dep: none.
- New `channels/mod.rs` with a trait providing `verify_request`, `parse_event -> InboundMessage{channel, external_user, thread_key, text, attachments}` and `send`.
- One-use link codes: random, stored hashed, valid 15 min, bound to the acting user and created from the authenticated desktop. Unlinked senders get one fixed refusal, so this introduces no self-signup.
- Inbound text is framed as untrusted data. Every send goes through `gateway`: blocklist, `scan_egress`, audit.

**H2. Slack** (C). Dep: H1.
- New `channels/slack.rs` with the route `POST /v1/channels/slack/events`.
- Verify HMAC-SHA256 over `v0:ts:body` with a 5-min skew limit, and answer `url_verification`.
- One SeraBot Slack app. The bot token and signing secret live in the age store (admin).
- Scopes: app_mentions:read, channels:history, chat:write, groups:history, im:history, im:read, im:write, reactions:write, users:read.
- Thread maps to conversation. Reactions show state (eyes, check, cross). `stop`/`cancel` keywords.
- Approve/Deny buttons only the requester can press, mapped to the existing approval binding. Keep the `@channel`/`@here` refusal from `token_connectors/slack.rs`.

**H3. Telegram upgrades** (C). Dep: H1.
- File: `telegram.rs`.
- Conversation keys for reply chains and forum topics, inline Approve/Deny with `answerCallbackQuery`, a 20 MB file cap, and the orchestrator pattern (main answers, delegates the rest).

**H4. WhatsApp Cloud API** (C). Dep: H1. Behind O7 and `[channels.whatsapp]`, default off.
- Webhook verify (GET) and `X-Hub-Signature-256` (POST).
- Per-number link codes. Phone numbers are stored hashed for lookup and sealed for display.
- Respect the 24 h window and use templates for outbound. Outbound is a `DESTRUCTIVE_TOOLS` entry with approval.
- "Quiet end": nothing is posted on failure.

**H5. Discord** (C, lower priority). Dep: H1.
- Interactions endpoint with Ed25519 verification (`ed25519-dalek` is in the lockfile; it becomes a direct dependency), mention-only, a per-guild role allowlist, 2000-character splitting.

**H6. Codex, flag-gated** (C, then V). Dep: P7b, P11, P4. Behind O2.
- Kind `openai-codex` on the Responses transport.
- PKCE through loopback 1455, or the device flow. JWT claims supply the account id, email and plan. Headers: `ChatGPT-Account-Id`, `originator`, `session-id`.
- Encrypted reasoning items are replayed and stripped when the route crosses providers. The rotating refresh relies on P11.

**H7. Release 0.52.0** (C, then V). Docs: `docs/channels.md`, the release notes.

Acceptance for v0.52:
- Slack fixture: a signed event becomes a turn and a threaded reply, and a bad signature is rejected.
- Telegram: a topic thread maps to a conversation.
- WhatsApp is off by default; its fixture passes when enabled.
- An unlinked sender gets nothing beyond the refusal.

Risks for v0.52:
- A public webhook URL means exposing the API (the existing reverse-proxy pattern). Slack Socket Mode would avoid that but adds a WebSocket dependency; defer it.

## 7. Other improvements folded in

These defects came from reading the code:
- `tasks.uncertain` is never set (B4).
- `gateway::is_ambiguous` matches on strings (P1).
- Handoffs bypass the daily quota (B3).
- `specs_for` swallows plugin errors (M3).
- `docs/marketplace.md` and `docs/orchestration.md` are stale (M9, C7).
- Antigravity's UA version is hard-coded (P6c).
- The admin sign-in path writes the age store, needs a restart and is not re-imported (P5).
- The project id is shared across accounts (P5).

## 8. Dependency graph and parallel lanes

```
v0.49: P1 ─┬─ P7b ──────────────┐
      P2 ─┼─ P3 ─┬─ P9a ─ P9b  │
      P8 ─┘      ├─ P10        │
                 ├─ P12 ─ P15  │
           P4 ───┼─ P5 ─ P6a ─ P6b ─ P6c
                 ├─ P7a        │
                 └─ P14 ─ P16 ─┤
           P11 ─ P13 (needs P1,P12) ─ P16 ; P17 (P9a,P4,P11) ; P18 last
v0.50: B1─B2 ; B3, B4, B7, C1 parallel ; B5(P13) ─ B6(B3,B4,B5) ; B9 ─ C2 ─ C3 ; C4, C5 ; C6 ; C7
v0.51: M1 ─ M2 ─ M3 ─ M4 ─ {M5, M6×n, M7, M8} ─ M9
v0.52: H1 ─ {H2, H3, H4, H5} ; H6 independent ; H7
```

- Parallel-safe sets (no shared files): {P1, P2, P8}; {P7b, P9a, P10} after P3; {B3, B4, B7, C1}; and the M6 manifests.
- Never run two items that edit `cloud_code.rs`, `ui/chat.rs` or `session.rs` at the same time.

## 9. Verification per item

- Each item ships with its tests in the same commit and runs the gate: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --locked`. On Windows use `scripts\cargo-win.bat test --workspace --locked`.
- A reviewer (V) runs after each multi-file item and before each release PR. The review checks:
  - UID scoping on every new store method;
  - no secret in a response, log or audit row;
  - approvals bind the canonical args;
  - the egress and blocklist scans run on every new outbound path.
- Release PR: version bump, release notes, `tasks/todo.md` updated, CI green, then merge.
- No release note claims live verification unless the owner ran it against a real account.
