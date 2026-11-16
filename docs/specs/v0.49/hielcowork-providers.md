# HielCowork provider reference notes (for SeraBot v0.49)

Status: research notes, read-only study of the owner's C# project at `E:/repo/Hiel Cowork` (WPF app, namespace `Hiel_Cowork`). Nothing was run or installed. No secrets or tokens are copied here; public OAuth client ids and the Antigravity client secret are referenced by file and line only.

Goal: make SeraBot's Antigravity and Muse Code sign-in providers follow HielCowork exactly, and capture what HielCowork does for OpenCode, Copilot and the other sign-in flows so v0.49 can reuse the patterns.

Treat HielCowork behaviour as the specification. Where `E:/repo/Hiel Cowork/docs/antigravity-integration.md` (dated 2026-10-24) disagrees with the code, the code wins. The doc is stale (different endpoint order, headers, thinking budgets, an 8-phase sanitizer that is not what the code does). Section 2.10 lists the differences.

Verification rule (AGENTS.md): nothing here has been exercised against a live Google, Meta or OpenCode account. Everything below is "what HielCowork's code does", not "what the provider guarantees". Do not report any of it as verified live.

## 0. File map (HielCowork)

| Area | File |
|---|---|
| Antigravity constants, version, session id | `Services/Providers/Antigravity/AntigravityConstants.cs` |
| Antigravity request envelope, thinking | `Services/Providers/Antigravity/AntigravityRequestTranslator.cs` |
| Antigravity tool schema sanitizer | `Services/Providers/Antigravity/AntigravitySchemaSanitizer.cs` |
| Antigravity chat + SSE + endpoint fallback | `Services/Providers/Antigravity/AntigravityChatProvider.cs` |
| All OAuth sign-ins, refresh, project discovery, Muse mint | `Services/OAuth/OAuthService.cs` |
| PKCE helper | `Services/OAuth/Pkce.cs` |
| Loopback callback listener | `Services/OAuth/LoopbackCallbackServer.cs` |
| Import sign-ins from other CLIs (Codex, Grok, OpenCode, Antigravity, Vibe) | `Services/OAuth/CliSessions.cs` |
| Muse chat transport | `Services/Providers/ResponsesChatProvider.cs` (`IsMuse`), selected in `Services/ProviderFactory.cs` line 44 |
| OpenCode Go/Zen/Free | `Services/Providers/OpenCode/OpenCodeProviderFactory.cs`, `docs/research-opencode.md` |
| Copilot | `Services/Copilot/CopilotService.cs`, `CopilotCatalog.cs`, `CopilotChatProvider.cs`, `CopilotRequestHook.cs` |
| Brand cards, model lists | `Services/Providers/ProviderCatalog.cs`, `Models/ProviderConfig.cs` (`ProviderTypes`, `ProviderModelPresets`) |
| Per-client quirks | `Services/Providers/ProviderClientProfile.cs`, `docs/decisions/ADR-001-provider-client-profiles.md` |
| Model listing and capabilities | `Services/ModelFetchService.cs` |
| Quota | `Services/Quota/Sources/AntigravityQuotaSource.cs`, `MuseQuotaSource.cs`, `OpenCodeGoQuotaSource.cs`, `ClaudePlanQuotaSource.cs`, `CodexQuotaSource.cs`, `CopilotQuotaSource.cs`, `MiniMaxPlanQuotaSource.cs` |
| HTTP retry, idle timeout | `Services/HttpStreamSupport.cs` |
| Tool schema flattening (Muse) | `Services/Providers/ToolSchemaBuilder.cs` |
| Thinking levels | `Models/ReasoningEffort.cs` |
| Antigravity extras | `Services/Media/AntigravityImageProvider.cs`, `Services/WebSearchService.cs` (`AntigravitySearchAsync`) |
| Cross-references inside the repo | `9router/src/lib/oauth/providers/{antigravity,muse}.js`, `9router/open-sse/providers/registry/muse.js`, `aio-proxy/docs/superpowers/specs/2026-09-06-muse-oauth-design.md` |
| Tests to mirror as fixtures | `HielCowork.Tests/AntigravityProviderTests.cs`, `MuseProviderTests.cs`, `MistralAndMuseTests.cs`, `ResponsesChatProviderTests.cs` |

## 1. Shared sign-in plumbing (OAuthService)

### 1.1 Credential model (`Models/ProviderConfig.cs`)

One `ProviderConfig` holds the whole sign-in: `AccessToken`, `RefreshToken`, `TokenExpiresAt`, `AccountId`, `SignedInEmail`, `LinkedCli`, `LastIdToken`, `BaseUrl`, `ModelId`, `ReasoningEffort`. The meaning of `AccountId` depends on the provider type, which is a smell worth avoiding in SeraBot:

| Provider | AccessToken | RefreshToken | AccountId |
|---|---|---|---|
| Antigravity | Google access token (short lived) | Google refresh token | Cloud Code companion project id |
| Muse | minted Model API key (`LLM|...`, no expiry) | Meta OIDC access token (kept only to re-mint and to read quota) | subscription tier name |
| Copilot | Copilot token (about 25-30 min) | long lived GitHub OAuth token | unused |
| Codex | ChatGPT access token (JWT) | rotating refresh token | ChatGPT account id from the JWT |

A run works on a copy of the provider; `AuthRoot` points at the live one. `CopySignInFrom` copies tokens, expiry, account, email, linked CLI.

### 1.2 Token retrieval and refresh (`GetValidAccessTokenAsync`, OAuthService.cs ~448)

1. Fail with "not signed in" when there is no access token.
2. `SyncFromRoot`: take whatever the live provider holds (another run may have refreshed).
3. `IsAccessFresh`: access token non-empty and `now + 5 min < expiry`, where expiry is `TokenExpiresAt` else the JWT `exp` else "fresh forever" (this is how the Muse key never refreshes).
4. One `SemaphoreSlim` per sign-in root id (not per run copy) so two sub-agents never spend the same rotating refresh token twice. Re-sync inside the gate.
5. Adopt a linked CLI's newer tokens if any (`AdoptLinkedCli`), re-check freshness.
6. `RefreshAsync`, then write back to the linked CLI (never for Antigravity/OpenCode/Vibe), then always `PublishToRoot` before releasing the gate so the next waiter sees new tokens or the sign-out.
7. Callers on HTTP 401 (Responses path): force `TokenExpiresAt = now - 10 min` and retry once, which triggers refresh on the next call.

`ApplyTokenResponse`: keep the old refresh token when the response has none (Google omits it on refresh); `expires_in` default 3600; expiry is JWT `exp` or `now + expires_in`; email from id_token/access JWT.

Refresh failure policy (Antigravity and Codex/Grok): on `HTTP 400`, `HTTP 401` or `invalid_grant` try adopting the linked CLI once, else `SignOut` (clears every credential field) and throw "The saved ... sign-in expired. Sign in again from Settings -> Providers." HTTP 403 on Codex/Grok gets a plan-specific message.

`CreateClient`: 30 s timeout, `User-Agent: HielCowork/1.0` on all auth calls. Streaming clients use an infinite timeout and an idle read timeout (section 6).

### 1.3 PKCE and loopback (`Pkce.cs`, `LoopbackCallbackServer.cs`)

- Verifier: 32 random bytes, base64url without padding. Challenge: SHA-256 of the ASCII verifier, base64url. State: 16 random bytes base64url. S256 only.
- Loopback server binds `127.0.0.1` and, if available, `[::1]` on a fixed port (or an ephemeral port found by probing), path fixed per provider. Redirect URI is `http://localhost:{port}{path}`.
- Callback handling: parse the request line only; `error` -> 400 page and failure; `state` must match exactly; `code` required; success page "You can close this tab". Captures `iss` too. No timeout of its own; callers wrap in a 5 minute linked `CancellationTokenSource` and turn the cancel into a "Timed out waiting for ... sign-in redirect." error.
- Fixed port busy: Antigravity falls back to an ephemeral port (Google desktop clients accept any loopback port); Codex falls back to device flow.

