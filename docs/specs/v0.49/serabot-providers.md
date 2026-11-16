# v0.49 research notes: providers, sign-in, multi-account, per-bot routes

Status: research notes (read-only study of the trees below). Nothing here is shipped.
Scope: SeraBot AI providers, sign-in providers (Antigravity, Claude, OpenAI Codex, Gemini,
OpenCode, Muse, Hermes), multi-account (OAuth and API), per-bot provider/model/thinking
and fallback model 1 and 2, main-bot auto-resume.

Trees read (all untrusted data, nothing executed or installed):

| Short name | Path |
|---|---|
| SeraBot | `H:/source/SeraBot/.claude/worktrees/project-analysis-performance-1db1d6` (`crates/serabot`, `crates/serabot-desktop`) |
| HielCowork | `E:/repo/Hiel Cowork` (C#; `Services/OAuth`, `Services/Providers/*`, `Services/Quota/Sources`, `docs/*.md`) |
| aio-proxy | `E:/repo/Hiel Cowork/aio-proxy` (TypeScript; `packages/plugins/*`, `website/docs/en/guide`) |
| openbot | `H:/source/openbot-hiel` (nightly-labs/openbot, TypeScript/Electron; `src/backend`, `docs`) |

Secrets rule for this file: public OAuth client ids are referenced by file and line only.
HielCowork keeps a default Antigravity "installed app" client secret as a plain constant
(`Services/OAuth/OAuthService.cs:56`). It is deliberately not copied here. SeraBot already
requires it from the age store (`antigravity_oauth_client_secret`).

Verification caveat (AGENTS.md): nothing below was run against a live provider account. Endpoint
facts come from reading code in the trees above and from SeraBot's own docs. Items marked
"UNVERIFIED" must be checked against vendor docs before coding.

---

## 1. Current SeraBot architecture

### 1.1 Layers

```
config.toml / admin store   provider PROFILES (endpoint, kind, model allowlist, egress consent)   admin-defined
age store (secrets.rs)      server-level secrets (OAuth app client secrets, Telegram token ...)
user_credentials (SQLite)   per-user, envelope-encrypted CREDENTIAL, one row per (user_id, provider)
ModelRouter (model_router)  catalogs, selection validation, pinned routes, complete_with_thinking
LlmClient (llm.rs)          OpenAI chat-completions client, plus Cloud Code envelope path
Runtime (runtime/*)         per-conversation selection (kv), turn loop, bot hint, thinking
API (api/connectors.rs ...) /v1/providers/*, /v1/models, /v1/bots/{id}
Desktop (serabot-desktop)   presets, provider cards, PKCE loopback helper, composer pickers
```

Key facts, with locations:

* Provider kinds are a closed list: `providers.rs:108-223` (`BUILTINS`): mistral, deepseek,
  minimax, minimax-cn, openai, groq, together, fireworks, openrouter, xai, cerebras, moonshot,
  gemini, perplexity, huggingface, nebius, sambanova, `muse-code`, `google-antigravity`, plus
  `openai-compatible` (custom). Everything except the last two OAuth kinds speaks
  `POST {base}/chat/completions`.
* `auth_plan()` (`providers.rs:315`): only `muse-code` (`OAuthStyle::MuseMint`) and
  `google-antigravity` (`OAuthStyle::RefreshToken`, token url pinned to
  `oauth2.googleapis.com/token`) can be `oauth`. `enforce_oauth_target()` pins the token hosts.
* Credential shape: `ProviderCredential` (`user_secrets.rs:383`) = `{api_key, refresh_token,
  expires_at?, generation?}` JSON, sealed with ChaCha20-Poly1305 under a per-user DEK; AAD is
  `credential|<user_id>|<provider>`. For OAuth providers `api_key` holds the access token.
* Storage key is `(user_id, provider)` (`migrations/0011_user_secrets.sql`, PK
  `(user_id, provider)`). `validate_provider` (`user_secrets.rs:609`) allows
  `[A-Za-z0-9._-]{1,64}`. Vault entries use reserved prefixes (`vault:`, `vaultprov:`) that
  bypass that check and are filtered out of `providers_for` (`user_secrets.rs:316`). That is
  the pattern to reuse for per-account rows.
* Rotation write-back: `UserCredentialWriter::provider(...)` (`user_secrets.rs:480-600`)
  merges the new refresh/access token under the write lock; `generation` refuses a rotation
  written for an older sign-in. The writer targets the credential OWNER (grantor for an
  admin grant), never the caller.
* Admin grants (`provider_grants`): the admin can let a member use the admin's credential for
  a provider; resolution is per call and the grantor is never revealed.
* Client construction: `UserProviderCredentials::client()` (`providers.rs:1021`) resolves
  `(user, provider)` then `open_provider_client()` (`providers.rs:1061`): builds `LlmClient`,
  installs `OAuthMaterial` for refresh, installs the credential writer, and for
  `google-antigravity` calls `with_cloud_code(project_id)` where `project_id` is a PROVIDER
  PROFILE setting (global, not per account).
* Refresh logic lives in `llm.rs`: on HTTP 401 with `oauth` installed it refreshes once and
  retries (`llm.rs:268`, `:351`, `refresh_oauth` at `:404`); Cloud Code path at
  `llm.rs:497-640`; `CloudPost::{Unauthorized, NotFound, SessionLimited, Retryable, Failed}`
  at `llm.rs:715-745`. There is no proactive expiry check beyond the stored `expires_at`.
* Model lists: `OFFICIAL_CATALOGS` (`providers.rs:45`) pins catalog hosts for mistral,
  deepseek, minimax(+cn), muse-code, perplexity; other kinds use `{base}/models`; Antigravity
  uses `POST /v1internal:fetchAvailableModels`. Cache is per `(user, provider)` in
  `ModelRouter` (TTL `catalog_ttl_secs`, fail-closed to the allowlist).
* Thinking: `thinking.rs` `levels_for(kind, model, tags)` is a conservative allow-list per
  kind (openai, mistral, openrouter, gemini, groq, xai, deepseek). `wire_fields()` emits
  `reasoning_effort` (openrouter: `reasoning.effort`; deepseek: `thinking.type` plus effort).
  Antigravity thinking is hard-coded inside the envelope builder (`cloud_code.rs:326+`):
  Claude ids containing `thinking` get a budget and the `anthropic-beta` header
  (`llm.rs:682`); Gemini 3/4 ids get a medium level. It is not user-selectable and not in
  `levels_for`. Muse has no entry at all.
* Per-conversation state: selection in kv `model_selection:<user>:<conv>`, thinking in kv
  `thinking:<user>:<conv>` (`runtime/models.rs`). User default route: `PUT
  /v1/providers/default-route` (`api/connectors.rs`). Turn route is pinned at enqueue
  (`PinnedRoute{selection, fingerprint}`, `model_router.rs:142,843`).
* Per-bot settings: `BotConfig` (`config.rs:1892`) has `model: Option<String>`,
  `thinking: Option<String>`, `role` (Specialist/Orchestrator), `delegates`. Stored as
  `config_json` in table `bots(user_id,name,config_json,prompt,enabled)`
  (`migrations/0010_user_fleet.sql`) and edited by `PUT /v1/bots/{id}` (`api/bots.rs:520`).
  There is NO provider, NO account, NO fallback field. `GET` view returns
  `fallback_provider/fallback_model` computed from `llm.classes` (`api/bots.rs:416-457`),
  which is a class heuristic (`ModelRouter::fallback_for_model`, `model_router.rs:800`), not
  a user setting.
* Delegation: `delegate.rs` (`DelegateTool`, main only, up to 16 handoffs per turn,
  `model_class` routine|synthesis, `orchestration.rs`). Subagent turns are queued with
  `teams::enqueue_turn_task_with_id`; a model failure fails that task.
* Runtime fallback today (`runtime/models.rs`): `fallback_selection` / `selection_or_fallback`
  / `heal_conversation_model` only act BEFORE a message is queued when the stored model is
  gone. The turn loop calls `router.complete_with_thinking(...)?` once (`runtime/turn_loop.rs:635`
  and `:940`); any provider error propagates and fails the turn. No mid-turn failover.

### 1.2 Providers and how each authenticates today

| Provider (kind) | Member-facing auth | Where tokens go | Notes |
|---|---|---|---|
| openrouter | Sign in (PKCE, user flow) or API key | per-user row (`api/connectors.rs:439-500`) | Only provider with a user sign-in. `user_sign_in_kind()` hard-codes `openrouter`. Code exchange is server-side (`openrouter.rs`). |
| google-antigravity | none for members | age store names `antigravity_access_token`, `antigravity_refresh_token`; startup import copies to the ADMIN's row (`import_age_provider_credentials`, `providers.rs:1140`) | Admin-only: `POST /v1/admin/providers/{id}/oauth/start|complete` (`api/admin.rs:205-370`) or CLI `serabot providers login google-antigravity` (`main.rs:287`). Response says `restart_required: true`. |
| muse-code | none for members | same age-store route (`muse_code_api_key`, `muse_code_identity`) | Device login (`login_muse_code`, `providers.rs:767`) bounded 900 s, then key mint `POST https://api.meta.ai/muse-code/key`. Admin-only. |
| openai, mistral, deepseek, gemini, xai, groq, ... | API key per user (`PUT /v1/providers/{id}/key`) | per-user row | ChatGPT-plan sign-in not implemented. |
| anthropic | NOT SUPPORTED | - | `docs/providers.md` "Not added": router speaks chat-completions only; Claude reachable only via OpenRouter/HF. |
| openai-codex (ChatGPT plan) | NOT SUPPORTED | - | Needs Responses API wire format. |
| opencode (Go / Zen) | NOT SUPPORTED | - | Only reachable by hand as `openai-compatible`; Go needs an extra header (see 3.5). |
| hermes (Nous) | NOT SUPPORTED | - | UNVERIFIED, see 3.7. |
| ollama / custom | none or key | per-user row | HTTPS or exact loopback. |

Desktop side (`crates/serabot-desktop/src`): `provider_presets.rs` carries `sign_in`
("pkce", "device", "pkce_google"); `ui/providers.rs:183` only special-cases `pkce`;
`oauth.rs` is the loopback PKCE listener (port 51121 preferred, `/oauth-callback`, state check,
50 ignored-request cap) and hands the code to the server which does the exchange (client
secret never reaches the desktop). `api.rs:4969-5000` has `provider_oauth_start/complete` for
the member route and `checked_oauth_start` (accepts `pkce` or `device` flows).

### 1.3 Defects and gaps (sign-in providers)

Numbered so the plan can reference them.

D1. One credential per `(user, provider)`. A user cannot hold two Google accounts, two OpenAI
    keys, or an API key beside an OAuth login for the same provider. `set_provider_key`
    even documents that setting a key REPLACES an OAuth pair (`api/connectors.rs:503`).
D2. Antigravity and Muse sign-in are admin-only and written to the age store, not the
    per-user store. Members cannot sign in at all. `restart_required: true` shows the
    credential is not hot-reloaded. The admin's login is then imported ONCE into the
    admin's user row, with a `kv` marker, so a later re-login via age store is not
    re-imported after the first successful import (`providers.rs:1156-1176` skips when the
    marker exists). Re-login after expiry therefore cannot update the live credential
    through that path (verify; the `/oauth/complete` handler writes the age store only).
D3. Antigravity `project_id` is a profile-level setting (`set_provider_project_if_empty`).
    Cloud Code project (`cloudaicompanionProject`) is per Google account. Two accounts or two
    users would share one project id, which is wrong and a cross-user data/identity
    leakage risk. Must be per account.
D4. Antigravity thinking is not user-controllable and not in the thinking catalog
    (`thinking.rs` has no `google-antigravity` arm); the picker offers no levels for it. Claude
    models served through Antigravity and Gemini 3/4 both have levels in HielCowork
    (`AntigravityRequestTranslator.cs:363-428`).
D5. Antigravity session handling: Cloud Code accumulates tokens per `sessionId` up to ~1M and
    answers HTTP 400 "exceeds the maximum number of tokens". SeraBot handles this
    (`CloudPost::SessionLimited`), but `sessionId` is not derived from
    (account, conversation) like HielCowork's FNV-1a derivation, so a second account would
    collide. Thought signatures are not stored (`docs/antigravity.md` Deferred), so
    multi-step Claude/Gemini thinking turns replay the sentinel only.
D6. Muse: no thinking levels, no quota read, no refresh path (identity token is long-lived;
    the key endpoint is not a refresh). A 401 on catalog re-mints (`llm.rs`
    `catalog_401_mints_a_muse_key`). The mint with `onboard` must happen only at login
    (aio-proxy design note), SeraBot's re-mint on 401 omits that distinction; verify.
D7. No Anthropic Messages client and no OpenAI Responses client. Claude API keys, Claude
    sign-in, ChatGPT/Codex sign-in all need new wire formats (chat-completions is not enough:
    Codex backend is Responses-only and SSE-only; Anthropic native has signed thinking blocks).
D8. No account identity metadata (email, plan, org, project, expiry) is stored beside the
    sealed token, so the UI cannot show "which account is this", cannot dedupe a second
    login of the same account, cannot show reauth-needed. `credential_status()` is row
    existence only.
D9. No per-bot provider or account; `bots.config_json` has `model`/`thinking` only. The
    "default provider with a default model and thinking level" and "fallback 1, 2" do not
    exist. Provider is inferred from the model id (`provider_for_model`), ambiguous when two
    providers list the same model id.
D10. No mid-turn failover and no automatic resume of a failed subagent turn. A 429, 5xx,
    expired token, exhausted quota, or removed model fails the turn (`turn_loop.rs:635 ?`).
    Only pre-queue healing exists.
D11. `thinking::levels_for` has no kind for anthropic, openai-codex, antigravity, muse,
    opencode, hermes. Levels also are not tied to the ACCOUNT's plan (a free account may not
    allow `xhigh`).
