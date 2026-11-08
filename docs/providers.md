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
| Google Antigravity | Sign in (Google OAuth PKCE) | Existing admin flow, Cloud Code. No API key. |
| Muse Code | Sign in (device code) | Existing admin flow. No API key to paste. |
| OpenAI | API key | Keys at platform.openai.com. ChatGPT sign-in is not offered to third-party apps. |
| Mistral AI | API key | console.mistral.ai. |
| Google Gemini | API key | Key from Google AI Studio, used on the OpenAI-compatible endpoint. Google's OAuth for the Gemini API needs a Google Cloud project per user, so it is not a one-click sign-in. |
| Anthropic | Not added | See "Not added" below. |
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

## Not added

* Anthropic: Claude's native Messages API is the supported way to use it.
  Anthropic's OpenAI-compatible endpoint is documented as a testing layer, not
  for production, and the router speaks chat completions only. Claude models
  are available through OpenRouter and Hugging Face today.
