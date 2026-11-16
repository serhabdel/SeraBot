# AI providers

How each provider connects, where its model list comes from, and which ones
have a real sign-in. Researched against each vendor's own documentation for
this release (0.21). A provider gets a sign-in button only when the vendor
documents a sign-in flow that a third-party desktop app can use. Everything
else takes an API key, and the console shows a "Get a key" link instead.

## Authentication

| Provider | How you connect | Notes |
|---|---|---|
| OpenRouter | Sign in (OAuth PKCE) or API key | [OAuth PKCE guide](https://openrouter.ai/docs/use-cases/oauth-pkce). No app registration and no client secret. The code is exchanged at `POST /api/v1/auth/keys` for a normal API key in the user's own account. The server does the exchange and stores the key sealed under the signed-in user. Callback is a `localhost` address with a free port. |
| Google Antigravity | Sign in (Google OAuth PKCE) | Per member, several accounts each (`docs/antigravity.md`). Cloud Code, no API key. The client secret is the server's (age store); each account has its own tokens, email and project. |
| Muse Code | Sign in (device code) | Per member, several accounts each. The server runs the device flow, mints the key with `onboard: true` once (retrying a 429 or 5xx after 5 s, then 10 s) and keeps Meta's identity token only to mint again and to read quota. `x-api-version: 1.0.0` goes on every call. No API key to paste. Chat uses the Responses transport (`POST https://api.meta.ai/v1/responses`, see "Wire formats"). |
| OpenAI | API key | Keys at platform.openai.com. ChatGPT sign-in is not offered to third-party apps. |
| Mistral AI | API key | console.mistral.ai. |
| Google Gemini | API key | Key from Google AI Studio, used on the OpenAI-compatible endpoint. Google's OAuth for the Gemini API needs a Google Cloud project per user, so it is not a one-click sign-in. |
| Anthropic (Claude) | API keys, several per member | The native Messages API (`POST https://api.anthropic.com/v1/messages`) with `x-api-key`. Each member adds as many keys as they like, each sealed under their own id. A Claude plan sign-in exists but is off by default and admin-only: see "Claude plan sign-in". |
| OpenCode Go | API key | `https://opencode.ai/zen/go/v1`, OpenAI-compatible. Every request carries `x-opencode-session: ses_<12 hex><14 base62>`, derived from the conversation so a retry reuses it. Usage meters: `GET /zen/go/v1/usage`. Key from `https://opencode.ai/auth`. |
| OpenCode Zen | API key | `https://opencode.ai/zen/v1`, plain OpenAI-compatible, pay as you go. No session header. The OpenCode Free lane is not supported. |
| Nous Portal (Hermes) | API key | Added as an `openai-compatible` provider. The desktop preset's base URL (`https://inference-api.nousresearch.com/v1`) is **not verified** against Nous's documentation by this release; check it before approving egress. No Nous sign-in is claimed. |
| xAI (Grok) | API key | console.x.ai. |
| DeepSeek | API key | platform.deepseek.com. |
| Groq | API key | console.groq.com. |
| Together AI | API key | api.together.ai. |
| Fireworks AI | API key | app.fireworks.ai. |
| Cerebras | API key | cloud.cerebras.ai. |
| Perplexity | API key | perplexity.ai account page. |
| Hugging Face | Access token (API key) | Fine-grained or read token with Inference Providers permission. Hugging Face has "Sign in with HF" with an `inference-api` scope, but it needs an OAuth app registered by the operator, so it is reported, not built. |
| Nebius Token Factory | API key | tokenfactory.nebius.com. |
| SambaNova Cloud | API key | cloud.sambanova.ai. |
| Moonshot (Kimi) | API key | platform.moonshot.ai. |
| MiniMax / MiniMax (China) | API key | Global and China consoles are separate. |
| Ollama / local OpenAI-compatible server | None | Your own server. A placeholder key is stored because the credential store keeps one per provider; Ollama ignores it. |
| Custom OpenAI-compatible | API key | Any `/v1/chat/completions` endpoint. HTTPS, or plain HTTP on exact loopback only. |

Checked and not implemented as a sign-in:

* GitHub Models: the REST API takes a token with the `models: read`
  permission (fine-grained PAT or GitHub App token). GitHub's OAuth device flow
  exists, but it needs a GitHub App registered by the operator, and the docs
  name no OAuth scope for inference. Left out until an operator wants it.
* Hugging Face "Sign in with HF": see the table. Needs an operator-registered app.
* Google Gemini OAuth: needs a Google Cloud project per user.

## Where model lists come from

The server asks each provider for its own model list with the signed-in
user's key, keeps the answer for about an hour per user and provider
(`catalog_ttl_secs`), and keeps only models that can chat.

| Provider | Request |
|---|---|
| Mistral | `GET https://api.mistral.ai/v1/models` (reads `capabilities` and `max_context_length`) |
| DeepSeek | `GET https://api.deepseek.com/models` |
| MiniMax | `GET https://api.minimax.io/v1/models` (China: `api.minimaxi.com`) |
| Perplexity | `GET https://api.perplexity.ai/v1/models` |
| Gemini | `GET https://generativelanguage.googleapis.com/v1beta/openai/models` (the `models/` prefix is removed) |
| Hugging Face | `GET https://router.huggingface.co/v1/models` |
| Nebius | `GET https://api.tokenfactory.nebius.com/v1/models` |
| SambaNova | `GET https://api.sambanova.ai/v1/models` |
| Anthropic | `GET https://api.anthropic.com/v1/models?limit=1000`, following `after_id` while `has_more` (at most 10 pages). Reads `display_name`, `max_input_tokens`, `max_tokens` and `capabilities` (thinking, image input) when present; the capability fields could not be checked against the current Anthropic documentation, so a missing field only means fewer tags |
| OpenCode Go / Zen | `GET https://opencode.ai/zen/go/v1/models` and `.../zen/v1/models` |
| OpenAI, Groq, Together, Fireworks, OpenRouter, xAI, Cerebras, Moonshot, custom | `GET {base_url}/models` |

What a list gives the console, when the provider returns it: a friendly name,
the context size, and tags (Vision, Tools, Reasoning, Free, Low cost). Tags
come only from fields the provider sends. Nothing is guessed from a model name.

Chat-capable filtering uses the provider's own flags first (Mistral
`completion_chat`, OpenRouter output modalities, Together `type`, Fireworks
`supports_chat`). When a list has none of those, ids that are clearly not chat
models (embeddings, moderation, speech, image generation, realtime) are dropped.