D12. `list_providers` (`api/connectors.rs:17`) builds secrets "status" from age-store names
    (`visible_secret_status`) only for admins; members see `credential_view`. No per-account
    list exists.
D13. `docs/providers.md` and `docs/model-routing.md` state "There is no automatic
    cross-provider fallback" (intended, now to be changed under explicit user consent per
    fallback provider).
D14. OpenRouter-only `user_sign_in_kind` means the generic `/v1/providers/{id}/oauth/*`
    contract exists for members but has one implementation; the contract (PKCE: start returns
    URL, desktop loopback, complete exchanges) is reusable for Antigravity, Codex, Claude.
    The device flow variant exists only in the admin handler (`flow: "device"` plus a
    background poll task in `api/admin.rs:231-300`), and it writes the age store.
D15. Egress: `data_egress_approved` is per provider profile (admin config). Fallback chains
    cross providers, so each fallback needs consent surfaced to the USER, who is the data
    owner. Today consent is an admin setting only.

---

## 2. Reference implementations studied

### 2.1 HielCowork (C#), the "follow this" reference for Antigravity and Muse

Files: `Services/OAuth/OAuthService.cs` (1694 lines), `Services/OAuth/LoopbackCallbackServer.cs`,
`Services/OAuth/CliSessions.cs`, `Services/Providers/Antigravity/*`,
`Services/Providers/ClaudeCode/*`, `Services/Providers/OpenCode/OpenCodeProviderFactory.cs`,
`Services/OpenCode/OpenCodeProxyService.cs`, `Services/Quota/Sources/*`,
`docs/antigravity-integration.md`, `docs/research-opencode.md`,
`docs/claude-subscription-compliance-audit.md`.

Model: one `ProviderConfig` row per account (type + `AccountId` + `AccessToken` +
`RefreshToken` + `TokenExpiresAt` + `SignedInEmail`). "Multi account" = several provider rows
of the same `ProviderType`. `OAuthService.GetValidAccessTokenAsync` (`:448`) refreshes if the
token is within a skew of expiry; `RefreshAsync` (`:1081`) dispatches per type. On
`invalid_grant`/400/401 it tries to ADOPT a newer refresh token from the linked CLI session
file (`CliSessions`, `AdoptLinkedCli`), else signs out and asks to sign in again.

#### Antigravity (Google Cloud Code PA)

* Authorize: `https://accounts.google.com/o/oauth2/v2/auth` (PKCE S256, `access_type=offline`,
  `prompt=consent`). Token: `https://oauth2.googleapis.com/token`. Userinfo:
  `https://www.googleapis.com/oauth2/v1/userinfo?alt=json` (email, used as the account label).
  Constants at `OAuthService.cs:52-65`; public client id also in SeraBot
  `cloud_code.rs:24`. Callback: loopback `127.0.0.1:51121/oauth-callback`
  (`OAuthService.cs:59`, SeraBot `cloud_code.rs:33-34` and desktop `oauth.rs`).
