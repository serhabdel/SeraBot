# OpenBot provider study (input to SeraBot v0.49)

Source studied (read-only): `nightly-labs/openbot`, Electron/TS app, commit `dfb9d81`, cloned in the session scratchpad. Paths below are relative to the repo root. All repo content was treated as untrusted data; nothing was run or installed. No tokens or secrets are copied here.

## 0. The key finding

OpenBot does NOT implement the Claude, Codex, Gemini/Antigravity, Muse, Grok, Cursor, Kimi or Hermes OAuth flows itself. It **wraps each vendor's own CLI** (or SDK) and drives the CLI's login, then keeps one **isolated CLI home folder per account** (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `GROK_HOME`, `GEMINI_HOME`). OpenBot never reads or refreshes the Claude/Codex OAuth token itself. Consequences:

- There are no authorize/token/refresh URLs, client IDs, scopes or PKCE code for Claude/Codex/Muse/Hermes in the repo. Those live inside the CLIs. Anything below that says "token endpoint" is only for the cases where OpenBot really talks HTTP: MCP OAuth (generic, spec-driven), the GitHub connector device flow, and the Antigravity quota read.
- Native OAuth in Rust (SeraBot's likely need, since SeraBot has no desktop CLI per user) cannot be copied from openbot. The per-user home-folder isolation and the surrounding orchestration (accounts, pool, failover, usage gate, effort mapping) are what can be copied. Section 9 says how to do both options.

The only first-party OAuth-like code in openbot:

| Flow | File | Notes |
|---|---|---|
| Generic MCP OAuth (DCR, PKCE, refresh) | `src/backend/mcp-oauth-provider.ts` | Uses `@modelcontextprotocol/sdk` `auth()`, `refreshAuthorization`. Loopback redirect `http://127.0.0.1:28417/mcp-auth` (`src/main/mcp-oauth-redirect-server.ts`), 10 min timeout `MCP_SIGN_IN_TIMEOUT_MS`. |
| GitHub connector device flow | `src/main/github-device-flow.ts`, `src/main/github-connector-config.ts` | `https://github.com/login/device/code` and `https://github.com/login/oauth/access_token`. Public GitHub App client id is a constant in `github-connector-config.ts` (`GITHUB_APP_CLIENT_ID`), overridable by `OPENBOT_GITHUB_CLIENT_ID`. |
| Antigravity quota read | `src/backend/antigravity-usage.ts` | Reads the CLI's token file, calls Google Code Assist (section 5). |

## 1. Architecture map

| Concern | Where |
|---|---|
| Provider ids and descriptor table (display name, authKind, signInMessage, localCodeSignIn, accountSignIn, skill folders) | `packages/contracts/src/agent-providers.ts` (`AGENT_PROVIDERS`, `AGENT_PROVIDER_DESCRIPTOR_TABLE` with `satisfies Record<AgentProviderId, ...>`, `KEY_ONLY_PROVIDERS = inception, deepseek, openrouter`, `MANAGED_RUNTIME_PROVIDERS`) |
| Provider ids present | codex, claude, grok, opencode, antigravity (shown as "Gemini"), cursor, cline, kimi, devin, copilot, mistral, pi, muse, inception, deepseek, openrouter, hermes, acp (custom) |
| Drivers (behavior per provider: signIn kind, codeSignIn, accounts, catalog rules, createClient, authState) | `src/backend/provider-drivers.ts` (`BUILT_IN_PROVIDER_DRIVERS`) |
| Runtime dispatch of sign-in | `src/backend/agent/provider-runtime.ts` (`#signIn`, `#startProviderCode`) |
| Pinned managed CLI runtimes | `native-runtime.lock.json` (per-platform asset, SHA-256, size, executable), `provider-runtime-blocklist.json` (per-provider blocked versions, all empty), `src/main/provider-runtime-*.ts` |
| Accounts | `src/backend/agent/provider-accounts.ts`, `provider-account-store.ts`, `provider-account-pool.ts`, `src/main/provider-accounts-host.ts` |
| Model catalog | `src/backend/agent/model-catalog.ts`, `provider-models.ts`, `model-choice.ts`, `acp-configuration.ts` |
| Usage limits and failover | `src/backend/agent/usage-limit-gate.ts`, `usage-read-gate.ts`, `account-usage.ts`, `turn-lifecycle.ts` (`classifyFailure`) |
| Docs | `docs/architecture/providers.md` (the best single source), `integrations.md` (sign-in kinds, "Add a provider"), `servers.md` (reasoning efforts), `agents.md` (agent self-management tools) |

Adding a provider = one descriptor row + one driver row (type checker finds the rest). That "registry row plus driver" shape is worth copying.

## 2. Sign-in kinds (the vocabulary)

From `integrations.md` and `ProviderSignIn` / `ProviderCodeSignIn` in `provider-drivers.ts`:

- `browser`: provider protocol returns a URL, OpenBot opens it (Codex app-server `account/login/start` type `chatgpt`, returns `authUrl` and `loginId`; the CLI owns the loopback listener, port 1455 in current Codex).
- `cli-command`: spawn the CLI and wait for exit (Claude `claude auth login --claudeai`, Grok `login`, 10 min timeout `CLI_LOGIN_TIMEOUT_MS`).
- `acp-authenticate`: start the ACP server and call `authenticate` with a method id (Antigravity/Gemini `oauth-personal`, separate process, `src/backend/acp-sign-in.ts`).
- `muse-device`: `muse serve` request `account/loginStart {type:"deviceCode"}` returns `verificationUrl` and `userCode`.
- `terminal`: open the CLI in an interactive terminal (Pi).
- `external`: user signs in themselves, OpenBot re-probes.
- Code sign-ins for remote hosts: `codex-device` (`account/login/start {type:"chatgptDeviceCode"}`), `cli` with flow `device` (Grok `--device-auth`, Cline, Kimi), `paste` (Claude: CLI prints link plus "Paste code here"; OpenBot types the pasted code into the CLI stdin via `submit(code)`), `link` (Cursor).
- Callback relay (`sign-in-relay-v1`): host captures the page URL (via a `BROWSER` shim command for Claude, an `xdg-open` shim for Antigravity, `authUrl` for Codex), the client browser signs in, and the client forwards only the callback query to the CLI's loopback listener on the host. Host checks `state`, flow is single use, 128-bit id, 10 min TTL, admin-only. Files: `src/main/sign-in-relay-host.ts`, `sign-in-relay-client.ts`, `loopback-callback-server.ts`, `src/backend/agent/browser-capture.ts`.

## 3. Claude (Claude Code)

### Runtime
`ClaudeAgentClient` in `src/backend/claude-client.ts` (3.5k lines) uses `@anthropic-ai/claude-agent-sdk` `query()` driving the user's `claude` CLI (`resolveClaudeCli`).

### Sign-in
- Browser: `claude auth login --claudeai`, env `DISABLE_AUTOUPDATE=1` when the CLI is OpenBot-managed. On Windows the CLI opens the page with rundll32, so OpenBot also catches the page (`windowsPageByOpenBot`).
- No browser (Linux/remote): `codeSignIn` flow `paste`.
- Sign-out: `claude auth logout`.
- API key fallback: a saved Anthropic API key is passed only as `ANTHROPIC_API_KEY` env when `claude auth status` says not logged in (`#fallbackApiKey`, `claudeFallbackApiKey`). It belongs to the own sign-in (first account) only. Process then also sets `CLAUDE_CODE_ENTRYPOINT=sdk-ts`.

### Credential storage (read-only inspection, no refresh)
`src/backend/claude-stored-sign-in.ts` reads the CLI's own store without starting the CLI, because `claude auth status` starts an OAuth refresh and exits after 2 s without saving rotated tokens, so the next process gets `invalid_grant` and clears the sign-in (anthropics/claude-code issues cited in the file).

- File: `<CLAUDE_CONFIG_DIR or ~/.claude>/.credentials.json`, JSON with `claudeAiOauth.{refreshToken, refreshTokenExpiresAt, subscriptionType}` (plus an access token that OpenBot does not read).
- macOS: Keychain generic password service `Claude Code-credentials` (suffix `-<first 8 hex of sha256(configDir)>` when `CLAUDE_CONFIG_DIR` is custom), account = `$USER`.
- Account metadata (email, organizationUuid, organizationName): `.config.json` or `<CLAUDE_CONFIG_DIR or ~>/.claude.json` key `oauthAccount`.
- Env vars that override the store (treated as "signed in from outside"): `CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR`, `CLAUDE_CODE_OAUTH_REFRESH_TOKEN`, `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_IDENTITY_TOKEN(_FILE)`, `CLAUDE_CODE_USE_BEDROCK|VERTEX|FOUNDRY|GATEWAY|MANTLE|...`, `CLAUDE_CODE_CUSTOM_OAUTH_URL`, `USE_STAGING_OAUTH`, `USE_LOCAL_OAUTH`.

### Multi-account
`accounts: { homeVariable: "CLAUDE_CONFIG_DIR", signIn: CLAUDE_SIGN_IN, signOut: auth logout }` in the driver. First account = the CLI's default home. Each additional account = its own folder created by `ProviderAccountStore` (`src/backend/agent/provider-account-store.ts`): ids `a<16 hex>`, label max length, `MAX_PROVIDER_ACCOUNTS = 10` (`packages/contracts/src/ipc-agent-status.ts`), store file `{version:1, accounts[], defaultLabels, pools:{provider:{leftOut[]}}}` written atomically; the folder is created before the account is saved and deleted after removal, credentials never go in the list file. Sign-in for an extra account runs the same CLI command with `CLAUDE_CONFIG_DIR` pointing at the new folder. A per-agent `providerAccountId` selects the account; `default` means the own sign-in. A second account with the same email is refused as a duplicate.

### Models and effort
- `supportedModels()` on the SDK query returns `ModelInfo[]` with `supportedEffortLevels` and `supportsEffort`. Effort set: `low, medium, high, xhigh, max` (`CLAUDE_EFFORTS`, line 141). Default effort = `high` if supported else first supported. A model with `supportsEffort === false` carries no effort.
- Live setters, no new session: `query.setModel(model)` and `query.applyFlagSettings({ effortLevel })` (null clears) and `applyFlagSettings({ fastMode })`, applied at the next turn (`claude-client.ts` ~1630-1670). A model change re-resolves the effort (`#resolveEffort`).
- Display names derived from ids by `claudeModelName` (`src/backend/agent/provider-models.ts`): `claude-haiku-4-5-20251001` becomes "Claude Haiku 4.5"; `[1m]` suffix = 1M context variant.
- Env knobs: `ENABLE_PROMPT_CACHING_1H=1` (1h cache TTL), `CLAUDE_CODE_AUTO_COMPACT_WINDOW`.

### Usage
Plan limits via the SDK's experimental `usage` call with `skipBehaviors:true` (avoids reading every transcript). Response `rate_limits.five_hour` and `seven_day` (or a model-family weekly key) are mapped to a primary window (300 min) and secondary (10080 min). HTTP 429 from the shared usage endpoint means a failed read: keep the last reading and back off (see section 8).

## 4. Codex / OpenAI

- Runtime: `CodexAppServerClient` (`src/backend/app-server-client.ts`) speaking the Codex app-server JSON-RPC; pinned binary in `native-runtime.lock.json` (codex `0.160.1`, tag `rust-v0.160.1`, Apache-2.0, per-platform tarball + SHA-256).
- Sign-in methods via `src/backend/agent/codex-login.ts`:
  - `account/login/start {type:"chatgpt", appBrand:"chatgpt"}` returns `{authUrl, loginId}`; OpenBot opens `authUrl`; the Codex CLI takes the loopback callback (port 1455); completion arrives as a notification carrying `loginId`; then `account/read {refreshToken:true}` must report `account.type === "chatgpt"`.
  - `account/login/start {type:"chatgptDeviceCode"}` for remote hosts.
  - `account/login/start {type:"apiKey", apiKey}` for key logins (no plan usage, so `validateAccount` rejects non-ChatGPT types for plan features).
  - Cancel: `account/login/cancel {loginId}`. Fallback CLI: `codex login` / `codex logout`.
- Multi-account: `homeVariable: "CODEX_HOME"`, same folder-per-account mechanism. `SSL_CERT_FILE` is stripped from the child env (it breaks Codex's own root discovery).
- Models: `model/list`, paged (`catalog.paged`), with Fast service tier fields (`serviceTierFields`). Efforts come from the model entries (`supportedReasoningEfforts`).
- Usage: `account/rateLimits/read` (+ `account/rateLimits/updated` push), reset credits `rateLimitResetCredits.availableCount`, spend with `account/rateLimitResetCredit/consume`.

## 5. Antigravity / Gemini (provider id `antigravity`)

Most detailed section of `providers.md` (lines ~796-990). Relevant facts for "follow Hiel Cowork" parity:

- Runtime: Google's Antigravity CLI (`google-antigravity/antigravity-cli`), not redistributable, so downloaded per user. Started as `antigravity acp --gemini-dir=<abs path> --app-data-dir=antigravity-acp` (`antigravityArgv`, `src/backend/acp-flavor-antigravity.ts`). The CLI ignores `GEMINI_HOME`; the folders are flags. Gemini dir must be absolute. Never resolve from `PATH` (the Antigravity editor installs a different `antigravity` command). Override: `OPENBOT_ANTIGRAVITY_PATH`, version at least 1.5.3 for extra accounts.
- Sign-in: ACP `authenticate {methodId:"oauth-personal"}` in a separate process (`acp-sign-in.ts`). The CLI opens the browser with `open`/`xdg-open` from PATH, so OpenBot prepends a private dir with shims that write the link to a file; it accepts only an `accounts.google.com` OAuth URL and registers it as a secret. On Windows the CLI calls `rundll32 url.dll,FileProtocolHandler` by absolute path, so no shim; remote relay not possible there. A status probe must NEVER call `authenticate`; auth is checked via `session/new` model discovery. `authRequired` ACP error means signed out.
- Reconnect of a connected account: ACP `logout` first (advertised in `agentCapabilities.auth.logout`; deletes `antigravity-oauth-token` and `cache/acp-auth-state.json`, does not revoke at Google), then `authenticate`.
- Token storage: file `<geminiHome>/antigravity-acp/antigravity-oauth-token`, JSON `{token:{access_token, token_type:"Bearer", refresh_token, expiry}, auth_method:"consumer", id_token_email}`; `settings.json` names the method; `cache/acp-auth-state.json {methodId}`. To force file storage instead of macOS Keychain/Secret Service every process gets `WSL_DISTRO_NAME=OpenBot` (`ANTIGRAVITY_ENV`).
- Legacy migration: 1.4 `acp_token.json` (refresh_token only) is copied once into the 1.5 file with an expired access token, plus marker `openbot-token-imported` (`src/backend/antigravity-token-import.ts`).
- Multi-account: extra account = own `GEMINI_HOME` folder passed as `--gemini-dir`; sandbox paths made by OpenBot; email read from `id_token_email` of the token file for duplicate detection. A saved Gemini API key (`GEMINI_API_KEY`, ACP method `gemini-api-key`) belongs to the first account only and wins over the Google sign-in; Reconnect/Disconnect are hidden while a key is in use.
- Models: the CLI lists the plan's models best first (`ownOrder`); first = default; efforts appear per model via the ACP `thought_level` config option (older 1.3 listed each effort as a separate model, e.g. `gemini-3.1-pro-high`; `acp-configuration.ts` ~748 collapses those). Models seen in docs include Gemini and the Claude and GPT-OSS models that Antigravity serves for some plans. Per-account `model/list` is read after connect because plans differ.
- Quota (not provided by CLI): `src/backend/antigravity-usage.ts`:
  - `POST https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist` returns the account project.
  - `POST https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary {project}` returns quota groups and buckets. Only the `daily-` host meters a Google account sign-in.
  - Headers: `Authorization: Bearer <access_token>` from the token file, and `User-Agent: antigravity/<desktop app version> <os>/<arch>` in Go platform names (403 without it; the "Gemini Models" group is sent only to the desktop app version). OpenBot does not refresh the token (it does not hold the CLI client secret): an expired token means no reading, and only a running CLI refreshes it. A model's reading takes the group whose "Models within this group" names it.
  - Test override env `OPENBOT_DEV_ANTIGRAVITY_CODE_ASSIST_URL` accepts loopback only.
- Failure text classification: `GEMINI_REQUEST_FAILURES` (rate limit, quota, unavailable model, service failure), "Usage Limit Reached" answer text with `⚠ ` prefix is a per-user quota that resets in days (`error.provider.antigravityQuotaReached`), a quota of 0 is not "wait".

## 6. OpenCode

- Runtime: `opencode acp` (ACP), pinned OpenCode 2 runtime, flavor in `src/backend/acp-flavor-opencode.ts`, config in `src/backend/opencode-config.ts`.
- No login step. The account is one key: spawn without a key lists only free models; with the key lists the paid Go catalog. Key goes ONLY into `OPENCODE_CONFIG_CONTENT` as `options.apiKey` of provider `opencode-go`, NEVER `OPENCODE_API_KEY` and never the free `opencode` provider (that broke the free tier's anonymous key, "provider authentication required" -32000, issue #7550). Signed-in state is derived from the models `session/new` returns. Env `OPENCODE_DISABLE_AUTOUPDATE=1` for managed installs. Key stored in `src/main/provider-credential-store.ts` (Electron `safeStorage`, 0600 envelope, status only crosses IPC).
- Custom endpoints (OpenAI-compatible, local) are injected as OpenCode providers in the same config content (`customProvidersOn`).
- Hardening via `OPENCODE_CONFIG` file in the provider state folder (denies `question`, MCP-resource tools, `report` and `opencode` skills; user global/project rules win). Steering mid-turn with `_session/steering` when advertised.
- Model order: free models first (`OPENCODE_RECOMMENDED_FREE_MODELS`), then OpenCode paid, then models behind a separate sign-in (`opencodeModelRank`). Fallback free list `OPENCODE_FREE_MODEL_FALLBACKS` is used only after authentication succeeded but discovery failed.
- Usage: `src/backend/opencode-usage.ts` (Go usage from the local SQLite db, read-only, 100 ms busy timeout).
- Error mapping: -32000 becomes actionable text naming the provider from the model id prefix before `/`; context-too-small regex; service failure -32603 variants; OpenCode 2 differences (`session not found` is -32602; `session/resume` may need a retry after 2 s).
- MCP: OpenBot runs stdio MCP servers itself and gives OpenCode an HTTP URL with bearer, shared per directory (`mcp-stdio-relay.ts`), because OpenCode 2 leaks stdio servers after `session/close`.

## 7. Hermes, Muse and the others

### Hermes
`hermes acp` (ACP), user's own install only (no managed runtime because a newer Hermes migrates `~/.hermes` forward). `HermesInstaller` (`src/main/hermes-installer.ts`) runs the official installer script pinned to a commit with a checked SHA-256, stage by stage (`--stage <name> --json`). Sign-in: `hermes auth add nous --type oauth` (Nous Portal device code); stdin closed (`closeInput`) so the "import shared credentials" prompt auto-answers yes; parse the `Open:` URL (not `Portal:`); sign-in file watched is `auth.json` mtime. Models are `provider:model` ids named `<Provider> · <model>`, grouped by provider. Settings injected through `HERMES_MANAGED_DIR` (`src/backend/hermes-settings.ts`) only when the user's `config.yaml` lacks them. Quirk: `session/resume` of a missing session creates a new one, so use `session/load` (`loadInsteadOfResume`). A failing turn can end as answer text `Error: ...`.

### Muse (`@muse-code/sdk` 1.4.2, MSP)
`muse serve --disable-shell --disable-write` over MSP (`src/backend/muse-client.ts`, `muse-protocol.ts`, `src/backend/agent/muse-login.ts`). Device sign-in: request `account/loginStart {type:"deviceCode"}` returns `{verificationUrl, userCode}` (https only enforced); the CLI saves the account when the user confirms on another device. Account probe: experimental `account/read` from a short-lived host (5 s deadline); a CLI without it stays "not verified". An explicit Meta API key is possible and goes through the encrypted credential store; with a saved key the sign-out is hidden. Profile generation is not possible on Muse (cannot be tool-free).

### Others (descriptors exist for): Grok (`GROK_HOME`, `login --device-auth`, `GROK_OAUTH2_REFERRER=openbot`), Cursor (extra accounts are API keys: `CURSOR_API_KEY`, empty `CURSOR_AUTH_TOKEN`, `AGENT_CLI_CREDENTIAL_STORE=memory`, needs CLI at least 2026.10.01; `ProviderKeyAccountSupport`), Kimi (region-aware sign-in), Devin, GitHub Copilot (`copilot --acp`), Mistral (`vibe-acp`, no extra accounts, global keychain item), Cline, Pi (terminal sign-in), Inception/DeepSeek/OpenRouter (key-only providers), custom ACP agents from the ACP registry (`https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json`).

Two account shapes (`ProviderAccountSupport` union in `provider-drivers.ts`): `ProviderHomeAccountSupport` (home-folder variable, browser sign-in) and `ProviderKeyAccountSupport` (stored API key per account, `keyEnvironment(key)`, optional `checkKey`). SeraBot should model both: OAuth accounts and API-key accounts per provider.

## 8. Cross-cutting runtime behavior worth copying

### Reasoning/thinking level
- One normalized scale per agent (`reasoningEffort`); protocol 8 levels `low, medium, high, xhigh, max`; `reasoning-efforts-v1` adds `none, minimal, ultra`. A host downgrades levels for older peers: none/minimal to low, ultra to max (`servers.md` ~584, `team-protocol/reasoning-efforts-v1.ts`).
- Each model carries `defaultReasoningEffort` and `supportedReasoningEfforts[]`. An effort unsupported by the chosen model is an error that names valid values, or resolves to the default. ACP agents expose efforts via config option category `thought_level` (`acp-configuration.ts`, `effortWireValue` maps OpenBot's name to the agent's own value, `reasoningEffortWireValues`). Some models encode effort in the id (e.g. `-high`, `-thinking`); `effortVariant` and `newestOfFamily` keep the variant when migrating to a newer model.
- Change takes effect at the next turn without replacing the session (Claude live setters).

### Backup models and failover (directly relevant to the user's request)
`providers.md` "Backup models and failover" (~line 1428):
- Per agent ordered list `AgentBackupModels` (max 8) of `{provider, model, account}`, plus an `automatic` flag, stored in `agent_json`.
- Two triggers: (1) plan usage limit holds the queue of every agent on that provider account and model until reset (`UsageLimitGate`, rechecked every 15 min, 30 s grace after reset); (2) provider error: `classifyFailure` in `turn-lifecycle.ts` yields `billing, provider_unavailable, rate_limit, authentication, network, timeout`. If the turn did no work yet (`!acted`) and a ready backup exists, the messages go back to the queue, the failed model is passed over for 10 min (`PROVIDER_FAILURE_HOLD_MS`), the agent moves to the first ready backup and the turn reruns silently. `usageLimitReturn` remembers the original model and the agent goes back when the hold ends (note `cause: "provider-error"`). A turn that already wrote text or ran a tool fails normally. Each message reruns at most once per failed model.
- Not covered by failover (gap SeraBot can fill): tasks, channel tasks, provider start failures, mid-turn failures.

### Account pool
`provider-account-pool.ts` (`pickPoolAccount`): use one account until a usage window nears its limit, then the next. Buffer = free percent kept per window: 15% for windows at most 24 h, 5% for longer, 10% if unknown. Among accounts that keep buffer, choose the one whose longest window resets first (its unused capacity is lost first); no reading sorts after accounts with a reading; if none keep buffer, pick the one with the most capacity left, ties keep the current account. Pools per provider with a `leftOut` list.

### Usage read pacing (`usage-read-gate.ts`)
One read at a time per provider/account; a reading answers for 4 min; after a failure wait 5, 10, 20, 40 min up to 1 h; credential change resets the gate. Needed because Claude's usage endpoint answers 429 for an hour when polled too often.

### Agent self-management tools (`docs/architecture/agents.md` ~893)
`openbot.create_agent` (name, instructions, optional provider/model/reasoningEffort; defaults to the caller's provider/model/effort; checks valid values first), `openbot.update_profile` (provider, model, reasoning effort, providerAccountId, backupModels `{automatic, order[]}`, access...), `openbot.update_agents` (batch with per-agent result, one failure does not undo the others), `openbot.read_agent`, `openbot.list_models` (per provider models with efforts and default; accounts listed with id, label, state, never email/credentials). Provider or account change waits until the agent is idle. This is the model for "the main bot manages other bots".

### Model migration when a model disappears
`moveAgentsOffUnlistedModels` (`src/backend/agent/custom-endpoints.ts`): same provider; prefers model whose effort the saved id names, else newest of the same family, else default or first.

### Idle process pool, history transfer
`idle-thread-pool.ts` closes idle sessions; `provider-history*.ts` and `agent-history-transfer.ts` transfer conversation text between providers when an agent switches provider (reasoning, diffs, images stay with the provider). Relevant to mid-task provider switching: only notes/text move.

### Security patterns
- Never hand the raw token to an agent: a loopback MCP OAuth proxy (`mcp-oauth-proxy.ts`) injects the token; bearer = HMAC(route, agent) with timing-safe compare; Host header restricted to 127.0.0.1/localhost.
- Secrets registered for log redaction (`registerSecretValue`), OAuth fetch wrapper `secureOAuthFetch` (1 MiB body, 20 s, public-IP checks for OAuth hosts).
- `AUTH_OVERRIDE_VARIABLES` list: a sign-in status is only trusted from the store if none of these env vars are set.
- Process confinement per provider state folder (`src/backend/process-confinement.ts`, `*StatePaths`).

## 9. How to implement in Rust in SeraBot

Constraints from `AGENTS.md`: per-user envelope-encrypted secrets (`user_secrets.rs`), all effects through `gateway`, every store method takes the acting user id, no self-signup, no local-process plugin runner without a new security design, secrets write-only, no logging of tokens.

### 9.1 Decide the credential model per provider (two tracks)

Track A, native OAuth in Rust (recommended for Claude, Codex/ChatGPT, Antigravity/Google, since SeraBot is a server with no per-user CLI):
- Implement a generic `OAuthProvider` trait in a new `crates/serabot/src/provider_auth/` module: `authorize_url(state, pkce)`, `exchange(code, verifier)`, `refresh(refresh_token)`, `revoke`. Use `oauth2` crate or hand-rolled reqwest; PKCE S256; `state` stored server-side; loopback or paste-code completion.
- OpenBot gives no endpoints/client IDs for these. Take the authorize/token URLs, public client IDs, scopes and redirect shapes from the Hiel Cowork source (`E:\repo\Hiel Cowork`) for Antigravity/Google and Muse (user said to follow it), and from the official Claude Code and Codex CLI behavior for Claude and Codex. Background knowledge, NOT verified from openbot and to be re-checked against Hiel Cowork or live CLI traffic before coding: Claude Code uses a public PKCE client at claude.ai authorize and an Anthropic OAuth token endpoint (scopes include `user:inference`, `user:profile`, `org:create_api_key`), with the result usable as a bearer for the Messages API behind the OAuth beta header; Codex uses auth.openai.com PKCE with loopback port 1455 and a device-code variant. Pin these in one `provider_auth/endpoints.rs` constants file with a comment on provenance; make them overridable only through a test-only env var restricted to loopback (copy `OPENBOT_DEV_ANTIGRAVITY_CODE_ASSIST_URL` rule).
- Redirect handling for a server: use the paste-code or device-code style (openbot's `paste` and `device` flows) rather than loopback on the server. For loopback-only providers use the relay idea: the admin's browser hits `http://127.0.0.1:<port>/...`, the desktop console forwards only the callback query to `POST /v1/provider-auth/{flow_id}/complete`; server validates `state`, single use, 10 min TTL, flow bound to the acting user (copy `sign-in-relay-host.ts` rules).
- Token storage: one envelope-encrypted blob per (user, provider, account_id): `{access_token, refresh_token, expires_at, scopes, email, org, plan}`. Refresh proactively (about 5 min before expiry) under a per-account async mutex so concurrent turns do not both spend a rotating refresh token (the exact bug openbot's `claude-stored-sign-in.ts` documents). On `invalid_grant` mark the account `needs_sign_in` and drop tokens, keep config; on network/5xx keep tokens (copy the MCP renewal table in `integrations.md`).

Track B, wrap the vendor CLI per user (what openbot does): only valid if SeraBot hosts CLIs under a per-user home folder (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `--gemini-dir`), which collides with the "no local-process runner" rule. Do not adopt without a security design. Keep it as the fallback for the desktop owner console only.

API-key accounts (all providers): `ProviderKeyAccountSupport` equivalent. Stored per (user, provider, account_id) in the envelope store, write-only, `check_key` optional, never shown.

### 9.2 Data model (SQLite, UID scoped)

```
provider_accounts(id TEXT PK, user_id, provider, kind CHECK(kind IN ('oauth','api_key')),
  label, email_hash_or_masked, status, plan, created_at, last_ok_at, last_error_code)
provider_account_secrets  -- via user_secrets.rs, key = "prov:{provider}:{account_id}"
provider_pools(user_id, provider, left_out_json)
bot_model_config(bot_id, user_id, provider, model, thinking, account_id,
  fallbacks_json /* [{provider, model, thinking, account_id}] max 8, openbot caps 8; user wants fallback 1, 2 */,
  auto_failover BOOL)
model_holds(user_id, provider, account_id, model, until, cause)
```
All Store methods take `uid` first (AGENTS.md). Cap accounts at 10 per provider per user like `MAX_PROVIDER_ACCOUNTS`.

### 9.3 Provider registry (copy the descriptor plus driver split)
`providers/registry.rs`: `ProviderDescriptor { id, display_name, auth_kinds: [Oauth, ApiKey, DeviceCode, PasteCode], supports_multi_account, efforts_scale, ... }` plus `trait ProviderDriver { async fn list_models(&Account) ; async fn stream_turn(...) ; fn classify_failure(&Error) -> FailureKind ; async fn read_usage(&Account) ; }`. A new provider = one descriptor + one driver impl; a compile-time exhaustive match over `ProviderId` replaces openbot's `satisfies Record<...>`.

Providers: `claude`, `codex`(OpenAI/ChatGPT), `antigravity` (Google), `opencode` (key + free tier), `hermes` (Nous), `muse`, `openrouter`/`deepseek` etc. as key-only. OpenCode follows openbot: one key, paid catalog only when the key is sent to the Go provider and never to the free tier.

### 9.4 Models and thinking level
- Normalize to `enum Effort { None, Minimal, Low, Medium, High, XHigh, Max, Ultra }`. Per-model `supported_efforts` and `default_effort` from the provider model list; map to provider wire values in the driver (Claude `effortLevel`, Codex `reasoningEffort`, ACP/Antigravity `thought_level` value names, id-suffix variants like `-thinking`/`-high`). Reject or clamp with an error that lists valid values.
- Per-bot default (provider, model, thinking) plus ordered fallbacks 1 and 2 (the user's wording; keep cap higher internally, like openbot's 8). Validate every fallback entry against the live catalog and the user's accounts when saved.
- Model catalog cache with refresh and `move_off_unlisted_model` logic (same family, same variant, newest).

### 9.5 Failover and auto-resume of a failed subagent
Port `classifyFailure` to `FailureKind { Billing, ProviderUnavailable, RateLimit, Authentication, Network, Timeout, ContextTooLarge, Other }` using status codes and provider error text (Gemini texts in section 5, OpenCode -32000 text, Claude 429 and `invalid_grant`). Rules:
1. Turn failed before any side effect (`!acted`): requeue the input, put `(provider, account, model)` in `model_holds` for 10 min, move the bot to the next ready fallback, rerun with no error banner; each message retries at most once per failed model.
2. Usage/plan limit with a reset time: hold until reset plus 30 s grace, recheck every 15 min, return to the primary afterward (`usage_limit_return`).
3. Account-level failover before model-level: try another account in the pool for the same provider/model (use `pickPoolAccount` rules: 15%/5%/10% buffers, earliest longest-window reset first).
4. Acted turn failed (tool calls already made): do NOT blindly replay external effects (AGENTS.md "Do not blindly replay external effects with unknown outcomes"). Instead the main bot gets a structured `SubagentFailed{cause, acted, last_checkpoint}` event and resumes the subagent with a summary prompt on the fallback, with side-effecting tools re-gated through approvals. This is the extension openbot lacks.
5. Subagent failure bubbling: the orchestrator ("main") bot can call a `bot_manage` tool to change a child's provider/model/thinking/fallbacks (mirror `openbot.update_profile`/`update_agents`, `list_models`, `read_agent`), applied when the child is idle, with the batch-result shape (per-agent ok/error). All through `gateway` with approval and audit; the manager may only touch bots owned by the same `uid`.

### 9.6 Multi-account UX and API
`/v1/providers`, `/v1/providers/{p}/accounts` (list/add/rename/remove, id and label and state only, never email or credentials in the agent-visible list), `/v1/providers/{p}/accounts/{id}/reconnect`, `/pool` (leftOut), `/usage`. Add-account flows: `start` returns `{flow_id, verification_url, user_code?}`, `status`, `complete {callback_query|code}`, `cancel`. Rate-limit usage polling with the `UsageReadGate` rules (4 min cache, backoff 5,10,20,40,60 min, reset on credential change).

### 9.7 Security checklist (map to AGENTS.md)
- Blocklist applies to model requests: run it in the provider layer for every provider.
- Never put a token in a prompt, log or error (register for redaction; error texts built from classified kinds, not raw upstream bodies; Google account emails stay out of logs).
- Provider HTTP egress through the existing egress list and public-IP pinning used by `plugin_runtime.rs`; only the fixed provider hosts are allowed.
- OAuth flows are admin-initiated for an invited user only; no open signup.
- Never expose raw OAuth tokens to bots or plugins; if a plugin needs an upstream token, use a loopback-style proxy that injects it (openbot `mcp-oauth-proxy.ts` pattern) behind gateway approval.

### 9.8 Test plan (synthetic fixtures only, per AGENTS.md)
Local fake authorization/token server for PKCE/device/paste flows, refresh rotation race test (two concurrent turns, one refresh), `invalid_grant` handling table, pool picker unit tests with synthetic usage windows, failover state machine tests (acted vs not acted, hold expiry, return to primary), effort clamp tests per provider, UID isolation tests (user B cannot read or use user A's account). Never claim live verification without an owner-approved real account.

### 9.9 Suggested phasing for v0.49
1. Registry + accounts + encrypted secrets + API-key accounts for all providers.
2. Fallback config per bot + failover state machine + holds.
3. Claude OAuth (paste-code), multi-account, usage read.
4. Codex device-code, Antigravity Google OAuth + quota, OpenCode key tier, Hermes/Muse.
5. Manager-bot tools (`list_models`, `update_bot_config`, `resume_subagent`) and UI (composer model/thinking/account picker, per-bot fallback editor, account pool panel).

## 10. Open items to verify before coding
- Exact Claude and Codex OAuth endpoints, client IDs and scopes: not in openbot; read Hiel Cowork source (user pointed to `E:\repo\Hiel Cowork`) for Antigravity/Muse and confirm Claude/Codex against current vendor CLI behavior and terms (`docs/provider-terms.md` in openbot discusses provider terms; read it before using consumer subscription tokens in a server product).
- Whether using a user's consumer Claude/ChatGPT/Google subscription token from a server-side multi-user agent is allowed by each vendor's terms (openbot keeps grants on the user's own computer; the server-side case differs).
- Antigravity quota read needs the CLI's client secret to refresh tokens, which openbot deliberately does not hold; SeraBot's native flow must hold its own client config.
