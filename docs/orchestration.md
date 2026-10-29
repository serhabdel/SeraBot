# Orchestration

SeraBot is a single-operator pipeline: one orchestrator bot delegates bounded work
to specialists. There is no free-form agent-to-agent chat, no multi-user auth, and
no plugin marketplace. Examples below use synthetic ids only.

## Decisions

**Model routing:** keep the in-process `ModelRouter` allowlist documented in
[Model routing](model-routing.md). Do not fine-tune. Do not add LiteLLM,
RouteLLM, or a live OpenRouter catalog. Optional `[llm.classes]` maps logical
classes (`routine`, `synthesis`) to models already listed in configuration; it
does not replace the allowlist or consent gates.

**Orchestrator:** `main` is the only bot with `role = "orchestrator"`. It routes
owner traffic and may queue one audited handoff per turn to a specialist. It does
not receive specialist-only tools (maker, analyst, watcher pipelines stay on
those bots). Specialists cannot delegate.

**Handoffs:** delegation is one queue row, not a conversation between models.
The parent turn finishes and releases its worker permit before the child runs
(see [Runtime concurrency](#runtime-concurrency)). The child writes one labeled
assistant reply into the parent conversation. There is no second model call to
“discuss” that reply.

## Conversation scope

Conversations form a shallow tree (depth at most two). Scope ids use a prefix and
slug; legacy ids without a prefix remain valid.

| Kind | Id pattern | Accepts turns | Parent |
| --- | --- | --- | --- |
| section | `sec:<slug>` | no | none |
| group | `grp:<slug>` | no | a section |
| direct (single-bot) | `dir:<slug>` | yes | section or group |
| room (multi-bot) | `room:<slug>` | yes | section or group |
| legacy | e.g. `owner`, numeric chat id | yes | none (routes to `main`) |

Slug grammar: `^[a-z][a-z0-9-]{0,31}$`. Conversation ids may be up to 128
ASCII alphanumeric characters plus `_`, `-`, and `:`.

Sections and groups are containers only; turns are rejected when the stored kind
is `section` or `group` (enforced once conversation metadata is persisted).

## Handoff schema

A handoff is validated before enqueue. Fields:

| Field | Rules |
| --- | --- |
| `id` | Unique handoff id |
| `parent_task_id` | Parent turn/task id |
| `conversation_id` | Must pass conversation id validation |
| `from_bot` | Must be `main` |
| `to_bot` | Enabled specialist listed in `main.delegates` |
| `skill` | Optional; if set, must be on the target bot’s skill allowlist |
| `objective` | Non-empty, at most 2000 characters |
| `memory_ids` | At most five existing memory ids (orchestrator attaches; no invented ids) |
| `model_class` | `routine` or `synthesis`; resolved via `[llm.classes]` when configured |

Audit event: `handoff.queued` with canonical handoff arguments; `handoff.completed`
when the specialist turn finishes.

Child context: specialist system prompt, objective, and attached memories only —
not the parent tool trace. Completion posts one assistant message on the parent
`conversation_id` with `name` set to the specialist.

## Bot fleet configuration

In `bots/*.toml`:

```toml
role = "specialist"   # default
delegates = []        # specialists must leave empty
```

Orchestrator example (`bots/main.toml`):

```toml
name = "main"
role = "orchestrator"
delegates = ["analyst", "maker", "watcher"]
```

Startup validation (`load_bots`):

- Exactly one **enabled** orchestrator, and its name must be `main`.
- Every name in `main.delegates` must refer to another **enabled** bot.
- Specialists with non-empty `delegates` are rejected.
- Disabled bots (e.g. `legal`) cannot appear in `delegates`.

Bots load at process start; changing fleet files requires restart.

## Bot creation (planned)

Owner API will write `bots/<name>.toml` and `prompts/<name>.txt` from a fixed
template: `enabled = false`, `role = "specialist"`, tools and skills drawn only
from existing allowlists, policy scan on description. New bots are not added to
`main.delegates` automatically and are not live until restart. No model-chosen
paths.

## Runtime concurrency

`runtime` holds a concurrency permit for the whole `process_task` turn. The
parent must **not** await the child inside `process_task` while holding that
permit — that would deadlock when `max_concurrent_turns` is saturated. The
delegate tool enqueues the child and ends the parent turn before the child runs.

## Non-goals

- Free-form agent-to-agent messaging
- Hot-reloading bot definitions without restart
- Copying specialist tool transcripts into room/direct transcripts (only the
  final assistant text)
- Fine-tuned or locally hosted chat models on the deployment box
- Proxy routers with live provider catalog discovery

## Related documents

- [Model routing](model-routing.md) — allowlist, consent, autoselect, classes
- `AGENTS.md` — product boundary and security invariants