* Scopes (space separated): cloud-platform, userinfo.email, userinfo.profile, cclog,
  experimentsandconfigs (identical to SeraBot `cloud_code.rs:26`).
* Refresh: `grant_type=refresh_token` with client id and the installed-app client secret to
  the token url. Google normally does not rotate the refresh token; handle both.
* Project onboarding (`EnsureAntigravityProjectAsync`, `OAuthService.cs:1502`): `POST
  {origin}/v1internal:loadCodeAssist` with metadata `{ideType:"ANTIGRAVITY", ideVersion,
  platform:"WINDOWS_AMD64"}`; if no `cloudaicompanionProject`, pick `allowedTiers` default
  (or `free-tier`) then `POST /v1internal:onboardUser` and poll until
  `response.cloudaicompanionProject.id`. Discovery origins: daily then prod
  (`AntigravityConstants.ProjectDiscoveryEndpoints`). SeraBot has the same flow in
  `cloud_code.rs` `discover_project` (~line 1355); store the result per account.
* Hosts, in order: `daily-cloudcode-pa.googleapis.com`, `cloudcode-pa.googleapis.com`,
  `daily-cloudcode-pa.sandbox.googleapis.com`, `autopush-cloudcode-pa.sandbox.googleapis.com`.
  (The HielCowork doc `antigravity-integration.md` 3.2 lists a different order; the code
  constant `AntigravityConstants.cs` and SeraBot `cloud_code.rs:37` agree with each other,
  trust those.) Fail over to next host on 429/403/5xx.
* Chat: `POST {origin}/v1internal:streamGenerateContent?alt=sse`. Headers: `Authorization:
  Bearer`, `Accept: text/event-stream`, `User-Agent: antigravity/hub/<ver> (aidev_client;
  os_type=windows; arch=amd64; cl=<n>)`, `X-Goog-Api-Client: google-cloud-sdk vscode/1.96.0`.
  `<ver>` is resolved at runtime from the public hub updater endpoint
  (`AntigravityConstants.UpdaterUrl`) with a hard-coded fallback; SeraBot hard-codes
  `2.25.0` (`cloud_code.rs:27,32`). A stale UA is a likely future breakage; add a
  version probe with the same 3 s cap and fallback.
* Envelope (`AntigravityRequestTranslator.cs:296-313`): `{requestId:"agent/<ms>/<8hex>",
  model, userAgent:"antigravity", requestType:"agent", project?, request:{contents,
  systemInstruction, tools, toolConfig, generationConfig, sessionId}}`. Omit `project` when
  unknown (empty string gives 404). `sessionId` = negative decimal string from FNV-1a 64
  over `account:conversation[:genN]` folded below 9e18; on HTTP 400 "exceeds the maximum
  number of tokens" bump N and retry once on the same host.
* Thinking mapping (`AntigravityRequestTranslator.BuildGenerationConfig`, `:363`):
  Gemini 3/4 ("level" models): off -> `thinkingConfig:{thinkingBudget:0}`; low/medium/high
  -> `{thinkingLevel, includeThoughts:true}` (max maps to high). Claude `*-thinking`: budget
  tokens (docs: 8192 low / 16384 medium / 32768 high), clamp to [1024, maxOutput-1024],
  `includeThoughts:true`, no `temperature` when thinking, add header
  `anthropic-beta: interleaved-thinking-2025-05-14`. `maxOutputTokens` up to 64000 (Claude)
  or 65536 (Gemini).
* SSE parsing: parts with `thought:true` are reasoning; `functionCall` parts carry optional
  `thoughtSignature` (store and replay; fall back to
  `skip_thought_signature_validator` for foreign history); `finishReason`
  `MALFORMED_FUNCTION_CALL` with empty output: re-sample up to 3 times; map STOP/MAX_TOKENS/
  SAFETY to stop/length/content_filter; tool results are `role:user` parts with
  `functionResponse{id,name,response}` and consecutive results merged.
* Models seen in HielCowork catalog: `claude-opus-5-5(-thinking)`, `claude-sonnet-5-5(-thinking)`,
  `gemini-4-argon`, `gemini-3.8-flash`, `gpt-oss-120b-medium`; legacy aliases normalized. SeraBot
  must not rewrite ids (docs/antigravity.md) but a small alias table for retired ids is useful
  as a failover helper.
* Quota (`AntigravityQuotaSource.cs`): `POST /v1internal:fetchAvailableModels
  {project}` returns `models.<id>.quotaInfo.{remainingFraction, resetTime}`. Use per account
  to gate selection and to show a meter. SeraBot already calls this endpoint for the catalog;
  parse `quotaInfo` in the same response.
* Local CLI discovery (`CliSessions.ReadAntigravity`) scans home directories for the CLI's
  tokens. Do NOT port: SeraBot rules forbid reading local credential files and the server
  runs remotely.

#### OpenAI Codex (ChatGPT plan)

* Authorize `https://auth.openai.com/oauth/authorize` (PKCE S256, `response_type=code`,
  scope `openid profile email offline_access`, `id_token_add_organizations=true`; aio-proxy
  also sends `codex_cli_simplified_flow=true`, `originator=codex-tui`). Fixed redirect
  `http://localhost:1455/auth/callback`. Token `https://auth.openai.com/oauth/token` (form:
  authorization_code + code_verifier; refresh: grant_type=refresh_token + client_id). Public
  client id at `OAuthService.cs:13`. Device flow for headless: `.../api/accounts/deviceauth/
  usercode`, `/token`, user page `https://auth.openai.com/codex/device`
  (`OAuthService.cs:24-28`, `SignInCodexDeviceAsync :678`).
* Account id: JWT claim in id_token/access_token (`JwtPayload.ExtractChatGptAccountId`);
  needed as header `ChatGPT-Account-Id`. Email also from JWT. Expiry from JWT `exp`.
* Inference (aio-proxy `openai-chatgpt/src/runtime/runtime.ts`): base
  `https://chatgpt.com/backend-api/codex` (`/responses`), `Authorization: Bearer`,
  `ChatGPT-Account-Id`, `Originator: codex-tui`, `session-id` header (stable per
  conversation, sha256 of a cache key), codex-style User-Agent whose version token is
  gated by the models endpoint (`minimal_client_version`). Responses API, streaming only.
  Refresh tokens ROTATE and are single-use: refresh must be single-flight per account with
  atomic write-back, otherwise two parallel turns invalidate each other. The existing
  `generation` guard in `ProviderCredential` is the right primitive.
* Thinking: Responses `reasoning:{effort: low|medium|high|xhigh, summary:"auto"}` and
  `include:["reasoning.encrypted_content"]`; replay encrypted reasoning items on tool
  loops; `store:false`.
* Quota: `CodexQuotaSource.cs` reads plan windows (primary/secondary, additional per-model
  limits). aio-proxy plan `2026-10-02-quota-aware-selection.md`: hold a candidate while a
  scoped window is at 0 remaining with a future reset.
* Policy: HielCowork `OAuthService.cs` comment says sign-in is offered only where the service
  allows a third-party app. OpenAI's stance is not stated in the trees read; UNVERIFIED, the
  owner must confirm terms before shipping.

#### Muse Code (Meta)

* Device flow (RFC 8628): `POST https://auth.meta.com/oidc/device/authorization/` (client id
  `1031625952748946`, `OAuthService.cs:41-44`, SeraBot `providers.rs:32`), poll
  `.../oidc/device/token/`, then `POST https://api.meta.ai/muse-code/key` with
  `Authorization: Bearer <identity token>` and body `{"onboard": true}` ONLY at login.
  Headers `x-api-version: 1.0.0`. The minted `api_key` is the inference bearer; the identity
  token is what is stored for re-mint (HielCowork `RefreshAsync` re-mints from the stored
  identity token). aio-proxy design (`docs/superpowers/specs/2026-09-06-muse-oauth-design.md`):
  no refresh grant, no re-mint on normal paths, `onboard:true` only after interactive login.
* Inference: Responses API `https://api.meta.ai/v1` (aio-proxy uses `openai.responses`);
  HielCowork treats it as OpenAI-compatible chat. SeraBot's current chat-completions path may
  work for plain chat but `custom` tools are rejected on this endpoint (aio-proxy note) and
  agentic surface is Responses: verify before declaring tool use "perfect".
* Models: `GET https://api.meta.ai/v1/models` (Muse Spark 1.1 to 1.3 and contributor
  variants in HielCowork `ProviderCatalog.cs:112,204`). Thinking: effort ladder, `xhigh` is
  already seen in SeraBot for groq `muse-spark`. UNVERIFIED which ladder the Meta endpoint
  accepts; take it from the live models response when present.