### 1.4 CLI session import (`CliSessions.cs`)

Reads another tool's credential file so the user does not sign in twice. For Antigravity: `~/.dsh-agy/accounts.json` (array `accounts[]` with `access`, `refresh`, `expires` ms, `email`, `projectId`/`managedProjectId`, `enabled`; first enabled account wins), `~/.gemini/antigravity-cli/oauth_creds.json` and `credentials.json` (`token.access_token`/`refresh_token`/`expiry`, or flat; `project_id`). Antigravity and OpenCode are read-only (no write-back); Codex and Grok are rotated so tokens are written back to the CLI file after refresh.

SeraBot note: a multi-user server must not read home-directory files of other tools (AGENTS.md: filesystem tools restricted to configured inboxes, UID scoping). If ever offered, it is an owner-desktop-only helper that uploads the credential into the owner's per-user store, never a server-side file read. See section 7.5.

### 1.5 Error formatting helper

`FormatAuthError(step, status, body)` -> `"{step} failed (HTTP {n}): {body trimmed, max 400 chars + ellipsis}"`, `(empty body)` when blank. Body is already provider output, so SeraBot must additionally scrub secrets before surfacing it.

## 2. Google Antigravity

### 2.1 OAuth (browser PKCE, desktop client with secret)

Defined in `Services/OAuth/OAuthService.cs` lines 52-65:

- Authorize URL `https://accounts.google.com/o/oauth2/v2/auth`, token URL `https://oauth2.googleapis.com/token`, userinfo `https://www.googleapis.com/oauth2/v1/userinfo?alt=json`.
- Client id: `AntigravityDefaultClientId` (line 55), overridable by env `AGY_CLIENT_ID`. Client secret: `AntigravityDefaultClientSecret` (line 56), overridable by env `AGY_CLIENT_SECRET`. This is the published Antigravity desktop client pair; do not copy the secret into docs or the public repo. SeraBot already keeps the secret in the age store (`antigravity_oauth_client_secret`, see `docs/antigravity.md`) and the client id at `crates/serabot/src/cloud_code.rs` line 24.
- Scopes (space separated): `cloud-platform`, `userinfo.email`, `userinfo.profile`, `cclog`, `experimentsandconfigs` (all under `https://www.googleapis.com/auth/`). **Must never include `openid`**: the code comment (line 57) says Google redirects desktop PKCE with `openid` into a hanging nativeapp consent screen.
- Callback: port 51121, path `/oauth-callback`, fallback to an ephemeral port.
- Authorize params: `client_id`, `response_type=code`, `redirect_uri`, `scope`, `code_challenge`, `code_challenge_method=S256`, `state`, `access_type=offline`, `prompt=consent` (consent is what guarantees a refresh token).
- Token exchange (form body, `Accept: application/json`): `client_id`, `client_secret`, `code`, `grant_type=authorization_code`, `redirect_uri`, `code_verifier`.
- After exchange: `ApplyTokenResponse`, best-effort userinfo GET for email (display only, failures ignored), then project discovery.
- Refresh (form body): `client_id`, `client_secret`, `refresh_token`, `grant_type=refresh_token`; if `AccountId` is empty re-run project discovery.
- Alternative implementation in `9router/src/lib/oauth/providers/antigravity.js`: same authorize params (no PKCE there), onboarding as fire-and-forget loop of up to 10 tries 5 s apart until `done == true`, and numeric client metadata (`ideType: 9`, a platform enum, `pluginType: 2`). The C# code, not 9router, is the reference for v0.49.

### 2.2 Project discovery (`EnsureAntigravityProjectAsync`, OAuthService.cs ~1502)

Every `v1internal` call needs `cloudaicompanionProject` (stored in `AccountId`, mirrored to `AuthRoot.AccountId`). Shortcut: if `AccountId` or the root's is already set, return it.

1. Endpoints tried in order (`ProjectDiscoveryEndpoints`): `https://daily-cloudcode-pa.googleapis.com`, then `https://cloudcode-pa.googleapis.com` (only these two).
2. `POST {endpoint}/v1internal:loadCodeAssist`, body `{"metadata": {"ideType":"ANTIGRAVITY","ideVersion":"<resolved version>","platform":"WINDOWS_AMD64"}}`, headers `Authorization: Bearer`, `User-Agent` (section 2.3), `X-Goog-Api-Client`.
3. Project is `cloudaicompanionProject` as a string or as `{ "id": ... }`.
4. Tier: from `allowedTiers[]`, the entry with `isDefault`, else the first with an id, else the literal `free-tier`.
5. No project yet: `POST :onboardUser` body `{"tierId": <tier>, "metadata": {...}}`, up to 3 attempts. After each: if success, read project from `response.cloudaicompanionProject` (the response is a long running operation, project under `response` or at root); sleep 3 s; re-call `loadCodeAssist`.
6. Any exception on an endpoint moves on to the next endpoint. If nothing is found return null and **omit `project` from the envelope**: sending `"project": ""` returns HTTP 404 "Requested entity was not found".

### 2.3 Identity headers and version

`AntigravityConstants.cs`:

- User-Agent: `antigravity/hub/{version} (aidev_client; os_type=windows; arch=amd64; cl={cl})`; fallback version `2.25.0`, `cl` constant `1003475615` (lines 41-42, 105-109).
- `X-Goog-Api-Client: google-cloud-sdk vscode/1.96.0` on every call.
- Version resolution once per process (`ResolveVersionAsync`): env `AGY_VERSION` wins; else GET the public auto-updater URL (line 40) with a 3 s timeout and read JSON `version` (must start with a digit); on any failure keep the fallback. Result cached for the process.
- Request id: `agent/{unix ms}/{8 hex}`.
- Session id: FNV-1a 64-bit (offset 14695981039346656037, prime 1099511628211) over the UTF-8 string `"{account}:{conversation}"` or `"{account}:{conversation}:gen{N}"` when generation N > 0; fold with `hash % 9_000_000_000_000_000_000`; result is `"-{folded}"` (`"-1"` if zero). `account` is the project id (default `"default"`), `conversation` is `CacheAffinity.ConversationKey`. Stable per account and conversation so the server-side cache and 1M-token session accumulator behave.
- Session wall: when HTTP 400 body contains `exceeds the maximum number of tokens`, bump a per-(account, conversation) generation counter (in-memory `ConcurrentDictionary`) once, which rotates the session id, and retry the same endpoint.
- Endpoint chain `FallbackEndpoints` (order matters): daily `https://daily-cloudcode-pa.googleapis.com`, prod `https://cloudcode-pa.googleapis.com`, sandbox daily `https://daily-cloudcode-pa.sandbox.googleapis.com`, autopush `https://autopush-cloudcode-pa.sandbox.googleapis.com`. `OrderedEndpoints(baseUrl)` puts a configured known host first and keeps the rest.

### 2.4 Request translation (`AntigravityRequestTranslator.BuildRequestEnvelope`)

Wire: `POST {endpoint}/v1internal:streamGenerateContent?alt=sse`, headers `Authorization`, `Accept: text/event-stream`, `User-Agent`, `X-Goog-Api-Client`. The 200 response is an SSE stream.

Envelope:

```json
{
  "requestId": "agent/<ms>/<hex>",
  "model": "<model id>",
  "userAgent": "antigravity",
  "requestType": "agent",
  "project": "<companion project, omitted when unknown>",
  "request": {
    "sessionId": "-<digits>",
    "systemInstruction": { "parts": [ { "text": "<all system messages joined by blank line>" } ] },
    "contents": [ { "role": "user|model", "parts": [ ... ] } ],
    "tools": [ { "functionDeclarations": [ ... ] } ],
    "toolConfig": { "functionCallingConfig": { "mode": "VALIDATED" } },
    "generationConfig": { ... }
  }
}
```

