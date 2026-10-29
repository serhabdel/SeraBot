# Model routing

SeraBot talks to OpenAI-compatible chat-completion endpoints. Routing is
configuration-driven; there is no automatic provider discovery, no automatic
cross-provider fallback, and no client-supplied endpoint.

## Providers and allowed models

`[llm]` is the reserved provider id `default`:

```toml
[llm]
base_url = "https://api.mistral.ai/v1"
model = "mistral-large-latest"
api_key_secret = "llm_api_key"        # name in the encrypted secret store
data_egress_approved = false
models = [
  "mistral-medium-latest",
  "mistral-small-latest",
  "codestral-latest",
]    # optional extra allowed models (no live catalog scrape)
```

Per-bot overrides (`model` in `bots/*.toml` or `llm.bot_models`) are also
allowed on the `default` provider and become catalog entries.

Additional profiles are explicit and must list every allowed model:

```toml
[llm.providers.secondary]
base_url = "https://api.example.com/v1"
api_key_secret = "secondary_llm_key"
models = ["example-model"]
data_egress_approved = false
```

At startup SeraBot loads one client per configured profile. A missing secret,
an empty model list, an unsafe endpoint, a reserved or malformed id, or a
client/endpoint mismatch is a startup error. `default` cannot be reused as a
profile id.

Endpoints must be HTTPS, except exact loopback HTTP for local fixtures and SSH
tunnels. Credentials, queries, fragments, redirects, and lookalike hosts such
as `127.0.0.1.example` are rejected.

## Consent

`data_egress_approved` is independent per provider and defaults to `false`.
Consent decisions are explicit configuration changes made by the owner on the
server. Selecting a profile in a client does **not** grant consent, and the
catalog shows whether each profile is approved. Without consent, selection and
generation fail closed.

Changing a conversation to another provider means that conversation's history
will be sent to that provider on future turns.

## Conversation selection

The owner selects a provider/model per conversation:

- Telegram: `/models`, `/providers` (same allowlist grouped by provider; `/providors` is accepted), `/model`, `/model auto`, `/model <provider> <model>`
- API: `GET /v1/models` and `PUT /v1/conversations/{id}/model`

Catalog responses list every configured pair as `provider / model` (API field
`entries`, plus grouped `providers`). Responses never include endpoints or
credentials. `selection` is the stored preference; `effective_selection` is the
concrete route used for the requested bot on the next turn.

Selection is stored server-side (`kv` key `model_selection:<conversation>`) and
validated against the configured allowlist before it is saved. Unknown
providers/models are rejected without changing the stored selection. Switching
does not affect other conversations.

### Default and autoselect

Requests without a stored selection use the bot's configured model on the
`default` provider when that model is allowed there; otherwise the configured
`[llm].model` on `default`.

`/model auto` (or API `{"provider":"auto"}`) stores explicit **auto** mode.
Auto never picks unapproved providers. On each turn SeraBot resolves auto
deterministically:

1. Bot `model` override in `bots/*.toml`, if that model exists on an approved
   provider (prefer `default`, then lowest provider id).
2. Else `llm.bot_models[<bot>]`, with the same provider preference.
3. Else `[llm].model` on `default` when approved.
4. Else the first approved `provider / model` in sorted provider id, then
   sorted model id order.

Auto mode re-resolves when bot defaults change; a fixed provider/model selection
does not.

### Model classes (orchestration handoffs)

Optional `[llm.classes]` maps logical names (`routine`, `synthesis`) to model ids
that must already appear in the configured allowlist. `ModelRouter::select_class`
picks the first egress-approved model in that class (preferring provider
`default`), then falls back to the autoselect order above when the class is
missing, empty, or has no approved models. Classes do not scrape provider
catalogs, bypass consent, or replace owner `/model` selections. See
[Orchestration](orchestration.md).

## Pinned turns

A route is pinned at enqueue time: provider, model, and a configuration
fingerprint over the endpoint, secret reference, and consent flag. All LLM calls
during that turn re-validate the pinned route. Consequently:

- Changing the conversation selection does not change queued, running, or
  resumed turns.
- Changing a provider endpoint, secret reference, or consent flag invalidates
  affected queued/resumed turns with a clear error; the owner must resubmit.
- Legacy queued turns without a pinned route fail closed instead of silently
  using the current selection.

Approval flow and gateway policy are unchanged: a provider change never bypasses
approvals, the blocklist, or audit.

## Policy and audit

Provider/model identifiers, the catalog, selections, tool specifications, and
message metadata are scanned by the content policy before crossing a boundary.
Catalog responses never include endpoints or credentials.

Audit records include:

- `model.selected` — provider/model for a conversation.
- `model.used` — provider/model and token usage for each LLM call.

Model content is not written to the audit log.

## Operational notes

- The catalog is not discovered from provider APIs; it is exactly the configured
  allowlist.
- Per-call auditing occurs after each model response, so usage totals are
  approximate if a call fails mid-stream.
- `max_output_tokens`, timeouts, retries, tool-round limits, and SSRF/fetch policy
  remain global under `[llm]` and gateway configuration.
- Large provider context windows (for example 1M tokens) are enforced by the API;
  SeraBot does not truncate prompts to a local input cap. Tune
  `max_output_tokens`, `[runtime].history_limit`, and tool limits for ~4 GB RAM
  hosts so concurrent turns stay within memory budget.
