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
models = ["mistral-medium-latest"]    # optional extra allowed models
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

- Telegram: `/models`, `/model`, `/model <provider> <model>`
- API: `GET /v1/models` and `PUT /v1/conversations/{id}/model`

Selection is stored server-side (`kv` key `model_selection:<conversation>`) and
validated against the configured allowlist before it is saved. Unknown
providers/models are rejected without changing the stored selection. Switching
does not affect other conversations.

Requests without a stored selection use the bot's configured model on the
`default` provider.

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
