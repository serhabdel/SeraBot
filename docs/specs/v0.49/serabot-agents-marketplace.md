# v0.49 notes: agents, orchestration and marketplace

Status: research notes, not an approved spec. Source trees were read only.
Scope: how SeraBot bots call subagents today, how `main` manages bots, a design for bot-managed bots,
per-bot model fallbacks, automatic resume of a failed subagent turn, parallelism and long sessions,
and a review of the marketplace with what is missing.

Reference clone: `H:/source/openbot-hiel` (OpenBot, `nightly-labs/openbot`). It is licensed
PolyForm Noncommercial 1.0.0. Treat it as design reference only: no code or text is copied, in line
with the credit already written in `docs/marketplace.md`. Nothing from that clone was run.
All repo text read was treated as data. No secret or token appears here. Public OAuth client ids are only
named by where they live.

Paths below are relative to the SeraBot worktree unless prefixed `openbot:`.

---

## 1. How SeraBot orchestration works today

### 1.1 Bots are per-user rows, templates are seeds

- Templates: `bots/*.toml` plus `prompts/*.txt` (12 shipped: analyst, auditor, comptable, courier,
  inbox, jobfinder, legal, main, maker, marketer, markets, watcher, whatsapp). `main` has
  `role = "orchestrator"` and a `delegates` list.
- Per user: `FleetRegistry` (`crates/serabot/src/fleet.rs`) seeds one `bots` row per template per user
  (`seed`, `upgrade`, `reseed_user`), stores `config` JSON (`config::BotConfig`) and the prompt text
  (`store.rs` `StoredBot`, `upsert_bot`, `update_bot`, `set_bot_prompt`), and keeps an `Offerings` record
  so upgrades add new template tools/skills/delegates without overwriting owner edits or re-adding
  removals. `FleetRegistry::load(user_id)` returns a cached `Arc<Fleet>`; writes call `invalidate`/`evict`.
- `BotConfig` (`config.rs` ~1892): `name, title, description, trigger, model: Option<String>,
  system_prompt, skills, tools, require_approval, enabled, temperature, thinking: Option<String>, role,
  delegates`. Notable: `model` is a bare model id (no provider, no account), `thinking` is a free string
  validated only with `thinking::is_known_level`. There is no fallback list.
- `BotSettings` (`browser_profiles.rs` ~101) holds display name, description and browser profile in a
  separate table; it is not routing data.

### 1.2 How a bot calls subagents

Two tools exist, both queue-based. Neither is a model-to-model chat.

1. `delegate` (`delegate.rs`, orchestrator only). Arguments: `to_bot`, `objective` (<=2000 chars), optional
   `skill`, `memory_ids` (<=5), required `model_class` (`routine` | `synthesis`). Checks, in order:
   `context.bot == "main"`; fleet loads for `context.user_id`; target is in `main.delegates`, enabled and
   `BotRole::Specialist`; `validate_handoff` (`orchestration.rs`); memory ids exist for this user;
   `ModelRouter::select_class` picks the model, `ModelRouter::pin` fixes a `PinnedRoute` (selection +
   provider fingerprint). It then writes audit `handoff.queued`, appends event `handoff.queued`, and
   enqueues a child `turn` task through `teams::enqueue_turn_task_with_id` with payload
   `{conversation_id, text=objective, bot, source:"handoff", route, help_depth, handoff:{handoff_id,
   parent_task_id, parent_turn_id, memory_ids, skill}}`. Up to `MAX_HANDOFFS_PER_TURN = 16` per turn
   (enforced again in `runtime/turn_loop.rs::execute_calls`), so several `delegate` calls in one model
   round fan out into parallel child tasks. `ToolSpec.external_effect` is `false`, so no approval.
2. `ask_teammate` (`ask_teammate.rs`, `collaboration.rs`): any specialist can ask one teammate one bounded
   question per turn (`MAX_HELP_DEPTH = 1`, one `ask_teammate` per turn).
3. Rooms (`teams.rs`): `team_ask` / `team_reply` for multi-bot rooms, one `team_ask` per turn, capped rounds.

Child turn behaviour (`runtime/turn_loop.rs::run_turn`): a handoff turn skips conversation history, uses
`handoff_system_prompt`, and gets a fresh turn row. On completion `finish_reply` posts one assistant
message into the parent conversation labelled with the specialist and audits `handoff.completed`. The
parent turn finishes first and releases its permit; there is no join, no result object returned to the
parent model, and no parent notification when a child fails.

### 1.3 How `main` manages bots today

It does not. `main` can route and delegate but has no tool that changes any bot. Bot changes are owner
REST calls only:

- `PUT /v1/bots/{id}` (`api/bots.rs::update_bot`): `enabled, prompt, tools, skills, require_approval,
  display_name, description, model {provider, model} | null, thinking | null, browser_profile`.
  Validates that the change leaves the fleet valid, checks the model against the live catalog
  (`ensure_model_catalogs`), scans text with `scan_owner_output`, then writes the row and invalidates
  the fleet cache.
- `POST /v1/bots` (`create_bot`), `DELETE /v1/bots/{id}`, catalog install.
- Per-conversation model: kv key `model_selection:{user}:{conversation}` read by
  `runtime/models.rs::stored_model_selection`; per-user default route kv `default_route:{user}`;
  `heal_conversation_model` swaps a vanished model for a working one with a chat notice.
- Per-conversation thinking: `conversation_thinking`, resolved by `turn_thinking` (conversation value,
  then `BotConfig.thinking`).
- `yolo_toggle` (0.28) is the one self-modifying tool a bot already has (per-conversation auto-approve,
  never for `DESTRUCTIVE_TOOLS`).

### 1.4 Model selection and routing

`model_router.rs`:
- Providers come from config (`[llm]` = id `default`, `[llm.providers.<id>]`), credentials resolved per
  acting user (`ProviderCredentials`), catalogs per user and provider (live list or allowlist fallback).
- `default_selection`, `autoselect` (order: bot model, `llm.bot_models`, `[llm].model`, first approved pair),
  `select_class` (`[llm.classes]` such as routine/synthesis), `resolve_selection`, `validate_selection`,
  `pin` / `validate_pinned` (provider fingerprint over endpoint, secret ref, egress approval, kind,
  catalog settings, auth label, project id).
- `fallback_for_model` exists but is only a display/heal helper: first class listing the model, then the
  first other model of that class.
- `complete_with_thinking`: `validate_pinned`, credential check, optional live catalog refresh, then
  `thinking::wire_fields(kind, model, tags, level)` and `LlmClient::complete_with`.
- `docs/model-routing.md` states: "There is no automatic cross-provider fallback". That is the line
  this design changes (consent-gated, see 3.3).