### Validation

A selection is checked against the same list the picker shows.

1. Live list for that user and provider, when the last fetch worked.
2. The configured allowlist, when the fetch failed, was never run, or the key
   was rejected.

A failed fetch drops the old live list for that user, so a provider that stops
answering falls back to its allowlist and never to a stale list. The provider
status is `ok`, `key_rejected` (HTTP 401 or 403), `unreachable` or
`unchecked`. Provider error bodies are never copied into errors or logs.

Data-egress consent still applies: nothing is fetched or sent to a provider
without it.

## Default provider

"Set as default" stores the signed-in user's own default route
(`PUT /v1/providers/default-route`). It applies to new conversations whose bot
does not name a model. Without one, the server default provider is the
starred one.

## Wire formats

Every request goes through the same gateway egress, blocklist and audit rules;
only the request and response shape differ.

* **Chat completions** (`POST {base}/chat/completions`): every provider not
  listed below.
* **Responses** (`POST {base}/responses` over SSE): Muse Code. `store: false`,
  the system messages in `instructions`, `function_call` and
  `function_call_output` items (a result whose call was never sent is dropped),
  no `temperature`, no `prompt_cache_key`, no `include`, no replayed reasoning
  items. Thinking is `reasoning: {effort, summary: "auto"}` (`max` is clamped to
  `high`; `off` omits it). A 400 that blames the reasoning block,
  `max_output_tokens`, a recursive tool schema (replaced by a flat one) or the
  hosted `web_search` tool is retried once with that part removed. Tool schemas
  go through a shared flattener (cycles become strings, arrays always get
  `items`). A 401 re-mints the Muse key (without `onboard`) when Meta's identity
  token is stored, otherwise the account is marked `needs_reauth`.
* **Anthropic Messages** (`POST {base}/messages` over SSE): `anthropic`.
  `anthropic-version: 2023-06-01`; `max_tokens` is always sent. Tool calls and
  results are `tool_use` and `tool_result` blocks, repaired so every call has
  exactly one result. Extended thinking is `thinking: {type: enabled,
  budget_tokens}` with low 4k, medium 10k, high 24k and xhigh 32k, clamped below
  `max_tokens` (omitted under 1024) and with no `temperature` while it is on.
  The signed `thinking` and `redacted_thinking` blocks of a reply that calls
  tools are stored with its first tool call and replayed unchanged on the next
  request of the loop; another provider never receives them, and a tool loop
  whose thinking cannot be replayed is sent without thinking. A 400 about
  thinking drops it and retries once; a "max_tokens: N > M" 400 lowers
  `max_tokens` to M and retries once. Errors map to the failover codes:
  `authentication_error` (key rejected, the account is parked as
  `needs_reauth`), `permission_error`, `not_found_error`, 413, `rate_limit_error`
  with `retry-after`, `overloaded_error` and 529. *Not verified:* whether newer
  Claude models take an `effort`-style parameter instead of `budget_tokens`; the
  current Anthropic documentation could not be read for this release, so only the
  budget form is sent.

## Claude plan sign-in

Off by default and admin-only (owner decision O1). Anthropic's terms do not let
a third-party product offer a claude.ai sign-in or route requests through plan
credentials, and the owner's HielCowork audit and openbot's notes both rate a
multi-user server as the risky shape. It is built and tested but ships off.

* It exists only for a provider of kind `anthropic` with `plan_oauth = true`
  under `[llm.providers.<id>]`, and only the server admin can start it. Members
  get a plain 403, and listings hide the sign-in from them. Turning the flag off
  again stops the stored plan accounts from being used (they are kept).
* `oauth_client_id_secret` names the age-store secret that holds the OAuth client
  id. The id is never committed.
* PKCE through the desktop's loopback `http://localhost:54545/callback` (fixed
  port), or a pasted `code#state`. The token exchange is JSON at
  `https://api.anthropic.com/v1/oauth/token`. No `claude-code` user agent is
  imitated and no CLI credential file is read.
* One account per Claude account: the fingerprint is
  `sha256("claude:account:<uuid>")`, and a login that does not say a stable
  account id is refused. Signing in twice updates the same row.
* Requests carry the bearer token plus `anthropic-beta: oauth-2025-04-20` over
  the Messages transport. The refresh token rotates on every refresh, so every
  refresh goes through the single-flight account refresher; `invalid_grant`
  parks the account in `needs_reauth` and keeps the row.
* The account is the admin's own. It is never shared: a provider grant shares
  only the legacy default key, and a plan token under the default row is refused.
* The providers list carries a `signin_risk` text for the page to show as a
  banner beside the sign-in.

None of this was run against a live account.