* Quota: `POST /muse-code/key` WITHOUT `onboard` returns `subs_usage` windows with
  `resets_at` (`MuseQuotaSource.cs:97-140`).

#### Claude (Claude Code sign-in)

* HielCowork chose the opposite of a token reuse: `ClaudeCliAuth.cs` shells out to `claude
  auth status --json` / `claude auth login` and never reads or copies credentials for chat;
  chat runs through the Claude Agent SDK sidecar (`ClaudeCodeProvider.cs`, Node sidecar).
  Its own audit (`docs/claude-subscription-compliance-audit.md`, 2026-10-10) puts the
  feature ON HOLD: Anthropic does not allow third-party developers to offer claude.ai login
  or plan rate limits, and the sidecar approach was judged closest to circumvention.
* aio-proxy `packages/plugins/claude-code/src/oauth` implements raw OAuth anyway:
  authorize `https://claude.ai/oauth/authorize` (PKCE S256, `code=true`, scope
  `org:create_api_key user:profile user:inference user:sessions:claude_code
  user:mcp_servers user:file_upload`, loopback `localhost:54545/callback`), token
  `https://api.anthropic.com/v1/oauth/token` (JSON body; refresh with client id and refresh
  token; header `anthropic-beta: oauth-2025-04-20`), identity via the token response
  `account.uuid/email_address` and `organization.uuid/name`, fallback bootstrap `GET
  /api/claude_cli/bootstrap`. Inference on `https://api.anthropic.com/v1/messages` with
  `Authorization: Bearer <access>` plus `anthropic-beta: oauth-2025-04-20` plus
  `anthropic-version: 2023-06-01`. Account dedupe fingerprint = sha256(`account:<uuid>`);
  the plugin refuses a login that returns no stable account id. Its docs carry a warning
  that third-party use risks suspension. Client id is injected at build time
  (`__AIO_PROXY_CLAUDE_CLIENT_ID__`, `constants.ts`), not committed.
* openbot (`docs/provider-terms.md`, read 2026-10-06 by that project): runs the unmodified
  `claude` CLI via the Agent SDK, sign-in is `claude auth login --claudeai` run by the CLI,
  credentials are never read, copied or stored by OpenBot. It quotes Anthropic's legal page:
  developers building products should use API-key auth; third parties may not offer claude.ai
  login or route requests through plan credentials on behalf of users; no collecting or
  intermediating claude.ai credentials. It rates "team members use Claude agents on your
  host" as HIGH risk. API key, Bedrock, Vertex, Foundry are the endorsed paths.
* Conclusion for SeraBot: see section 3.2. The plan-OAuth path is a policy risk, not a
  technical gap. The API-key path is clean and fully supported.

#### OpenCode