- `thinking.rs`: levels `off, minimal, low, medium, high, xhigh, max`; `levels_for(kind, model, tags)`
  has rules for openai, mistral, openrouter, gemini, groq, xai, deepseek only. Anthropic, antigravity,
  muse-code, minimax, moonshot etc fall to the empty rule, so no thinking control is ever applied to them.
  `wire_fields` emits `reasoning_effort` (OpenAI style, `off` -> `none`), `reasoning: {effort}`
  (openrouter) or `thinking: {type}` (deepseek). No Anthropic `thinking`/`budget_tokens` mapping and no
  Gemini `thinkingConfig` mapping exist.

### 1.5 Worker, concurrency and failure handling

`runtime/worker.rs` and `store.rs`:
- Single global queue table `tasks` (`status, priority, run_after, attempts, max_attempts, lease_until,
  worker_id, fencing_token, uncertain, last_error, user_id`). `claim_task` picks the oldest highest-priority
  `queued` row across all users. There is no per-user or per-bot fairness and no per-user concurrency cap.
- `config.rs` ~257: `runtime.max_concurrent_turns` must be 1 or 2 (validation rejects more). The worker
  holds a `Semaphore` of that size. With 16 handoffs and 2 permits, children run two at a time and a
  heavy fan-out starves every other user in the 10-user circle.
- Lease heartbeat renews every `lease/3`; `recover_expired_tasks` requeues expired running tasks unless
  `uncertain = 1`. Wall clock limit `turn_max_secs` -> `stop_overdue_task` -> `fail_task_final` (no replay).
- `process_task` error path: errors classed permanent (`Forbidden, Rejected, Validation, Config,
  PolicyViolation, NotFound, InvalidState`) -> `fail_task_final`; anything else (including every
  `External`/`Upstream` model error) -> `fail_task` with linear backoff `lease_secs*attempts` (capped
  3600 s) until `attempts >= max_attempts`. On failure the owner sees a generic "This request failed"
  message and a `task.failed` event. The retry re-enters `run_turn`, finds the stored turn, and calls
  `advance` with the stored `TurnState` (messages, evidence, pending approval). It reuses the same
  `route`: `run_turn` refuses a route different from the queued one (`turn route does not match queued
  route`). So a model outage retries the same dead model until attempts run out.
- `TurnState` is persisted on every round (`update_turn`), so a retry resumes mid-conversation; a turn
  already `completed | stopped | limit_reached` is not re-answered; `sending_response` with unknown
  delivery is not resent; a pending action in `outcome_unknown` blocks resume.
- Findings: `tasks.uncertain` is never set by production code (only tests and `api/jobs.rs` read it); the
  lease-expiry safety (`uncertain` -> `failed`) is therefore inert. Outcome ambiguity for tool actions is
  tracked on `actions.status = 'outcome_unknown'` by `gateway.rs::is_ambiguous`, which matches only
  `SeraError::External` text containing "timeout", "unknown" or "connection" (fragile string matching).
- Limits: `turn_limits.rs` default 1500 tool calls and 200 rounds per turn (ceiling 1500 each), per-user
  override via `get_user_turn_limits`. Daily quota counts tasks per user through
  `enqueue_task_with_quota`; handoff and resume tasks use `enqueue_task` and bypass it.
- Provider errors: `llm.rs` maps all HTTP failures to `SeraError::External(String)`; Cloud Code
  (`classify_cloud`, `cloud_code.rs`) knows 401, 403, 404, 429 and 5xx, but the status is flattened into a
  string. `SeraError::Upstream{status, code, message}` exists (used by connectors) but the LLM path does
  not use it. `retry.rs` retries transport and 429/5xx inside one request (`MAX_ATTEMPTS = 3`,
  `MAX_RETRY_AFTER = 30 s`).

### 1.6 Gateway invariants that any new tool must keep

`gateway.rs`: `execute` refuses a tool not in `context.allowed_tools`; approval when
`tool.requires_approval(args)` or the bot's `require_approval` list; approvals bind user, canonical args
hash, destination, artifact hash, policy version and expiry; `execute_approved` consumes the approval
(`approved -> dispatching`) and marks `outcome_unknown` for ambiguous failures; `DESTRUCTIVE_TOOLS` can
never be auto-approved (YOLO). Plugin tools always need approval, never get standing grants and are
never auto-approved (`plugin_runtime.rs::PluginTool`). Everything user-owned takes `user_id`.

---

## 2. What OpenBot does (design reference)

### 2.1 Agents managing agents (`openbot:src/backend/agent/openbot-tool-router.ts`, `model-tools.ts`, `tool-guidance.ts`)

- Tools on the built-in `openbot` namespace: `create_agent`, `update_profile`, `update_agents` (bulk),
  `read_agent`, `list_models`, `list_agents`, sections tools, routine tools, task tools, wait tools.
- `list_models` returns, per provider, the models with `defaultReasoningEffort` and
  `supportedReasoningEfforts`, plus provider accounts as `{id, label, state}` only (never email or
  credentials). It is the only source an agent may copy provider/model/effort from.
- `update_profile` takes `agentId` plus `provider, model, reasoningEffort, providerAccountId,
  backupModels, access, computerUse, name, title, instructions, avatar`. Rules worth copying:
  - validated against the live model list before anything is written, so a bad model leaves other
    fields of the same call unchanged;
  - an agent cannot widen its own or another agent's power: `widensAgentSettings` refuses Full access,
    Computer Use on, auto-approve; only the user can (`error.agent.onlyUserWidensSettings`);
  - a provider change needs an atomic provider+model pair and an idle agent (`requireIdle`: no started
    mailbox delivery, no active turn, no working tasks);
  - `read_agent` before `update_profile` is instructed in guidance; it reports profile, model, account,
    backup models, access, installed skills, routines.
- Backup models (`openbot:packages/contracts/src/team-protocol/agent-backup-models-v1.ts`):
  `backupModels = { automatic: bool, order: [{provider, model, providerAccountId?}] }`, at most 8, each
  choice unique, own model and own provider sign-in omitted. `null` means "ask in the chat". Read-only
  `usageLimitReturn` records the model the agent left.

### 2.2 Automatic switch and return (`openbot:src/backend/agent/usage-limit-gate.ts`, `agent-service.ts` ~2340-2640)

- `UsageLimitGate` keeps in-memory entries keyed `provider + accountId + model`:
  - `limits`: the plan refused a turn (reset time reported or rechecked every 15 min by a usage read; ends
    at reset + 30 s grace, or when a turn on that key completes). While set, no agent on that key starts a
    turn; messages stay queued instead of failing one by one.
  - `failures`: a provider error failed a turn; the model is passed over for 10 minutes
    (`PROVIDER_FAILURE_HOLD_MS`). They hold no queue; they only trigger a move for agents set to switch
    automatically.
- `#usageLimitMove(agentId)`: only an agent with no started work moves. If it has a `usageLimitReturn` and
  that model is ready again, it goes back (move `"return"`). Else if its current model is limited or failed
  and it has queued work, and `backupModels.automatic`, it moves to the first ready backup
  (`#modelChoiceReady`: provider on and signed in, account connected, model listed; and not limited or
  failed). The move keeps the reasoning effort when the new model supports it, else uses that model's default.
  The agent gets a visible status line (`modelSwitchedForLimit`, `modelSwitchedForError`,
  `modelReturnedAfterLimit/Error`).
