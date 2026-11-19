# Live step events (0.37)

The server streams one `turn.step` event per tool phase on `GET /v1/events`.
After a reload, `GET /v1/conversations/{id}/activity` returns the same events
for that conversation, oldest first (at most 200).

Existing fields stay. 0.37 adds `tool`, `input_summary`, and, when they
apply, `result_summary`, `error`, `duration_ms`, `cards`, and `plan`.
Clients that only read `phase`, `icon`, and `label` keep working.

## Phases

| `phase` | Meaning |
| --- | --- |
| `running` | The tool call started. |
| `progress` | The call returned and the work is still going (a generation, or a background media job). |
| `waiting_approval` | The owner must approve a destructive action. `approval_id` is set. |
| `done` | The call finished. |
| `failed` | The call failed, the owner denied it, or policy blocked the output. |

`running` is the started card. `done` is the finished card. `failed` is the
failed card.

## Payload

```json
{
  "task_id": "…",
  "turn_id": "…",
  "conversation_id": "dir:main",
  "bot": "legal",
  "call_id": "…",
  "step": 1,
  "phase": "done",
  "tool": "tavily_search",
  "icon": "search",
  "label": "Searching the web",
  "input_summary": "Searching the web",
  "result_summary": "Tavily search",
  "duration_ms": 840,
  "cards": [],
  "plan": {
    "goal": "Find the filing",
    "done": 1,
    "total": 3,
    "items": [{ "text": "Search", "status": "done" }]
  }
}
```

- `tool` is the tool name. `input_summary` is a short label built from the
  tool name and, at most, the host of a URL. Queries, file contents, and
  secrets are never copied into the event.
- `result_summary` is the first line of the tool text, at most 160
  characters, and only when it passes the content policy.
- `error` is a short message. If it looks like a credential, the event says
  `The step failed.` instead.
- `duration_ms` is how long the call took. A resumed approval has no duration.
- `cards` uses the schema in [Tool results](tool-results.md). Omitted when
  empty or when the policy rejects the card text.
- `plan` is the conversation's current goal and checklist. Omitted when there
  is no plan or the plan text is blocked.

A background media job emits another `turn.step` with `tool` `media_job`
and `call_id` equal to the job id when the file is saved or the job fails.
The older `media_job.created` and `media_job.updated` events are unchanged.

`help.queued` is emitted when `ask_teammate` queues a teammate. It carries
ids and bot names only.

`turn.refused` is an audit-only event. It is appended, and written to the audit
log, when a turn that ran no tools ends with a short reply that opens with a
refusal ("I can't help", "I'm unable to", "I don't have access to", and a few
similar phrases). It never changes or blocks the reply. The payload carries
`bot`, `turn_id`, `conversation_id`, the matched `phrase`, and a `snippet` of at
most 160 characters. Clients can ignore it.

## Orchestration events (0.50)

These ride the same `GET /v1/events` stream. Every payload is small and carries
ids and short codes only (no objective, reply or provider text), so a client can
coalesce a burst into one refresh and look the rest up by id.

| Event | Payload |
|---|---|
| `handoff.started` | `handoff_id`, `child_task_id`, `parent_task_id`, `to_bot`, `conversation_id` |
| `handoff.completed` | same as `handoff.started` |
| `handoff.failed` | the same ids plus `status` (`failed_resumable` or `failed`), `outcome_code`, `effects_unknown`, `resume_count` |
| `model.failover` | `conversation_id`, `task_id`, `bot`, `from` and `to` (`provider`, `model`, `account`), `reason` |
| `provider.hold_set` | `provider`, `account_id`, `model`, `kind`, `until` |
| `provider.hold_cleared` | `provider`, `account_id`, and `model`/`kind` when a hold expired |
| `task.waiting_model` | `task_id`, `turn_id`, `conversation_id`, `until` |

`handoff.queued` and `handoff.resumed` are unchanged. `route_switched` is still
emitted next to `model.failover`.

## Memory events (0.61)

`turn.memory` says which memories a turn used and saved. It is sent at most
twice per turn: once when the turn starts (`recalled`) and once after the
post-turn extraction (`saved`). It is not sent when a side is empty, and it
is returned by `/activity` together with `turn.step`.

```json
{
  "task_id": "…",
  "turn_id": "…",
  "conversation_id": "dir:main",
  "recalled": [{"id": "…", "kind": "fact", "origin": "explicit", "excerpt": "Prefers the 8am ferry"}],
  "saved": [{"id": "…", "kind": "fact", "origin": "extracted", "excerpt": "…", "pending": true}],
  "recalled_total": 1,
  "saved_total": 0
}
```

Each side lists at most 8 items, and each excerpt is at most 120 characters.
`excerpt` is left out when the outgoing-text scan rejects it. The desktop
draws these inside the reply with the same `turn_id`.