* Two lanes (`OpenCodeProviderFactory.cs`): Go (subscription) at `https://opencode.ai/zen/go/v1`
  needs header `x-opencode-session: ses_<12 hex><14 base62>` on EVERY request (rejected
  without it since 2026-09-06); key from the user's OpenCode sign-in at
  `https://opencode.ai/auth` or pasted. Zen (pay-as-you-go) at `https://opencode.ai/zen/v1`,
  plain OpenAI-compatible. aio-proxy `opencode-go` plugin does the same
  (`x-opencode-session` from the request's session key).
* The "Free" no-key lane needed a spoofed UA, forged session id and decoy tools; HielCowork
  moved it to a local proxy sidecar and flags it as impersonation. Do NOT build this.
* HielCowork reads `opencode/auth.json` and `opencode.db` from disk for import. Do NOT port.

#### Gemini

* HielCowork offers Gemini API-key plus Antigravity OAuth (Google AI plan). The consumer
  Gemini OAuth for the Gemini API is not offered (needs a Google Cloud project per user;
  SeraBot `docs/providers.md` agrees). "Gemini sign-in" for SeraBot therefore means the
  Antigravity account (Gemini 3/4 models through Cloud Code), not a new Google OAuth.

### 2.2 aio-proxy (TypeScript), the multi-account and failover reference

* Account model: every provider id owns one credential (one login). Multiple accounts =
  multiple provider ids of the same plugin, each with a fingerprint derived from a STABLE
  account identity (`claudeLoginResult` builds `fingerprint = sha256("account:<uuid>")` and
  `suggestedKey = claude-<12 hex>`), so logging into the same account twice is detected and
  does not create a duplicate. Credential port: `read()` and `refresh(revision, fn)` with a
  revision counter for single-flight refresh (`currentClaudeCredential` in
  `claude-code/src/oauth/credential.ts`).
* Routing (`website/docs/en/guide/routing/routing-rules.md`): per provider `priority`
  (higher first) and `weight` (traffic split inside a tier); `providerId/model` bypasses
  routing; failover walks remaining candidates then next tier; session affinity pins a
  conversation to one provider for cache hits; cooldown after failures; quota-aware hold
  (plan `2026-10-02-quota-aware-selection.md`): candidate is held while a fresh snapshot
  (age <= 10 min) shows an in-scope window at 0 remaining with a future reset; unknown or
  stale quota never holds; all held -> synthesized 429 with `Retry-After`.
* Take from it: priority + weight per account, stable-identity fingerprint, single-flight
  refresh with revision, affinity, cooldown, quota hold with trust window. Do not take: its
  proxy surface, global per-model overrides.

### 2.3 openbot (TypeScript/Electron)

* Architecture: openbot does not implement providers as HTTP clients. It runs each vendor's
  own CLI/agent as a child process over ACP (Agent Client Protocol) or the Claude Agent SDK
  (`src/backend/claude-client.ts`, `acp-client.ts`, `acp-sign-in.ts`, `cli.ts`,
  `acp-flavor-antigravity.ts`, `acp-flavor-opencode.ts`) and signs in by invoking the CLI's
  own login (`claude auth login --claudeai`, Codex "sign in with ChatGPT" in the CLI, ACP
  `authenticate` for Antigravity), never touching tokens. Therefore: "claude/opencode/
  antigravity as openbot does it" is a local-process model. SeraBot's AGENTS.md forbids a
  local-process plugin runner without a new security design and has no Node runtime, so
  this model is NOT directly portable. What IS portable:
  * Error taxonomy per provider (`acp-flavor-*.ts`): map raw provider failures to stable
    user-facing kinds: rate_limited, quota_reached (with reset time), model_unavailable,
    service_failure, billing, credentials_rejected, unreadable_content, auth_required
    (provider derived from `provider/model` id). These drive failover classification (4.3).
  * Provider isolation: plan `006-provider-loss-ends-only-its-own-work.md`: when one provider
    process dies only that provider's agents fail, others keep going; some agents have
    "their own process" (e.g. "a Codex agent on a second account"), i.e. multi-account is an
    agent-level binding. Matches the per-bot account pin in 4.2.
  * API keys for agents: `src/backend/agent-api-keys.ts`: per-agent key files, value never in
    a tool argument or result, registered for redaction. SeraBot equivalent already exists
    (gateway redaction, vault).
  * Subagents and orchestrator: Slack/Discord/Telegram "Orchestrator" agent answers short
    requests itself and hands other work to the best agent (`docs/messaging.md`).
* Messaging connectors (user also asked for WhatsApp/Slack): openbot supports Slack, Discord
  and Telegram only (no WhatsApp in `docs/messaging.md`). Design: one app/bot per platform,
  a relay service ("Signal") holds platform tokens and forwards events to the host; a
  per-platform Orchestrator agent in an "Integrations" section; thread = conversation;
  reaction states (eyes = received, check/cross/stop = outcome); approvals as buttons only
  the requester can press; one workspace answers to one host; disconnect revokes the token.
  See `docs/messaging.md` and `docs/architecture/channels-and-messaging.md`. SeraBot has
  `telegram.rs` only; Slack/Discord/WhatsApp are separate work items (out of scope here,
  noted for the roadmap; WhatsApp needs Meta Business API or an unofficial bridge, the
  latter is a policy risk).
* Hermes (Nous Research): openbot's comparison page
  (`apps/auth-api/src/content/compare/hermes-agent.ts`) states Hermes Agent uses API keys
  from many providers and "signs in with ChatGPT, SuperGrok or GitHub Copilot". Nothing in any
  tree describes a "Hermes" inference provider endpoint or OAuth. See 3.7.

---

## 3. Per-provider target spec for SeraBot

Auth flow families (all member-scoped, results land in the per-user account store of 5.1):

* F-PKCE-LOOPBACK: server builds the authorize URL, desktop owns the loopback listener and
  state check, server exchanges code and verifier (existing contract, `oauth.rs` +
  `provider_oauth_start/complete`). Needed for Antigravity (51121), Codex (1455 fixed),
  Claude (54545, if enabled).
* F-DEVICE: server starts device authorization, returns `{url, user_code, interval,
  expires_in, flow_id}`, a bounded background task polls, writes the account row, emits a
  `provider.signin` event. Needed for Muse; Codex device variant for headless/remote.
* F-KEY: pasted key, validated by one catalog call, then sealed.

### 3.1 Antigravity (kind `google-antigravity`, flow F-PKCE-LOOPBACK)

Endpoints, scopes, headers: section 2.1 (identical to what `cloud_code.rs` already encodes).
Changes required:
1. Move sign-in to the member path; store per account: access, refresh, `expires_at`,
   `generation`, plus non-secret meta `{email, project_id, tier, user_agent_version}`.
2. Discover the project at sign-in AND lazily at first use when missing, per account
   (D3). Never read `project_id` from the profile for member accounts; profile value
   remains only as an admin/default seed.
3. Derive `sessionId` from `(account_id, conversation_id, generation)`; keep per-process
   generation counters in memory plus persisted `generation` bump when the 1M wall hits.
4. Add `google-antigravity` arm to `thinking::levels_for` and a wire mapper in `cloud_code`:
   Gemini 3/4: `off|low|medium|high` (max -> high; also `minimal` for 3.8 flash per docs);
   Claude thinking ids: `low|medium|high` -> budgets 8192/16384/32768, `off` -> choose
   non-thinking sibling id if the catalog has one, else omit thinkingConfig.
5. Store and replay `thoughtSignature` (new `reasoning_signature` field on stored assistant
   messages) so multi-step thinking turns validate (D5).
6. Quota: parse `quotaInfo.remainingFraction/resetTime` from `fetchAvailableModels` into the
   account row (`quota_json`), feed the account selector (4.4).
7. UA version probe with 3 s timeout and fallback (HielCowork pattern).
8. Errors: map to the taxonomy in 4.3 (403 -> account_forbidden or host fallback first;
   429 -> rate_limited with `Retry-After`; 404 model not found -> model_unavailable; 401
   after refresh -> needs_reauth).
Client secret: stays server-level in the age store (not per user, not committed).

### 3.2 Claude (two kinds, decision required)

A. `anthropic` (API key, multi-account): new wire client for the native Messages API.
   * `POST https://api.anthropic.com/v1/messages`, headers `x-api-key`, `anthropic-version:
     2023-06-01`, `content-type: application/json`; stream with `stream:true` (SSE events
     message_start, content_block_start/delta/stop, message_delta, message_stop).
   * Request: `system` (string/blocks), `messages` with content blocks `text`, `tool_use`,
     `tool_result`, `thinking` (signed) and `redacted_thinking`; `tools:[{name, description,
     input_schema}]`; `max_tokens` required.
   * Thinking: `thinking:{type:"enabled", budget_tokens}` (>= 1024 and < max_tokens; no
     temperature change) mapped from levels (low 4k / medium 10k / high 24k / xhigh 32k as
     defaults, clamp by model). Signed thinking blocks must be passed back byte-identical on
     tool-use turns; strip them when crossing providers (4.3). UNVERIFIED: newer models may
     expose an `effort` style parameter; read the live docs and the `GET /v1/models`
     `capabilities` fields when implementing.
   * Catalog: `GET https://api.anthropic.com/v1/models` (`x-api-key`, `anthropic-version`,
     cursor pagination `limit`). Add to `OFFICIAL_CATALOGS`-style pinning
     (`api.anthropic.com`).
   * Errors: 401 `authentication_error` (key rejected), 403 `permission_error`, 404
     `not_found_error` (model), 413, 429 `rate_limit_error` (+ `retry-after`), 500, 529
     `overloaded_error`. Map to taxonomy.
   * `docs/providers.md` "Not added" section is superseded by this kind (update the docs).
B. `claude-plan` (Claude Code / claude.ai OAuth, multi-account): technically straightforward
   (section 2.1/2.2: PKCE, token url, bearer + `anthropic-beta: oauth-2025-04-20`, same
   Messages client as A). Policy blocker: the sources read (openbot `provider-terms.md`,
   HielCowork compliance audit) both conclude Anthropic does not permit third-party apps to
   offer claude.ai login or route requests via plan credentials, and openbot rates multi-user
   hosting as high risk. SeraBot is exactly that shape (server holds the token, multi-user).
   Recommendation: implement A fully (multi API-key accounts, plus Bedrock/Vertex later);
   implement B only behind `[llm.providers.<id>] plan_oauth = true` default false,
   admin-gated, with an in-UI risk banner and per-account "own use only" (never granted to
   others via `provider_grants`), and ship it OFF. If the owner wants the supported route,
   the only supported shape is a local unmodified Claude Code binary on the USER's own PC
   (openbot style). That needs a new security design for a local-process runner on the
   desktop (not allowed today). Record as an owner decision, do not silently enable.
Multi-account for Claude: A = N API-key accounts (label, workspace); B = N plan accounts
fingerprinted by `account.uuid` (aio-proxy method).

### 3.3 OpenAI Codex (kind `openai-codex`, flows F-PKCE-LOOPBACK + F-DEVICE)

Endpoints and headers: section 2.1. Needs a new Responses client (SSE only,
`store:false`, `instructions`, `input` items, `reasoning`, `include` encrypted reasoning).
Account meta: email, `chatgpt_account_id`, plan (from JWT claims), expiry from JWT. Refresh
single-flight with rotation (token is replaced every refresh; write-back before releasing
the lock). Levels: off? (no) `low|medium|high|xhigh` (model gated). Models from the codex
`models` endpoint with `client_version` query (gated by `minimal_client_version`).
Quota windows into account row. Policy: UNVERIFIED terms for third-party use; ship behind an
admin flag like 3.2B until confirmed.

### 3.4 Muse (kind `muse-code`, flow F-DEVICE)

Keep device flow and mint (already correct), but: member-scoped storage, per-account meta
(email if present, plan, `subs_usage` snapshot), re-mint only through an explicit "Reconnect"
(login) path, quota refresh by non-onboard mint call, 401 -> needs_reauth (no silent
`onboard:true`). Evaluate switching inference to the Responses API for agentic tool use
(UNVERIFIED). Add thinking levels from the models response; default none.

### 3.5 OpenCode (kinds `opencode-go`, `opencode-zen`, flow F-KEY)

Plain OpenAI-compatible with a request hook: for Go set `x-opencode-session: ses_` +
12 hex + 14 base62, stable per conversation (derive from conversation id so a retry reuses
it). Bases `https://opencode.ai/zen/go/v1` and `https://opencode.ai/zen/v1`. Key from
`https://opencode.ai/auth` (paste; there is no documented third-party OAuth in the trees
read; aio-proxy's `opencode-go` "OAuth" plugin is a key page plus paste: UNVERIFIED if a real
code flow exists). Catalog `GET {base}/models`. No Free lane, no spoofing, no CLI file
import. Add to `NON_CHAT_ID_HINTS` filter as needed.

### 3.6 Gemini

Two accounts types: `gemini` API key (exists), and Antigravity (3.1) for plan access. No new
Google OAuth.

### 3.7 Hermes (UNVERIFIED)

No tree documents a Hermes inference endpoint. Candidate meaning: Nous Research's Hermes
models served by Nous Portal (OpenAI-compatible) or via OpenRouter. Action for the
implementer: read `https://hermes-agent.nousresearch.com/docs` and Nous portal docs first.
Safe default: preset kind `openai-compatible` with the documented base URL, API-key account,
catalog `GET {base}/models`. Do not claim a Hermes OAuth until the docs state one. Note that
"Hermes Agent" (the agent) is a competitor product, not a provider.

### 3.8 Existing providers: small fixes while in there

* Add `anthropic`, `openai-codex`, `opencode-go`, `opencode-zen`, `nous` (placeholder, API
  key) to `BUILTINS`, `canonical_kind`, desktop presets (`provider_presets.rs`).
* Thinking tables: add arms for anthropic (budget), openai-codex (effort), antigravity,
  muse, opencode (passthrough `reasoning_effort` only when catalog tags say `reasoning`).
* `docs/providers.md`: fix "Anthropic: Not added", Gemini/Antigravity/Muse rows to say
  "sign in per member", add policy notes.

---

## 4. Plan: data model, API, routing, UI

### 4.1 Data model (migration `0037_provider_accounts.sql`)

Latest migration in tree: `0036_console_pages.sql`. SQLite/WAL, all rows UID-scoped.

```sql
BEGIN;

CREATE TABLE IF NOT EXISTS provider_accounts (
    user_id        TEXT NOT NULL REFERENCES users(id),
    provider       TEXT NOT NULL,              -- provider profile id (config key)
    account_id     TEXT NOT NULL,              -- uuid v7, stable; part of sealed-row key
    label          TEXT NOT NULL,              -- user-visible name, default = email or "Key 1"
    auth_kind      TEXT NOT NULL CHECK (auth_kind IN ('api_key','oauth','device','minted')),
    fingerprint    TEXT NOT NULL,              -- sha256 of stable identity (never of a secret)
    identity_json  TEXT NOT NULL DEFAULT '{}', -- non-secret: email, plan, org, project_id, tier
    status         TEXT NOT NULL DEFAULT 'ok'
                   CHECK (status IN ('ok','needs_reauth','rate_limited','quota_exhausted','disabled')),
    priority       INTEGER NOT NULL DEFAULT 0, -- higher tried first
    weight         INTEGER NOT NULL DEFAULT 1,
    cooldown_until TEXT,                       -- RFC3339, set on 429/overload
    quota_json     TEXT,                       -- last snapshot {windows:[{scope,remaining,resets_at}], sampled_at}
    last_error     TEXT,                       -- stable code only, never provider text
    last_used_at   TEXT,
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL,
    PRIMARY KEY (user_id, provider, account_id)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_provider_accounts_fp
    ON provider_accounts(user_id, provider, fingerprint);
CREATE INDEX IF NOT EXISTS idx_provider_accounts_user ON provider_accounts(user_id, provider, status);

-- bots keep config_json; new route fields live inside it (serde defaults), no SQL change.

CREATE TABLE IF NOT EXISTS route_events (       -- audit-adjacent, for the chat banner and debugging
    id            TEXT PRIMARY KEY,
    user_id       TEXT NOT NULL REFERENCES users(id),
    conversation_id TEXT, task_id TEXT, bot TEXT,
    from_provider TEXT, from_model TEXT, from_account TEXT,
    to_provider   TEXT, to_model TEXT, to_account TEXT,
    reason        TEXT NOT NULL,                -- taxonomy code
    created_at    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_route_events_conv ON route_events(user_id, conversation_id, created_at);

COMMIT;
```

Secrets stay in `user_credentials`, keyed by a reserved provider id
`acct:<provider>:<account_id>` (<= 64 chars: shorten with a 12-hex account id). Mirror the
vault pattern: new `UserSecrets::set_account/get_account/delete_account` that bypass
`validate_provider` for the reserved prefix; add `ACCOUNT_PROVIDER_PREFIX` to the exclusion
list in `providers_for`; AAD stays `credential|user|<reserved id>` so a blob cannot move
between accounts or users. Admin still cannot enumerate: no list method returns values.
`ProviderCredential` gains `account_id` awareness only in the writer (target row), not the
JSON. `provider_accounts.identity_json` holds ONLY non-secret fields.

Legacy migration: on first read, an existing `(user, provider)` row is exposed as a virtual
account `legacy` (account_id `default`) mapped to the old row; first edit moves it into the
new scheme (copy-then-delete in one transaction). `provider_grants` keeps granting the admin's
`default` account only. `import_age_provider_credentials` writes into a new account with
fingerprint = sha256 of the Google/Meta identity when known, else of the age name.

Bots (inside `BotConfig`, `config.rs:1892`; all `#[serde(default, skip_serializing_if)]`):

```rust
pub struct BotRoute {
    pub provider: String,            // provider profile id
    pub model: String,
    pub thinking: Option<String>,    // validated against thinking::levels_for for THIS route
    pub account: Option<String>,     // None = pool (priority/weight), Some = pinned
}
pub struct BotConfig {
    // existing: model, thinking stay as the legacy single-route fields (read as primary when
    // `route` is None; written both ways until the console migrates).
    pub route: Option<BotRoute>,          // default provider + model + thinking
    pub fallbacks: Vec<BotRoute>,         // max 2 (fallback 1, fallback 2), may be empty
    pub failover: FailoverPolicy,         // { enabled: bool, switch_account_first: bool, max_switches: u8 }
}
```

`ModelSelection` (`model_router.rs:47`) gets `account: Option<String>` (serde default) so a
stored conversation selection and `PinnedRoute` can name the account; the pin fingerprint
must include the account id and the profile fingerprint so a removed or re-signed account
invalidates queued turns with a clear error (existing pinned-turn rule).

Resolution order for a turn (extends `ModelRouter::default_selection`/`autoselect`):
conversation selection -> bot `route` -> user default route -> bot legacy `model` -> server
default. Fallbacks apply only to failures (4.3), not to selection.

### 4.2 API (member-scoped, `/v1`)

Existing routes keep working (they act on the `default`/only account for compatibility).

| Method | Path | Purpose |
|---|---|---|
| GET | `/v1/providers` | add `accounts:[{id,label,auth_kind,status,priority,weight,identity,quota,cooldown_until,last_used_at}]` per provider; never secrets |
| GET | `/v1/providers/{id}/accounts` | list accounts |
| POST | `/v1/providers/{id}/accounts` | add API-key account `{label, api_key}`; validates with one catalog call; dedupes by fingerprint (hash of key prefix+length is NOT enough: use provider identity when the validation call returns one, else sha256(key) kept server-side only) |
| PATCH | `/v1/providers/{id}/accounts/{aid}` | `{label?, priority?, weight?, status?: "ok"|"disabled"}` |
| DELETE | `/v1/providers/{id}/accounts/{aid}` | delete sealed row and account row; refuse if a bot pins it unless `?force=true` (then pins become pool) |
| POST | `/v1/providers/{id}/accounts/{aid}/test` | catalog or lightweight call, returns stable status code |
| POST | `/v1/providers/{id}/signin/start` | body `{flow:"pkce"|"device", redirect_uri?, challenge?, state?}` -> `{flow, url, flow_id, user_code?, interval?, expires_in}` |
| POST | `/v1/providers/{id}/signin/complete` | PKCE: `{flow_id, code, verifier, redirect_uri}` -> creates or refreshes the account, returns `{account_id, created:false|true}` (same fingerprint = refresh in place, `reauth` fixes `needs_reauth`) |
| GET | `/v1/providers/{id}/signin/{flow_id}` | device flow status `pending|ok|denied|expired` (no secrets) |
| GET | `/v1/providers/{id}/accounts/{aid}/quota` | cached snapshot, `?refresh=1` re-reads |
| GET | `/v1/bots/{id}/route` , PUT | `{route, fallbacks:[...], failover}` validated: provider exists and user has an account (or grant), model valid in the live list, thinking level valid for that model, fallback providers are egress-approved |
| PUT | `/v1/providers/default-route` | add `account?` |
| GET | `/v1/models` | entries gain `account_ids` that can serve the model and `thinking_levels` per entry |
| GET | `/v1/route-events?conversation=` | recent failovers for the chat banner |

Old `/v1/providers/{id}/oauth/*` become aliases to `signin/*` for OpenRouter. Admin
`/v1/admin/providers/{id}/oauth/*` stays for the admin's own account but writes the
per-user account store (not the age store) and drops `restart_required`.
Audit events: `provider.account_added|signed_in|reauthed|removed|disabled`,
`model.failover`, `model.account_cooldown`. Provider error text never copied.

New tools through `gateway` (UID-scoped, audited, approval-gated):
* `providers.list` (read, no approval): providers, accounts (labels/status only), models,
  thinking levels, quota summary.
* `bots.list`, `bots.get` (read).
* `bots.set_route` (main only, `external_effect: true` so it needs approval unless the
  user has a standing grant): `{bot, route, fallbacks, failover}`; refuses to add credentials
  or accounts, refuses to change egress consent, refuses providers the user cannot use.
* `bots.create`, `bots.set_enabled` (main only, approval-gated), reuse `fleet.rs` update paths.
* `subagent.retry` (main only): `{task_id, route_override?}` see 4.5.
Tool/model output stays untrusted: tool arguments are validated like API input.

### 4.3 Failover engine (new `route_chain.rs`)

```rust
pub enum FailKind { RateLimited{retry_after}, Overloaded, ModelUnavailable, ContextTooLong,
                    QuotaExhausted{resets_at}, CredentialsRejected, AccountForbidden,
                    ProviderDown, Transport }           // switchable
pub enum NoSwitch { PolicyBlocked, EgressDenied, ClientCancelled, InvalidRequest, Content }
fn classify(provider_kind, &SeraError/http status/body-code) -> Result<FailKind, NoSwitch>
```
Classification inputs per provider (stable codes, never body text copied): HTTP status,
`retry-after`, Anthropic `error.type`, Google `status` (RESOURCE_EXHAUSTED, UNAVAILABLE,
DEADLINE_EXCEEDED), OpenAI-style `code`, openbot's regex families as a last resort.

`ModelRouter::complete_with_failover(user, bot_route_chain, request, ctx)`:
1. Candidate list = for the primary route: accounts of that provider ordered by (not
   cooling, not quota-held, not needs_reauth, priority desc, weighted pick, conversation
   affinity first); then `fallbacks[0]`, `fallbacks[1]` each with its own account ordering.
2. Attempt candidate; refresh token on 401 once (existing); on `FailKind` record cooldown /
   status on the account row (`rate_limited` until `retry_after`, `needs_reauth` on
   refresh invalid_grant, `quota_exhausted` until `resets_at`), write `route_events`, emit
   an `route_switched` event to the chat, continue with next candidate.
3. Order of switching when `switch_account_first`: other accounts of the same provider
   and model, then next route. `max_switches` default 3 per turn.
4. Between providers: rebuild the request from the neutral message list. Strip provider-bound
   reasoning (Anthropic signed thinking, Codex encrypted reasoning items, Gemini/Antigravity
   thoughtSignature -> sentinel). Re-validate thinking level for the target (drop if
   unsupported, never send). Re-scan egress (`gateway.scan_egress`) and blocklist for the new
   provider; require `data_egress_approved` for that provider AND the user's per-bot
   consent flag on the fallback chain (D15). The pinned route fingerprint covers the whole
   chain, so editing the chain invalidates queued turns.
5. Idempotency: failover is allowed only for the LLM call itself (no external effect has been
   emitted by the failed attempt). Tool effects already executed in earlier rounds are in
   history, not replayed.
6. All candidates failed -> `SeraError::Rejected{code:"all_routes_failed", ...}` with the list
   of stable reasons; the turn fails resumably (4.5).
Wire into `turn_loop.rs:635` and `:940` in place of `complete_with_thinking`.

Affinity: keep a conversation on the same account (Antigravity sessionId, Codex cache key,
Anthropic prompt cache) until it fails; selection is sticky per `(conversation, provider)`
in kv.

### 4.4 Account selection and health

* Order: not disabled -> not `needs_reauth` -> not cooling -> not quota-held -> priority desc
  -> weighted random inside a priority tier (aio-proxy rule) -> sticky affinity first.
* Quota hold (aio-proxy rule adapted): hold only when a snapshot is <= 10 minutes old, an
  in-scope window has `remaining <= 0` and `resets_at` is in the future; unknown/stale never
  holds. Scope = account or model patterns (Antigravity per-model, Codex per additional
  limit, Muse account-wide).
* Proactive refresh: a background sweeper refreshes tokens expiring within 5 minutes and
  quotas every 5-10 minutes for accounts used in the last day; refresh is single-flight per
  account (`tokio::sync::Mutex` per account id plus the DB `generation` guard) and writes
  back before releasing, which is mandatory for rotating refresh tokens (Codex, Claude).
* `needs_reauth` surfaces as a banner and a chat notice; no silent retry loop.

### 4.5 Subagents, main auto-resume, bot management

* Subagent task failure today ends the task. Change: a subagent turn runs through
  `complete_with_failover` with the SPECIALIST's own chain; if its chain is exhausted the
  task ends `failed_resumable` with `{reason_codes, last_stage, effects_unknown:boolean}`.
* The orchestrator gets a system message in its next turn: "subagent X failed: <codes>".
  Auto-resume policy owned by the main bot, bounded: up to `N=2` resumes per task,
  each choosing from (a) same bot with `route_override` = next unused account/fallback,
  (b) another provider via `providers.list`, (c) give up and report. Implement as tool
  `subagent.retry({task_id, route_override?})`; the server validates the override with the
  same rules as `bots.set_route` and records `route_events`.
* Never blind-replay an unknown external effect (AGENTS.md): if the failed turn had a tool
  call whose outcome is unknown (`effects_unknown = true`), `subagent.retry` refuses and the
  main must ask the user or verify through a read tool first.
* Parallelism and budgets: reuse `MAX_HANDOFFS_PER_TURN = 16`; add a per-user concurrency
  limit across accounts (provider rate limits) via a semaphore per `(provider, account)`
  sized by config (default 4) so fan-out spreads across accounts instead of piling on one.
* The main bot is `role == Orchestrator` and is the only role that gets `bots.*` and
  `subagent.retry`. Specialists never get them. Bot edits act on the acting user's fleet only.

### 4.6 Desktop UI plan (Dioxus, `crates/serabot-desktop/src`)

Providers page (`ui/providers.rs`, `provider_view.rs`, `provider_presets.rs`):
* Provider card shows an Accounts list: label, email/plan chip, status chip (ok, needs
  sign-in, cooling until HH:MM, quota 0 until reset), quota meter, priority drag handle,
  enable switch, Test, Remove. Buttons: "Add account" (API key sheet or "Sign in with ..."
  per `sign_in`: pkce, pkce_google, device, key). Desktop runs the loopback listener for pkce
  flows (`oauth.rs`; Codex needs fixed port 1455, Antigravity 51121, Claude 54545; if the
  port is busy offer the device flow when the provider has one).
* Device flow dialog: code, "Open page" button, countdown, polls `signin/{flow_id}`.
* Presets: add anthropic, openai-codex (ChatGPT), opencode-go, opencode-zen, nous (key),
  fix `ui/providers.rs:183` which only handles `pkce`; extend `ProviderPreset.sign_in` to the
  full set and add a `risk` note field (shown for claude-plan and codex).
* Policy banner for plan-based sign-ins (text from 3.2, 3.3).

Bot settings (`ui_bots.rs`, bot sheet): "Model route" block: Primary (provider, account or
"any", model, thinking), Fallback 1, Fallback 2 (optional, same pickers, each with consent
line "this provider will receive the conversation"), "Auto-switch on failure" toggle,
validation errors inline (model not on the live list, level unsupported). Show effective route
and last switch.

Composer and chat (`ui/chat.rs`, `chat_compose.rs`, `chat_feed.rs`, `chat_scroll.rs`,
`chat_plan.rs`, `chat_failure.rs`, `activity.rs`): model chip = `provider/account · model ·
thinking` with a combined picker (provider -> account -> model, search, tags from
`picker_rows`), thinking segmented control limited to `thinking_levels` of the chosen route;
inline "Switched to X because Y" system row from `route_events`; per-turn route badge; failed
turn card with "Retry on <fallback>" actions; subagent tree in the sidebar with each
subagent's route and state (running, waiting approval, failed resumable, resumed on <route>),
parallel runs collapsed by default with counts. Long sessions: windowed message list
(render only viewport plus overscan), incremental markdown parse, collapse completed tool
blocks, keep scroll anchor (`chat_scroll.rs`), summary rows after compaction. Heavy
parallelism: batch `events` into frames (coalesce at 8 to 16 ms), per-subagent progress rows
instead of streaming every token to the DOM.

---

## 5. How to implement in Rust in SeraBot

### 5.1 Crate layout (new modules in `crates/serabot/src`)

```
accounts.rs            ProviderAccount store API over provider_accounts + sealed rows
                       (list/add/patch/delete/mark_status/cooldown/quota) ; all take user_id
auth_flows/mod.rs      trait SignInFlow; registry by kind
auth_flows/pkce.rs     pkce_s256(), state, loopback validation (move from cloud_code.rs)
auth_flows/google.rs   Antigravity authorize url, code exchange, userinfo, project discovery
auth_flows/openai.rs   Codex authorize url, code and device exchange, JWT claims (no verify needed for display, only decode)
auth_flows/claude.rs   behind plan_oauth flag
auth_flows/muse.rs     device flow + mint (from providers.rs)
wire/anthropic.rs      Messages client: request build, SSE parse, error map, thinking
wire/responses.rs      Responses client (Codex, Muse): request build, SSE parse, reasoning items
wire/cloud_code.rs     existing cloud_code.rs split: envelope, sse, thinking, quota
route_chain.rs         FailKind classify, candidate ordering, complete_with_failover
route_events.rs        store + event emission
tools_bots.rs          gateway tools providers.list, bots.*, subagent.retry
```
Keep `llm.rs` for chat-completions; introduce a transport enum instead of kind sniffing:

```rust
pub enum Wire { ChatCompletions, CloudCode, AnthropicMessages, Responses }
pub trait ModelBackend: Send + Sync {            // one per (provider, account) at call time
    async fn complete(&self, req: &NeutralRequest, ctl: &CallCtl) -> Result<LlmResponse, ProviderError>;
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>;
    fn thinking_levels(&self, model: &str, tags: &[String]) -> Vec<&'static str>;
}
```
`ProviderError` carries the stable `FailKind` already classified by the backend, so
`route_chain` never parses provider text.

### 5.2 Credentials per account

* `ProviderCredential` unchanged (api_key = bearer, refresh_token, expires_at, generation).
* `UserCredentialWriter::provider(users, owner, reserved_id)` already writes to an arbitrary
  id; pass `acct:<provider>:<account_id12>`. Extend `ProviderCredentials` trait:

```rust
pub trait ProviderCredentials: Send + Sync {
    fn accounts(&self, user_id: &str, provider: &str) -> Vec<AccountView>;   // health, no secrets
    fn client(&self, user_id: &str, provider: &str, account: &str, profile: &ProviderConfig) -> Result<Backend>;
    fn available(&self, user_id: &str, provider: &str) -> bool;              // any usable account
}
```
* A granted admin credential appears as a synthetic read-only account `grant` (never
  listed with the grantor identity).
* Refresh: `AccountRefresher { locks: DashMap<AccountKey, Arc<tokio::sync::Mutex<()>>> }`;
  inside the lock re-read the row, compare `generation`, refresh, write via
  `update_provider`, then release. Google: client secret from `server_secrets.require(...)`.
  Codex/Claude: rotated refresh token must be persisted before the new access token is used.
* Expiry: store `expires_at` from `expires_in` minus 60 s (or JWT `exp`); refresh before
  use when within skew, in addition to the reactive 401 path.

### 5.3 Sign-in endpoints (server)

* One handler pair `signin_start/complete` dispatching on canonical kind to
  `auth_flows::*`. State, verifier and `flow_id` kept in a small in-memory map with TTL
  (10 min), keyed by `(user_id, flow_id)`, value includes the `expected_provider` and the
  redirect URI; reject completion from a different user or provider. PKCE verifier stays on
  the desktop (existing contract); for device flows the verifier-less `device_code` stays
  server-side only.
* Redirect validation: exact loopback (`127.0.0.1`/`[::1]`/`localhost` per provider),
  fixed ports per provider (Antigravity 51121, Codex 1455, Claude 54545), fixed path.
  Reuse `validate_loopback_redirect`.
* Token endpoints pinned by host/path (`enforce_oauth_target`-style table per kind);
  redirects disabled; bodies capped at 64 KiB; errors mapped to stable codes; never log
  codes, verifiers, tokens or ids.
* After exchange: compute identity (Google userinfo email; Codex JWT account id and email;
  Claude `account.uuid`; Muse mint response), fingerprint = sha256(`<provider>:<stable id>`),
  upsert account (same fingerprint = replace secret in place, reset status to `ok`), run
  provider-specific onboarding (Antigravity project discovery) and store the result in
  `identity_json`; one catalog fetch to prove the account works; audit event.
* Device flows: start spawns a bounded task (existing Muse code is the template, cap 900 s,
  `slow_down` handling), writes the account row on success, publishes a `provider.signin`
  event; the member poll endpoint reads state.

### 5.4 Per-bot routing in `ModelRouter`

* `BotConfig` additions (4.1) with `serde(default)`; `LoadedBot` unchanged shape.
* `ModelRouter::chain_for(user, bot, conversation) -> RouteChain` builds the ordered
  candidates; `PinnedRoute` becomes `PinnedChain { primary: PinnedRoute, fallbacks:
  Vec<PinnedRoute>, fingerprint }`. `validate_pinned` checks every link.
* `thinking::levels_for(kind, model, tags)` stays pure; add `plan: Option<&str>` later if
  account plans restrict levels. Wire mappers per backend consume the chosen level.
* `api/bots.rs` PUT gains `route`, `fallbacks`, `failover` with the validations in 4.2; keep
  `model`/`thinking` accepted and mapped onto `route` for older clients.
* Fleet seeding (`bots/*.toml`, `prompts/`): templates may name `provider = "..."`; resolved
  per user against the accounts they actually have; if absent fall back to the user's default
  route.

### 5.5 Tests (synthetic fixtures only, per AGENTS.md)

* Wiremock servers for: Google token + userinfo + loadCodeAssist/onboardUser +
  streamGenerateContent (200, 400 session wall, 429, 403, 404, MALFORMED_FUNCTION_CALL);
  OpenAI token (rotating refresh), device endpoints, Responses SSE; Meta device + mint +
  quota; Anthropic messages (stream, tool_use, signed thinking, 429/529); OpenCode with
  header assertion `x-opencode-session`.
* Cross-user isolation: user B cannot list, test, pin, or refresh user A's accounts; admin
  cannot read values; AAD blob move fails; grant never reveals grantor.
* Refresh races: N concurrent turns on one account perform exactly one refresh and all see
  the rotated token; stale `generation` write refused.
* Failover table tests: each `FailKind` and `NoSwitch` case; egress-unapproved fallback is
  refused; reasoning blocks stripped across providers; pinned chain fingerprint invalidates
  on edit; `effects_unknown` blocks `subagent.retry`.
* API tests in `api/tests.rs` style for every new route, including validation errors.
* Desktop unit tests for preset table, picker rows, loopback listener (existing `oauth.rs`
  tests), device dialog state machine.
* Never report a live integration verified unless run against the owner's real account.

### 5.6 Phasing (each phase = one PR, gate = fmt, clippy -D warnings, test, commit with tests)

1. v0.49.0 accounts foundation: migration 0037, `accounts.rs`, reserved-prefix sealing, legacy
   virtual account, `/accounts` API, Providers UI accounts list, API-key multi-account for
   all existing kinds, per-account health and test. No new wire formats.
2. Member sign-in for Antigravity and Muse on the new store (D2, D3, D5), per-account project
   and session ids, thinking levels, quota snapshot, drop age-store/`restart_required` path,
   updated docs.
3. `anthropic` Messages backend (API keys, multi account, thinking), preset, docs; OpenCode
   Go/Zen backends (hook), Nous preset (after doc check).
4. Bot routes: `BotConfig` route/fallbacks, `PinnedChain`, `route_chain.rs` failover in turn
   loop, `route_events`, composer/bot UI, banner.
5. Main bot tools (`providers.list`, `bots.*`, `subagent.retry`), `failed_resumable`, per
   `(provider, account)` concurrency limits, subagent tree UI.
6. Codex (Responses backend, rotating refresh) and Claude plan OAuth, BOTH behind admin flags
   default off, with risk banners; ship only after the owner confirms terms.

---

## 6. Risks and open decisions

* O1. Anthropic plan OAuth and OpenAI Codex plan OAuth for a multi-user server: vendor terms
  likely forbid or at least do not clearly allow it (3.2, 3.3). Owner decision; default OFF.
* O2. Antigravity and Muse are plan-bound consumer surfaces using vendor-internal endpoints
  and impersonating client headers (UA, `X-Goog-Api-Client`, `userAgent:"antigravity"`).
  They can break or be rate-banned without notice; keep version probe and graceful
  degrade; never present as "official API".
* O3. Per-account state leaks: project ids, session ids, quota and error text must be per
  account and never shared across users; add tests (5.5).
* O4. Cross-provider failover sends the conversation to another vendor. Needs explicit
  per-chain consent in the UI and audit (D15), plus blocklist and egress scan per hop.
* O5. Rotating refresh tokens (Codex, Claude) make races destructive: a lost write means a
  forced re-login. Single-flight and write-before-use are non-negotiable.
* O6. Hermes and OpenCode OAuth specifics are UNVERIFIED; do not ship claims without docs.
* O7. Messaging (Slack, Discord, WhatsApp) is not part of this note beyond 2.3; schedule as
  its own spec (`docs/specs/v0.49/messaging.md`) using openbot's relay and orchestrator
  design and SeraBot's `telegram.rs` per-user linking as the base.
* O8. File and line references above are from the tree as read; line numbers may drift.