- The queue moves with the agent: the failed turn's message is not lost and not replayed blindly; it waits
  in the mailbox and runs on the new model.

### 2.3 Subagents / tasks (`openbot:src/backend/agent-tasks/*`)

A task is a second conversation of one agent with its own thread and provider session, `origin` user or
agent (`start_task` tool), `outcome` completed | failed | interrupted, redacted `error`, `closed_at`.
Messages carry a task id in the mailbox record. There are explicit wait objects (`agent-waits`) so an agent
can wait for another agent or task. Boot recovery (`agent/boot-recovery.ts`) reconciles started work after
a restart and gives a "Resume to try again" state instead of silently replaying.

### 2.4 Channels (`openbot:docs/messaging.md`, `docs/architecture/channels-and-messaging.md`, `apps/slack-app`)

Slack, Discord and Telegram only. WhatsApp appears only in marketing comparison pages
(`apps/auth-api/src/content/compare/*`), not as a connector. Slack model: one distributed Slack app;
workspace installs through an OAuth page on OpenBot's hosted API; events arrive at a hosted "signal"
service and are relayed to the user's own computer. A "Slack Orchestrator" agent is created on connect,
answers short requests itself and hands other work to the best-fit agent; posts come from the one bot
identity; reactions show state (eyes, working, check, cross, stop); approvals appear in the thread as
Approve/Deny buttons answerable only by the requester; bot scopes are a fixed list
(`openbot:packages/contracts/src/slack-app.ts` `SLACK_BOT_SCOPES`: app_mentions:read, channels:history,
channels:join, channels:read, chat:write, files:read, files:write, groups:history, groups:read,
im:history, im:read, im:write, mpim:history, reactions:write, users:read). Per-workspace memories, thread
context fetch tools, `stop`/`cancel` keywords.

### 2.5 Plugin marketplace and MCP sign-in (`openbot:marketplace/plugin-catalog`, `docs/architecture/integrations.md`)

- 17 catalog entries, one bundle per directory with `plugin.json`, ordered by `catalog.json`. Real hosts:
  `https://mcp.canva.com/mcp`, `https://mcp.linear.app/mcp`, `https://mcp.notion.com/mcp`,
  `https://mcp.sentry.dev/mcp`, `https://mcp.vercel.com`, `https://mcp.stripe.com`,
  `https://mcp.posthog.com/mcp`, `https://mcp.context7.com/mcp`, `https://mcp.aave.com/mcp`,
  `https://cobrain.space/mcp`. Key-based and local entries use `npx` stdio (airtable, brave-search,
  firecrawl, resend) or a loopback http server (figma, `127.0.0.1:3845`) and the Paper CLI.
- Auth kinds in a listing: `link` (browser OAuth), `key` (header for http, env for stdio), `local`.
- MCP OAuth is done by the host (`openbot:src/backend/mcp-oauth-provider.ts`, built on the MCP SDK
  `refreshAuthorization`): discovery, dynamic client registration, PKCE, token store in encrypted storage,
  `refreshAccessToken` before a thread starts, token revocation (RFC 7009) on sign-out, a per-row sign-in
  state with `needsSignIn`. Error table: `invalid_grant` drops tokens but keeps the client registration;
  `invalid_client`/`unauthorized_client` drops tokens and registration; network or 5xx keeps everything.
  `secureOAuthFetch` bounds bodies to 1 MiB and 20 s and requires every OAuth endpoint to resolve to public
  addresses when the MCP host does.
- The agent never sees the token: `McpOAuthProxy` is a loopback proxy at `127.0.0.1:<port>/mcp/<route>`;
  the bearer is an HMAC of the route; one refresh and one retry on upstream 401; MCP-style error `-32001`
  when signed out. (This is the desktop shape; SeraBot is server-side so the token is simply never put in
  a prompt or tool result.)
- Loopback callback: fixed port 28417, path `/mcp-auth`; remote hosts use a callback relay (client listens,
  host holds the code exchange, 10 minute single-use flow, state checked on the host, only an https page
  opened after the user sees its host name).
- Suggestion tool: `suggest_marketplace_app` renders an install card when an agent reaches a service in the
  browser that has a plugin.

---

## 3. Design for v0.49

Goals from the request: (a) bots managing bots, (b) per-bot default model with fallbacks, (c) automatic
resume of a failed subagent by switching model or provider, (d) heavy parallelism and long sessions. All
under AGENTS.md: UID scoping, gateway approvals, no replay of unknown external effects, per-user secrets.

### 3.1 Bot model profile (the data model)

Replace the single `BotConfig.model: Option<String>` with a profile, stored in the existing `config` JSON
(no row migration needed; serde defaults keep old rows valid):

```
BotConfig {
  ...
  model: Option<String>,            // kept for read compat; migrated into profile.primary
  thinking: Option<String>,         // kept; migrated into profile.primary.thinking
  profile: Option<ModelProfile>,
}
ModelProfile {
  primary:   Option<ModelChoice>,   // provider, model, account_id?, thinking?
  fallbacks: Vec<ModelChoice>,      // at most 3 (request asked for fallback 1 and 2, optional); cap 4
  fallback_mode: Off | Ask | Auto,  // default Auto only within the same provider account; see 3.3
  allow_cross_provider: bool,       // default false; owner-only switch, see 3.3
}
ModelChoice { provider: String, model: String, account_id: Option<String>, thinking: Option<String> }
```

Rules:
- `thinking` is stored per choice because the level set differs per model (`thinking::levels_for`). On
  switch the level is kept when the new model supports it, else the model default (same rule as OpenBot).
- Validation reuses `ModelRouter::validate_selection` plus `catalog_gate`; choices are unique; the primary
  is not repeated in fallbacks; a choice whose provider has no credential for this user is rejected with the
  existing `no_credential_error` wording.
- Resolution order for a turn (extends `selection_or_fallback` and `autoselect`): conversation pick, then
  `profile.primary`, then legacy `model`, then user default route, then router default. A subagent
  (`delegate`) resolves its `route` from the target specialist's profile, with `model_class` only used when
  the profile has no primary (so a per-bot choice wins over a class).
- Account selection ties into the multi-account provider work (v0.49 provider scope): `account_id` is the
  opaque id of one of the user's credential rows for that provider. The agent-facing list never exposes the
  account secret or email, only `{id, label, state}`.

### 3.2 (a) Bots managing other bots

New gateway tools, registered only for the orchestrator (`main`) in `tools.rs`/`gateway` registry and
filtered like `delegate` in `turn_loop.rs::advance`. All take the acting `user_id` from `ToolContext`;
none takes a user id from the model.