Message mapping (history first normalised by `OpenAiChatHistory.ForProvider`):

- System messages: concatenated into `systemInstruction.parts[0].text`; separators and tool-group display rows dropped.
- User: image attachments become `inlineData {mimeType, data(base64)}` parts placed before the text part; then `{text}`.
- Assistant: for non-Claude models, replayed reasoning becomes `{thought: true, text}` (Claude path rejects unsigned thought blocks, so it is skipped); then `{text}`; then one `functionCall {id, name, args}` part per tool call, each carrying `thoughtSignature` = the cached signature for that call id, else the sentinel `skip_thought_signature_validator`.
- Tool result: `role: "user"` with `functionResponse {id, name, response {result, is_error}}`. Name resolution order: message tool name, id -> name map from earlier assistant calls, FIFO queue of names, then the sanitised call id. `is_error` comes from the tool status (SeraBot's current code hard-codes false).
- Turn coalescing: adjacent same-role turns are merged, but a function-response turn is never merged with a plain user text turn (and vice versa). Empty turns skipped.
- Claude models: strip any trailing `model` turn (assistant prefill is rejected with thinking). HielCowork does this for every `claude*` id; SeraBot currently only for `claude` + `thinking`.
- Empty conversation: a single `user` turn `"Hello"`.
- Thought signature cache (`CacheThoughtSignature`): static concurrent map call id -> signature, FIFO eviction beyond 2000 entries; filled while parsing the SSE stream from the `thoughtSignature` field of the part holding a `functionCall`. In SeraBot this must be per user and per conversation, not a process-global map (UID scoping).

### 2.5 Tool declarations and schema sanitizing (`AntigravitySchemaSanitizer`)

Tool name rules (`SanitizeToolName`): replace anything outside `[A-Za-z0-9_]` with `_`; prefix `_` if it starts with a digit; truncate to 64; empty -> `tool`; remap `web_search` -> `agy_web_search` and `search_web` -> `agy_search_web` (collision with Antigravity's built-ins) and reverse-map on the way back (`FromWireToolName`). Skip tools named `google_search`/`googleSearch`. De-duplicate names case-insensitively after sanitising. Declaration: `{name, description, parameters?}`; `parameters` omitted when the sanitised schema is empty.

Schema sanitizer is an allow-list (anything unknown makes Cloud Code return HTTP 400 INVALID_ARGUMENT). Allowed keys: `type, format, title, description, nullable, items, enum, default, properties, required, additionalProperties`. Rules, in order (`SanitizeSchema`):

1. Non-object schema -> `{type: string}`.
2. `type` array (`["string","null"]`) -> first non-null as `type`, `nullable: true`.
3. `const` with no `enum` -> single-element `enum`.
4. No type yet and `anyOf`/`oneOf`: take the first non-null branch (recursively sanitised, merged into this node), a `null` branch sets `nullable`.
5. Copy allowed keys: `properties` recursed; `items` recursed (array `items` -> first element; missing/other -> `{type:string}`); `additionalProperties` recursed if object, kept if boolean; `enum` copied if non-empty; `required` strings only; `nullable: true` honoured.
6. Missing `type` inferred: has `properties` -> `object`; has `items` -> `array`; else `string`.
7. `array` without `items` -> `items: {type: string}`.
8. `required` filtered to keys present in `properties`; dropped when none remain or there are no properties.

Not handled by HielCowork's code (but handled by the older doc and by SeraBot's `cloud_code.rs`): `$ref`/`$defs` hoisting, `allOf` merging, constraint hints appended to descriptions, empty-object placeholder property. Keep SeraBot's richer `sanitize_schema` (hoist refs, hints, placeholder) and add HielCowork's rules where missing (union `type` arrays, `const` -> `enum`, filtering `required`). Upstream of the sanitizer, `ToolSchemaBuilder.Build` flattens recursive `$ref` cycles to `string` (depth 6), forces `items` on arrays, and removes open `additionalProperties`.

### 2.6 Thinking and generation config (`BuildGenerationConfig`)

Inputs: `ReasoningEffort` (Off, Low, Medium, High, Max; UI labels None/Low/Medium/High/Max), model id, `MaxTokens`.

- `maxOutputTokens`: caps are 64,000 for Claude ids and 65,536 otherwise. If configured `MaxTokens <= 4096` (the app default) use the cap, else the requested value; clamp to `[1024, cap]`.
- `temperature` = configured value, only when the model is not Claude or effort is Off.
- "Level" thinking models (`IsLevelThinkingModel`): not `claude`/`gpt-oss`; id ends with `-tiered` -> yes; ends with `-low|-medium|-high` -> no (level is baked into the id, so no `thinkingConfig` is sent at all); otherwise id starts with `gemini-3` or `gemini-4` -> yes.
  - Effort Off -> `thinkingConfig: { thinkingBudget: 0 }`.
  - Else `thinkingConfig: { thinkingLevel: "low"|"medium"|"high", includeThoughts: true }` with Low->low, Medium->medium, High->high, **Max->high**, unknown->medium.
- Claude with `thinking` in the id, or any effort other than Off: when effort is not Off, budget = `clamp(ToTokenBudget(effort), 1024, max(1024, maxOutputTokens-1024))` where the table is Low 4,000 / Medium 10,000 / High 24,000 / Max 32,000; `thinkingConfig: { thinkingBudget: budget, includeThoughts: true }` only when `maxOutputTokens > budget`. Effort Off sends no thinking config.
- The older doc claims `anthropic-beta: interleaved-thinking-2025-05-14` and `Client-Metadata` headers; the current code sends neither. SeraBot has `requests_interleaved_thinking` in `cloud_code.rs`; verify before keeping it, since HielCowork's current code does not.
- SeraBot gap: `generation_config` in `crates/serabot/src/cloud_code.rs` hard-codes budget 8192 and level `medium` and ignores the bot's thinking level. v0.49 must feed the level in.

### 2.7 Response parsing (`ParseSseStreamAsync`)

- Each line: skip blank and `:` keepalive; accept `data: ...` or a raw JSON line; `[DONE]` ends.
- A top-level `error` object ends the stream with `"HTTP {code}: {message}"` (4xx/5xx code) or `"Antigravity error ({status}): {message}"`.
- Payload may be wrapped as `{response: {...}}`; use `modelVersion` as the answered model.
- `candidates[0].content.parts[]`: `thought:true` text -> reasoning stream; other text -> content stream; `functionCall` -> tool call (`id` or generated `call_xxxxxxxx`, wire name mapped back, `args` object as JSON string) and store the `thoughtSignature`.
- `finishReason` ignored when `FINISH_REASON_UNSPECIFIED`. Mapping: `STOP`->stop, `MAX_TOKENS`->length, `SAFETY|RECITATION|BLOCKLIST|PROHIBITED_CONTENT|SPII`->content_filter, any tool call present -> `tool_calls`.
- `MALFORMED_FUNCTION_CALL` with no content and no tool calls: re-sample, up to 3 total attempts, then error "The model repeatedly emitted a malformed tool call (MALFORMED_FUNCTION_CALL)."
- Stream ended with no candidate, finish reason or usage: error "stream cut off".
- Usage (`usageMetadata`): input = `promptTokenCount - cachedContentTokenCount`; cache read = `cachedContentTokenCount`; output = `candidatesTokenCount + thoughtsTokenCount`; reasoning = `thoughtsTokenCount`.

### 2.8 HTTP, retry and failover (`SendWithEndpointFallbackAsync`)

Per endpoint: `HttpStreamSupport.SendWithRetryAsync` with 2 retries (section 6). Then, on a non-success final response:

1. HTTP 400 `exceeds the maximum number of tokens`: bump session generation once and retry the same endpoint.
2. HTTP 400/404 for a Claude 5.5 slug: retry once with a "wire id" model override (`TryGetWireFallbackModel`). The map is `claude-opus-5-5` and `claude-opus-5-5-thinking` -> `claude-opus-5-5-thinking`, and the two Sonnet ids map to themselves, so only plain `claude-opus-5-5` actually changes (to its thinking variant, which also changes thinking behaviour). The code comment says the older 4.6 aliases retired on 2026-11-02. Low value; optional to port.
3. HTTP 429, 403, 503 or >= 500 and another endpoint remains: move to the next endpoint.
4. Otherwise stop and format the error.

Error formatting (`ExtractApiErrorMessage`): parse `error.message` from either an object or the first element of an array -> `"HTTP {n}: {message}"`; else `"HTTP {n}: {body, max 500 chars + ellipsis}"` (status name when body blank). Cancellation is rethrown, everything else becomes a `ProviderResponse{IsError}` with request stats (attempts, retry notes).

### 2.9 Model list, capabilities, quota, extras

Model list (`ModelFetchService.FetchAntigravityModelsAsync` ~446): `POST {endpoint}/v1internal:fetchAvailableModels`, body `{"project": <AccountId>}` (project omitted when empty), same headers, 15 s timeout, endpoints in `OrderedEndpoints` order, first 200 wins. Parse `models` as a map keyed by model id. Exclude ids listed in `tabModelIds`, `imageGenerationModelIds`, `audioTranscriptionModelIds`, `deprecatedModelIds`, and legacy `claude-opus-4-6`/`claude-sonnet-4-6`. Capabilities per model: context from `maxTokens` (default 1,048,576), max output from `maxOutputTokens` (default 64,000 Claude / 65,536 other), tools yes, reasoning yes, vision yes except `gpt-oss`. The curated presets are appended after the live list when missing. Failure returns the presets.

Presets (`ProviderModelPresets`, Models/ProviderConfig.cs ~709): `gemini-4-argon-high`, `gemini-4-argon-medium`, `gemini-4-argon-low`, `gemini-3.8-flash-tiered`, `gemini-3.1-pro-low`, `claude-opus-5-5-thinking`, `claude-opus-5-5`, `claude-sonnet-5-5-thinking`, `claude-sonnet-5-5`. `gpt-oss-120b` leaves Antigravity on 2026-11-02 per the code comment. Catalog default model: `gemini-4-argon-high`. Default base URL: the daily host. Brand card hosts: daily and prod. Web-search default `EffectiveWebSearchMode` text mentions Google Search grounding.

Connection check (`CheckAntigravityConnectionAsync`): same `fetchAvailableModels` call, 200 -> "OAuth is valid (project X)", 401/403 -> key rejected, otherwise "Could not reach Cloud Code PA endpoints."

Quota (`AntigravityQuotaSource`): per endpoint, first `POST /v1internal:retrieveUserQuotaSummary` with `{"project":...}`; fallback `fetchAvailableModels`.
- Summary shape: `groups[]` with `displayName`, `groupId`, `buckets[]` each with `remainingFraction` (0-1), `window` or `bucketId` (contains `week` -> label `week`; `5h`/`hour` -> `5h`), `resetTime` ISO. `usedPercent = round((1 - remaining) * 100, 1)` clamped 0-100. A group is the "Claude/GPT pool" when its name contains Claude/GPT or its id starts with `c-`; windows are selected from the pool matching the active model, else all.
- Fallback shape: `models.<id>.quotaInfo.remainingFraction` and `resetTime`, label `5h`; exact model match, else same pool, else first.
- All failures -> `Unavailable("Couldn't refresh")`. Token comes from the same `GetValidAccessTokenAsync`.

Other uses of the same token (optional for v0.49): image generation `gemini-3.1-flash-image` via the same envelope (`imageGenerationModelIds`, 30 min cache of discovered models, fallbacks `gemini-3-pro-image`, `gemini-3.1-flash-lite-image`, stream first then `generateContent`); web search via `tools:[{googleSearch:{}}]` with models `gemini-3.1-flash-lite`, `gemini-3.8-flash-tiered`, `gemini-2.5-flash` and citations from `groundingMetadata.groundingChunks[].web`.

### 2.10 Doc vs code differences (use the code)

| Item | `antigravity-integration.md` | Code |
|---|---|---|
| Endpoint order | daily sandbox, autopush sandbox, prod | daily, prod, daily sandbox, autopush sandbox |
| User-Agent | `antigravity/1.18.3 windows/amd64` | `antigravity/hub/{ver} (aidev_client; ...)` with live version |
| `X-Goog-Api-Client` | `gl-node/22.17.0` | `google-cloud-sdk vscode/1.96.0` |
| `Client-Metadata`, `anthropic-beta` headers | sent | not sent (metadata only in `loadCodeAssist`/`onboardUser` bodies) |
| Claude thinking budgets | 8192/16384/32768 | 4000/10000/24000/32000 (Max) |
| Schema sanitizer | 8-phase pipeline, uppercase types | allow-list sanitizer, lowercase types |
| `onboardUser` polling | 2 s x 5 | 3 attempts, 3 s apart, re-check `loadCodeAssist` |
| Thought signatures | stored on `ChatMessage.ReasoningSignature` | static cache keyed by tool call id, sentinel fallback |

## 3. Meta Muse Code (subscription)

### 3.1 Sign-in: device authorization then key mint

Constants at `Services/OAuth/OAuthService.cs` lines 41-44: public client id `MuseClientId` (line 41), device URL `https://auth.meta.com/oidc/device/authorization/`, token URL `https://auth.meta.com/oidc/device/token/`, key mint URL `https://api.meta.ai/muse-code/key`. SeraBot already has the same constants in `crates/serabot/src/providers.rs` (lines 32-38). Meta publishes no refresh token for this flow.

Flow (`SignInMuseDeviceAsync`, line 906):

1. `POST device URL`, form body `client_id`, headers `Accept: application/json`, `x-api-version: 1.0.0`. No scope.
2. Response needs `device_code`, `user_code`; display `verification_uri_complete` else `verification_uri` else `https://auth.meta.com/`. `interval` default 5 s (min 1), `expires_in` default 300 s. Show the code and open the browser.
3. Poll `POST token URL` form `grant_type=urn:ietf:params:oauth:grant-type:device_code`, `client_id`, `device_code`, same two headers. Sleep `interval` before each poll. Handling: `authorization_pending` continue; `slow_down` interval += 5 capped at 30; `access_denied|authorization_denied|expired_token` -> "Meta sign-in was denied or expired. Try again."; HTTP 400 with no error code -> continue; other non-success -> error `"Meta device poll failed (HTTP n): ..."`; success needs non-empty `access_token`. Timeout at the `expires_in` deadline.
4. Mint (`MintMuseKeyAsync`, line 1000): `POST https://api.meta.ai/muse-code/key`, JSON `{"onboard": true}`, `Authorization: Bearer <meta access token>`, `Accept`, `x-api-version: 1.0.0`. Retry HTTP 429 and >= 500 up to 3 attempts, waiting 5 s then 10 s (the device code is one-shot, so a transient mint failure must not fail the login). Non-success error envelope: `title` + `detail` joined with `: `, plus ` - {action_url}`; message prefix `Muse Code key mint failed:`.
5. Mint success checks: `is_subs_active == false` -> "Muse Code subscription is inactive - activate it on muse.ai first."; no `api_key` and (`require_payment` true or an `action_url`/`require_payment_action_url`) -> "Muse Code subscription required: {url}"; no `api_key` otherwise -> "response did not contain an API key".
6. Persist: `AccessToken = api_key`; `RefreshToken = <Meta access token>`; `TokenExpiresAt = null`; `BaseUrl = https://api.meta.ai/v1`; `SignedInEmail = user_email` (trim, lowercase); `AccountId = subs_tier_name`. Also mirror to `AuthRoot`.

Design cross-check (`aio-proxy/.../2026-09-06-muse-oauth-design.md`, written by the owner for a sibling project): `onboard: true` only on the interactive login mint; quota and catalog must never send `onboard`; do not store a half-finished login (no key -> fail the whole login); the Meta OIDC access token is used only for later quota reads, never sent to `/v1`; discard any `refresh_token`/`expires_in` Meta might return; derive a stable account identity from `user_id`/email. HielCowork's quota call sends body `{}`; the design says no `onboard`. Both agree.

Refresh (`RefreshAsync`, line 1081): there is no refresh. If a stored Meta access token exists, re-mint with it (`MintMuseKeyAsync`); if not, throw "Meta Muse subscription key is invalid or revoked. Sign in again from Settings -> Providers." Because the key has no expiry it is only re-minted when empty or when an HTTP 401 forces the "stale" path. If the Meta access token has expired the re-mint fails and the user must sign in again.

Auth header on every Muse request (`ApplyAuthHeaders` line 605): `Authorization: Bearer <api key>` plus `x-api-version: 1.0.0`.

### 3.2 Chat transport: Responses API, not chat completions

`ProviderFactory` routes `MuseOAuth` to `ResponsesChatProvider` (the same class as Codex/Copilot/Grok, with `IsMuse` branches). `POST {BaseUrl}/responses` (`https://api.meta.ai/v1/responses`), `Accept: text/event-stream`, streaming SSE. The owner's 9router registry confirms Meta serves both `chat/completions` and `responses` on the same key, and states reasoning only round-trips on Responses (`9router/open-sse/providers/registry/muse.js` comment). SeraBot currently uses `/chat/completions` for `muse-code`; v0.49 should add a Responses path for Muse.

Payload (`BuildPayload`, ResponsesChatProvider.cs ~646):

- `model`, `input[]`, `stream: true`, `store: false`.
- `instructions` = system messages joined with a blank line.
- Input items: user text as `{role:"user", content}` or, with images, parts `input_text` and `input_image {image_url: data URI}`; assistant text `{role:"assistant", content}`; each tool call as `{type:"function_call", call_id, name, arguments}`; each tool result as `{type:"function_call_output", call_id, output}`. Outputs whose `call_id` was never emitted as a `function_call` are dropped (the endpoint rejects orphans). Empty input -> a user message `...`.
- Muse specifics: **no `temperature`**, **no `prompt_cache_key`**, **no `include`**/`reasoning.encrypted_content`, **no replay of reasoning items** (replay is only for the OpenAI dialect: Codex and Copilot).
- Reasoning (when effort != Off): `"reasoning": {"effort": <low|medium|high|max>, "summary": "auto"}`, nested, never top-level `reasoning_effort`. `ToApiEffort(Max)` is `"max"`; if Meta rejects a level the generic 400 fallback strips reasoning entirely. Recommendation for SeraBot: clamp Max to high for Muse unless a live check proves `max` is accepted.
- `max_output_tokens` included by default; dropped and retried once if the 400 body mentions `max_output_tokens`/`max output token`.
- Tools: `{type:"function", name, description, parameters}` using `ToolSchemaBuilder.Build`; `$ref` cycles collapsed, untyped/open objects collapsed to `string`, arrays always get `items`. On a 400 containing "Recursive JSON schema" (or `invalid_request_error` with `"param":"parameters"`), retry with `BuildFlat` (flat parameter map only).
- Hosted search: when the effective web search mode is native, append `{"type":"web_search"}`. On a 400 whose body mentions `web_search` with unknown/unsupported/invalid/"not support", retry without it.
- One combined 400 retry strips whichever of reasoning / `max_output_tokens` / recursive schema / web search the 400 body implicated; any other 400 is returned as an error.
- 401: force token stale, re-fetch (which re-mints) and retry once.
- SSE events handled: `response.output_text.delta`, `response.reasoning_summary_text.delta` / `response.reasoning_text.delta`, `response.output_item.added` and argument deltas for function calls, `response.completed|done|incomplete|failed|error`, `response.created|in_progress` for model/usage. Usage: input = `input_tokens - cached_tokens`; output = `output_tokens`; reasoning = `output_tokens_details.reasoning_tokens`. Empty content + no tool calls + no reasoning -> "The subscription endpoint returned an empty response."
- Model id rewrite: none for Muse.

Error mapping (`ErrorFromHttp`): 401 -> "Login expired. Sign in again from Settings -> Providers."; 402 or 403 on Muse -> "Meta rejected this Muse Code request (HTTP n). Ensure your Muse Code subscription is active on muse.ai."; 429 -> "Rate limited by the subscription endpoint. Wait a moment and retry."; else `"HTTP n: {body max 800}"`. `HttpStreamSupport` treats HTTP 402 and quota-looking bodies as `QuotaExhausted` without retrying.

### 3.3 Models, context, media

- Catalog (`ProviderCatalog.cs` brand `meta`): name "Meta Muse", default `muse-spark-1.3`, models `muse-spark-1.3`, `muse-spark-1.3-contributor`, `muse-spark-1.2`, `muse-spark-1.2-contributor`, `muse-spark-1.1` (the brand card for an API-key connection offers the same list on `https://api.meta.ai/v1` with a key from `https://dev.meta.ai`; the OAuth plan label is "Muse Code").
- Live list: `GET {BaseUrl}/models` with the auth headers; keep only ids that start with `muse-spark-`; sort; fall back to presets on failure or empty (`ModelFetchService` line 85). Capabilities are read from the listing when present.
- Context window: all Muse models are 1,048,576 tokens (`ModelContextResolver.cs` ~94 and 180).
- Image generation capability for Muse: default `muse-image-1.0` (`Services/Media/MediaCapabilities.cs` line 72); not needed for v0.49 chat.
- Tests with fixtures worth copying: `HielCowork.Tests/MuseProviderTests.cs` (quota parsing, labels, presets, flattened tool schema), `MistralAndMuseTests.cs`.

### 3.4 Quota (`MuseQuotaSource`)

`POST https://api.meta.ai/muse-code/key`, body `{}`, `Authorization: Bearer <Meta OIDC access token>` (the stored `RefreshToken`, or the root's), `x-api-version: 1.0.0`, 20 s timeout. If there is no token, or it starts with `LLM|` (legacy value that was the API key itself), return `Unavailable("Re-sign in to enable live quota")`.

Parse: `is_subs_active == false` -> unavailable "Muse Code subscription is inactive"; `require_payment == true` -> unavailable "Payment required to activate Muse Code subscription"; read `subs_usage.window {used_percent, window_duration_mins, resets_at}` and `subs_usage.weekly {used_percent, resets_at}`. `used_percent` may be number or numeric string, clamped 0-100. `window` label from minutes: >= 60 -> `{h}h` (one decimal when not whole, e.g. `1.5h`), < 60 -> `{n}m`, unknown -> `window`; weekly label `week`. `resets_at` may be unix seconds, unix ms (>= 1e12), or ISO-8601. Zero windows -> "Couldn't refresh". Console link `https://dev.meta.ai`.

## 4. OpenCode (Go, Zen, Free)

Source: `Services/Providers/OpenCode/OpenCodeProviderFactory.cs`, `docs/research-opencode.md`, `Services/OAuth/CliSessions.cs`, `Services/Quota/Sources/OpenCodeGoQuotaSource.cs`, `ProviderCatalog.cs`.

- **Go** (subscription, $5/mo then $10): OpenAI-compatible chat at `https://opencode.ai/zen/go/v1`. Authorization `Bearer <api key>`. The gate rejects requests lacking an `x-opencode-session` header (since 2026-09-06 per the code comment); the hook mints one id per provider instance (so per run/conversation) of the form `ses_` + 12 hex + 14 base62 characters and attaches it on every request (on each `HttpRequestMessage`, not default headers, so retries keep it). No User-Agent or client header is forged. Default model `glm-5.3-flash`; the card lists `glm-5.3-flash, glm-5.3, kimi-k2.7-code, kimi-k3, deepseek-v4.1-flash, qwen3.8-max, mimo-v2.6-flash, minimax-m3, grok-4.7, muse-spark-1.3-contributor` and presets add `deepseek-v4-pro, qwen3.8-flash, mimo-v2.6-pro, space-bunny-free`. Context default 1,000,000.
- **Zen** (pay as you go): plain OpenAI-compatible at `https://opencode.ai/zen/v1`, no hook. Default `claude-fable-5-1`. Key from `https://opencode.ai/auth`.
- **Key acquisition**: OpenCode has no paste-able OAuth. The card offers "Sign in with the OpenCode CLI" (user runs `opencode auth login`; HielCowork launches the CLI) or paste a key. Import reads `~/.local/share/opencode/auth.json` (honouring `XDG_DATA_HOME`; HielCowork found OpenCode uses the Unix-style path even on Windows), with fallbacks `~/.opencode/auth.json`, `%LOCALAPPDATA%/opencode/auth.json`, `%APPDATA%/opencode/auth.json`. The file is a flat object keyed by provider id with `{"type":"api","key"}` or `{"type":"oauth","access","refresh","expires"}`; **only entries whose id starts with `opencode` are read** (the same file holds the user's other providers, which are not the app's to take). v2 stores credentials in SQLite `opencode.db` (read-only open; table `credentials`, column guesses `key|access_token|api_key` with `provider|name|id` filter; best effort). Never written back.
- **Usage meters (Go)**: `GET https://opencode.ai/zen/go/v1/usage`, Bearer key. Response `usage.rolling` (label `5h`), `usage.weekly` (`week`), `usage.monthly` (`month`), each with `status` (must be `ok` or absent), `percent` (used, clamped 0-100) and `resetsAt` ISO. Failure -> "Couldn't refresh", console `https://opencode.ai/auth`.
- **Free lane** (no key): a localhost proxy `127.0.0.1:27680` that spoofs OpenCode's own client (User-Agent `opencode/...`, `x-opencode-client: desktop`, a forged session id, and four decoy tools `bash/glob/grep/read` injected into every request). **Do not port this to SeraBot.** It is impersonation of another vendor's client against a lane meant for that client, which HielCowork's own docs call out as fragile and a contract violation risk, and AGENTS.md forbids bypassing policy and forged identities. The owner asked to see how openbot does OpenCode; that is a separate study. For HielCowork parity, implement Go and Zen only.
- `ProviderModelPresets`/`ProviderCatalog` infer an OpenCode lane from a legacy OpenAI-compatible card by host (`opencode.ai`, `/zen/go`) so old saved configs get the Go header hook (`OpenCodeProviderFactory.TryInferLane`).

## 5. Other sign-in flows in HielCowork (reference for v0.49 marketplace of sign-ins)

All in `OAuthService.cs` unless noted. Public client ids are constants at lines 13-48.

- **ChatGPT / Codex** (PKCE or device): authorize `https://auth.openai.com/oauth/authorize`, token `https://auth.openai.com/oauth/token`, scope `openid profile email offline_access`, loopback `http://localhost:1455/auth/callback` (device fallback when the port is busy: `.../api/accounts/deviceauth/usercode`, poll `.../deviceauth/token`, verify page `https://auth.openai.com/codex/device`). Requests carry `ChatGPT-Account-ID` (from the JWT), `OpenAI-Beta: responses=v1`, `originator`, `session_id` (conversation key). Refresh tokens rotate. Tokens importable from `~/.codex/auth.json` and written back. Quota: `CodexQuotaSource`.
- **Grok Build (xAI)** (PKCE loopback on an ephemeral port, device fallback): authorize/token/device under `https://auth.x.ai/oauth2/`; scope includes `grok-cli:access api:access conversations:read conversations:write`; chat base `https://cli-chat-proxy.grok.com/v1`; headers `x-xai-token-auth`, `x-grok-client-identifier`, `x-grok-client-version`, `x-grok-conv-id`, `x-grok-session-id`, `x-grok-model-override`. Rotating refresh token, imported from `~/.grok/auth.json` with write-back. Models list `.../models-v2` with fallback `/models`.
- **GitHub Copilot** (device flow, `SignInCopilotDeviceAsync` ~1217): `POST https://github.com/login/device/code` form `client_id`, `scope=read:user` (public client id constant at line 35); poll `https://github.com/login/oauth/access_token` with `grant_type=urn:ietf:params:oauth:grant-type:device_code`; store the long-lived GitHub token as `RefreshToken`. Exchange: `GET https://api.github.com/copilot_internal/v2/token` with `Authorization: Bearer <github token>` plus the editor headers; response `token`, `expires_at` (unix), `endpoints.api` (accept only https hosts ending `githubcopilot.com`; business/enterprise accounts have a different host). 403/404 -> "no active Copilot subscription". Refresh = repeat the exchange; on HTTP 401 sign out. Editor headers on every call (`CopilotService.ApplyEditorHeaders`): `User-Agent`, `Editor-Version`, `Editor-Plugin-Version`, `Copilot-Integration-Id: vscode-chat`, `X-GitHub-Api-Version` (a recent date; the old version returns an under-reported legacy catalog). Catalog `GET {api}/models` with `OpenAI-Intent: model-access`; auto router `POST {api}/models/session` with `{"auto_mode":{"model_hints":["auto"]}}` returning `session_token` and `available_models`, restricted to first-party clients (404 sets a process-wide "unavailable" flag). These values imitate the VS Code extension and are not a published API; HielCowork notes this in `ProviderConfig.OAuthHelpText`.
- **OpenRouter** (PKCE, returns a plain API key): `https://openrouter.ai/auth?callback_url=&code_challenge=&code_challenge_method=S256&state=&key_label=`; exchange `POST https://openrouter.ai/api/v1/auth/keys` JSON `{code, code_verifier, code_challenge_method}` -> `key`. Saved like a pasted key, nothing to refresh.
- **Mistral** (vibe-style browser flow, returns an API key): `POST https://console.mistral.ai/api/vibe/sign-in` `{code_challenge, code_challenge_method}` -> `process_id, sign_in_url, poll_url, expires_at`; both URLs must be under the console hosts (origin check `IsUnderBase`); poll every 3 s (tolerate 3 consecutive failures; 410 = expired); on `completed` take `exchange_token`; `POST .../vibe/sign-in/{process_id}/exchange` `{exchange_token, code_verifier}` -> `api_key`. Optional `GET https://console.mistral.ai/api/vibe/whoami` -> `api_base` accepted only for https hosts under `mistral.ai` (`MistralTenantApiBase`).
- **Claude plan (personal use)** (`ProviderTypes.ClaudeCodeSubscription`): not an HTTP provider; it drives the Claude Agent SDK with the Claude Code CLI's own login on the same computer, offered only for the owner's own use. Quota (`ClaudePlanQuotaSource`) calls `GET https://api.anthropic.com/api/oauth/usage` with the CLI's OAuth token, `anthropic-beta: oauth-2025-04-20`, and a `claude-code/...` User-Agent (without it the endpoint rate-limits). Do not copy the User-Agent spoof or the CLI token read into SeraBot; the card text itself says Anthropic does not allow third-party apps to offer plan sign-in. This is the open question for the v0.49 "Claude Code sign-in with multi-account" goal and needs an owner decision (API-key accounts are safe; plan OAuth is not provably allowed).
- **Policy text in `ProviderCatalog.cs` header**: HielCowork states Anthropic and Google forbid using Claude or Gemini subscriptions in other apps (Feb 2026), yet offers Antigravity OAuth. Flag for the owner: Antigravity is the one Google subscription path, it uses Google's own desktop client credentials, and the risk is account suspension. Make it opt-in per user with a visible warning.

## 6. Cross-cutting behaviour to mirror

`HttpStreamSupport.cs`:
- `SendWithRetryAsync`: default 3 attempts (Antigravity passes 2); retries connection exceptions and HTTP 408, 429, 529, >= 500; backoff `2^attempt` seconds; honours `Retry-After` (seconds or date) and `retry-after-ms`; refuses to sit out a wait over 60 s and returns the response to the caller (so failover can happen); HTTP 402 or a quota-looking body (`insufficient_quota`, `quota exceeded`, `usage_limit_reached`, ...) is returned immediately flagged `QuotaExhausted`. Records attempts and retry notes in `RequestStats`.
- Streaming idle timeout: 90 s minimum (configurable up, "Model wait" setting); a stalled stream raises "No data received for N seconds".
- Response status `ResponseHeadersRead` so tokens stream.

Provider client profiles (`ProviderClientProfile.cs`): a record of quirks per brand (empty-reply strategy, tool cap), selected by brand key or host. Only Mistral/GLM use it today; Antigravity "graduated to a dedicated client". Use the same idea for SeraBot per-provider capability flags (supports temperature, supports `max` effort, native web search, tool cap).

Empty-reply handling in the agent loop (`EmptyReplyStrategy`): resend same / answer-now nudge / fresh context with tool results bundled, no tools. Relevant to auto-resume when a sub-agent model returns thinking with no answer.

Multi-account in HielCowork: there is no first-class "N accounts for one provider with automatic rotation". Each `ProviderConfig` is one connection; several can live under one brand card; the Router provider (`ProviderTypes.Router`, `Services/Routing/RoutedChatProvider`) sends each request to a model/connection and moves on when one fails. `dsh-agy`'s `accounts.json` (an `accounts[]` array with an `enabled` flag) is the only multi-account store it reads, and only the first enabled entry is used. SeraBot must design real multi-account itself (section 7.4).

## 7. How to implement in Rust in SeraBot

### 7.1 Current state in this repo (skimmed, not audited)

- `crates/serabot/src/cloud_code.rs` (1,884 lines): Antigravity PKCE login on loopback (port 51121, fallback ephemeral), `loadCodeAssist`/`onboardUser` discovery, envelope builder, `sanitize_schema` with `$ref` hoisting and hints, SSE/JSON parse, host allow-list `CLOUD_CODE_HOSTS` (same four hosts and order as HielCowork), user agent `antigravity/hub/2.25.0 ...`, `google-cloud-sdk vscode/1.96.0`.
- `crates/serabot/src/providers.rs`: provider registry (`muse-code`, `google-antigravity` are `auth: oauth`), `login_muse_code` (device flow), `muse_mint_api_key`, `OAuthStyle::{MuseMint, ...}`, 401-driven refresh in `llm.rs::refresh_oauth` (Muse = re-mint, Google = refresh grant), per-user credential store in `user_secrets.rs`, `thinking.rs` (`levels_for`, `wire_fields`).
- Docs: `docs/antigravity.md`, `docs/providers.md`.

### 7.2 Antigravity gap list (do in `cloud_code.rs` + `llm.rs`)

1. **Thinking level mapping** (section 2.6): replace the hard-coded `8192` / `"medium"` with the bot's effort. Add a pure function `antigravity_thinking(model, effort, max_output) -> Option<Value>` and a unit test table. Respect: baked-in level suffixes send no config; Off -> `thinkingBudget: 0` for level models; Max -> `high`; Claude budgets 4000/10000/24000/32000 clamped to `[1024, max_output-1024]` and only when `max_output > budget`; temperature only when not Claude or effort is Off. `maxOutputTokens`: cap 64,000 Claude / 65,536 other, clamp lower bound 1024.
2. **Thought signatures**: capture `thoughtSignature` from `functionCall` parts and replay it on that call id; sentinel only as fallback. Store per `(user_id, conversation_id)` with a bounded LRU (2000), inside the turn state, never in a process-global map.
3. **Reasoning**: parse `thought: true` parts into a separate reasoning stream (stored for display, bounded and sanitised as in `thinking.rs`); replay as `{thought:true,text}` on later turns for non-Claude models only.
4. **Session id**: derive `sessionId` deterministically (FNV-1a over `"{project}:{conversation}[:genN]"`, fold `% 9e18`, prefix `-`), not a fresh random id per request; bump the generation once on the `exceeds the maximum number of tokens` 400 and retry the same origin. Keep the generation counter in the conversation row (survives restarts) rather than process memory.
5. **Claude prefill**: strip trailing `model` turns for any `claude*` model, not only `claude`+`thinking`.
6. **`is_error`**: set from the real tool outcome instead of `false`.
7. **Images**: allow `inlineData` user parts for vision-capable models when the attachment pipeline provides bytes (after the blocklist/policy scan).
8. **MALFORMED_FUNCTION_CALL**: resample up to 3 times when no content/tool calls came back.
9. **Tool name aliasing**: `web_search`/`search_web` -> `agy_*` and back; exclude `google_search`.
10. **Error formatting**: stream `error` objects -> `HTTP {code}: {message}` or `Antigravity error ({status}): {message}`; HTTP errors -> `error.message` else a 500-char body snippet. In SeraBot, all of these strings must pass the secret/blocklist scrub and must never include tokens (existing rule: "Response bodies are dropped on errors"; keep dropping bodies, but keep `error.message` since it is provider text, truncated and control-character stripped).
11. **Failover**: keep the 4-origin chain; per-origin retry 2; failover on 429/403/503/5xx; the Claude "wire alias" retry is optional (see 2.8).
12. **Version**: HielCowork resolves the live Hub version from a public updater URL at runtime. SeraBot should keep a pinned constant and make it config-overridable; an outbound call to an updater is a new egress destination and needs the egress list and owner approval. Do not add it silently.
13. **Quota**: implement `retrieveUserQuotaSummary` (with `fetchAvailableModels.quotaInfo` fallback) as a read-only call routed through `gateway` (it is an outbound effect), returning `Vec<QuotaWindow{label, used_percent, resets_at}>`. Use it for model failover decisions (mark an account exhausted until `resets_at`).
14. **Model catalog**: replicate the exclusion lists (`tabModelIds`, `imageGenerationModelIds`, `audioTranscriptionModelIds`, `deprecatedModelIds`), skip `claude-*-4-6`, carry per-model `maxTokens`/`maxOutputTokens` into the capability record, append curated presets.
15. **Sanitizer**: add union `type` arrays -> `nullable`, `const` -> `enum`, filter `required` to existing properties, and make sure `anyOf/oneOf` null-flattening matches section 2.5. Property-test: the output contains only allow-listed keys.

### 7.3 Muse gap list (do in `providers.rs` + `llm.rs`)

1. Add a Responses transport for `muse-code` (new `ApiShape::Responses` or a small `responses.rs`): `POST {base}/responses`, SSE events listed in 3.2, payload rules (no `temperature`, no `prompt_cache_key`, no `include`, nested `reasoning {effort, summary:"auto"}`, `store:false`, `instructions`, orphan `function_call_output` pruning).
2. Send `x-api-version: 1.0.0` on **every** Muse call (chat, catalog, mint, quota), not only the mint. Today `providers.rs` only sets it on the mint.
3. Persist both the minted key and the Meta access token per account in the per-user envelope store (two fields of one `ProviderCredential`), plus `email`, `tier_name`, `account_id` as non-secret columns. Never reuse one generic `account_id` field for a tier name as HielCowork does.
4. Re-mint on 401 only when a Meta access token exists; otherwise surface "re-sign-in required" and mark the credential `needs_reauth` so the UI and the auto-resume logic skip it. Re-mint never sends `onboard`; only the interactive login mint does (follow the aio-proxy design). Retry the mint on 429/5xx (5 s, 10 s).
5. Device flow details to match: poll interval default 5 s (min 1), `expires_in` default 300 s, `slow_down` +5 s capped at 30, HTTP 400 with no error code continues, denied/expired errors are terminal. SeraBot is a server: run the poll server-side and return `{verification_uri_complete, user_code, expires_at}` to the client; the client shows the code and opens the browser. No loopback needed, so this works for remote members. Pin the verification URL to `auth.meta.com` (already in `pin_muse_verification_uri`).
6. Error mapping: 401 "Login expired"; 402/403 "Meta rejected this Muse Code request ... subscription active on muse.ai"; 429 rate-limit; other `HTTP n: <truncated body>`. Never include the key.
7. Model list: `GET https://api.meta.ai/v1/models`, filter ids starting `muse-spark-`, fall back to the five presets; context window 1,048,576 for every `muse-spark-*`.
8. Quota: `POST https://api.meta.ai/muse-code/key` with the Meta access token, body `{}`, parse `subs_usage.window`/`weekly` per 3.4 (resets as seconds, ms or ISO). Treat `is_subs_active == false` and `require_payment` as terminal states with their own messages.
9. Thinking: effort -> `{effort: low|medium|high, summary: "auto"}`, clamp `Max` to `high` for Muse; Off -> omit `reasoning`. On a 400 that mentions reasoning/effort/unknown parameter, retry once without reasoning (HielCowork behaviour), and log a provider capability note so the same account stops sending it.
10. Tool schemas for Muse: flatten recursive `$ref`, collapse untyped objects/open `additionalProperties`, always give arrays `items` (HielCowork's `ToolSchemaBuilder`); on "Recursive JSON schema" 400 retry with the flat form. A shared `schema.rs` function used by Muse, Antigravity and the OpenAI-compatible path avoids four copies (HielCowork's own comment says that duplication caused bugs).

### 7.4 Multi-account design (new for v0.49, informed by HielCowork's gaps)

HielCowork stores one sign-in per `ProviderConfig` and shares it via `AuthRoot`; rotating tokens forced a per-root refresh gate. SeraBot should model accounts explicitly:

- Table `provider_accounts(user_id, provider_kind, account_id TEXT, label, email, auth_kind /* oauth|api_key|device */, enabled, priority, status /* ok|needs_reauth|exhausted|disabled */, exhausted_until, created_at, last_used_at)` plus the secret material in the existing per-user envelope store keyed by `(user_id, provider_kind, account_id)`. Every store method takes `user_id` (AGENTS.md: no unscoped path).
- Selection per request: for the bot's configured provider pick accounts ordered by `priority`, skipping `needs_reauth`, `disabled` and `exhausted_until > now`. On `QuotaExhausted`/429-with-long-retry-after/402, set `exhausted_until` (from quota `resets_at` when known, else `Retry-After`, else a short cooldown) and try the next account, then the bot's fallback model/provider (the user's fallback chain from the v0.49 spec).
- Single-flight refresh per `(user_id, provider_kind, account_id)` using a `tokio::sync::Mutex` map; re-read the stored credential inside the lock (equivalent of `SyncFromRoot`); write refreshed tokens before releasing so the next waiter reads them. Run-scoped copies must never refresh independently (HielCowork's bug class).
- Refresh failure policy: Google 400/401/`invalid_grant` -> `needs_reauth` (do not delete the row; keep the audit trail), emit an event for the console; Copilot 401 same; Muse re-mint failure same.
- Sign-in surfaces: Antigravity PKCE must run on the user's own device because the redirect is loopback. Flow: the desktop/mobile client starts a login (server returns authorize URL pieces and a state/verifier it keeps), the client runs the loopback listener, then POSTs `{code, state}` to the server, which exchanges the code using the client secret from the age store (this is what `complete_antigravity_login` already models). Muse, Copilot, Codex-device and Mistral flows are server-pollable. Invitation-only users only; no sign-up.
- Admin never reads member credentials (AGENTS.md); the admin sees account labels, status and audit rows only.

### 7.5 What not to port, and approval gates

- No CLI credential file imports on the server (`CliSessions` equivalent). If offered, owner-desktop-only, read locally, uploaded as the owner's own account, never written back.
- No OpenCode Free proxy, decoy tools, forged `x-opencode-*` identity, or `claude-code/*` User-Agent spoofing.
- No runtime fetch of the Antigravity updater URL without an egress-list entry and owner approval.
- Each new outbound host (`auth.meta.com`, `api.meta.ai`, `*.cloudcode-pa.googleapis.com`, `opencode.ai`, `api.github.com`, `api.githubcopilot.com`, `console.mistral.ai`, `openrouter.ai`) goes through the existing egress approval and `gateway` audit; per-account data egress consent as in the current `data_egress_approved` field.
- Sign-in flows that create accounts or grant OAuth scopes are user-initiated only; nothing is signed in by an agent.

### 7.6 Suggested module layout and tests

- `cloud_code.rs`: split into `cloud_code/{auth.rs, envelope.rs, schema.rs, sse.rs, quota.rs, catalog.rs}` only if touched heavily; keep public API stable.
- New `provider_accounts.rs` for the table, selection, cooldown and single-flight refresh; thin adapters per provider (`antigravity`, `muse`, later `copilot`, `opencode`).
- New `thinking` rows for `google-antigravity` and `muse-code` in `thinking.rs::levels_for` and `wire_fields`: Antigravity levels are `off|low|medium|high|max` (max maps to `high` for level models), Muse `off|low|medium|high`.
- Tests with synthetic fixtures only (wiremock is already used): mirror `AntigravityProviderTests.cs` and `MuseProviderTests.cs` cases: envelope shape per model family, thinking table, signature round trip, sanitizer allow-list property test, session id determinism (FNV-1a known vectors), 400 session-wall rotation, endpoint failover order, device-poll state machine (`authorization_pending`, `slow_down`, denied, expired, 400 without error), mint retry on 429, mint failure leaves no stored half login, 401 -> single re-mint, quota parsing with seconds/ms/ISO and string percentages, `is_subs_active=false`, `require_payment`.
- Docs to update when implemented: `docs/antigravity.md`, `docs/providers.md`, `docs/marketplace.md` if sign-in providers are listed there. Report nothing as live-verified.

## 8. Quick reference tables

Thinking by provider (HielCowork behaviour):

| Provider/model | Off | Low | Medium | High | Max |
|---|---|---|---|---|---|
| Antigravity level model (gemini-3/4, `-tiered`) | `thinkingBudget:0` | `thinkingLevel:low` | `medium` | `high` | `high` |
| Antigravity id with `-low/-medium/-high` suffix | no thinking config | same | same | same | same |
| Antigravity Claude | no thinking config (temperature sent) | budget 4000 | 10000 | 24000 | 32000 (all clamped to `[1024, maxOut-1024]`) |
| Muse (Responses) | omit `reasoning` | `{effort:low,summary:auto}` | medium | high | `max` (recommend clamp to high) |
| Copilot | only levels its catalog lists for the model; others omitted | | | | |

Endpoints by provider:

| Provider | Auth | Chat | Models | Quota |
|---|---|---|---|---|
| Antigravity | Google PKCE (loopback 51121) | `POST /v1internal:streamGenerateContent?alt=sse` | `POST /v1internal:fetchAvailableModels` | `POST /v1internal:retrieveUserQuotaSummary` (fallback models quotaInfo) |
| Muse | Meta device flow + `POST /muse-code/key` | `POST https://api.meta.ai/v1/responses` | `GET https://api.meta.ai/v1/models` (`muse-spark-*`) | `POST https://api.meta.ai/muse-code/key` (`subs_usage`) |
| OpenCode Go | key | `https://opencode.ai/zen/go/v1` + `x-opencode-session` | presets | `GET .../go/v1/usage` |
| OpenCode Zen | key | `https://opencode.ai/zen/v1` | presets | none |
| Copilot | GitHub device flow + token exchange | per-account host from the token response | `GET {api}/models` | `CopilotQuotaSource` |