| Tool | Effect | Approval |
|------|--------|----------|
| `bot_list` | name, title, role, enabled, model profile summary, delegates | none (read) |
| `bot_read` | one bot: profile, tools, skills, require_approval, thinking support per model | none |
| `model_list` | providers, models, supported thinking levels, account `{id,label,state}` | none (read; scanned by egress policy) |
| `bot_set_model` | set `primary` and/or `fallbacks` and thinking level for a bot the caller manages | owner approval (state change on routing and data egress); the approval summary shows old and new provider/model, never args beyond that |
| `bot_set_enabled`, `bot_set_delegates` | enable/disable, change delegates (still passes `validate_bot_fleet`) | approval |
| `bot_create` (optional, later) | create a specialist from a template; same constraints as `POST /v1/bots` | approval |

Constraints (copying OpenBot's `widensAgentSettings` principle):
- A bot may never change `tools`, `require_approval`, or `skills` to be wider, never touch `main`'s own
  role, and never enable `yolo` or auto-approve for another bot. Only the owner can, through REST.
- `main` can only manage bots in its own `delegates` list plus itself (profile only). Specialists get no
  management tools.
- Provider changes require the target to be idle for this user: no `running` task for that bot and user, no
  `awaiting_approval` turn on it. Reuse a store query over `tasks` (`user_id`, `bot`, `status`).
- Changes go through one shared function used by both REST `update_bot` and the tool, so validation, egress
  scanning (`scan_owner_output`), catalog checks and the audit trail cannot diverge. Audit event
  `bot.profile_changed` with before/after provider and model ids, the initiating bot, the task id.
- The approval binds user + canonical args (bot, provider, model, account id, thinking) + policy version +
  expiry, per AGENTS.md. A changed argument means a new approval.
- Extra guard against prompt injection: a management tool call is rejected when the turn source is a
  handoff, team turn or routine run (`TurnState.handoff/team` non-empty), the same idea as OpenBot's
  `CHANNEL_MANAGEMENT_TOOLS` rule "only a person's request may start them". Tool and model output remain
  untrusted data.

### 3.3 (b) and (c): fallbacks and automatic resume

Principle: a failed turn is resumable because `TurnState` is durable; what is not safe to repeat is an
external effect whose outcome is unknown. So resume = "re-enter the stored turn on a different route", never
"restart the objective".

Error classification. Add `llm_failure.rs` with

```
enum LlmFailure {
  Transient,        // 5xx, timeout, connection reset, 529 overloaded
  RateLimited { retry_after: Option<Duration> },
  UsageLimit { resets_at: Option<DateTime<Utc>> },   // plan/quota exhausted (429 with quota body, or provider-specific)
  AuthExpired,      // 401, refresh failed, revoked grant, invalid_grant
  ModelGone,        // 404 model, "model not found/deprecated", catalog no longer lists it
  ContextTooLong,   // 400 context length (needs compaction, not a switch)
  PolicyOrContent,  // 400 content/safety refusals, PolicyViolation, never switch to evade
  BadRequest,       // other 4xx, never retry
}
```

Source of the status: make `llm.rs` and `cloud_code.rs` return `SeraError::Upstream{status, code, message}`
instead of `External(String)` for model calls (the variant already exists and is documented as safe to
show), keeping `Retry-After` in a small struct. Add `fn classify_llm(error: &SeraError) -> LlmFailure`
with table-driven tests of status/body fixtures per provider kind. Remove the string matching in
`gateway::is_ambiguous` in favour of an `ambiguous: bool` on `Upstream`/`External`.

Decision table (what the runtime does after the in-request `retry::send` budget is spent):

| Failure | Action |
|---------|--------|
| Transient, RateLimited | Retry same route with backoff (existing `fail_task`), after 2 attempts move to next fallback |
| UsageLimit | Hold that provider+account+model key until reset (new in-memory gate like OpenBot, persisted `provider_holds` table so restart keeps it), move to next fallback |
| AuthExpired | Try one refresh; if it fails, mark the credential `needs_sign_in`, move to the next fallback on a different account or provider, and tell the owner |
| ModelGone | Move to next fallback immediately; also heal the conversation selection (existing `heal_conversation_model`) |
| ContextTooLong | Compact or truncate then retry same route; not a fallback case |
| PolicyOrContent, BadRequest | Fail final. Never switch model to get around a policy block |

Cross-provider safety. Moving to another provider sends the same prompt to a different company. Today each
provider has a `data_egress_approved` flag (`ModelRouter::pin` requires it) and `docs/model-routing.md`
says no automatic cross-provider fallback. Keep that guarantee as the default:
- `fallback_mode = Auto` may switch among choices that are all `data_egress_approved` for this user.
  `allow_cross_provider = false` (default) restricts automatic moves to the same provider (other model or
  other account). Cross-provider needs `allow_cross_provider = true`, settable only through owner REST or
  an approved `bot_set_model` call, and every choice already passes the egress approval.
- The switch re-runs `scan_model_messages` and `scan_egress` against the new route, because the blocklist
  applies to model requests regardless of provider. Vault redaction (`redact_messages`) is unchanged.
- Fingerprint: the child's `PinnedRoute` is rebuilt with `router.pin(&next)`; `validate_pinned` still
  guards config drift. The queued `route` in the task payload is updated through a store method
  `rebind_turn_route(user, task_id, old_fingerprint, new_route)` guarded by fencing token and by the old
  fingerprint, so the "route does not match queued route" check keeps meaning something. Audit event
  `model.failover` with from/to provider and model, failure class, task id, handoff id.

Resume mechanics, in `runtime/worker.rs::process_task` error branch and `turn_loop.rs::advance`:

1. `advance` wraps `router.complete_with_thinking`. On error it calls `classify_llm`. If the class is
   switchable and the bot's profile has a next ready choice (not held, credential connected, catalog lists
   the model), it records `model.failover`, rebinds the route, updates `state.route`, persists the turn
   (`update_turn`), and continues the same loop iteration with the new route. No task failure, no new task.
2. If no choice is ready, the turn is parked, not failed: new task status `waiting_model` with
   `run_after = earliest hold expiry` (or a 60 s poll), so the lease is released and a worker permit is not
   held. When a route becomes ready the task is claimed again and `run_turn` re-enters `advance` from stored
   state. The owner sees one chat line ("Waiting for a model to be available").
3. For a handoff child that fails after all fallbacks, the parent conversation gets one assistant line
   naming the specialist and the reason class, and `main` receives a synthetic follow-up turn
   ("subagent X failed: <class>, options: retry on <other model>, skip, ask owner") so `main` can decide,
   which is the "main can auto resume" behaviour requested. That follow-up is bounded: a counter in
   `HandoffContext` (`resume_count`, max 2) so a failing task cannot loop.

Idempotency and no blind replay:
- The unit of resume is the persisted `TurnState`. Model calls are idempotent from our side (a completion
  is just retried); tool calls already have results in `state.messages` and are never re-executed.
- A tool call in flight when the failure happened: if its action is `outcome_unknown` or `dispatching`
  (`store.get_action`), the turn is not resumed automatically. It is parked as `needs_review`, the owner
  reconciles (existing `reconcile_dispatching_actions`, `api/jobs.rs` reliability list). This keeps the
  existing `run_turn` check and extends it to failover.
- Make `tasks.uncertain` real: set it when a task's turn has a `dispatching` action at failure time or when
  a lease expires during a tool call with `external_effect`, so `recover_expired_tasks` and `fail_task`
  stop replaying it. Today nothing sets it.
- `delegate` is itself not idempotent (re-running the parent model could queue the same objective twice).
  Add an idempotency key `handoff_key = sha256(parent_turn_id, to_bot, objective)`; a unique index on
  `(user_id, parent_task_id, handoff_key)` in a small `handoffs` table makes a duplicate call return the
  existing child task id. This also gives the parent a place to read child status (see 3.4).
- Plugin tools (`plugin.*`) and connector writes (`DESTRUCTIVE_TOOLS`) always have `external_effect` and
  approvals, so they cannot be re-executed by a resume; a resume only continues the model conversation.

Persistence of fallbacks across restarts: provider holds go in a table `provider_holds(user_id, provider,
account_id, model, kind, until, reason_code)`, user-scoped, pruned in `maintenance()`.

### 3.4 (d) Parallelism limits and durable long sessions

Problems found: global permit count capped at 1 or 2; no per-user fairness; 16 handoffs per turn can
monopolise; parent never learns child status; handoff tasks bypass the daily quota.

Proposed limits (all config, validated, per-user clamped):
- `runtime.max_concurrent_turns`: raise the ceiling from 2 to a configurable value (default 4, ceiling 8).
  The ceiling of 2 was the guard for a single operator; the circle is up to 10 users. Keep SQLite WAL
  writes short, confirm with a load test (`scripts/` fixture) before raising the default.
- New `runtime.max_turns_per_user` (default 2) enforced in `claim_task`: add a `NOT EXISTS`/count
  predicate on running tasks per `user_id`, and order claims with a per-user round-robin tiebreak (oldest
  `updated_at` of that user's last claim) so one user's fan-out cannot starve others.
- `max_children_per_parent_turn` stays 16 queued, but only `max_active_children` (default 4) of them run
  at once; the rest remain `queued`. Add `priority` lower for children than for owner turns so owner chat
  stays responsive.
- Count handoff/resume tasks against a separate daily child quota (`child_daily_limit`, default 500 per
  user) so fan-out is bounded but not blocked by the 100-ish user task quota.
- Child concurrency shares provider limits: a per-provider+account semaphore (`max_inflight_per_account`,
  default 3) in `ModelRouter` or a `ProviderLimiter`, so fifteen children do not each trigger 429s. 429
  handling feeds the hold table above.
- Long sessions: history is bounded by `history_limit`; add rolling summarisation (conversation summary
  already exists for owner turns, see `memory_context`) and a compaction step when the estimated prompt
  size nears the model's context (`estimate_prompt_chars` is already computed per call). Compaction is the
  action for `ContextTooLong`. Turn wall clock `turn_max_secs` stays, but a long job should be split into
  checkpointed turns: allow a turn to end with `limit_reached` and a "continue" affordance (exists:
  `LIMIT_REACHED_STATUS`) and let the owner or `main` queue a `resume` task (`resume_turn` exists for
  approvals; extend its payload for "continue after limit").
- Parent visibility: new read tool `handoff_status` (orchestrator) returning child status, outcome class and
  last error code from the `handoffs` table; `main` can poll in a later turn instead of guessing.
- Observability: events `handoff.started|completed|failed`, `model.failover`, `provider.hold_set|cleared`
  for the console activity dock (`runtime/activity.rs` already emits step events).

### 3.5 What the chat UI and API need (desktop crate)

Not specced here beyond the data the backend must expose: per-bot profile editor (primary, up to 3
fallbacks, thinking per choice, mode, cross-provider toggle), a per-bot "using fallback" badge with the model
it will return to, subagent tree/status panel fed by `handoff_status` and events, provider hold banners,
and a queue view that shows `waiting_model`. These ride on `GET /v1/bots` and a new
`GET /v1/handoffs?conversation_id=`.

---

## 4. Marketplace review

### 4.1 Current state (`marketplace.rs`, `plugin_runtime.rs`, `mcp_client.rs`, `api/marketplace.rs`, `plugins/`)

- Schema v1 manifest (`PluginManifest`); `apps[].server` is `http{url}` or `stdio{command,args}`;
  `apps[].auth[]` kinds `oauth-link` and `api-key` with `fields[].secretKey`; `permissions.egress` host list.
  Ten built-in manifests compiled in (`BUILTIN_MANIFESTS`): brave-search, elevenlabs, exa, firecrawl, github,
  linear, notion, resend, tavily, workspace-files. All hosts are `*.example.invalid` placeholders.
- Per-user state: `marketplace_installs` (migration 0035). Install, enable, disable, uninstall, config
  (`PUT /v1/marketplace/plugins/{slug}/config` writes secret `marketplace.<slug>.<secretKey>` to the per-user
  envelope store; values never returned).
- Runtime: `PluginTools::specs_for`, `tool_for`; tools named `plugin.<slug>.<tool>`; MCP client
  `PROTOCOL_VERSION = 2025-06-18`, 1000 tool cap, 10 min catalog cache; egress check (https, listed host, no
  credentials) plus DNS resolution pinned to public addresses; 30 s request timeout, 2 MiB response cap,
  8000 char output cap, 400 char description cap, 8 KiB schema cap; every call needs approval and cannot get
  a standing grant or YOLO.

### 4.2 Defects and gaps in what exists

1. `docs/marketplace.md` is stale: it says the tool runtime "is not part of this release" and that the
   desktop shows "coming soon". The runtime shipped in v0.38 and `RUNTIME_CALLS_PLUGINS = true`.
2. `oauth-link` plugins cannot run. Their manifests declare no `fields`, and `PluginTools::client_for`
   requires a first `fields` entry to find a secret, otherwise returns "plugin declares no key field". There
   is no OAuth flow for plugins, so github, linear, notion, elevenlabs and workspace-files are
   not runnable even when a real host is given. (ElevenLabs works separately through
   `elevenlabs_mcp.rs` and `elevenlabs_oauth.rs`, outside the marketplace.)
3. `api-key` plugins always send the key as `Authorization: Bearer` (`McpAuth::Bearer`), but real services
   differ (`x-api-key`, `Authorization: Bearer`, custom headers). `McpAuth::Header(&'static str)` exists but
   is unused by the marketplace; the manifest has no way to declare the header name. Only the first field of
   the first auth option is used, so multi-field auth (key + workspace id) is impossible.
4. Placeholder hosts: no manifest points at a live service, so nothing in the marketplace is actually usable
   today. AGENTS.md keeps placeholders blocked.
5. Only the first `http` app of a plugin is used (`http_url`), so a bundle with several MCP servers is
   silently truncated.
6. Per-user, one credential per plugin: no multi-account (two GitHub accounts) support; `marketplace_installs`
   key is `(user_id, slug)`.
7. Tool metadata ignored: MCP `annotations` (`readOnlyHint`, `destructiveHint`) are not read. Every plugin tool
   needs approval, including pure reads, which makes plugins tedious. `elevenlabs_mcp.rs::tool_is_read` already
   implements a read heuristic that can be generalised.
8. Failure handling: `specs_for` swallows list errors, so a signed-out or down plugin silently disappears
   from the tool list with no owner-visible state, and `external()` maps 401 to a generic "answered with
   status 401" instead of a "sign in again" state.
9. The catalog is build-time only (compiled-in or folder); there is no signed catalog update, no version
   pinning, no per-plugin tool allow/deny list for the owner.
10. Overlap and drift: native token connectors (`token_connectors/*.rs`: GitHub, Slack, Todoist, Exa),
    Google/Outlook/Notion/OneDrive connectors, AgentMail, Composio (hosted, thousands of toolkits), and
    marketplace manifests describe the same services three ways. Tool names differ
    (`github_create_issue` vs `plugin.github.*`).
11. No marketplace-to-chat suggestion: nothing tells a bot "a plugin exists for this".

### 4.3 What is natively covered outside the marketplace

| Service | Where | Auth | Tools |
|---------|-------|------|-------|
| GitHub | `token_connectors/github.rs` | per-user PAT | `github_search_repos`, `github_list_issues`, `github_get_issue`, `github_create_issue`, `github_comment_issue`, `github_get_file` (read + 2 writes with approval). No PRs, no reviews, no actions, no commits, no OAuth. |
| Slack | `token_connectors/slack.rs` | per-user bot token | `slack_list_channels`, `slack_read_channel`, `slack_post_message` (approval; refuses `@channel`/`@here`). Outbound tool only, no inbound events. |
| Todoist | `token_connectors/todoist.rs` | token | list/add/complete tasks |
| Exa | `token_connectors/exa.rs` | key | search |
| Google | `docs/connectors.md` | OAuth refresh token | Drive list, Sheets read, Calendar list, Gmail list (read-only) |
| Outlook / Outlook calendar | same | OAuth | list messages and events (read-only) |
| Notion | same | OAuth | `notion_query_database` only |
| OneDrive | `onedrive.rs` | per-user OAuth + PKCE | upload, list |
| Telegram | `telegram.rs` | bot token, per user link | the only inbound chat channel |
| WhatsApp | `bots/whatsapp.toml` | via Composio only | drafts, sends after approval; no native connector |
| Composio | `composio.rs`, `composio_mcp.rs` | per-user key (`ak_`, `uak_`, `ck_`) | search, connect, execute across toolkits |
| ElevenLabs | `elevenlabs_mcp.rs` | OAuth (CIMD, fixed loopback 51122) | hosted MCP |

### 4.4 Missing, ranked

Marketplace entries to ship with real hosts (verify each host, terms and OAuth metadata at implementation
time; the hosts named in OpenBot's catalog are the starting list, to be re-checked):

1. GitHub (hosted MCP, OAuth or PAT): PRs, reviews, commits, code search, Actions status. Replace the
   placeholder manifest; keep the native token connector as the cheap fallback and align tool names.
2. Canva (`mcp.canva.com/mcp`, browser OAuth): create/edit designs, search library, export, comments.
   Needs the OAuth flow in 4.5. Output exports must go through the artifact store and blocklist scan.
3. Linear (`mcp.linear.app/mcp`, OAuth), Notion (`mcp.notion.com/mcp`, OAuth): fill existing placeholders.
4. Slack as an inbound channel plus MCP-style tools (see 4.6), Gmail/Calendar write scopes (send is a
   `DESTRUCTIVE_TOOL`), Google Docs/Sheets write, Microsoft 365 Teams/Calendar write.
5. WhatsApp: no first-class connector. Options: WhatsApp Cloud API (Meta business account, webhook,
   templates, 24 h window) as an owner-configured channel, not per member self-service. Keep it approval
   gated. Do not use unofficial web-client libraries (account ban risk and ToS). OpenBot has no WhatsApp
   either, so there is no reference to follow; it must be designed from Meta's docs.
6. Developer and data: Sentry, Vercel, Stripe (restricted to read tools plus approval), PostHog, Context7,
   Airtable, Figma (local-only: stays off, stdio/loopback), Paper (stdio: stays off).
7. Search and scraping already native (Tavily, Exa, Brave, Firecrawl): decide one path (native or plugin) to
   avoid duplicates; Brave search, Resend and Firecrawl manifests need a header name field.
8. Calendly, HubSpot, Asana, Jira/Atlassian, ClickUp, Monday, Intercom: the user's Claude connectors list
   shows these exist as hosted MCP servers; add as manifests once the generic OAuth lands.

### 4.5 Manifest and runtime changes (schema v2)

Keep schema v1 loading; add v2 fields (`deny_unknown_fields` means an explicit version bump):

```
apps[].auth[]:
  kind: "oauth" | "api-key" | "none"
  oauth: { resourceMetadataUrl?, authorizationServer?, scopes[], clientId?, usesCimd?: bool,
           pkce: true, resource: <MCP url for RFC 8707> }
  header: "Authorization" | "x-api-key" | ...   (api-key only; default Authorization with Bearer)
  scheme: "bearer" | "raw"
  fields[]: id, label, secretKey, secret: bool
tools: { allow?: [..], deny?: [..], readOnly?: [..] }   // owner-visible, manifest default, owner override
accounts: { multiple: bool }                              // multi-account
```

New module `mcp_oauth.rs` (generalising `elevenlabs_oauth.rs` + `onedrive.rs` PKCE):
- Discovery: fetch the MCP server's protected resource metadata (RFC 9728, from the `WWW-Authenticate`
  `resource_metadata` of a 401 or `/.well-known/oauth-protected-resource`), then authorization server
  metadata (RFC 8414, fall back to OIDC discovery). Cap bodies at 64 KiB, 20 s, every endpoint must resolve
  to public IPs (reuse `plugin_runtime::resolve_checked`).
- Client identity: prefer CIMD (client id is the https URL of a document SeraBot serves, as
  `elevenlabs_oauth::cimd_client_id` and `/.well-known/serabot-oauth-client.json` already do), else dynamic
  client registration (RFC 7591), else a configured public client id. Public client ids live in config
  (`[marketplace.oauth.<slug>]`), never in secrets.
- Flow: authorization code + PKCE S256, `state` single use bound to `(user_id, slug, account_id)`, RFC 8707
  `resource` on authorize, token and refresh, loopback redirect handled by the desktop `oauth.rs` listener
  (fixed port 51121/51122 pattern) which hands the code to the server; server-side token exchange.
- Storage: `UserSecrets` keys `marketplace.<slug>.<account>.oauth` holding `{access, refresh, expires_at,
  scope, client registration}`; refresh before expiry with a per-account async lock (rotation safe: write
  the rotated refresh token before using the new access token, like `llm.rs` persistence); `invalid_grant`
  drops tokens and marks `needs_sign_in`; `invalid_client` drops the registration too; network errors keep
  state (same table as OpenBot 2.5). Revocation (RFC 7009) on uninstall, best effort.
- Tables: extend `marketplace_installs` with `account_id` (migration 0037) or add
  `marketplace_accounts(user_id, slug, account_id, label, state, created_at, last_ok_at)`; the primary key
  becomes `(user_id, slug, account_id)`. Tool names for second accounts: `plugin.<slug>.<account>.<tool>` or
  a required `account` argument defaulting to the primary.
- Tool risk from annotations: `readOnlyHint = true` tools skip approval (still audited, still egress/output
  scanned); everything else keeps approval, never standing grant for destructive hints. Bounded by owner
  override per tool.
- Status surface: `GET /v1/marketplace/plugins/{slug}` returns `state` (`not_installed`, `needs_setup`,
  `needs_sign_in`, `ready`, `unreachable`), last error code, last tool-list time; `specs_for` records the
  failure instead of swallowing it, and `main` gets a one-line note ("Canva needs sign-in") instead of a
  silent missing tool.

### 4.6 Channels (Slack, WhatsApp and others, as OpenBot does Slack)

Inbound channels are a separate layer from marketplace tools. Today only Telegram is inbound.

Slack (self-hosted, no relay service, fits the single binary):
- Per-user Slack app install is heavy for 10 users; prefer one SeraBot-owned Slack app installed by the owner
  into the workspace, events to `POST /v1/channels/slack/events` with signing secret verification
  (HMAC-SHA256 over `v0:ts:body`, 5 minute skew), `url_verification` challenge, and link each Slack user id to a
  SeraBot user through an explicit per-user link code, so a Slack message always runs as exactly one SeraBot
  user. Unlinked Slack users get a fixed refusal.
- Scopes: start from OpenBot's list, drop `files:write`, `channels:join` and `mpim:*` until needed. Bot token
  and signing secret live in the age-encrypted server store (workspace-level, admin set) or the
  envelope store; never the member store.
- Behaviour copied as design: mention or DM starts a thread conversation (`conv id = slack:<team>:<channel>:<ts>`
  mapped into a `dir:`/`room:` conversation), reactions for state, `stop`/`cancel` keywords, Approve/Deny
  buttons only usable by the requesting user (maps onto the existing approval binding), posts pass the same
  blocklist and `scan_egress`.
- Reuse `slack_post_message`'s existing refusals for `@channel` and `@here`.

WhatsApp: Cloud API webhook (`GET` verify token challenge, `POST` with `X-Hub-Signature-256` HMAC), per-number
link to a user, 24 h customer-service window and approved templates for outbound, media size limits, phone
numbers are PII (store hashed for lookup, sealed for display). Outbound sends are `DESTRUCTIVE_TOOLS` entries
with approval showing exact text and recipient (the `whatsapp` bot already promises this). Gate behind an
owner-enabled `[channels.whatsapp]` section, default off.

Discord: same shape as Slack (interaction endpoint, Ed25519 signature), lower priority.

A shared `channels/` module should define one trait (`verify_request`, `parse_event -> InboundMessage{channel,
external_user, thread_key, text, attachments}`, `send(OutboundMessage)`) and route into the existing
`teams::enqueue_turn_task` path with quotas applied, as `telegram.rs` does.

---

## 5. How to implement in Rust in SeraBot

Ordered so each step is one PR with tests in the same commit (AGENTS.md gate: `cargo fmt`, `clippy -D
warnings`, `cargo test --workspace --locked`).

### Step 1: error classification (foundation)
- `crates/serabot/src/llm_failure.rs`: `LlmFailure`, `classify_llm(&SeraError)`, `ProviderHint` with
  per-kind body patterns (OpenAI `insufficient_quota`, `rate_limit_exceeded`, `model_not_found`,
  `context_length_exceeded`; Gemini `RESOURCE_EXHAUSTED`; Cloud Code 429/403 bodies from `classify_cloud`).
- Change `llm.rs` (lines ~270-310, ~560-600) and `cloud_code.rs` to emit `SeraError::Upstream{status, code,
  message}` and keep `Retry-After`. Update `process_task`'s `permanent` match to treat `BadRequest` and
  `PolicyOrContent` as permanent, so non-switchable failures stop retrying.
- Replace `gateway::is_ambiguous` string matching with a typed ambiguity check; add tests.
- Tests: table of (status, body, kind) -> class; ambiguity cases.

### Step 2: model profile
- `config.rs`: add `ModelProfile`, `ModelChoice`, `FallbackMode`; `BotConfig.profile` with `#[serde(default)]`;
  one-time in-memory migration of legacy `model`/`thinking` in `fleet.rs::rows_to_bots` so old rows keep
  working. `validate_metadata`-style `validate_profile` (<=4 fallbacks, unique choices, thinking level valid via
  `thinking::supports` when a live catalog is known).
- `api/bots.rs::update_bot`: accept `profile`; reuse validation; keep `model`/`thinking` accepted for old
  clients and map them to `profile.primary`.
- `runtime/models.rs`: `bot_model_hint` returns the primary; add `bot_route_chain(user, bot) ->
  Vec<ModelSelection>` (primary then ready fallbacks).
- `delegate.rs`: pick `route` from the specialist's chain head, class as fallback only when no primary.
- Extend `thinking.rs` first: add `anthropic` (budget tokens, adaptive levels), `google-antigravity`/Gemini
  `thinkingConfig`, `muse-code`, `minimax`, `moonshot` rules and wire shapes, because without them the
  per-bot thinking level silently does nothing for those providers. Confirm field shapes against each
  provider's current docs before coding (not verified here).

### Step 3: failover in the turn loop
- `runtime/failover.rs`: `ProviderHolds` (in-memory + `provider_holds` table, migration 0037), `next_route(
  user, bot, state.route, failure) -> Option<PinnedRoute>`, rebind via `Store::rebind_turn_route`.
- `turn_loop.rs::advance`: wrap the model call; on switchable failure record audit `model.failover`, set
  `state.route`, `update_turn`, `continue`. On no route: return a new `TurnOutcome::Waiting{until}` handled in
  `worker.rs::process_task` by `store.park_task_waiting(user, id, token, until)` (new status `waiting_model`,
  claimable when `run_after <= now`).
- Guard: failover is skipped when `state.pending` is `Some` with action `outcome_unknown`/`dispatching`.
- Mark `tasks.uncertain` through `store.mark_task_uncertain(user, id)` when a tool with `external_effect`
  has begun dispatch and the process then fails or loses its lease.
- Tests with the existing fake LLM fixtures in `runtime/tests.rs`: 429 -> fallback same provider; usage limit
  -> hold + next account; auth expired -> refresh once then fallback; model gone -> heal; policy block -> fail
  final with no switch; unknown-outcome action blocks resume; two parallel children do not share holds across
  users; cross-provider off by default.

### Step 4: bot management tools
- `bot_tools.rs` implementing `Tool` for `bot_list`, `bot_read`, `model_list`, `bot_set_model`,
  `bot_set_enabled`, `bot_set_delegates`; shared `fleet_mutation.rs` with the single validated write path used
  by REST and tools; register in `tools.rs`; filter by orchestrator in `advance` like `DELEGATE_TOOL`; add
  `bot_set_model`/`bot_set_enabled`/`bot_set_delegates` to approval-required (override `requires_approval`
  to true and `allow_standing_grant` false; keep out of YOLO via `is_destructive_tool` list or
  `auto_approve_allowed` false).
- Refuse when `state.handoff`/`state.team` is set or the turn source is a routine.
- Add the tool names to `known_tool_names()`, `bots/main.toml`, `docs/approvals.md`, and the tool notice text.
- Tests: user A's `main` cannot read or change user B's bots (UID scoping); widening tools refused; fleet
  validity preserved; audit rows written; approval binds the args.

### Step 5: delegation hardening and parallelism
- Migration `0037`/`0038`: `handoffs(id, user_id, parent_task_id, parent_turn_id, child_task_id, to_bot,
  key_hash, status, outcome_code, resume_count, created_at, updated_at)` with `UNIQUE(user_id, parent_task_id,
  key_hash)`; `provider_holds`.
- `delegate.rs`: idempotency key; write the row in the same transaction as the enqueue; `handoff_status` tool.
- `store.rs::claim_task`: per-user concurrency predicate and fair ordering; `config.rs`: relax the
  `max_concurrent_turns <= 2` rule to a configured ceiling and add `max_turns_per_user`, `max_active_children`,
  `child_daily_limit`.
- `worker.rs`: on child failure, enqueue the bounded synthetic follow-up turn to `main`.
- Tests: fairness (user A with 16 children does not block user B's single turn), duplicate delegate call
  returns the same child, resume_count cap, quota behaviour.

### Step 6: marketplace runtime
- `marketplace.rs`: schema v2 structs behind `schemaVersion: 2`; keep v1 loader; validate header names against
  a deny list (`host`, `content-length`, `cookie`), `oauth.resource` equals the app URL origin.
- `mcp_oauth.rs`: discovery, CIMD/DCR, PKCE, token exchange/refresh/revoke using the shared `reqwest` setup
  and `resolve_checked`; `api/marketplace.rs`: `POST /v1/marketplace/plugins/{slug}/oauth/start` returning the
  authorize URL and a handoff id, `POST .../oauth/complete` taking `{handoff, code, state}` from the desktop
  listener, `DELETE .../accounts/{id}`.
- `plugin_runtime.rs`: choose `McpAuth` from the manifest; use `OAuthTokens` with refresh lock; read MCP
  `annotations`; record per-plugin health in a small table or the install row; surface `needs_sign_in`.
- Desktop (`serabot-desktop/src/marketplace.rs`, `ui_marketplace.rs`, `oauth.rs`): Sign in button, account
  list, tool allow/deny toggles, state badges.
- Replace placeholder hosts one plugin at a time, only after the live host, its OAuth metadata and terms
  are checked; any manifest still on `*.example.invalid` stays blocked. Do not claim a plugin works until it
  ran against the real, owner-approved account (AGENTS.md).
- Refresh `docs/marketplace.md` and `docs/orchestration.md` (stale: says the runtime is absent, says main only
  has `delegate`).

### Step 7: channels
- `channels/mod.rs` trait plus `channels/slack.rs` (signature verification, event parsing, thread mapping,
  link codes) and later `channels/whatsapp.rs`. Routes under `api/` exempt from the session middleware but
  protected by signature verification and replay window; secrets in the server store; link table
  `channel_links(user_id, channel, external_id_hash, created_at)` created through an authenticated console
  flow, so no open self-signup is introduced.

### Cross-cutting rules to keep
- Every new store method takes `user_id`; no unscoped read of user rows. Holds and handoffs are per user.
- No secret, token or OAuth code in logs, prompts, audit details or tool output; reuse `registerSecretValue`
  style redaction already in `gateway.redact_*`.
- Model failover never bypasses `scan_model_messages`, `scan_egress` or egress approval.
- A tool or model failure message shown to the owner is a fixed code plus provider label, never a provider body.
- Placeholder hosts, `stdio` plugins and unverified OAuth stay off.

---

## 6. Open questions and risks

- Raising concurrency above 2: SQLite WAL write contention and the single age-store path are untested at 4+
  workers. Needs a load fixture before changing the default.
- Cross-provider failover default: a conservative default (off) keeps the AGENTS.md data-egress stance but
  reduces the "auto resume" value. The owner must opt in per bot or per user. Confirm with the owner.
- Anthropic and Gemini thinking wire formats must be confirmed from current provider docs; the repo has none
  for them.
- OAuth for hosted MCP servers varies (DCR vs CIMD vs fixed client). Each service must be tested live with the
  owner's own account; synthetic fixtures only prove the protocol code.
- WhatsApp Cloud API requires a Meta business setup and has template rules; it is an owner decision, not
  something to automate.
- OpenBot is noncommercial-licensed: keep to design reference, write SeraBot's own manifests and text.
- The `tasks.uncertain` gap means current lease-expiry replay safety is weaker than the docs claim; fix is in
  Step 3 and should not wait for the rest of v0.49.

## 7. File index (read for this note)

SeraBot: `crates/serabot/src/{orchestration,delegate,ask_teammate,teams,collaboration,fleet,gateway,
model_router,thinking,turn_limits,retry,llm,providers,marketplace,plugin_runtime,mcp_client,elevenlabs_oauth,
elevenlabs_mcp,token_connectors,connectors,composio,config,store,error}.rs`,
`crates/serabot/src/runtime/{mod,turn_loop,worker,models}.rs`, `crates/serabot/src/api/{bots,marketplace,
connectors}.rs`, `crates/serabot/migrations/0035_marketplace_installs.sql`, `plugins/*/plugin.json`,
`bots/*.toml`, `docs/{orchestration,model-routing,marketplace,connectors,composio,approvals}.md`,
`crates/serabot-desktop/src/{marketplace,oauth}.rs`.

OpenBot: `src/backend/agent/{model-tools,openbot-tool-router,tool-guidance,usage-limit-gate,
failure-signal,agent-settings-refused}.ts`, `src/backend/agent-service.ts` (model move logic),
`src/backend/agent-tasks/*`, `src/backend/{mcp-oauth-provider,mcp-oauth-proxy,channel-management}.ts`,
`packages/contracts/src/{slack-app,team-protocol/agent-backup-models-v1}.ts`, `marketplace/plugin-catalog/*`,
`docs/{messaging,architecture/integrations}.md`, `apps/slack-app/README.md`.
